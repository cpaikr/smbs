//! GitHub release metadata: the platform key, version ordering, and the
//! `release-manifest.json` published with every release.

use std::{cmp::Ordering, collections::BTreeMap, fmt, time::Duration};

use serde::{Deserialize, Serialize};

pub const REPOSITORY: &str = "cpaikr/smbs";
const MANIFEST_NAME: &str = "release-manifest.json";
const MANIFEST_LIMIT: u64 = 64 << 10;
const METADATA_TIMEOUT: Duration = Duration::from_secs(15);
const USER_AGENT: &str = concat!(
    "smbs/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/cpaikr/smbs)"
);

/// The release-target key for this build, or `None` for a platform without
/// published binaries (it can still be built from source).
pub fn target_key() -> Option<&'static str> {
    match env!("SMBS_BUILD_TARGET") {
        "x86_64-unknown-linux-gnu" => Some("linux-x64-gnu"),
        "aarch64-unknown-linux-gnu" => Some("linux-arm64-gnu"),
        "aarch64-apple-darwin" => Some("darwin-arm64"),
        "x86_64-pc-windows-msvc" => Some("windows-x64-msvc"),
        _ => None,
    }
}

/// A stable `MAJOR.MINOR.PATCH` version, ordered numerically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let mut next = || {
            parts
                .next()
                .filter(|part| {
                    !part.is_empty()
                        && part.bytes().all(|b| b.is_ascii_digit())
                        && (part.len() == 1 || !part.starts_with('0'))
                })
                .and_then(|part| part.parse().ok())
        };
        let version = Self(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }

    /// The version of this executable. Fixture builds can pretend to be
    /// older so upgrade tests have something to upgrade from.
    pub fn running() -> Self {
        #[cfg(feature = "fixture-origin")]
        if let Some(version) = std::env::var("SMBS_FIXTURE_RUNNING_VERSION")
            .ok()
            .and_then(|text| Self::parse(&text))
        {
            return version;
        }
        Self::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is MAJOR.MINOR.PATCH")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// `release-manifest.json`, written by `scripts/release/assemble.sh`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema_version: u32,
    pub version: String,
    pub tag: String,
    pub targets: BTreeMap<String, ManifestAsset>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestAsset {
    pub asset: String,
    pub sha256: String,
}

/// A manifest that passed validation for one platform.
#[derive(Debug, Clone)]
pub struct Release {
    pub version: Version,
    pub url: String,
    /// `None` when the release has no binary for this platform.
    pub asset: Option<ManifestAsset>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    /// A newer stable release exists.
    Newer,
    Equal,
    /// This executable is newer than the latest release (a development build).
    Ahead,
}

pub fn compare(running: Version, latest: Version) -> Comparison {
    match latest.cmp(&running) {
        Ordering::Greater => Comparison::Newer,
        Ordering::Equal => Comparison::Equal,
        Ordering::Less => Comparison::Ahead,
    }
}

/// Why release metadata could not be used.
#[derive(Debug)]
pub enum FetchError {
    /// Network failure, timeout, or unexpected status: worth retrying later.
    Unavailable(String),
    /// No stable release has been published yet.
    NoRelease,
    /// The metadata exists but is malformed or inconsistent.
    Invalid(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) | Self::Invalid(message) => f.write_str(message),
            Self::NoRelease => f.write_str("No stable smbs release has been published yet."),
        }
    }
}

/// The public page for a release.
pub fn release_url(version: Version) -> String {
    format!("https://github.com/{REPOSITORY}/releases/tag/v{version}")
}

/// HTTPS client for GitHub release downloads.
pub struct GitHub {
    agent: ureq::Agent,
    origin: String,
}

impl GitHub {
    pub fn new() -> Self {
        let origin = fixture_origin().unwrap_or_else(|| "https://github.com".to_owned());
        let tls = ureq::tls::TlsConfig::builder()
            // The OS certificate store, so TLS-inspecting proxies trusted by
            // the machine keep working.
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let config = ureq::Agent::config_builder()
            .https_only(!origin.starts_with("http://127.0.0.1:"))
            .tls_config(tls)
            .user_agent(USER_AGENT)
            .timeout_connect(Some(Duration::from_secs(10)))
            .build();
        Self {
            agent: config.into(),
            origin,
        }
    }

    /// Fetches and validates the latest release manifest. GitHub's
    /// `releases/latest` link excludes drafts and prereleases and needs no
    /// API token.
    pub fn latest(&self) -> Result<Release, FetchError> {
        let url = format!(
            "{}/{REPOSITORY}/releases/latest/download/{MANIFEST_NAME}",
            self.origin
        );
        let Some(bytes) = self.get(&url, MANIFEST_LIMIT, METADATA_TIMEOUT)? else {
            // Releases before 0.3.0 have no manifest; tell them apart from
            // "nothing published" by the checksum file every release has.
            let sums = format!(
                "{}/{REPOSITORY}/releases/latest/download/SHA256SUMS",
                self.origin
            );
            return Err(match self.get(&sums, MANIFEST_LIMIT, METADATA_TIMEOUT)? {
                Some(_) => FetchError::Invalid(
                    "The latest release has no release-manifest.json (it predates self-upgrade); reinstall with the installer from the README.".to_owned(),
                ),
                None => FetchError::NoRelease,
            });
        };
        let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|error| {
            FetchError::Invalid(format!("The release manifest is invalid: {error}."))
        })?;
        validate(&manifest)
    }

    /// Downloads one release asset with a size cap and an overall deadline.
    pub fn download(
        &self,
        version: Version,
        asset: &str,
        limit: u64,
        timeout: Duration,
    ) -> Result<Vec<u8>, FetchError> {
        let url = format!(
            "{}/{REPOSITORY}/releases/download/v{version}/{asset}",
            self.origin
        );
        self.get(&url, limit, timeout)?.ok_or_else(|| {
            FetchError::Invalid(format!("{asset} is missing from release v{version}."))
        })
    }

    /// `Ok(None)` for 404; other non-200 statuses are `Unavailable`.
    fn get(&self, url: &str, limit: u64, timeout: Duration) -> Result<Option<Vec<u8>>, FetchError> {
        let mut response = self
            .agent
            .get(url)
            .config()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .build()
            .call()
            .map_err(|error| {
                FetchError::Unavailable(format!("Contacting GitHub failed: {error}."))
            })?;
        match response.status().as_u16() {
            200 => {}
            404 => return Ok(None),
            status => {
                return Err(FetchError::Unavailable(format!(
                    "GitHub returned HTTP status {status}."
                )));
            }
        }
        response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map(Some)
            .map_err(|error| match error {
                ureq::Error::BodyExceedsLimit(_) => {
                    FetchError::Invalid(format!("A release file is larger than {limit} bytes."))
                }
                other => {
                    FetchError::Unavailable(format!("Downloading from GitHub failed: {other}."))
                }
            })
    }
}

