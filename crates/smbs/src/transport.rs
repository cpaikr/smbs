use std::time::Duration;

use encoding_rs::EUC_KR;
use ureq::http::header::{CONTENT_TYPE, RETRY_AFTER};

use crate::{ErrorCode, SmbsError};

pub(crate) const SMBS_ORIGIN: &str = "http://www.smbs.biz";
const USER_AGENT: &str = concat!(
    "smbs/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/cpaikr/smbs)"
);
/// Observed chart documents are about 2 KB per decade of months.
const BODY_LIMIT: u64 = 1 << 20;
const MAX_RETRY_AFTER_SECONDS: u64 = 86_400;

/// Fixed-origin HTTP GET returning EUC-KR-decoded text.
#[derive(Debug)]
pub(crate) struct Transport {
    agent: ureq::Agent,
    origin: String,
}

impl Transport {
    pub(crate) fn new(origin: String) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // A redirect is a source change (e.g. to a login or error page); never follow it.
            .max_redirects(0)
            .max_redirects_will_error(false)
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent(USER_AGENT)
            .build();
        Self {
            agent: config.into(),
            origin,
        }
    }

    /// Returns the public URL (always on smbs.biz, for citation) and the decoded body.
    pub(crate) fn get(&self, path: &str, arr_value: &str) -> Result<(String, String), SmbsError> {
        let path_and_query = format!("{path}?arr_value={arr_value}");
        let public_url = format!("{SMBS_ORIGIN}{path_and_query}");
        let unavailable =
            |message: String| SmbsError::source(ErrorCode::SourceUnavailable, message, &public_url);

        let mut response = self
            .agent
            .get(format!("{}{path_and_query}", self.origin))
            .call()
            .map_err(|error| {
                unavailable(match error {
                    ureq::Error::Timeout(_) => "The smbs.biz request timed out.".to_owned(),
                    // The URL is public and carries no credentials, so the cause is safe to show.
                    other => truncate(&format!("The smbs.biz request failed: {other}."), 300),
                })
            })?;

        let status = response.status().as_u16();
        if status != 200 {
            return Err(status_error(
                status,
                response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok()),
                &public_url,
            ));
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(BODY_LIMIT)
            .read_to_vec()
            .map_err(|error| match error {
                ureq::Error::BodyExceedsLimit(_) => SmbsError::source(
                    ErrorCode::SourceChanged,
                    format!("smbs.biz returned more than {BODY_LIMIT} bytes."),
                    &public_url,
                ),
                _ => unavailable("Reading the smbs.biz response failed.".to_owned()),
            })?;
        // The source answers malformed requests with an HTML error page; reject
        // that early, but accept either XML media type (RFC 7303).
        let media_type = content_type.split(';').next().unwrap_or_default().trim();
        if !matches!(media_type, "text/xml" | "application/xml") {
            return Err(SmbsError::source(
                ErrorCode::SourceChanged,
                format!("smbs.biz returned Content-Type \"{content_type}\" instead of XML."),
                &public_url,
            ));
        }
        let text = EUC_KR
            .decode_without_bom_handling_and_without_replacement(&bytes)
            .ok_or_else(|| {
                SmbsError::source(
                    ErrorCode::SourceParseFailure,
                    "smbs.biz returned bytes that are not valid EUC-KR.",
                    &public_url,
                )
            })?;
        Ok((public_url, text.into_owned()))
    }
}

/// Classifies a non-200 status.
///
/// - 429, and 503 with `Retry-After`: `rate_limited`, retryable after a wait.
/// - 408 and other 5xx: `source_unavailable`, retryable.
/// - Redirects (e.g. to a login or error page) and other 4xx: `source_changed`;
///   retrying the same request will not help.
fn status_error(status: u16, retry_after: Option<&str>, public_url: &str) -> SmbsError {
    let message = format!("smbs.biz returned HTTP status {status}.");
    let code = match status {
        429 => ErrorCode::RateLimited,
        503 if retry_after.is_some() => ErrorCode::RateLimited,
        408 | 500..=599 => ErrorCode::SourceUnavailable,
        _ => ErrorCode::SourceChanged,
    };
    let mut error = SmbsError::source(code, message, public_url);
    if code == ErrorCode::RateLimited {
        error.recovery_hint = Some(retry_hint(retry_after));
    }
    error
}

/// Never suggests an immediate retry; honors a bounded `Retry-After` in seconds.
fn retry_hint(retry_after: Option<&str>) -> String {
    match retry_after
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds <= MAX_RETRY_AFTER_SECONDS)
    {
        Some(seconds) => {
            format!("smbs.biz asked clients to wait {seconds} seconds before retrying.")
        }
        None => "Wait before retrying, and keep request pacing enabled.".to_owned(),
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ErrorCode::{RateLimited, SourceChanged, SourceUnavailable};

    #[test]
    fn retry_hint_uses_only_bounded_seconds() {
        assert!(retry_hint(Some("120")).contains("120 seconds"));
        for ignored in [
            None,
            Some("soon"),
            Some("999999"),
            Some("Wed, 21 Oct 2026 07:28:00 GMT"),
        ] {
            assert!(retry_hint(ignored).starts_with("Wait before retrying"));
        }
    }

    #[test]
    fn statuses_are_classified_by_what_a_retry_can_fix() {
        let classify = |status, retry_after| {
            let error = status_error(status, retry_after, "http://www.smbs.biz/x");
            (error.code, error.retryable, error.recovery_hint.is_some())
        };

        assert_eq!(classify(429, None), (RateLimited, true, true));
        assert_eq!(classify(503, Some("30")), (RateLimited, true, true));
        assert_eq!(classify(503, None), (SourceUnavailable, true, false));
        assert_eq!(classify(500, None), (SourceUnavailable, true, false));
        assert_eq!(classify(408, None), (SourceUnavailable, true, false));
        for status in [301, 302, 400, 403, 404, 410] {
            assert_eq!(
                classify(status, None),
                (SourceChanged, false, false),
                "{status}"
            );
        }
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("가나다", 2), "가나...");
        assert_eq!(truncate("abc", 3), "abc");
    }
}
