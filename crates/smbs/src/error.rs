use serde::Serialize;
use thiserror::Error;

/// Stable failure categories. Callers branch on these, never on messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request was rejected before any network access.
    InvalidRequest,
    /// The source could not be reached, timed out, or returned a non-200 status.
    SourceUnavailable,
    /// The source answered with a shape this client does not support.
    SourceChanged,
    /// The response could not be decoded or parsed.
    SourceParseFailure,
    /// Local state (such as the pacing lock) failed.
    InternalError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Error)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub struct SmbsError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_hint: Option<String>,
}

impl SmbsError {
    pub(crate) fn invalid(
        message: impl Into<String>,
        parameter: &str,
        recovery_hint: impl Into<String>,
    ) -> Self {
        Self {
            code: ErrorCode::InvalidRequest,
            message: message.into(),
            retryable: false,
            parameter: Some(parameter.to_owned()),
            source_url: None,
            recovery_hint: Some(recovery_hint.into()),
        }
    }

    pub(crate) fn source(code: ErrorCode, message: impl Into<String>, source_url: &str) -> Self {
        debug_assert!(matches!(
            code,
            ErrorCode::SourceUnavailable | ErrorCode::SourceChanged | ErrorCode::SourceParseFailure
        ));
        Self {
            code,
            message: message.into(),
            retryable: code == ErrorCode::SourceUnavailable,
            parameter: None,
            source_url: Some(source_url.to_owned()),
            recovery_hint: None,
        }
    }

    pub(crate) fn internal(message: impl Into<String>, recovery_hint: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::InternalError,
            message: message.into(),
            retryable: false,
            parameter: None,
            source_url: None,
            recovery_hint: Some(recovery_hint.into()),
        }
    }
}