/// Checks a manifest's schema, version, tag, and this platform's entry.
fn validate(manifest: &Manifest) -> Result<Release, FetchError> {
    let invalid = |message: String| Err(FetchError::Invalid(message));
    if manifest.schema_version != 1 {
        return invalid(format!(
            "Unsupported release manifest schema {}; upgrade by rerunning the installer.",
            manifest.schema_version
        ));
    }
    let Some(version) = Version::parse(&manifest.version) else {
        return invalid(format!(
            "The latest release has an invalid version \"{}\".",
            manifest.version
        ));
    };
    if manifest.tag != format!("v{version}") {
        return invalid(format!(
            "Release tag {} does not match version {version}.",
            manifest.tag
        ));
    }
    let asset = match target_key() {
        Some(key) => manifest.targets.get(key).cloned(),
        None => None,
    };
    if let (Some(asset), Some(key)) = (&asset, target_key()) {
        let extension = if key.starts_with("windows") {
            "zip"
        } else {
            "tar.gz"
        };
        let expected = format!("smbs-{version}-{key}.{extension}");
        let hex = asset.sha256.len() == 64
            && asset
                .sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
        if asset.asset != expected || !hex {
            return invalid(format!(
                "The release manifest entry for {key} is malformed."
            ));
        }
    }
    Ok(Release {
        version,
        url: release_url(version),
        asset,
    })
}

/// A loopback release origin for offline tests; production builds always use
/// `https://github.com`.
fn fixture_origin() -> Option<String> {
    #[cfg(feature = "fixture-origin")]
    if let Ok(origin) = std::env::var("SMBS_FIXTURE_RELEASE_ORIGIN") {
        assert!(
            origin.starts_with("http://127.0.0.1:"),
            "SMBS_FIXTURE_RELEASE_ORIGIN must be a loopback origin"
        );
        return Some(origin);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_strict_and_numeric() {
        assert!(Version::parse("0.10.0").unwrap() > Version::parse("0.9.9").unwrap());
        assert_eq!(Version::parse("1.2.3").unwrap().to_string(), "1.2.3");
        for bad in [
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.2.3-rc1",
            "v1.2.3",
            "",
            "1..3",
            "1.2.x",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        let v = |text| Version::parse(text).unwrap();
        assert_eq!(compare(v("0.2.0"), v("0.3.0")), Comparison::Newer);
        assert_eq!(compare(v("0.3.0"), v("0.3.0")), Comparison::Equal);
        assert_eq!(compare(v("0.4.0"), v("0.3.0")), Comparison::Ahead);
    }

    #[test]
    fn manifests_are_validated_for_this_platform() {
        let key = target_key().expect("tests run on a release platform");
        let extension = if key.starts_with("windows") {
            "zip"
        } else {
            "tar.gz"
        };
        let manifest = |version: &str, tag: &str, asset: &str, sha: &str| Manifest {
            schema_version: 1,
            version: version.to_owned(),
            tag: tag.to_owned(),
            targets: BTreeMap::from([(
                key.to_owned(),
                ManifestAsset {
                    asset: asset.to_owned(),
                    sha256: sha.to_owned(),
                },
            )]),
        };
        let sha = "a".repeat(64);
        let good = format!("smbs-0.3.0-{key}.{extension}");
        let release = validate(&manifest("0.3.0", "v0.3.0", &good, &sha)).unwrap();
        assert_eq!(release.version.to_string(), "0.3.0");
        assert_eq!(
            release.url,
            "https://github.com/cpaikr/smbs/releases/tag/v0.3.0"
        );
        assert!(release.asset.is_some());
        for (version, tag, asset, sha) in [
            ("0.3", "v0.3", good.as_str(), sha.as_str()),
            ("0.3.0", "v0.3.1", good.as_str(), sha.as_str()),
            ("0.3.0", "v0.3.0", "smbs-0.2.0-other.zip", sha.as_str()),
            ("0.3.0", "v0.3.0", good.as_str(), "XYZ"),
        ] {
            assert!(
                matches!(
                    validate(&manifest(version, tag, asset, sha)),
                    Err(FetchError::Invalid(_))
                ),
                "{version} {tag} {asset} {sha}"
            );
        }
        let mut other_platform = manifest("0.3.0", "v0.3.0", &good, &sha);
        other_platform.targets = BTreeMap::new();
        assert!(validate(&other_platform).unwrap().asset.is_none());
    }
}
