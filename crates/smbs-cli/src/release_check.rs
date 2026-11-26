//! Update checks: `smbs version --check`, and the cached advisory added to
//! successful rate commands.
//!
//! Evidence about the latest release is cached per user and compared with the
//! running version on every use, so replacing the executable changes the
//! result immediately. Incidental checks refresh at most once per
//! [`FRESH_FOR`], wait [`RETRY_AFTER`] after a failed refresh, and spend at
//! most [`INCIDENTAL_BUDGET`] on the network. `SMBS_NO_UPDATE_CHECK` turns
//! them off. None of this can change a command's outcome.

use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::release::{self, Comparison, FetchError, GitHub, Release, Version};

const CACHE_FILE: &str = "release-check-v1.json";
pub const CACHE_DIR_ENV: &str = "SMBS_CACHE_DIR";
pub const OPT_OUT_ENV: &str = "SMBS_NO_UPDATE_CHECK";
/// Evidence younger than this is used without a refresh.
const FRESH_FOR: Duration = Duration::from_secs(24 * 3600);
/// After a failed refresh, incidental checks wait this long before retrying.
const RETRY_AFTER: Duration = Duration::from_secs(3600);
/// Network time an incidental refresh may add to a command.
const INCIDENTAL_BUDGET: Duration = Duration::from_millis(1500);
/// Explicit checks may wait longer; they exist to answer the question.
const EXPLICIT_BUDGET: Duration = Duration::from_secs(15);
/// Without a successful refresh for this long, incidental checks say so.
const STALE_NOTICE_AFTER: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Newer,
    Equal,
    Ahead,
    /// No stable release has been published yet.
    NoStableRelease,
    /// No usable release evidence.
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// Observed by this invocation or within the refresh interval.
    Fresh,
    /// The latest refresh failed; this is older evidence.
    Stale,
    Unavailable,
}

/// `result.releaseCheck` of `smbs version --check`. Problems are reported
/// here rather than failing the command.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseCheck {
    pub comparison: CheckOutcome,
    pub current_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_url: Option<String>,
    /// Whether the latest release has a binary for this platform.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installable: Option<bool>,
    pub freshness: Freshness,
    /// Seconds since the evidence was observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// An entry of the envelope's `advisories`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Advisory {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_url: Option<String>,
}

