//! `smbs upgrade` and `smbs version --check` against a loopback release
//! origin. The "release" is built from this test binary itself, so the
//! staged `--version` self-test passes for real.
#![cfg(feature = "fixture-origin")]

mod common;

use std::{collections::HashMap, io::Write, path::PathBuf, process::Command};

use common::{Fixture, response};
use serde_json::Value;
use sha2::{Digest, Sha256};

const CURRENT: &str = env!("CARGO_PKG_VERSION");
const OLD: &str = "0.0.1";
const EXE: &str = if cfg!(windows) { "smbs.exe" } else { "smbs" };

fn target_key() -> &'static str {
    if cfg!(windows) {
        "windows-x64-msvc"
    } else if cfg!(target_os = "macos") {
        "darwin-arm64"
    } else if cfg!(target_arch = "aarch64") {
        "linux-arm64-gnu"
    } else {
        "linux-x64-gnu"
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// An installer-style install of this test binary claiming to be `OLD`.
struct Install {
    root: PathBuf,
    bin: PathBuf,
}

impl Install {
    fn new(name: &str, with_receipt: bool) -> Self {
        let root = std::env::temp_dir().join(format!("smbs-upgrade-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(EXE);
        std::fs::copy(env!("CARGO_BIN_EXE_smbs"), &exe).unwrap();
        let install = Self { root, bin };
        if with_receipt {
            install.write_receipt(OLD, &hex(&std::fs::read(&exe).unwrap()));
        }
        install
    }

    fn exe(&self) -> PathBuf {
        self.bin.join(EXE)
    }

    fn write_receipt(&self, version: &str, sha256: &str) {
        let receipt = serde_json::json!({
            "schemaVersion": 1,
            "version": version,
            "target": target_key(),
            "executable": self.exe().to_str().unwrap(),
            "source": "https://github.com/cpaikr/smbs/releases",
            "sha256": sha256,
        });
        std::fs::write(self.bin.join(".smbs-receipt.json"), format!("{receipt}\n")).unwrap();
    }

    fn receipt(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.bin.join(".smbs-receipt.json")).unwrap())
            .unwrap()
    }

    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.bin)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn run(&self, fixture: &Fixture, running: &str, args: &[&str]) -> Value {
        self.run_with(fixture, running, &[], args)
    }

    fn run_with(
        &self,
        fixture: &Fixture,
        running: &str,
        env: &[(&str, &str)],
        args: &[&str],
    ) -> Value {
        let output = Command::new(self.exe())
            .args(args)
            .envs(env.iter().copied())
            .env("SMBS_FIXTURE_RELEASE_ORIGIN", &fixture.origin)
            .env("SMBS_FIXTURE_RUNNING_VERSION", running)
            .env("SMBS_CACHE_DIR", self.root.join("cache"))
            .output()
            .unwrap();
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.matches('\n').count(), 1, "{stdout}");
        let value: Value = serde_json::from_str(&stdout).unwrap();
        let expected = i32::from(value["ok"] != true);
        assert_eq!(output.status.code(), Some(expected), "{value}");
        value
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A release archive holding this test binary and a license.
fn archive(binary: &[u8]) -> (String, Vec<u8>) {
    let files: [(&str, &[u8]); 2] = [(EXE, binary), ("LICENSE.md", b"license")];
    if cfg!(windows) {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in files {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        ("zip".to_owned(), writer.finish().unwrap().into_inner())
    } else {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, bytes).unwrap();
        }
        (
            "tar.gz".to_owned(),
            builder.into_inner().unwrap().finish().unwrap(),
        )
    }
}

/// Routes for one published release `version` of this binary.
struct Release {
    version: String,
    sha256: Option<String>,
    include_platform: bool,
}

impl Release {
    fn of(version: &str) -> Self {
        Self {
            version: version.to_owned(),
            sha256: None,
            include_platform: true,
        }
    }

    fn serve(self) -> Fixture {
        let binary = std::fs::read(env!("CARGO_BIN_EXE_smbs")).unwrap();
        let (extension, bytes) = archive(&binary);
        let asset = format!("smbs-{}-{}.{extension}", self.version, target_key());
        let sha = self.sha256.unwrap_or_else(|| hex(&bytes));
        let targets = if self.include_platform {
            serde_json::json!({ target_key(): { "asset": asset, "sha256": sha } })
        } else {
            serde_json::json!({})
        };
        let manifest = serde_json::json!({
            "schemaVersion": 1,
            "version": self.version,
            "tag": format!("v{}", self.version),
            "targets": targets,
        });
        let routes = HashMap::from([
            (
                "/cpaikr/smbs/releases/latest/download/release-manifest.json".to_owned(),
                manifest.to_string().into_bytes(),
            ),
            (
                format!("/cpaikr/smbs/releases/download/v{}/{asset}", self.version),
                bytes,
            ),
        ]);
        serve(routes)
    }
}

fn serve(routes: HashMap<String, Vec<u8>>) -> Fixture {
    Fixture::start(move |path| match routes.get(path) {
        Some(body) => response("200 OK", "application/octet-stream", "", body),
        None => response("404 Not Found", "text/plain", "", b"not found"),
    })
}

