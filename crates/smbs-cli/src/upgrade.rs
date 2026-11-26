//! `smbs upgrade`: replace an installer-managed executable with the latest
//! verified release, or restore the previous one.
//!
//! Only an executable described by its `.smbs-receipt.json` is managed: the
//! receipt's version, target, path, and SHA-256 must match the running file.
//! Builds from source, copies, and package-manager installs are refused before
//! any network request.

use std::{
    fs::{File, OpenOptions, TryLockError},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CliError, CliErrorCode,
    release::{self, Comparison, FetchError, GitHub, Release, Version},
};

pub const RECEIPT_NAME: &str = ".smbs-receipt.json";
pub const RECEIPT_SOURCE: &str = "https://github.com/cpaikr/smbs/releases";
const LOCK_NAME: &str = ".smbs-upgrade.lock";
const RECEIPT_LIMIT: u64 = 64 << 10;
const ARCHIVE_LIMIT: u64 = 64 << 20;
const EXECUTABLE_LIMIT: u64 = 128 << 20;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const SELF_TEST_TIMEOUT: Duration = Duration::from_secs(10);
const EXECUTABLE_NAME: &str = if cfg!(windows) { "smbs.exe" } else { "smbs" };
/// Prefixes of files a run creates beside the executable; swept on the next run.
const LEFTOVER_PREFIXES: [&str; 3] = [".smbs-new-", ".smbs-old-", ".smbs-receipt-new-"];

/// `.smbs-receipt.json`, written by the installers and by `smbs upgrade`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Receipt {
    pub schema_version: u32,
    pub version: String,
    pub target: String,
    pub executable: String,
    /// Where the binary came from; always the GitHub releases of this repo.
    pub source: String,
    pub sha256: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeResult {
    running_version: String,
    latest_version: String,
    /// A newer release exists and has a binary for this platform.
    update_available: bool,
    /// Whether the latest release has a binary for this platform.
    installable: bool,
    updated: bool,
    target: &'static str,
    release_url: String,
    executable: String,
}

/// An executable this tool may replace, as established by its receipt.
struct Installation {
    executable: PathBuf,
    receipt_path: PathBuf,
}

/// Human progress on stderr, only when stderr is a terminal.
struct Progress(bool);

impl Progress {
    fn new() -> Self {
        use std::io::IsTerminal;
        Self(std::io::stderr().is_terminal())
    }

    fn stage(&self, line: &str) {
        if self.0 {
            let _ = writeln!(std::io::stderr(), "{line}");
        }
    }
}

