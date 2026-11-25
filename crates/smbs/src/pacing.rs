//! Cross-process request pacing.
//!
//! Every `smbs` process on this machine that shares a state directory locks one
//! file before each request, sleeps the interval while holding the lock, and
//! keeps the lock until the request finishes. Concurrent agents and scripts
//! therefore cannot burst the source, and slow connection setup, cancellation,
//! process death, or wall-clock changes cannot release a queued burst.

use std::{
    fs::{File, OpenOptions, TryLockError},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Serialize;

use crate::SmbsError;

/// Project decision: smbs.biz documents no limit and is a small JSP site.
pub const DEFAULT_INTERVAL_MS: u64 = 1_000;
pub const MAX_INTERVAL_MS: u64 = 60_000;
pub const INTERVAL_ENV: &str = "SMBS_REQUEST_INTERVAL_MS";
pub const STATE_DIR_ENV: &str = "SMBS_STATE_DIR";
const LOCK_FILE: &str = "request-pacing-v1.lock";
const LOCK_POLL: Duration = Duration::from_millis(25);

/// Where the effective request interval came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntervalSource {
    Option,
    Environment,
    Default,
}

/// Validated pacing settings, resolved before any network access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacingConfig {
    interval: Duration,
    source: IntervalSource,
    /// `None` only when the interval is zero: pacing is then disabled.
    lock_path: Option<PathBuf>,
}

impl PacingConfig {
    /// Resolves the interval from `explicit`, then `SMBS_REQUEST_INTERVAL_MS`,
    /// then the default, and the lock location from `SMBS_STATE_DIR` or the
    /// per-user state directory.
    ///
    /// # Errors
    /// `invalid_request` for an out-of-range or malformed interval or a relative
    /// state directory; `internal_error` when no state directory can be found.
    pub fn resolve(explicit_ms: Option<&str>) -> Result<Self, SmbsError> {
        Self::resolve_with(explicit_ms, |name| std::env::var_os(name))
    }

    fn resolve_with(
        explicit_ms: Option<&str>,
        env: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Result<Self, SmbsError> {
        let (interval, source) = if let Some(value) = explicit_ms {
            (
                parse_interval(value, "request-interval-ms")?,
                IntervalSource::Option,
            )
        } else if let Some(value) = env(INTERVAL_ENV) {
            let value = value
                .to_str()
                .ok_or_else(|| invalid_interval(INTERVAL_ENV))?;
            (
                parse_interval(value, INTERVAL_ENV)?,
                IntervalSource::Environment,
            )
        } else {
            (
                Duration::from_millis(DEFAULT_INTERVAL_MS),
                IntervalSource::Default,
            )
        };
        let lock_path = if interval.is_zero() {
            None
        } else {
            let directory = match env(STATE_DIR_ENV) {
                Some(directory) => {
                    let directory = PathBuf::from(directory);
                    if !directory.is_absolute() {
                        return Err(SmbsError::invalid(
                            format!("{STATE_DIR_ENV} must be an absolute directory path."),
                            STATE_DIR_ENV,
                            "Unset it to use the per-user state directory.",
                        ));
                    }
                    directory
                }
                None => default_state_directory(&env).ok_or_else(|| {
                    state_error(format!(
                        "No per-user state directory is available; set {STATE_DIR_ENV}."
                    ))
                })?,
            };
            Some(directory.join(LOCK_FILE))
        };
        Ok(Self {
            interval,
            source,
            lock_path,
        })
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn source(&self) -> IntervalSource {
        self.source
    }

    /// Blocks until this process may send one request. Hold the returned permit
    /// until the request completes.
    pub(crate) fn acquire(&self) -> Result<Permit, SmbsError> {
        let Some(path) = &self.lock_path else {
            return Ok(Permit(None));
        };
        loop {
            match try_acquire(path, self.interval)
                .map_err(|error| state_error(format!("Request pacing state failed: {error}")))?
            {
                Attempt::Wait => std::thread::sleep(LOCK_POLL),
                Attempt::Ready(mut file, delay) => {
                    std::thread::sleep(delay);
                    write_interval(&mut file, self.interval).map_err(|error| {
                        state_error(format!("Request pacing state failed: {error}"))
                    })?;
                    return Ok(Permit(Some(file)));
                }
            }
        }
    }
}

/// Holding the open file holds the OS lock; dropping it (or process exit) releases it.
#[derive(Debug)]
pub(crate) struct Permit(#[allow(dead_code)] Option<File>);

enum Attempt {
    Ready(File, Duration),
    Wait,
}

fn try_acquire(path: &Path, interval: Duration) -> std::io::Result<Attempt> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(Attempt::Wait),
        Err(TryLockError::Error(error)) => return Err(error),
    }

    let mut delay = interval;
    let length = file.metadata()?.len();
    if length != 0 {
        if length != 8 {
            return Err(std::io::Error::other("invalid pacing record length"));
        }
        let mut record = [0; 8];
        file.read_exact(&mut record)?;
        let previous = u64::from_le_bytes(record);
        if previous > MAX_INTERVAL_MS {
            return Err(std::io::Error::other("invalid pacing interval record"));
        }
        // Honor the longest interval among processes that use different values.
        delay = interval.max(Duration::from_millis(previous));
    }
    // Persist the longer cooldown in case this process dies while waiting.
    write_interval(&mut file, delay)?;
    // Never unlink or replace this file: every process must lock the same inode.
    Ok(Attempt::Ready(file, delay))
}

fn write_interval(file: &mut File, interval: Duration) -> std::io::Result<()> {
    let milliseconds = u64::try_from(interval.as_millis()).expect("bounded interval");
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&milliseconds.to_le_bytes())?;
    file.flush()
}