/// The cache file. Disposable: anything unreadable is treated as absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cache {
    schema_version: u32,
    target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_failure_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Evidence {
    /// Unix seconds.
    observed_at: u64,
    /// `None`: no stable release was published when observed.
    latest: Option<LatestRelease>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LatestRelease {
    version: String,
    release_url: String,
    installable: bool,
}

/// `smbs version --check`: always refreshes, falling back to cached evidence
/// (marked stale) when the refresh fails.
pub fn explicit_check() -> ReleaseCheck {
    let store = Store::locate();
    let mut cache = store.as_ref().map(Store::load).unwrap_or_default();
    let now = now();
    let refreshed = refresh(&mut cache, now, EXPLICIT_BUDGET);
    if let Some(store) = &store {
        store.save(&cache);
    }
    let fresh = refreshed.is_ok();
    report(&cache, now, fresh, refreshed.err())
}

/// The advisory for a successful rate command, if any. Quiet when the
/// evidence is fresh and shows no newer release.
pub fn incidental_advisory() -> Option<Advisory> {
    if opted_out() {
        return None;
    }
    let store = Store::locate()?;
    let mut cache = store.load();
    let now = now();
    let fresh = cache
        .evidence
        .as_ref()
        .is_some_and(|evidence| age(evidence.observed_at, now).is_some_and(|age| age < FRESH_FOR));
    let cooling_down = cache
        .last_failure_at
        .and_then(|at| age(at, now))
        .is_some_and(|age| age < RETRY_AFTER);
    let mut refreshed = fresh;
    if !fresh && !cooling_down {
        // Record the attempt first: if the process dies or the cache cannot be
        // written, the cooldown still stops a request on every run.
        cache.last_failure_at = Some(now);
        if store.save(&cache) {
            refreshed = refresh(&mut cache, now, INCIDENTAL_BUDGET).is_ok();
            store.save(&cache);
        }
    }
    advisory_for(&report(&cache, now, refreshed, None))
}

fn advisory_for(check: &ReleaseCheck) -> Option<Advisory> {
    if check.comparison == CheckOutcome::Newer {
        return Some(Advisory {
            code: "update_available",
            message: format!(
                "smbs {} is available (this is {}). Run `smbs upgrade`, or rerun the installer.",
                check.latest_version.as_deref().unwrap_or_default(),
                check.current_version
            ),
            release_url: check.release_url.clone(),
        });
    }
    let long_unchecked = check
        .age_seconds
        .is_none_or(|age| age >= STALE_NOTICE_AFTER.as_secs());
    (check.freshness != Freshness::Fresh && long_unchecked).then(|| Advisory {
        code: "update_check_unavailable",
        message: format!(
            "Could not check for smbs updates recently. Run `smbs version --check` for details, or set {OPT_OUT_ENV}=1 to stop these checks."
        ),
        release_url: None,
    })
}

/// Fetches the latest release into `cache`. On failure the previous evidence
/// is kept and the failure time recorded.
fn refresh(cache: &mut Cache, now: u64, budget: Duration) -> Result<(), String> {
    let latest = match GitHub::new().latest_within(budget) {
        Ok(Release {
            version,
            url,
            asset,
        }) => Some(LatestRelease {
            version: version.to_string(),
            release_url: url,
            installable: asset.is_some(),
        }),
        Err(FetchError::NoRelease) => None,
        Err(error) => {
            cache.last_failure_at = Some(now);
            return Err(error.to_string());
        }
    };
    cache.evidence = Some(Evidence {
        observed_at: now,
        latest,
    });
    cache.last_failure_at = None;
    Ok(())
}

fn report(cache: &Cache, now: u64, fresh: bool, problem: Option<String>) -> ReleaseCheck {
    let running = Version::running();
    let mut check = ReleaseCheck {
        comparison: CheckOutcome::Unavailable,
        current_version: running.to_string(),
        latest_version: None,
        release_url: None,
        installable: None,
        freshness: Freshness::Unavailable,
        age_seconds: None,
        problem,
    };
    let Some(evidence) = &cache.evidence else {
        return check;
    };
    let Some(age) = age(evidence.observed_at, now) else {
        return check;
    };
    check.age_seconds = Some(age.as_secs());
    check.freshness = if fresh {
        Freshness::Fresh
    } else {
        Freshness::Stale
    };
    match &evidence.latest {
        None => check.comparison = CheckOutcome::NoStableRelease,
        Some(latest) => {
            let Some(version) = Version::parse(&latest.version) else {
                return check;
            };
            check.comparison = match release::compare(running, version) {
                Comparison::Newer => CheckOutcome::Newer,
                Comparison::Equal => CheckOutcome::Equal,
                Comparison::Ahead => CheckOutcome::Ahead,
            };
            check.latest_version = Some(latest.version.clone());
            check.release_url = Some(latest.release_url.clone());
            check.installable = Some(latest.installable);
        }
    }
    check
}

fn opted_out() -> bool {
    std::env::var_os(OPT_OUT_ENV).is_some_and(|value| !value.is_empty() && value != "0")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// `None` for a timestamp in the future, which is not trusted.
fn age(at: u64, now: u64) -> Option<Duration> {
    now.checked_sub(at).map(Duration::from_secs)
}

/// The per-user cache file for this platform's release evidence.
struct Store {
    path: PathBuf,
    target: String,
}

impl Store {
    fn locate() -> Option<Self> {
        let directory = match std::env::var_os(CACHE_DIR_ENV) {
            Some(directory) => Some(PathBuf::from(directory)).filter(|path| path.is_absolute()),
            None => default_cache_directory(),
        }?;
        Some(Self {
            path: directory.join(CACHE_FILE),
            target: release::target_key()
                .unwrap_or(env!("SMBS_BUILD_TARGET"))
                .to_owned(),
        })
    }

    fn load(&self) -> Cache {
        let empty = Cache {
            schema_version: 1,
            target: self.target.clone(),
            ..Cache::default()
        };
        std::fs::read(&self.path)
            .ok()
            .filter(|bytes| bytes.len() <= 64 << 10)
            .and_then(|bytes| serde_json::from_slice::<Cache>(&bytes).ok())
            .filter(|cache| cache.schema_version == 1 && cache.target == self.target)
            .unwrap_or(empty)
    }

    /// Replaces the cache file atomically; returns whether it was written.
    fn save(&self, cache: &Cache) -> bool {
        use std::io::Write as _;
        let Some(directory) = self.path.parent() else {
            return false;
        };
        let written = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(directory)?;
            let mut file = tempfile::NamedTempFile::new_in(directory)?;
            file.write_all(&serde_json::to_vec(cache).expect("the cache serializes"))?;
            file.persist(&self.path).map_err(|error| error.error)?;
            Ok(())
        })();
        written.is_ok()
    }
}