/// Runs `smbs upgrade [--check]`.
///
/// `--check` is read-only: it creates, removes, and locks nothing. A real
/// upgrade takes the install directory's lock before downloading and inspects
/// the installation again under it, so concurrent upgrades cannot act on a
/// stale view.
///
/// # Errors
/// A [`CliError`] with one of the upgrade codes; see the module docs.
pub fn run(check_only: bool) -> Result<UpgradeResult, CliError> {
    let target = release::target_key().ok_or_else(|| {
        error(
            CliErrorCode::UpgradeUnsupported,
            format!(
                "No prebuilt smbs binaries exist for {}.",
                env!("SMBS_BUILD_TARGET")
            ),
            "Build the new version from source with cargo.",
        )
    })?;
    let running = Version::running();
    let executable = running_executable()?;
    let directory = executable
        .parent()
        .ok_or_else(|| unmanaged("The executable has no parent directory.".to_owned()))?
        .to_path_buf();
    let first_look = Installation::inspect(&executable, running, target, false);
    let (installation, lock) = if check_only {
        (first_look?, None)
    } else {
        // An unmanaged binary is refused before any file is created, unless an
        // interrupted upgrade left a receipt that may describe it.
        if first_look.is_err() && staged_receipts(&directory).is_empty() {
            return Err(first_look.err().expect("checked above"));
        }
        let lock = UpgradeLock::acquire(&directory)?;
        let installation = Installation::inspect(&executable, running, target, true)?;
        remove_leftovers(&directory);
        (installation, Some(lock))
    };

    let progress = Progress::new();
    progress.stage("Checking for updates...");
    let github = GitHub::new();
    let latest = github.latest().map_err(source_error)?;
    let newer = release::compare(running, latest.version) == Comparison::Newer;
    let mut result = UpgradeResult {
        running_version: running.to_string(),
        latest_version: latest.version.to_string(),
        update_available: newer && latest.asset.is_some(),
        installable: latest.asset.is_some(),
        updated: false,
        target,
        release_url: latest.url.clone(),
        executable: plain(&installation.executable),
    };
    if !newer || check_only {
        return Ok(result);
    }
    let Some(asset) = latest.asset.clone() else {
        return Err(error(
            CliErrorCode::UpgradeReleaseInvalid,
            format!("Release v{} has no binary for {target}.", latest.version),
            "Wait for a release that includes this platform, or build from source.",
        ));
    };

    progress.stage(&format!("Downloading smbs {}...", latest.version));
    let archive = github
        .download(
            latest.version,
            &asset.asset,
            ARCHIVE_LIMIT,
            DOWNLOAD_TIMEOUT,
        )
        .map_err(source_error)?;
    progress.stage("Verifying...");
    if hex(&Sha256::digest(&archive)) != asset.sha256 {
        return Err(verification(format!(
            "{} does not match the SHA-256 in the release manifest.",
            asset.asset
        )));
    }
    let binary = extract(&archive, &asset.asset)?;

    progress.stage("Installing...");
    installation.replace(&binary, &latest, target)?;
    drop(lock);
    progress.stage(&format!("Updated smbs {running} -> {}.", latest.version));
    result.updated = true;
    Ok(result)
}

fn unmanaged(message: String) -> CliError {
    error(
        CliErrorCode::UnmanagedInstallation,
        message,
        "smbs upgrade only replaces binaries installed by the smbs installer. Reinstall with the one-line installer from the README, or upgrade the way you installed it.",
    )
}

fn running_executable() -> Result<PathBuf, CliError> {
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| unmanaged(format!("Cannot locate the running executable: {error}.")))
}

/// The install directory's exclusive upgrade lock, held until dropped.
struct UpgradeLock(#[allow(dead_code)] File);

impl UpgradeLock {
    fn acquire(directory: &Path) -> Result<Self, CliError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join(LOCK_NAME))
            .map_err(|error| failed(format!("Cannot create the upgrade lock: {error}.")))?;
        match file.try_lock() {
            Ok(()) => Ok(Self(file)),
            Err(TryLockError::WouldBlock) => {
                let mut error = error(
                    CliErrorCode::UpgradeFailed,
                    "Another smbs upgrade is running.".to_owned(),
                    "Wait for it to finish, then run `smbs version`.",
                );
                error.retryable = true;
                Err(error)
            }
            Err(TryLockError::Error(error)) => Err(failed(format!(
                "Cannot lock the install directory: {error}."
            ))),
        }
    }
}

impl Receipt {
    fn new(version: Version, target: &str, executable: &Path, sha256: String) -> Self {
        Self {
            schema_version: 1,
            version: version.to_string(),
            target: target.to_owned(),
            executable: plain(executable),
            source: RECEIPT_SOURCE.to_owned(),
            sha256,
        }
    }

    /// Whether this receipt describes `executable` (canonical) running as
    /// `version` on `target`, with contents hashing to `sha256`.
    fn describes(&self, executable: &Path, version: Version, target: &str, sha256: &str) -> bool {
        self.schema_version == 1
            && self.version == version.to_string()
            && self.target == target
            && self.source == RECEIPT_SOURCE
            && self.sha256 == sha256
            && std::fs::canonicalize(&self.executable).ok().as_deref() == Some(executable)
    }
}