#[test]
fn check_then_upgrade_replaces_the_binary_and_receipt() {
    let install = Install::new("happy", true);
    let fixture = Release::of(CURRENT).serve();

    let check = install.run(&fixture, OLD, &["upgrade", "--check"]);
    let result = &check["result"];
    assert_eq!(result["runningVersion"], OLD);
    assert_eq!(result["latestVersion"], CURRENT);
    assert_eq!(result["updateAvailable"], true);
    assert_eq!(result["updated"], false);
    assert_eq!(result["installable"], true);
    assert_eq!(result["target"], target_key());
    assert_eq!(
        fixture.paths().len(),
        1,
        "--check fetches only the manifest"
    );
    assert_eq!(
        install.files(),
        [".smbs-receipt.json", EXE],
        "--check creates nothing"
    );

    let upgrade = install.run(&fixture, OLD, &["upgrade"]);
    assert_eq!(upgrade["result"]["updated"], true, "{upgrade}");
    let receipt = install.receipt();
    assert_eq!(receipt["version"], CURRENT);
    assert_eq!(receipt["target"], target_key());
    assert_eq!(
        receipt["sha256"],
        hex(&std::fs::read(install.exe()).unwrap())
    );

    // Now current: nothing to do, and leftovers (on Windows, the old image)
    // are swept.
    let again = install.run(&fixture, CURRENT, &["upgrade"]);
    assert_eq!(again["result"]["updateAvailable"], false);
    assert_eq!(again["result"]["updated"], false);
    assert_eq!(
        install.files(),
        [".smbs-receipt.json", ".smbs-upgrade.lock", EXE]
    );

    let version = install.run(&fixture, CURRENT, &["version", "--check"]);
    assert_eq!(version["result"]["releaseCheck"]["comparison"], "equal");
}

#[test]
fn a_checksum_mismatch_changes_nothing() {
    let install = Install::new("checksum", true);
    let before = std::fs::read(install.exe()).unwrap();
    let fixture = Release {
        sha256: Some("0".repeat(64)),
        ..Release::of(CURRENT)
    }
    .serve();
    let value = install.run(&fixture, OLD, &["upgrade"]);
    assert_eq!(
        value["error"]["code"], "upgrade_verification_failed",
        "{value}"
    );
    assert_eq!(std::fs::read(install.exe()).unwrap(), before);
    assert_eq!(install.receipt()["version"], OLD);
    assert_eq!(
        install.files(),
        [".smbs-receipt.json", ".smbs-upgrade.lock", EXE]
    );
}

#[test]
fn a_candidate_reporting_the_wrong_version_is_rejected() {
    // Published as 999.0.0, but the binary reports CURRENT.
    let install = Install::new("self-test", true);
    let before = std::fs::read(install.exe()).unwrap();
    let fixture = Release::of("999.0.0").serve();
    let value = install.run(&fixture, OLD, &["upgrade"]);
    assert_eq!(
        value["error"]["code"], "upgrade_verification_failed",
        "{value}"
    );
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("999.0.0")
    );
    assert_eq!(std::fs::read(install.exe()).unwrap(), before);
    assert_eq!(install.receipt()["version"], OLD);
    assert_eq!(
        install.files(),
        [".smbs-receipt.json", ".smbs-upgrade.lock", EXE]
    );
}

#[test]
fn unmanaged_binaries_are_refused_before_any_request() {
    let fixture = Release::of(CURRENT).serve();

    let no_receipt = Install::new("no-receipt", false);
    let value = no_receipt.run(&fixture, OLD, &["upgrade", "--check"]);
    assert_eq!(value["error"]["code"], "unmanaged_installation");

    let wrong_hash = Install::new("wrong-hash", false);
    wrong_hash.write_receipt(OLD, &"0".repeat(64));
    let value = wrong_hash.run(&fixture, OLD, &["upgrade"]);
    assert_eq!(value["error"]["code"], "unmanaged_installation");

    let wrong_version = Install::new("wrong-version", true);
    let value = wrong_version.run(&fixture, "0.0.2", &["upgrade"]);
    assert_eq!(value["error"]["code"], "unmanaged_installation");

    assert_eq!(fixture.paths(), Vec::<String>::new());
}

#[test]
fn a_release_without_this_platform_is_reported() {
    let install = Install::new("platform", true);
    let fixture = Release {
        include_platform: false,
        ..Release::of(CURRENT)
    }
    .serve();
    // Newer, but not installable here: not reported as an available upgrade.
    let check = install.run(&fixture, OLD, &["upgrade", "--check"]);
    assert_eq!(check["result"]["latestVersion"], CURRENT);
    assert_eq!(check["result"]["updateAvailable"], false);
    assert_eq!(check["result"]["installable"], false);
    let value = install.run(&fixture, OLD, &["upgrade"]);
    assert_eq!(value["error"]["code"], "upgrade_release_invalid");
    let version = install.run(&fixture, OLD, &["version", "--check"]);
    assert_eq!(version["result"]["releaseCheck"]["installable"], false);
}

