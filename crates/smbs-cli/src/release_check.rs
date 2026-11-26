//! Update checks: `smbs version --check`, and the cached advisory added to
//! successful rate commands.
//!
//! Evidence about the latest release is cached per user and compared with the
//! running version on every use, so replacing the executable changes the
//! result immediately. Incidental checks refresh at most once per
//! [`FRESH_FOR`], at most once per [`RETRY_AFTER`] after an attempt that did
//! not succeed (once per [`FRESH_FOR`] after a persistent problem), and never
//! keep a command waiting longer than [`INCIDENTAL_BUDGET`]: the refresh runs
//! on a worker thread that is abandoned at the deadline, which also bounds
//! TLS certificate revocation lookups that the HTTP timeouts cannot.
//! `SMBS_NO_UPDATE_CHECK` turns them off. None of this can change a command's
//! outcome.

use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::release::{self, Comparison, FetchError, GitHub, Release, Version};

const CACHE_FILE: &str = "release-check-v1.json";
pub const CACHE_DIR_ENV: &str = "SMBS_CACHE_DIR";
pub const OPT_OUT_ENV: &str = "SMBS_NO_UPDATE_CHECK";
/// Evidence younger than this is used without a refresh.
const FRESH_FOR: Duration = Duration::from_secs(24 * 3600);
/// After an attempt that did not succeed, incidental checks wait this long.
const RETRY_AFTER: Duration = Duration::from_secs(3600);
/// The longest an incidental check may delay a command.
const INCIDENTAL_BUDGET: Duration = Duration::from_millis(1500);
/// Explicit checks may wait longer; they exist to answer the question.
const EXPLICIT_BUDGET: Duration = Duration::from_secs(15);
/// Project decision: an unreachable release check is reported in incidental
/// notices only after this long without a successful refresh, so machines
/// that are often offline are not told on every command. `version --check`
/// always reports it.
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
    /// Observed within the refresh interval.
    Fresh,
    /// Older evidence: the latest refresh failed or has not completed.
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
    pub freshness: Freshness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
}

/// The cache file. Disposable: anything unreadable is treated as absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cache {
    schema_version: u32,
    target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<Evidence>,
    /// When an incidental refresh last started; recorded before fetching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_attempt_at: Option<u64>,
    /// When a refresh last failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_failure_at: Option<u64>,
    /// A failure that retrying soon will not fix, such as an unsupported
    /// manifest; shown immediately and retried once per [`FRESH_FOR`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_problem: Option<String>,
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
/// (marked stale) when the refresh fails. Cache problems are reported too.
pub fn explicit_check() -> ReleaseCheck {
    let now = now();
    let mut problems = Vec::new();
    let store = Store::locate()
        .map_err(|problem| problems.push(problem))
        .ok();
    let mut cache = store.as_ref().map(Store::load).unwrap_or_default();
    if let Err(problem) = refresh(&mut cache, now, EXPLICIT_BUDGET) {
        problems.push(problem);
    }
    if let Some(store) = &store
        && let Err(problem) = store.save(&mut cache)
    {
        problems.push(problem);
    }
    let mut check = report(&cache, now);
    check.problem = (!problems.is_empty()).then(|| problems.join(" "));
    check
}

/// The advisory for a successful rate command, if any. Quiet when the
/// evidence is fresh and shows no newer release.
pub fn incidental_advisory() -> Option<Advisory> {
    if opted_out() {
        return None;
    }
    let store = match Store::locate() {
        Ok(store) => store,
        Err(problem) => return Some(problem_advisory(&problem, None)),
    };
    let mut cache = store.load();
    let now = now();
    let fresh = cache
        .evidence
        .as_ref()
        .and_then(|evidence| age(evidence.observed_at, now))
        .is_some_and(|age| age < FRESH_FOR);
    let retry_after = if cache.last_problem.is_some() {
        FRESH_FOR
    } else {
        RETRY_AFTER
    };
    let cooling_down = cache
        .last_attempt_at
        .max(cache.last_failure_at)
        .and_then(|at| age(at, now))
        .is_some_and(|age| age < retry_after);
    if !fresh && !cooling_down {
        // Record the attempt first: if the process dies, the deadline
        // abandons the worker, or another process starts meanwhile, the
        // cooldown still stops a request on every run.
        cache.last_attempt_at = Some(now);
        if let Err(problem) = store.save(&mut cache) {
            return Some(problem_advisory(&problem, Some(&report(&cache, now))));
        }
        let (sender, receiver) = mpsc::channel();
        let mut working = cache.clone();
        std::thread::spawn(move || {
            let _ = refresh(&mut working, now, INCIDENTAL_BUDGET);
            let _ = store.save(&mut working);
            let _ = sender.send(working);
        });
        if let Ok(updated) = receiver.recv_timeout(INCIDENTAL_BUDGET) {
            cache = updated;
        }
    }
    advisory_for(&report(&cache, now), &cache)
}