impl Installation {
    /// Establishes that the receipt beside `executable` describes it. With
    /// `repair` (only under the lock), a receipt staged by an upgrade that was
    /// interrupted after swapping the binary is accepted and moved into place.
    fn inspect(
        executable: &Path,
        running: Version,
        target: &str,
        repair: bool,
    ) -> Result<Self, CliError> {
        let directory = executable
            .parent()
            .ok_or_else(|| unmanaged("The executable has no parent directory.".to_owned()))?;
        let receipt_path = directory.join(RECEIPT_NAME);
        let sha256 = read_capped(executable, EXECUTABLE_LIMIT)
            .map(|bytes| hex(&Sha256::digest(bytes)))
            .map_err(|error| unmanaged(format!("Cannot read the running executable: {error}.")))?;
        let parse = |path: &Path| {
            read_capped(path, RECEIPT_LIMIT)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Receipt>(&bytes).ok())
        };
        let installation = Self {
            executable: executable.to_path_buf(),
            receipt_path: receipt_path.clone(),
        };
        if parse(&receipt_path)
            .is_some_and(|receipt| receipt.describes(executable, running, target, &sha256))
        {
            return Ok(installation);
        }
        if repair {
            for staged in staged_receipts(directory) {
                if parse(&staged)
                    .is_some_and(|receipt| receipt.describes(executable, running, target, &sha256))
                    && std::fs::rename(&staged, &receipt_path).is_ok()
                {
                    return Ok(installation);
                }
            }
        }
        let message = if receipt_path.exists() {
            format!(
                "The receipt at {} does not describe the running smbs {running}.",
                plain(&receipt_path)
            )
        } else {
            format!(
                "{} has no installer receipt ({RECEIPT_NAME}).",
                plain(executable)
            )
        };
        Err(unmanaged(message))
    }

    /// Stages the new binary and receipt beside the old ones, then swaps them
    /// in, restoring the old binary if any step fails. The caller holds the
    /// upgrade lock.
    fn replace(&self, binary: &[u8], latest: &Release, target: &str) -> Result<(), CliError> {
        let directory = self
            .executable
            .parent()
            .expect("inspected executables have a parent");
        let pid = std::process::id();
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let staged = directory.join(format!(".smbs-new-{pid}{suffix}"));
        let staged_receipt = directory.join(format!(".smbs-receipt-new-{pid}.json"));
        let backup = directory.join(format!(".smbs-old-{pid}{suffix}"));
        let cleanup = || {
            let _ = std::fs::remove_file(&staged);
            let _ = std::fs::remove_file(&staged_receipt);
        };

        let staging = (|| -> Result<(), CliError> {
            write_new(&staged, binary, true)
                .map_err(|error| failed(format!("Cannot write the new executable: {error}.")))?;
            // A shell started by a packaged Windows app can silently redirect
            // writes under %LOCALAPPDATA% to private storage, so other
            // programs would never see the new binary.
            let staged_parent = std::fs::canonicalize(&staged)
                .ok()
                .and_then(|path| path.parent().map(Path::to_path_buf));
            if staged_parent.as_deref() != Some(directory) {
                return Err(error(
                    CliErrorCode::UpgradeUnsupported,
                    format!(
                        "Writes to {} are redirected, so an upgrade would not be visible to other programs.",
                        plain(directory)
                    ),
                    "Run smbs upgrade from a regular terminal, or rerun the installer there.",
                ));
            }
            self_test(&staged, latest.version)?;
            let receipt = Receipt::new(
                latest.version,
                target,
                &self.executable,
                hex(&Sha256::digest(binary)),
            );
            let mut bytes = serde_json::to_vec(&receipt).expect("receipts serialize");
            bytes.push(b'\n');
            write_new(&staged_receipt, &bytes, false)
                .map_err(|error| failed(format!("Cannot write the new receipt: {error}.")))
        })();
        if let Err(error) = staging {
            cleanup();
            return Err(error);
        }

        // 1. Keep the old executable under a backup name. A running Windows
        //    executable can be renamed but not overwritten. On Unix a hard
        //    link (or, where links are unsupported, a copy) keeps the
        //    original path in place until the atomic rename.
        let set_aside = inject("set-aside").and_then(|()| {
            if cfg!(windows) {
                std::fs::rename(&self.executable, &backup)
            } else {
                std::fs::hard_link(&self.executable, &backup)
                    .or_else(|_| std::fs::copy(&self.executable, &backup).map(drop))
            }
        });
        if let Err(error) = set_aside {
            cleanup();
            return Err(failed(format!(
                "Cannot set the old executable aside: {error}."
            )));
        }
        // 2. New executable in.
        if let Err(error) =
            inject("executable").and_then(|()| std::fs::rename(&staged, &self.executable))
        {
            cleanup();
            if cfg!(windows) {
                // The old binary was renamed away; it must come back.
                let restored =
                    inject("restore").and_then(|()| std::fs::rename(&backup, &self.executable));
                return Err(self.rollback_error(
                    &format!("Cannot move the new executable into place: {error}."),
                    restored.err(),
                    &backup,
                ));
            }
            // On Unix the old binary never left its path.
            let _ = std::fs::remove_file(&backup);
            return Err(failed(format!(
                "Cannot move the new executable into place: {error}. The previous smbs is unchanged."
            )));
        }
        // 3. New receipt in. A failure puts the old executable back.
        if let Err(error) =
            inject("receipt").and_then(|()| std::fs::rename(&staged_receipt, &self.receipt_path))
        {
            let restored =
                inject("restore").and_then(|()| std::fs::rename(&backup, &self.executable));
            let _ = std::fs::remove_file(&staged);
            if restored.is_ok() {
                let _ = std::fs::remove_file(&staged_receipt);
            }
            // Otherwise the staged receipt is kept: it describes the new binary
            // now in place, and the next `smbs upgrade` adopts it.
            return Err(self.rollback_error(
                &format!("Cannot write the new receipt: {error}."),
                restored.err(),
                &backup,
            ));
        }
        sync_directory(directory);
        // 4. Drop the backup. On Windows the running image stays locked until
        //    this process exits; the next upgrade sweeps it.
        let _ = std::fs::remove_file(&backup);
        Ok(())
    }

    fn rollback_error(
        &self,
        cause: &str,
        restore_error: Option<std::io::Error>,
        backup: &Path,
    ) -> CliError {
        match restore_error {
            None => failed(format!("{cause} The previous smbs was restored.")),
            Some(restore) => error(
                CliErrorCode::UpgradeRecoveryRequired,
                format!("{cause} Restoring the previous smbs also failed: {restore}."),
                &format!(
                    "Run `smbs upgrade` again to finish, or rename {} to {}, or reinstall with the one-line installer from the README.",
                    plain(backup),
                    plain(&self.executable)
                ),
            ),
        }
    }
}