fn parse_interval(value: &str, parameter: &str) -> Result<Duration, SmbsError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_interval(parameter));
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|milliseconds| *milliseconds <= MAX_INTERVAL_MS)
        .map(Duration::from_millis)
        .ok_or_else(|| invalid_interval(parameter))
}

fn invalid_interval(parameter: &str) -> SmbsError {
    SmbsError::invalid(
        format!("{parameter} must be an integer from 0 through {MAX_INTERVAL_MS} milliseconds."),
        parameter,
        format!(
            "Omit it for the {DEFAULT_INTERVAL_MS} ms default. Use 0 only against a local fixture: unpaced requests risk being blocked by smbs.biz."
        ),
    )
}

fn default_state_directory(env: &impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let non_empty = |name: &str| env(name).filter(|value| !value.is_empty());
    if cfg!(windows) {
        non_empty("LOCALAPPDATA").map(|root| PathBuf::from(root).join("smbs"))
    } else {
        non_empty("XDG_STATE_HOME")
            .map(|root| PathBuf::from(root).join("smbs"))
            .or_else(|| non_empty("HOME").map(|root| PathBuf::from(root).join(".local/state/smbs")))
    }
}

fn state_error(message: String) -> SmbsError {
    SmbsError::internal(
        message,
        format!(
            "Check {STATE_DIR_ENV} and its permissions, and use the same directory for every smbs process. Stop all smbs processes before deleting a damaged {LOCK_FILE}."
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, ffi::OsString, time::Instant};

    use super::*;
    use crate::ErrorCode;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        move |name| map.get(name).cloned()
    }

    fn temp_state_dir(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("smbs-pacing-unit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        directory
    }

    #[test]
    fn interval_is_bounded_and_strict() {
        for value in [
            "",
            "-1",
            "+1",
            " 500",
            "1.5",
            "60001",
            "18446744073709551616",
        ] {
            assert_eq!(
                parse_interval(value, "x").unwrap_err().code,
                ErrorCode::InvalidRequest,
                "{value}"
            );
        }
        assert_eq!(parse_interval("0", "x").unwrap(), Duration::ZERO);
        assert_eq!(
            parse_interval("60000", "x").unwrap(),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn precedence_is_option_then_environment_then_default() {
        let state = if cfg!(windows) { "C:\\state" } else { "/state" };
        let both = env(&[(INTERVAL_ENV, "2000"), (STATE_DIR_ENV, state)]);
        let option = PacingConfig::resolve_with(Some("3000"), &both).unwrap();
        assert_eq!(
            (option.interval(), option.source()),
            (Duration::from_secs(3), IntervalSource::Option)
        );
        let environment = PacingConfig::resolve_with(None, &both).unwrap();
        assert_eq!(
            (environment.interval(), environment.source()),
            (Duration::from_secs(2), IntervalSource::Environment)
        );
        let default = PacingConfig::resolve_with(None, env(&[(STATE_DIR_ENV, state)])).unwrap();
        assert_eq!(
            (default.interval(), default.source()),
            (Duration::from_secs(1), IntervalSource::Default)
        );
        assert_eq!(
            default.lock_path,
            Some(PathBuf::from(state).join(LOCK_FILE))
        );
        // A bad environment value is an error even when it would be overridden later.
        let bad = env(&[(INTERVAL_ENV, "fast")]);
        assert_eq!(
            PacingConfig::resolve_with(None, &bad)
                .unwrap_err()
                .parameter
                .as_deref(),
            Some(INTERVAL_ENV)
        );
    }

    #[test]
    fn zero_disables_pacing_and_needs_no_state_directory() {
        let config = PacingConfig::resolve_with(Some("0"), env(&[])).unwrap();
        assert!(config.lock_path.is_none());
        let relative = PacingConfig::resolve_with(None, env(&[(STATE_DIR_ENV, "relative")]));
        assert_eq!(relative.unwrap_err().code, ErrorCode::InvalidRequest);
        let missing = PacingConfig::resolve_with(None, env(&[]));
        assert_eq!(missing.unwrap_err().code, ErrorCode::InternalError);
    }

    #[test]
    fn a_long_held_permit_does_not_shorten_the_next_cooldown() {
        let directory = temp_state_dir("held");
        let config = PacingConfig {
            interval: Duration::from_millis(100),
            source: IntervalSource::Option,
            lock_path: Some(directory.join(LOCK_FILE)),
        };
        let permit = config.acquire().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        drop(permit);
        let released = Instant::now();
        drop(config.acquire().unwrap());
        assert!(released.elapsed() >= Duration::from_millis(100));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_waiting_process_honors_the_longer_recorded_interval() {
        let directory = temp_state_dir("longer");
        let path = directory.join(LOCK_FILE);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&path, 300_u64.to_le_bytes()).unwrap();
        let config = PacingConfig {
            interval: Duration::from_millis(50),
            source: IntervalSource::Option,
            lock_path: Some(path),
        };
        let started = Instant::now();
        drop(config.acquire().unwrap());
        assert!(started.elapsed() >= Duration::from_millis(300));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_damaged_record_fails_closed() {
        let directory = temp_state_dir("damaged");
        let path = directory.join(LOCK_FILE);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&path, b"bad").unwrap();
        let config = PacingConfig {
            interval: Duration::from_millis(10),
            source: IntervalSource::Option,
            lock_path: Some(path),
        };
        assert_eq!(config.acquire().unwrap_err().code, ErrorCode::InternalError);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