fn advisory_for(check: &ReleaseCheck, cache: &Cache) -> Option<Advisory> {
    let advisory = |code, message, release_url| Advisory {
        code,
        message,
        release_url,
        freshness: check.freshness,
        age_seconds: check.age_seconds,
    };
    if let Some(problem) = &cache.last_problem {
        return Some(problem_advisory(problem, Some(check)));
    }
    let stale_note = match (check.freshness, check.age_seconds) {
        (Freshness::Stale, Some(age)) => {
            format!(" (last confirmed {} days ago)", age / (24 * 3600))
        }
        _ => String::new(),
    };
    let latest = check.latest_version.as_deref().unwrap_or_default();
    match check.comparison {
        CheckOutcome::Newer if check.installable == Some(true) => Some(advisory(
            "update_available",
            format!(
                "smbs {latest} is available{stale_note}; this is {}. Run `smbs upgrade`, or rerun the installer.",
                check.current_version
            ),
            check.release_url.clone(),
        )),
        CheckOutcome::Newer => Some(advisory(
            "update_incomplete_distribution",
            format!(
                "smbs {latest} is released{stale_note} but has no binary for this platform; build it from source, or wait for a release that includes it."
            ),
            check.release_url.clone(),
        )),
        _ if cache.last_failure_at.is_some()
            && check
                .age_seconds
                .is_none_or(|age| age >= STALE_NOTICE_AFTER.as_secs()) =>
        {
            Some(advisory(
                "update_check_unavailable",
                format!(
                    "Could not check for smbs updates recently. Run `smbs version --check` for details, or set {OPT_OUT_ENV}=1 to stop these checks."
                ),
                None,
            ))
        }
        _ => None,
    }
}

fn problem_advisory(problem: &str, check: Option<&ReleaseCheck>) -> Advisory {
    Advisory {
        code: "update_check_problem",
        message: format!("{problem} Set {OPT_OUT_ENV}=1 to stop these checks."),
        release_url: None,
        freshness: check.map_or(Freshness::Unavailable, |check| check.freshness),
        age_seconds: check.and_then(|check| check.age_seconds),
    }
}

/// Fetches the latest release into `cache`. On failure the previous evidence
/// is kept and the failure recorded.
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
        Err(FetchError::Unavailable(message)) => {
            cache.last_failure_at = Some(now);
            return Err(message);
        }
        Err(FetchError::Invalid(message)) => {
            cache.last_failure_at = Some(now);
            cache.last_problem = Some(message.clone());
            return Err(message);
        }
    };
    cache.evidence = Some(Evidence {
        observed_at: now,
        latest,
    });
    cache.last_failure_at = None;
    cache.last_problem = None;
    Ok(())
}