/// Receipts staged by earlier upgrades in `directory`.
fn staged_receipts(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(".smbs-receipt-new-") && name.ends_with(".json")
        })
        .map(|entry| entry.path())
        .collect()
}

/// Test hook: fixture builds fail the named replacement steps listed in
/// `SMBS_FIXTURE_FAIL_AT` (comma-separated), to exercise rollback.
#[cfg_attr(
    not(feature = "fixture-origin"),
    allow(clippy::unnecessary_wraps, reason = "fails only in fixture builds")
)]
fn inject(step: &str) -> std::io::Result<()> {
    #[cfg(feature = "fixture-origin")]
    if std::env::var("SMBS_FIXTURE_FAIL_AT")
        .is_ok_and(|steps| steps.split(',').any(|listed| listed == step))
    {
        return Err(std::io::Error::other(format!("injected failure at {step}")));
    }
    let _ = step;
    Ok(())
}

/// Extracts the executable from a release archive, which must contain exactly
/// the executable and `LICENSE.md` at its root.
fn extract(archive: &[u8], name: &str) -> Result<Vec<u8>, CliError> {
    let entries = if Path::new(name).extension().is_some_and(|ext| ext == "zip") {
        zip_entries(archive)
    } else {
        tar_entries(archive)
    }
    .map_err(|error| verification(format!("{name} could not be read: {error}.")))?;
    let mut names: Vec<&str> = entries.iter().map(|(entry, _)| entry.as_str()).collect();
    names.sort_unstable();
    let mut expected = [EXECUTABLE_NAME, "LICENSE.md"];
    expected.sort_unstable();
    if names != expected {
        return Err(verification(format!(
            "{name} has unexpected contents: {}.",
            names.join(", ")
        )));
    }
    Ok(entries
        .into_iter()
        .find(|(entry, _)| entry == EXECUTABLE_NAME)
        .map(|(_, bytes)| bytes)
        .expect("checked above"))
}

