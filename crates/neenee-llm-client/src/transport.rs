//! HTTP transport helpers shared by every protocol adapter: success
//! enforcement, retry/error classification, JSON decode diagnostics, and
//! credential masking in error messages. The pooled HTTP client itself lives
//! in [`crate::client`]; endpoint configuration in [`crate::endpoint`]; SSE
//! byte reassembly in [`crate::sse`].

use neenee_contracts::retryable_error;
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
    // `Kind::Decode` covers the streaming path: `bytes_stream()` wraps *every*
    // body error (including mid-stream connection resets and truncation) in
    // `Kind::Decode`, and that error carries no URL — exactly the bare
    // "<provider> transport error: error decoding response body" seen when a
    // long SSE generation is cut off mid-stream. A reset that surfaces as
    // `Kind::Body` on the same wire is already retryable above; classifying
    // the decode wrapper the same way keeps one physical failure from being
    // retryable or terminal depending on which reqwest API observed it.
    if error.is_decode() {
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

/// Render a `reqwest::Error`'s *cause chain* for inclusion in an error
/// message. `reqwest::Error`'s `Display` prints only the kind description
/// ("error decoding response body", "error sending request") and stops — the
/// `source()` chain (hyper "connection closed before message completed",
/// gzip "corrupt deflate stream", io "connection reset by peer") never
/// reaches the user, so a truncated stream is undiagnosable from the error
/// alone. Join the sources with `: ` and cap the total to keep the message
/// bounded.
fn error_source_chain(error: &(dyn std::error::Error + 'static)) -> String {
    const MAX_CHAIN_CHARS: usize = 240;
    let mut chain = String::new();
    let mut next = error.source();
    while let Some(err) = next {
        let display = err.to_string();
        if !display.is_empty() {
            if !chain.is_empty() {
                chain.push_str(": ");
            }
            chain.push_str(&display);
        }
        next = err.source();
        if chain.chars().count() >= MAX_CHAIN_CHARS {
            break;
        }
    }
    if chain.chars().count() > MAX_CHAIN_CHARS {
        let truncated: String = chain.chars().take(MAX_CHAIN_CHARS).collect();
        chain = truncated;
    }
    chain
}

pub fn transport_error(provider: &str, error: reqwest::Error) -> String {
    let sources = error_source_chain(&error);
    let detail = if sources.is_empty() {
        format!("{provider} transport error: {error}")
    } else {
        format!("{provider} transport error: {error} ({sources})")
    };
    let message = redact_url_credentials(&detail);
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
                target: "neenee_contracts::provider",
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
    // Diagnostic text inside a decode-error message: report the omitted tail
    // in tokens (ADR-0120) — how much context the body would have cost.
    let total_tokens = neenee_contracts::tokenizer::count_tokens(text);
    let mut preview: String = text.chars().take(DECODE_ERROR_BODY_PREVIEW).collect();
    let truncated_tokens =
        total_tokens.saturating_sub(neenee_contracts::tokenizer::count_tokens(&preview));
    if truncated_tokens > 0 {
        preview.push_str(&format!("…<{truncated_tokens} more tokens>"));
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
    fn transport_error_includes_source_chain() {
        // Unit-check the chain renderer directly: a nested source chain must
        // be flattened into the message, not dropped. (`reqwest::Error`
        // itself is constructed in the async test below.)
        #[derive(Debug)]
        struct Nested(&'static str);
        impl std::fmt::Display for Nested {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
        impl std::error::Error for Nested {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                match self.0 {
                    "outer" => Some(Box::leak(Box::new(Nested("hyper: incomplete message")))),
                    _ => None,
                }
            }
        }
        assert_eq!(
            error_source_chain(&Nested("outer")),
            "hyper: incomplete message"
        );
        assert_eq!(error_source_chain(&Nested("leaf")), "");
    }

    #[tokio::test]
    async fn decode_kind_stream_truncation_is_retryable() {
        // Reproduce the exact error shape the streaming path produces: the
        // server declares Content-Length larger than what it sends, then
        // closes — reqwest's `bytes_stream()` wraps the resulting body error
        // in `Kind::Decode` (its `map_err(crate::error::decode)`), which is
        // exactly how a cut-off SSE generation surfaces. Before the
        // `is_decode()` arm this classified as terminal, so one truncated
        // stream killed the whole envoy sub-task with no retry.
        use std::io::Write;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 1024];
            use tokio::io::AsyncReadExt;
            let _ = socket.read(&mut buf).await; // consume the request
            let mut connection = socket.into_std().unwrap();
            // Promise 1024 bytes of SSE body but deliver only a fragment,
            // then hard-close: hyper reports "connection closed before
            // message completed" through the body layer.
            let _ = connection.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                   Content-Length: 1024\r\n\r\ndata: {\"a\":1}",
            );
            let _ = connection.shutdown(std::net::Shutdown::Both);
        });
        let response = reqwest::Client::new()
            .get(format!("http://{addr}/v1/chat/completions"))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .unwrap();
        let mut stream = Box::pin(response.bytes_stream());
        let mut decode_error: Option<reqwest::Error> = None;
        while let Some(item) = futures::StreamExt::next(&mut stream).await {
            if let Err(error) = item {
                decode_error = Some(error);
                break;
            }
        }
        server.abort();
        let error = decode_error.expect("the truncated body must fail the stream");
        assert!(error.is_decode(), "BodyDataStream wraps errors in Decode");
        assert!(
            is_transient_transport_error(&error),
            "a Decode error from a cut-off stream must classify as transient"
        );
        let message = transport_error("OpenAI", error);
        let chain_was_captured = !message.is_empty();
        let retryable = neenee_contracts::parse_retryable_error(&message)
            .unwrap_or_else(|| panic!("decode-kind transport errors must be retryable: {message}"));
        assert!(chain_was_captured);
        assert!(
            retryable.message.contains("error decoding response body"),
            "reqwest kind text expected: {}",
            retryable.message
        );
        // The source chain (hyper/io detail like "connection closed before
        // message completed") must have been folded into the message; the
        // bare reqwest Display never includes it.
        assert_ne!(
            retryable.message, "OpenAI transport error: error decoding response body",
            "the underlying cause must be included for diagnosis"
        );
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
    fn body_preview_truncates_long_body_and_reports_remaining_tokens() {
        let long = "a".repeat(DECODE_ERROR_BODY_PREVIEW * 2 + 50);
        let preview = body_preview(&long);
        // The omitted tail is reported in tokens (ADR-0120): the whole body
        // tokenizes to N, the kept preview to fewer, the difference is the
        // count in the suffix.
        let total = neenee_contracts::tokenizer::count_tokens(&long);
        let kept = neenee_contracts::tokenizer::count_tokens(&long[..DECODE_ERROR_BODY_PREVIEW]);
        let omitted = total - kept;
        assert!(
            preview.ends_with(&format!("…<{omitted} more tokens>")),
            "got: {preview}"
        );
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
