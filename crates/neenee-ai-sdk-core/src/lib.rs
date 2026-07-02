//! Shared transport substrate for the protocol-specific AI SDK crates.
//!
//! This crate intentionally owns only cross-protocol mechanics: endpoint
//! configuration, per-turn tool/usage state, SSE byte reassembly, retry/error
//! classification, and bounded JSON decode diagnostics. OpenAI, Anthropic, and
//! Google request/response semantics live in their own SDK crates.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod endpoint;
pub mod sse;

pub use endpoint::{Endpoint, NEENEE_USER_AGENT, TurnState};

use neenee_core::retryable_error;
use std::time::SystemTime;

pub fn retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    if let Some(milliseconds) = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<f64>().ok())
    {
        return Some(milliseconds.max(0.0) as u64);
    }
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<f64>() {
        return Some((seconds.max(0.0) * 1000.0) as u64);
    }
    let parsed = httpdate::parse_http_date(value).ok()?;
    let now = SystemTime::now();
    Some(
        parsed
            .duration_since(now)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64,
    )
}

pub async fn ensure_success(
    response: reqwest::Response,
    provider: &str,
) -> Result<reqwest::Response, String> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let retry_after = retry_after_ms(response.headers());
    let body = response.text().await.unwrap_or_default();
    let message = format!("{} HTTP {}: {}", provider, status, body);
    if status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error() {
        Err(retryable_error(message, retry_after))
    } else {
        Err(message)
    }
}

fn is_transient_io_kind(kind: std::io::ErrorKind) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        kind,
        ConnectionReset
            | ConnectionAborted
            | ConnectionRefused
            | BrokenPipe
            | UnexpectedEof
            | NotConnected
            | TimedOut
    )
}

fn chain_has_transient_io(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut next: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(err) = next {
        if let Some(io) = err.downcast_ref::<std::io::Error>()
            && is_transient_io_kind(io.kind())
        {
            return true;
        }
        next = err.source();
    }
    false
}

fn is_transient_transport_error(error: &reqwest::Error) -> bool {
    if error.is_timeout() || error.is_connect() || error.is_request() || error.is_body() {
        return true;
    }
    chain_has_transient_io(error)
}

pub fn transport_error(provider: &str, error: reqwest::Error) -> String {
    let message = format!("{} transport error: {}", provider, error);
    if is_transient_transport_error(&error) {
        retryable_error(message, None)
    } else {
        message
    }
}

const DECODE_ERROR_BODY_PREVIEW: usize = 2048;

pub async fn decode_response_json(
    response: reqwest::Response,
    provider: &str,
) -> Result<serde_json::Value, String> {
    let bytes = response
        .bytes()
        .await
        .map_err(|error| transport_error(provider, error))?;
    let text = String::from_utf8_lossy(&bytes);
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) => Ok(value),
        Err(error) => {
            let preview = body_preview(&text);
            tracing::warn!(
                target: "neenee_core::provider",
                provider = provider,
                error = %error,
                body_len = text.len(),
                body_preview = %preview,
                "{} response was not valid JSON",
                provider,
            );
            Err(format!(
                "{provider} error decoding response body: {error} (raw body preview: {preview})"
            ))
        }
    }
}

fn body_preview(text: &str) -> String {
    let total_chars = text.chars().count();
    let mut preview: String = text.chars().take(DECODE_ERROR_BODY_PREVIEW).collect();
    let truncated_chars = total_chars.saturating_sub(preview.chars().count());
    if truncated_chars > 0 {
        preview.push_str(&format!("…<{truncated_chars} more chars>"));
    }
    preview = preview
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_supports_seconds_and_milliseconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "2.5".parse().unwrap());
        assert_eq!(retry_after_ms(&headers), Some(2_500));

        headers.insert("retry-after-ms", "750".parse().unwrap());
        assert_eq!(retry_after_ms(&headers), Some(750));
    }

    #[test]
    fn transient_io_kinds_are_retryable() {
        use std::io::ErrorKind::*;
        for kind in [
            ConnectionReset,
            ConnectionAborted,
            ConnectionRefused,
            BrokenPipe,
            UnexpectedEof,
            NotConnected,
            TimedOut,
        ] {
            assert!(is_transient_io_kind(kind), "{kind:?} should be transient");
        }
    }

    #[test]
    fn logical_io_kinds_are_not_retryable() {
        use std::io::ErrorKind::*;
        for kind in [InvalidData, InvalidInput, PermissionDenied, NotFound] {
            assert!(
                !is_transient_io_kind(kind),
                "{kind:?} must not be transient"
            );
        }
    }

    #[test]
    fn connection_reset_is_found_deep_in_the_source_chain() {
        #[derive(Debug)]
        struct Wrap(Box<dyn std::error::Error + Send + Sync + 'static>);
        impl std::fmt::Display for Wrap {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "wrapper")
            }
        }
        impl std::error::Error for Wrap {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(self.0.as_ref())
            }
        }

        let io = std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "connection reset by peer",
        );
        let nested = Wrap(Box::new(Wrap(Box::new(io))));
        assert!(
            chain_has_transient_io(&nested),
            "a reset buried two wrappers deep must still be detected"
        );

        let benign = Wrap(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bad utf8",
        )));
        assert!(
            !chain_has_transient_io(&benign),
            "a non-transient io kind must not be flagged"
        );
    }

    #[test]
    fn body_preview_short_body_passes_through() {
        assert_eq!(body_preview("<html>502</html>"), "<html>502</html>");
    }

    #[test]
    fn body_preview_truncates_long_body_and_reports_remaining_chars() {
        let long = "a".repeat(DECODE_ERROR_BODY_PREVIEW * 2 + 50);
        let preview = body_preview(&long);
        assert_eq!(
            preview.chars().count(),
            DECODE_ERROR_BODY_PREVIEW
                + format!("…<{} more chars>", DECODE_ERROR_BODY_PREVIEW + 50)
                    .chars()
                    .count()
        );
        assert!(preview.ends_with(&format!("…<{} more chars>", DECODE_ERROR_BODY_PREVIEW + 50)));
    }

    #[test]
    fn body_preview_escapes_control_characters() {
        let preview = body_preview("line1\nline2\ttab\rend");
        assert!(
            !preview.contains('\n') && !preview.contains('\t') && !preview.contains('\r'),
            "control chars must be escaped: {preview:?}"
        );
        assert!(preview.contains("\\n") && preview.contains("\\t") && preview.contains("\\r"));
    }

    #[test]
    fn body_preview_truncates_on_char_boundary() {
        let chars = "日".repeat(DECODE_ERROR_BODY_PREVIEW + 10);
        let preview = body_preview(&chars);
        assert!(!preview.contains('\u{FFFD}'));
    }
}
