//! `smbs version --check`: compare this executable with the latest release.

use serde::Serialize;

use crate::release::{self, Comparison, FetchError, GitHub, Version};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Newer,
    Equal,
    Ahead,
    /// No stable release has been published yet.
    NoStableRelease,
    /// The release metadata could not be fetched or used.
    Unavailable,
}

/// The report embedded as `result.releaseCheck`. Problems are reported here
/// rather than failing the command.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

pub fn explicit_check() -> ReleaseCheck {
    let running = Version::running();
    let mut check = ReleaseCheck {
        comparison: CheckOutcome::Unavailable,
        current_version: running.to_string(),
        latest_version: None,
        release_url: None,
        installable: None,
        problem: None,
    };
    match GitHub::new().latest() {
        Ok(latest) => {
            check.comparison = match release::compare(running, latest.version) {
                Comparison::Newer => CheckOutcome::Newer,
                Comparison::Equal => CheckOutcome::Equal,
                Comparison::Ahead => CheckOutcome::Ahead,
            };
            check.latest_version = Some(latest.version.to_string());
            check.release_url = Some(latest.url);
            check.installable = Some(latest.asset.is_some());
        }
        Err(FetchError::NoRelease) => check.comparison = CheckOutcome::NoStableRelease,
        Err(error) => check.problem = Some(error.to_string()),
    }
    check
}