fn report(cache: &Cache, now: u64) -> ReleaseCheck {
    let running = Version::running();
    let mut check = ReleaseCheck {
        comparison: CheckOutcome::Unavailable,
        current_version: running.to_string(),
        latest_version: None,
        release_url: None,
        installable: None,
        freshness: Freshness::Unavailable,
        age_seconds: None,
        problem: None,
    };
    let Some(evidence) = &cache.evidence else {
        return check;
    };
    let Some(age) = age(evidence.observed_at, now) else {
        return check;
    };
    check.age_seconds = Some(age.as_secs());
    check.freshness = if age < FRESH_FOR {
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
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    // Fixture builds pointed at a fake smbs.biz but not at a fake release
    // origin would otherwise reach github.com from tests.
    let fixture_without_release_origin = cfg!(feature = "fixture-origin")
        && set("SMBS_FIXTURE_ORIGIN")
        && !set("SMBS_FIXTURE_RELEASE_ORIGIN");
    fixture_without_release_origin
        || std::env::var_os(OPT_OUT_ENV).is_some_and(|value| !value.is_empty() && value != "0")
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
    fn locate() -> Result<Self, String> {
        let directory = match std::env::var_os(CACHE_DIR_ENV).filter(|value| !value.is_empty()) {
            Some(directory) => {
                let directory = PathBuf::from(directory);
                if !directory.is_absolute() {
                    return Err(format!(
                        "{CACHE_DIR_ENV} must be an absolute directory; the update check cache is disabled."
                    ));
                }
                directory
            }
            None => default_cache_directory().ok_or_else(|| {
                format!("No per-user cache directory was found; set {CACHE_DIR_ENV}.")
            })?,
        };
        Ok(Self {
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

    /// Writes `cache`, first taking newer evidence another process stored
    /// meanwhile (along with its newer success), so a failing process cannot
    /// replace fresh evidence with old.
    fn save(&self, cache: &mut Cache) -> Result<(), String> {
        let on_disk = self.load();
        if let Some(theirs) = on_disk.evidence
            && cache
                .evidence
                .as_ref()
                .is_none_or(|ours| theirs.observed_at > ours.observed_at)
        {
            if cache
                .last_failure_at
                .is_none_or(|failed| theirs.observed_at >= failed)
            {
                cache.last_failure_at = None;
                cache.last_problem = None;
            }
            cache.evidence = Some(theirs);
        }
        self.write(cache).map_err(|error| {
            format!(
                "Cannot write the update check cache at {}: {error}.",
                self.path.display()
            )
        })
    }

    /// Replaces the file atomically. `std::fs::rename` (POSIX semantics on
    /// current Windows) also succeeds while another process has it open.
    fn write(&self, cache: &Cache) -> std::io::Result<()> {
        use std::io::Write as _;
        let directory = self
            .path
            .parent()
            .ok_or_else(|| std::io::Error::other("the cache path has no parent"))?;
        std::fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(&serde_json::to_vec(cache).expect("the cache serializes"))?;
        let temporary = file.into_temp_path();
        std::fs::rename(&temporary, &self.path)?;
        let _ = temporary.keep();
        Ok(())
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

    fn cache_with(latest: Option<(&str, bool)>, observed_at: u64) -> Cache {
        Cache {
            schema_version: 1,
            target: "t".to_owned(),
            evidence: Some(Evidence {
                observed_at,
                latest: latest.map(|(version, installable)| LatestRelease {
                    version: version.to_owned(),
                    release_url: format!("https://example.invalid/v{version}"),
                    installable,
                }),
            }),
            ..Cache::default()
        }
    }

    fn code(cache: &Cache, now: u64) -> Option<&'static str> {
        advisory_for(&report(cache, now), cache).map(|advisory| advisory.code)
    }

    #[test]
    fn reports_compare_cached_evidence_with_the_running_version() {
        let running = Version::running().to_string();
        let day = FRESH_FOR.as_secs();

        let check = report(&cache_with(Some(("999.0.0", true)), 100), 160);
        assert_eq!(check.comparison, CheckOutcome::Newer);
        assert_eq!(check.freshness, Freshness::Fresh);
        assert_eq!(check.age_seconds, Some(60));
        assert_eq!(
            code(&cache_with(Some(("999.0.0", true)), 100), 160),
            Some("update_available")
        );
        assert_eq!(
            code(&cache_with(Some(("999.0.0", false)), 100), 160),
            Some("update_incomplete_distribution")
        );

        // Current and recently checked: quiet, even after a failed refresh.
        let mut current = cache_with(Some((&running, true)), 100);
        current.last_failure_at = Some(100 + 2 * day);
        let check = report(&current, 100 + 2 * day);
        assert_eq!(check.comparison, CheckOutcome::Equal);
        assert_eq!(check.freshness, Freshness::Stale);
        assert_eq!(code(&current, 100 + 2 * day), None);
        // After a week without success, the failure is reported.
        let week = STALE_NOTICE_AFTER.as_secs();
        assert_eq!(code(&current, 100 + week), Some("update_check_unavailable"));
        // A refresh still in flight (attempt recorded, no failure) is not a failure.
        let mut in_flight = cache_with(Some((&running, true)), 100);
        in_flight.last_attempt_at = Some(100 + week);
        assert_eq!(code(&in_flight, 100 + week), None);

        // A stale update notice says how old it is.
        let stale = advisory_for(
            &report(&cache_with(Some(("999.0.0", true)), 100), 100 + 3 * day),
            &cache_with(Some(("999.0.0", true)), 100),
        )
        .unwrap();
        assert_eq!(stale.freshness, Freshness::Stale);
        assert!(stale.message.contains("3 days ago"), "{}", stale.message);

        // A persistent problem is shown at once.
        let mut problem = cache_with(Some((&running, true)), 100);
        problem.last_problem = Some("Unsupported release manifest schema 2.".to_owned());
        assert_eq!(code(&problem, 160), Some("update_check_problem"));

        assert_eq!(
            report(&cache_with(None, 100), 160).comparison,
            CheckOutcome::NoStableRelease
        );
        // Evidence from the future is not trusted.
        let check = report(&cache_with(Some(("999.0.0", true)), 500), 160);
        assert_eq!(check.comparison, CheckOutcome::Unavailable);
        assert_eq!(check.freshness, Freshness::Unavailable);
    }

    #[test]
    fn saving_keeps_newer_evidence_from_another_process() {
        let directory =
            std::env::temp_dir().join(format!("smbs-release-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let store = Store {
            path: directory.join(CACHE_FILE),
            target: "t".to_owned(),
        };
        assert_eq!(store.load().evidence, None);

        // Another process stored fresh evidence at 500.
        let mut theirs = cache_with(Some(("9.9.9", true)), 500);
        store.save(&mut theirs).unwrap();
        assert_eq!(store.load(), theirs);

        // This process loaded evidence from 100 and its refresh failed at 400.
        let mut ours = cache_with(Some(("9.9.8", true)), 100);
        ours.last_failure_at = Some(400);
        store.save(&mut ours).unwrap();
        let saved = store.load();
        assert_eq!(saved.evidence, theirs.evidence);
        assert_eq!(saved.last_failure_at, None, "a later success supersedes");

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