fn tar_entries(archive: &[u8]) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut entries = Vec::new();
    for entry in tar.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            return Err(std::io::Error::other("an entry is not a regular file"));
        }
        let path = entry.path()?.to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        (&mut entry)
            .take(EXECUTABLE_LIMIT + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > EXECUTABLE_LIMIT {
            return Err(std::io::Error::other("an entry is too large"));
        }
        entries.push((path, bytes));
    }
    Ok(entries)
}

fn zip_entries(archive: &[u8]) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    let mut zip =
        zip::ZipArchive::new(std::io::Cursor::new(archive)).map_err(std::io::Error::other)?;
    let mut entries = Vec::new();
    for index in 0..zip.len() {
        let mut file = zip.by_index(index).map_err(std::io::Error::other)?;
        if !file.is_file() {
            return Err(std::io::Error::other("an entry is not a regular file"));
        }
        let path = file.name().map_err(std::io::Error::other)?.into_owned();
        let mut bytes = Vec::new();
        (&mut file)
            .take(EXECUTABLE_LIMIT + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > EXECUTABLE_LIMIT {
            return Err(std::io::Error::other("an entry is too large"));
        }
        entries.push((path, bytes));
    }
    Ok(entries)
}

/// Runs the staged executable's `--version`, which must print exactly
/// `smbs <version>` within the deadline.
fn self_test(staged: &Path, version: Version) -> Result<(), CliError> {
    let mut child = Command::new(staged)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| verification(format!("The new executable does not start: {error}.")))?;
    let started = Instant::now();
    loop {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        if started.elapsed() > SELF_TEST_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(verification(
                "The new executable did not answer --version in time.".to_owned(),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child
        .wait_with_output()
        .map_err(|error| verification(format!("Running the new executable failed: {error}.")))?;
    let printed = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || printed.trim_end() != format!("smbs {version}") {
        return Err(verification(format!(
            "The new executable reports \"{}\", not smbs {version}.",
            printed.trim_end()
        )));
    }
    Ok(())
}

/// A path for messages and receipts, without the `\\?\` prefix that
/// canonicalization adds to ordinary drive paths on Windows. Other verbatim
/// forms (UNC, volume GUIDs) are kept as they are.
fn plain(path: &Path) -> String {
    use std::path::{Component, Prefix};
    let text = path.display().to_string();
    match path.components().next() {
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::VerbatimDisk(_)) => {
            text.get(4..).map_or(text.clone(), str::to_owned)
        }
        _ => text,
    }
}

fn remove_leftovers(directory: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if LEFTOVER_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn write_new(path: &Path, bytes: &[u8], executable: bool) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    file.sync_all()
}

fn sync_directory(directory: &Path) {
    #[cfg(unix)]
    if let Ok(directory) = File::open(directory) {
        let _ = directory.sync_all();
    }
    #[cfg(not(unix))]
    let _ = directory;
}

fn read_capped(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(std::io::Error::other(
            "not a regular file of a supported size",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn source_error(error: FetchError) -> CliError {
    match error {
        FetchError::Unavailable(message) => {
            let mut error = self::error(
                CliErrorCode::UpgradeSourceUnavailable,
                message,
                "Check the network connection and try again later.",
            );
            error.retryable = true;
            error
        }
        FetchError::NoRelease => self::error(
            CliErrorCode::UpgradeReleaseInvalid,
            error.to_string(),
            "No release to upgrade to yet.",
        ),
        FetchError::Invalid(message) => self::error(
            CliErrorCode::UpgradeReleaseInvalid,
            message,
            "Reinstall with the one-line installer from the README.",
        ),
    }
}

fn verification(message: String) -> CliError {
    error(
        CliErrorCode::UpgradeVerificationFailed,
        message,
        "Nothing was changed. Try again later, or reinstall with the one-line installer.",
    )
}

fn failed(message: String) -> CliError {
    error(
        CliErrorCode::UpgradeFailed,
        message,
        "Check that the install directory is writable, then try again.",
    )
}

fn error(code: CliErrorCode, message: String, hint: &str) -> CliError {
    CliError {
        code,
        message,
        retryable: false,
        parameter: None,
        source_url: None,
        recovery_hint: Some(hint.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, *bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in files {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn archives_must_hold_exactly_the_executable_and_license() {
        let good: &[(&str, &[u8])] = &[(EXECUTABLE_NAME, b"binary"), ("LICENSE.md", b"license")];
        assert_eq!(
            extract(&tar_gz(good), "smbs-1.0.0-x.tar.gz").unwrap(),
            b"binary"
        );
        assert_eq!(extract(&zip(good), "smbs-1.0.0-x.zip").unwrap(), b"binary");
        let only_binary: &[(&str, &[u8])] = &[(EXECUTABLE_NAME, b"binary")];
        let extra: &[(&str, &[u8])] = &[
            (EXECUTABLE_NAME, b"binary"),
            ("LICENSE.md", b"x"),
            ("extra", b"x"),
        ];
        let nested: &[(&str, &[u8])] = &[("bin/smbs", b"binary"), ("LICENSE.md", b"x")];
        for files in [only_binary, extra, nested] {
            let error = extract(&tar_gz(files), "a.tar.gz").unwrap_err();
            assert_eq!(error.code, CliErrorCode::UpgradeVerificationFailed);
        }
        let error = extract(b"not an archive", "a.tar.gz").unwrap_err();
        assert_eq!(error.code, CliErrorCode::UpgradeVerificationFailed);
    }

    #[test]
    fn receipts_reject_unknown_fields() {
        let receipt = Receipt {
            schema_version: 1,
            version: "0.3.0".to_owned(),
            target: "linux-x64-gnu".to_owned(),
            executable: "/home/u/.local/bin/smbs".to_owned(),
            source: RECEIPT_SOURCE.to_owned(),
            sha256: "a".repeat(64),
        };
        let json = serde_json::to_string(&receipt).unwrap();
        // The installers write these keys in this order.
        assert_eq!(
            json,
            format!(
                r#"{{"schemaVersion":1,"version":"0.3.0","target":"linux-x64-gnu","executable":"/home/u/.local/bin/smbs","source":"{RECEIPT_SOURCE}","sha256":"{}"}}"#,
                "a".repeat(64)
            )
        );
        assert_eq!(serde_json::from_str::<Receipt>(&json).unwrap(), receipt);
        let extra = json.replacen('{', r#"{"manager":"x","#, 1);
        assert!(serde_json::from_str::<Receipt>(&extra).is_err());
    }

    #[test]
    fn plain_paths_drop_only_the_local_verbatim_prefix() {
        assert_eq!(
            plain(Path::new(r"\\?\C:\Users\u\smbs.exe")),
            r"C:\Users\u\smbs.exe"
        );
        assert_eq!(
            plain(Path::new(r"\\?\UNC\server\share\smbs.exe")),
            r"\\?\UNC\server\share\smbs.exe"
        );
        assert_eq!(
            plain(Path::new("/usr/local/bin/smbs")),
            "/usr/local/bin/smbs"
        );
    }

    #[test]
    fn leftovers_are_swept_but_other_files_are_kept() {
        let directory =
            std::env::temp_dir().join(format!("smbs-upgrade-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        for name in [
            ".smbs-new-1",
            ".smbs-old-2.exe",
            ".smbs-receipt-new-3.json",
            "smbs",
            RECEIPT_NAME,
        ] {
            std::fs::write(directory.join(name), b"x").unwrap();
        }
        remove_leftovers(&directory);
        let mut left: Vec<String> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, [RECEIPT_NAME, "smbs"]);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