#[test]
fn releases_without_a_manifest_are_told_apart_from_none() {
    let install = Install::new("no-manifest", true);

    // A release from before self-upgrade: checksums but no manifest.
    let old_release = serve(HashMap::from([(
        "/cpaikr/smbs/releases/latest/download/SHA256SUMS".to_owned(),
        b"abc  smbs-0.2.0-x.zip\n".to_vec(),
    )]));
    let value = install.run(&old_release, OLD, &["upgrade"]);
    assert_eq!(value["error"]["code"], "upgrade_release_invalid");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("predates")
    );

    let nothing = serve(HashMap::new());
    let version = install.run(&nothing, OLD, &["version", "--check"]);
    assert_eq!(
        version["result"]["releaseCheck"]["comparison"],
        "no_stable_release"
    );
}

#[test]
fn an_unreachable_release_origin_is_retryable() {
    let install = Install::new("offline", true);
    // Bind and drop a listener to get a closed loopback port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let output = Command::new(install.exe())
        .args(["upgrade", "--check"])
        .env(
            "SMBS_FIXTURE_RELEASE_ORIGIN",
            format!("http://127.0.0.1:{port}"),
        )
        .env("SMBS_FIXTURE_RUNNING_VERSION", OLD)
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["error"]["code"], "upgrade_source_unavailable");
    assert_eq!(value["error"]["retryable"], true);
}

#[test]
fn a_failed_receipt_swap_restores_the_old_binary() {
    let install = Install::new("rollback", true);
    let receipt_before = std::fs::read(install.bin.join(".smbs-receipt.json")).unwrap();
    let fixture = Release::of(CURRENT).serve();
    let value = install.run_with(
        &fixture,
        OLD,
        &[("SMBS_FIXTURE_FAIL_AT", "receipt")],
        &["upgrade"],
    );
    assert_eq!(value["error"]["code"], "upgrade_failed", "{value}");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("restored")
    );
    assert_eq!(
        std::fs::read(install.bin.join(".smbs-receipt.json")).unwrap(),
        receipt_before
    );
    // The restored binary still runs and still matches its receipt.
    let check = install.run(&fixture, OLD, &["upgrade", "--check"]);
    assert_eq!(check["result"]["updateAvailable"], true, "{check}");
    // On Windows the old image may stay as .smbs-old-* until the next upgrade.
    let leftovers: Vec<String> = install
        .files()
        .into_iter()
        .filter(|name| {
            name.starts_with(".smbs-")
                && name != ".smbs-receipt.json"
                && name != ".smbs-upgrade.lock"
        })
        .collect();
    assert!(
        leftovers
            .iter()
            .all(|name| cfg!(windows) && name.starts_with(".smbs-old-")),
        "{leftovers:?}"
    );
}

#[test]
fn an_interrupted_upgrade_is_finished_by_the_next_run() {
    let install = Install::new("recovery", true);
    let fixture = Release::of(CURRENT).serve();
    // The receipt swap fails and so does putting the old binary back: the new
    // binary is in place with the old receipt.
    let value = install.run_with(
        &fixture,
        OLD,
        &[("SMBS_FIXTURE_FAIL_AT", "receipt,restore")],
        &["upgrade"],
    );
    assert_eq!(
        value["error"]["code"], "upgrade_recovery_required",
        "{value}"
    );
    assert!(
        value["error"]["recoveryHint"]
            .as_str()
            .unwrap()
            .contains("smbs upgrade")
    );
    assert_eq!(install.receipt()["version"], OLD);

    // The binary in place is CURRENT. `--check` repairs nothing; a real run
    // adopts the staged receipt under the lock and sweeps leftovers.
    let check = install.run(&fixture, CURRENT, &["upgrade", "--check"]);
    assert_eq!(check["error"]["code"], "unmanaged_installation");
    let again = install.run(&fixture, CURRENT, &["upgrade"]);
    assert_eq!(again["result"]["updateAvailable"], false, "{again}");
    assert_eq!(install.receipt()["version"], CURRENT);
    assert_eq!(
        install.files(),
        [".smbs-receipt.json", ".smbs-upgrade.lock", EXE]
    );
}

#[test]
fn a_concurrent_upgrade_is_refused_and_leftovers_wait_for_the_lock() {
    let install = Install::new("lock", true);
    let fixture = Release::of(CURRENT).serve();
    std::fs::write(
        install.bin.join(".smbs-new-1"),
        b"staged by another upgrade",
    )
    .unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(install.bin.join(".smbs-upgrade.lock"))
        .unwrap();
    lock.try_lock().unwrap();

    let value = install.run(&fixture, OLD, &["upgrade"]);
    assert_eq!(value["error"]["code"], "upgrade_failed", "{value}");
    assert_eq!(value["error"]["retryable"], true);
    assert!(
        install.bin.join(".smbs-new-1").exists(),
        "a locked directory is not swept"
    );
    assert_eq!(
        fixture.paths(),
        Vec::<String>::new(),
        "no download while locked"
    );

    drop(lock);
    let upgraded = install.run(&fixture, OLD, &["upgrade"]);
    assert_eq!(upgraded["result"]["updated"], true, "{upgraded}");
    assert!(!install.bin.join(".smbs-new-1").exists());
}