fn default_cache_directory() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if cfg!(windows) {
        absolute("LOCALAPPDATA").map(|root| root.join("smbs").join("cache"))
    } else if cfg!(target_os = "macos") {
        absolute("HOME").map(|root| root.join("Library/Caches/smbs"))
    } else {
        absolute("XDG_CACHE_HOME")
            .map(|root| root.join("smbs"))
            .or_else(|| absolute("HOME").map(|root| root.join(".cache/smbs")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_with(latest: Option<&str>, observed_at: u64) -> Cache {
        Cache {
            schema_version: 1,
            target: "t".to_owned(),
            evidence: Some(Evidence {
                observed_at,
                latest: latest.map(|version| LatestRelease {
                    version: version.to_owned(),
                    release_url: format!("https://example.invalid/v{version}"),
                    installable: true,
                }),
            }),
            last_failure_at: None,
        }
    }

    #[test]
    fn reports_compare_cached_evidence_with_the_running_version() {
        let running = Version::running().to_string();
        let newer = "999.0.0";

        let check = report(&cache_with(Some(newer), 100), 160, true, None);
        assert_eq!(check.comparison, CheckOutcome::Newer);
        assert_eq!(check.freshness, Freshness::Fresh);
        assert_eq!(check.age_seconds, Some(60));
        assert_eq!(advisory_for(&check).unwrap().code, "update_available");

        let check = report(&cache_with(Some(&running), 100), 160, false, None);
        assert_eq!(check.comparison, CheckOutcome::Equal);
        assert_eq!(check.freshness, Freshness::Stale);
        // Recently checked and current: quiet even though this refresh failed.
        assert!(advisory_for(&check).is_none());
        let week = STALE_NOTICE_AFTER.as_secs();
        let check = report(&cache_with(Some(&running), 100), 100 + week, false, None);
        assert_eq!(
            advisory_for(&check).unwrap().code,
            "update_check_unavailable"
        );

        let check = report(&cache_with(None, 100), 160, true, None);
        assert_eq!(check.comparison, CheckOutcome::NoStableRelease);
        assert!(advisory_for(&check).is_none());

        // Evidence from the future is not trusted.
        let check = report(&cache_with(Some(newer), 500), 160, true, None);
        assert_eq!(check.comparison, CheckOutcome::Unavailable);
        assert_eq!(check.freshness, Freshness::Unavailable);

        let check = report(&Cache::default(), 160, false, Some("offline".to_owned()));
        assert_eq!(check.comparison, CheckOutcome::Unavailable);
        assert_eq!(check.problem.as_deref(), Some("offline"));
    }

    #[test]
    fn the_cache_round_trips_and_rejects_other_targets() {
        let directory =
            std::env::temp_dir().join(format!("smbs-release-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let store = Store {
            path: directory.join(CACHE_FILE),
            target: "t".to_owned(),
        };
        assert_eq!(store.load().evidence, None);
        let cache = cache_with(Some("9.9.9"), 42);
        assert!(store.save(&cache));
        assert_eq!(store.load(), cache);
        let other = Store {
            path: store.path.clone(),
            target: "other".to_owned(),
        };
        assert_eq!(other.load().evidence, None);
        std::fs::write(&store.path, b"not json").unwrap();
        assert_eq!(store.load().evidence, None);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
