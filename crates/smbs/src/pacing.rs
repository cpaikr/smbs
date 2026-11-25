//! Cross-process request pacing.
//!
//! Every `smbs` process on this machine that shares a state directory locks one
//! file before each request, sleeps the interval while holding the lock, and
//! keeps the lock until the request finishes. Concurrent agents and scripts
//! therefore cannot burst the source, and slow connection setup, cancellation,
//! process death, or wall-clock changes cannot release a queued burst.

use std::{
    ffi::OsString,
    fs::{File, OpenOptions, TryLockError},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    time::{Duration, Instant},
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
/// Covers several queued processes, each holding the lock for an interval plus
/// up to the 30 s request deadline. Waiters are not served in order, so the
/// wait is bounded instead of hanging silently.
const LOCK_WAIT_LIMIT: Duration = Duration::from_secs(120);

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

type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

impl PacingConfig {
    /// Typed settings for library callers. `state_dir` must be absolute;
    /// `None` uses the per-user state directory. A zero interval disables
    /// pacing and needs no state directory.
    ///
    /// # Errors
    /// `invalid_request` for an interval above [`MAX_INTERVAL_MS`] or a
    /// relative directory; `internal_error` when no state directory exists.
    pub fn new(interval: Duration, state_dir: Option<PathBuf>) -> Result<Self, SmbsError> {
        Self::build(interval, IntervalSource::Option, state_dir, &|name| {
            std::env::var_os(name)
        })
    }

    /// Resolves the interval from `explicit`, then `SMBS_REQUEST_INTERVAL_MS`,
    /// then the default, and the lock directory from `SMBS_STATE_DIR` or the
    /// per-user state directory.
    ///
    /// # Errors
    /// As [`PacingConfig::new`], plus `invalid_request` for a malformed
    /// `SMBS_REQUEST_INTERVAL_MS`.
    pub fn from_env(explicit: Option<Duration>) -> Result<Self, SmbsError> {
        Self::from_env_with(explicit, &|name| std::env::var_os(name))
    }

    fn from_env_with(explicit: Option<Duration>, env: Env<'_>) -> Result<Self, SmbsError> {
        let (interval, source) = if let Some(interval) = explicit {
            (interval, IntervalSource::Option)
        } else if let Some(value) = env(INTERVAL_ENV) {
            let value = value
                .to_str()
                .ok_or_else(|| invalid_interval(INTERVAL_ENV))?;
            (
                parse_interval_ms(value, INTERVAL_ENV)?,
                IntervalSource::Environment,
            )
        } else {
            (
                Duration::from_millis(DEFAULT_INTERVAL_MS),
                IntervalSource::Default,
            )
        };
        let state_dir = env(STATE_DIR_ENV).map(PathBuf::from);
        Self::build(interval, source, state_dir, env)
    }

    fn build(
        interval: Duration,
        source: IntervalSource,
        state_dir: Option<PathBuf>,
        env: Env<'_>,
    ) -> Result<Self, SmbsError> {
        if interval > Duration::from_millis(MAX_INTERVAL_MS) {
            return Err(invalid_interval("requestInterval"));
        }
        let lock_path = if interval.is_zero() {
            None
        } else {
            let directory = match state_dir {
                Some(directory) if directory.is_absolute() => directory,
                Some(_) => {
                    return Err(SmbsError::invalid(
                        format!("{STATE_DIR_ENV} must be an absolute directory path."),
                        STATE_DIR_ENV,
                        "Unset it to use the per-user state directory.",
                    ));
                }
                None => default_state_directory(env).ok_or_else(|| {
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
        let failed =
            |error: std::io::Error| state_error(format!("Request pacing state failed: {error}"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(failed)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(failed)?;
        let deadline = Instant::now() + LOCK_WAIT_LIMIT;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(LOCK_POLL);
                }
                Err(TryLockError::WouldBlock) => {
                    let mut error = SmbsError::internal(
                        format!(
                            "Waited {} s for other smbs processes to finish their requests.",
                            LOCK_WAIT_LIMIT.as_secs()
                        ),
                        "Run fewer smbs processes at once, then retry.",
                    );
                    error.retryable = true;
                    return Err(error);
                }
                Err(TryLockError::Error(error)) => return Err(failed(error)),
            }
        }
        let delay = recorded_delay(&mut file, self.interval).map_err(failed)?;
        // Persist the longer cooldown in case this process dies while waiting.
        write_interval(&mut file, delay).map_err(failed)?;
        std::thread::sleep(delay);
        write_interval(&mut file, self.interval).map_err(failed)?;
        // Never unlink or replace this file: every process must lock the same inode.
        Ok(Permit(Some(file)))
    }
}

/// Holding the open file holds the OS lock; dropping it (or process exit) releases it.
#[derive(Debug)]
pub(crate) struct Permit(#[allow(dead_code)] Option<File>);

/// The wait before this request: the longer of our interval and the one the
/// previous holder recorded, so processes with different values honor the largest.
fn recorded_delay(file: &mut File, interval: Duration) -> std::io::Result<Duration> {
    let length = file.metadata()?.len();
    if length == 0 {
        return Ok(interval);
    }
    if length != 8 {
        return Err(std::io::Error::other("invalid pacing record length"));
    }
    let mut record = [0; 8];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut record)?;
    let previous = u64::from_le_bytes(record);
    if previous > MAX_INTERVAL_MS {
        return Err(std::io::Error::other("invalid pacing interval record"));
    }
    Ok(interval.max(Duration::from_millis(previous)))
}

fn write_interval(file: &mut File, interval: Duration) -> std::io::Result<()> {
    let milliseconds = u64::try_from(interval.as_millis()).expect("bounded interval");
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&milliseconds.to_le_bytes())?;
    file.flush()
}

/// Parses a millisecond interval from text (a flag or environment value),
/// naming `parameter` in errors.
///
/// # Errors
/// `invalid_request` unless `value` is an integer from 0 through [`MAX_INTERVAL_MS`].
pub fn parse_interval_ms(value: &str, parameter: &str) -> Result<Duration, SmbsError> {
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

fn default_state_directory(env: Env<'_>) -> Option<PathBuf> {
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
    use std::collections::HashMap;

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

    fn config(interval_ms: u64, path: PathBuf) -> PacingConfig {
        PacingConfig {
            interval: Duration::from_millis(interval_ms),
            source: IntervalSource::Option,
            lock_path: Some(path),
        }
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
                parse_interval_ms(value, "x").unwrap_err().code,
                ErrorCode::InvalidRequest,
                "{value}"
            );
        }
        assert_eq!(parse_interval_ms("0", "x").unwrap(), Duration::ZERO);
        assert_eq!(
            parse_interval_ms("60000", "x").unwrap(),
            Duration::from_secs(60)
        );
        assert!(PacingConfig::new(Duration::from_millis(60_001), None).is_err());
    }

    #[test]
    fn precedence_is_option_then_environment_then_default() {
        let state = if cfg!(windows) { "C:\\state" } else { "/state" };
        let both = env(&[(INTERVAL_ENV, "2000"), (STATE_DIR_ENV, state)]);
        let option = PacingConfig::from_env_with(Some(Duration::from_secs(3)), &both).unwrap();
        assert_eq!(
            (option.interval(), option.source()),
            (Duration::from_secs(3), IntervalSource::Option)
        );
        let environment = PacingConfig::from_env_with(None, &both).unwrap();
        assert_eq!(
            (environment.interval(), environment.source()),
            (Duration::from_secs(2), IntervalSource::Environment)
        );
        let default = PacingConfig::from_env_with(None, &env(&[(STATE_DIR_ENV, state)])).unwrap();
        assert_eq!(
            (default.interval(), default.source()),
            (Duration::from_secs(1), IntervalSource::Default)
        );
        assert_eq!(
            default.lock_path,
            Some(PathBuf::from(state).join(LOCK_FILE))
        );
        let bad = env(&[(INTERVAL_ENV, "fast")]);
        assert_eq!(
            PacingConfig::from_env_with(None, &bad)
                .unwrap_err()
                .parameter
                .as_deref(),
            Some(INTERVAL_ENV)
        );
    }

    #[test]
    fn zero_disables_pacing_and_needs_no_state_directory() {
        let config = PacingConfig::from_env_with(Some(Duration::ZERO), &env(&[])).unwrap();
        assert!(config.lock_path.is_none());
        let relative = PacingConfig::from_env_with(None, &env(&[(STATE_DIR_ENV, "relative")]));
        assert_eq!(relative.unwrap_err().code, ErrorCode::InvalidRequest);
        let missing = PacingConfig::from_env_with(None, &env(&[]));
        assert_eq!(missing.unwrap_err().code, ErrorCode::InternalError);
    }

    #[test]
    fn a_long_held_permit_does_not_shorten_the_next_cooldown() {
        let directory = temp_state_dir("held");
        let config = config(100, directory.join(LOCK_FILE));
        let permit = config.acquire().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        drop(permit);
        let released = Instant::now();
        drop(config.acquire().unwrap());
        assert!(released.elapsed() >= Duration::from_millis(100));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_second_holder_waits_for_release_and_then_the_interval() {
        let directory = temp_state_dir("contended");
        let config = config(100, directory.join(LOCK_FILE));
        let permit = config.acquire().unwrap();
        let waiter = {
            let config = config.clone();
            std::thread::spawn(move || {
                let permit = config.acquire().unwrap();
                (Instant::now(), permit)
            })
        };
        std::thread::sleep(Duration::from_millis(150));
        let released = Instant::now();
        drop(permit);
        let (acquired, _permit) = waiter.join().unwrap();
        assert!(acquired - released >= Duration::from_millis(100));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_waiting_process_honors_the_longer_recorded_interval() {
        let directory = temp_state_dir("longer");
        let path = directory.join(LOCK_FILE);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&path, 300_u64.to_le_bytes()).unwrap();
        let started = Instant::now();
        drop(config(50, path).acquire().unwrap());
        assert!(started.elapsed() >= Duration::from_millis(300));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_damaged_record_fails_closed() {
        let directory = temp_state_dir("damaged");
        let path = directory.join(LOCK_FILE);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&path, b"bad").unwrap();
        assert_eq!(
            config(10, path).acquire().unwrap_err().code,
            ErrorCode::InternalError
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
