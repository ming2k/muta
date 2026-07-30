//! HTTP transport helpers shared by every protocol adapter: success
//! enforcement, retry/error classification, JSON decode diagnostics, and
//! credential masking in error messages. The pooled HTTP client itself lives
//! in [`crate::client`]; endpoint configuration in [`crate::endpoint`]; SSE
//! byte reassembly in [`crate::sse`].

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
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response.text().await.unwrap_or_default();
    let message = match http_error_body_detail(content_type.as_deref(), &body) {
        Some(detail) => format!("{provider} HTTP {status}: {detail}"),
        None => format!("{provider} HTTP {status}"),
    };
    if status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error() {
        Err(retryable_error(message, retry_after))
    } else {
        Err(message)
    }
}

/// Keep structured provider diagnostics, but do not surface a reverse
/// proxy's HTML error document as transcript content. Besides being noise,
/// those pages commonly carry CRLF/control bytes and can be surprisingly
/// large. The HTTP status already contains the useful gateway failure.
fn http_error_body_detail(content_type: Option<&str>, body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    let looks_html = content_type.is_some_and(|value| {
        value
            .split(';')
            .next()
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
    }) || {
        let lower = trimmed
            .chars()
            .take(32)
            .collect::<String>()
            .to_ascii_lowercase();
        lower.starts_with("<!doctype html") || lower.starts_with("<html")
    };
    (!looks_html).then(|| body_preview(trimmed))
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

/// Query parameter names that may carry credentials in provider URLs —
/// Google's `?key=` is the notable one; some relays accept `api_key` /
/// `access_token` the same way. A `reqwest::Error`'s `Display` embeds the
/// request URL, so formatting it verbatim would leak the credential into
/// logs and user-facing errors.
const CREDENTIAL_QUERY_PARAMS: [&str; 4] = ["key", "api_key", "apikey", "access_token"];

/// Mask credential-carrying query parameter values inside a formatted error
/// message. A value runs until `&`, whitespace, or `)` (reqwest wraps URLs
/// in parentheses). Only `name=` occurrences immediately preceded by `?` or
/// `&` count as query parameters, so prose like "key=value" is left alone.
fn redact_url_credentials(message: &str) -> String {
    let mut redacted = message.to_string();
    for name in CREDENTIAL_QUERY_PARAMS {
        for prefix in [format!("?{name}="), format!("&{name}=")] {
            let mut search_from = 0;
            while let Some(found) = redacted[search_from..].find(&prefix) {
                let value_start = search_from + found + prefix.len();
                let value_len = redacted[value_start..]
                    .find(|c: char| c == '&' || c.is_whitespace() || c == ')')
                    .unwrap_or(redacted.len() - value_start);
                if value_len > 0 {
                    redacted.replace_range(value_start..value_start + value_len, "***");
                    search_from = value_start + 3;
                } else {
                    // Empty value: continue from the delimiter so a following
                    // `&name=` parameter is still scanned.
                    search_from = value_start;
                }
            }
        }
    }
    redacted
}

pub fn transport_error(provider: &str, error: reqwest::Error) -> String {
    let message = redact_url_credentials(&format!("{} transport error: {}", provider, error));
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

    #[test]
    fn http_error_body_hides_html_gateway_pages() {
        let body = "<html>\r\n<head><title>504 Gateway Time-out</title></head>\r\n</html>";
        assert_eq!(
            http_error_body_detail(Some("text/html; charset=utf-8"), body),
            None
        );
        assert_eq!(http_error_body_detail(None, body), None);
    }

    #[test]
    fn http_error_body_keeps_bounded_structured_diagnostics() {
        let body = "{\"error\":{\"message\":\"rate limited\"}}\r\n";
        assert_eq!(
            http_error_body_detail(Some("application/json"), body),
            Some("{\"error\":{\"message\":\"rate limited\"}}".to_string())
        );
    }

    #[test]
    fn redact_url_credentials_masks_google_style_key_param() {
        let message = "google transport error: error sending request for url \
                       (https://generativelanguage.googleapis.com/v1/models/gemini-3:streamGenerateContent?alt=sse&key=AIza-secret)";
        let redacted = redact_url_credentials(message);
        assert!(!redacted.contains("AIza-secret"), "key leaked: {redacted}");
        assert!(
            redacted.contains("alt=sse"),
            "non-secret params stay: {redacted}"
        );
        assert!(redacted.contains("key=***"), "masked in place: {redacted}");
    }

    #[test]
    fn redact_url_credentials_masks_each_known_param_and_stops_at_ampersand() {
        let message = "see (https://x.test/v1?api_key=sk-1&model=g) and (https://y.test/v1?access_token=tok%20)";
        let redacted = redact_url_credentials(message);
        assert!(!redacted.contains("sk-1"));
        assert!(!redacted.contains("tok%20"));
        assert!(redacted.contains("model=g"));
    }

    #[test]
    fn redact_url_credentials_leaves_prose_and_empty_values_alone() {
        // No `?`/`&` immediately before `key=` → not a query parameter.
        assert_eq!(
            redact_url_credentials("key=value unchanged"),
            "key=value unchanged"
        );
        // Empty value: nothing to mask, scanner still terminates.
        assert_eq!(
            redact_url_credentials("(https://x.test/?key=)"),
            "(https://x.test/?key=)"
        );
    }
}
