use async_trait::async_trait;
use neenee_core::{Tool, WebSearchConfig, truncate_utf8};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};

use crate::tools::search::SearchProvider;

const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;

/// A bounded, content-addressed observation of one public web page.
///
/// The raw body is deliberately not retained. Consumers get a stable hash for
/// change detection plus a short text preview for review and notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebPageSnapshot {
    pub requested_url: String,
    pub final_url: String,
    pub title: String,
    pub text_preview: String,
    pub content_hash: String,
    pub content_type: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub body_bytes: usize,
    pub checked_at_ms: u64,
}

/// Result of a conditional page observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSnapshotResult {
    Modified(WebPageSnapshot),
    NotModified { checked_at_ms: u64 },
}

/// Fetch a URL and return its text content (HTML stripped to text).
pub struct WebFetchTool {
    config: Arc<WebSearchConfig>,
    /// Cached HTTP client, built once from `config` on first use so repeated
    /// fetches reuse the connection pool and keep-alive. Rebuilding a
    /// `reqwest::Client` per call (the old behaviour) pays a fresh TLS
    /// handshake and defeats pooling.
    client: OnceLock<Result<reqwest::Client, String>>,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self {
            config: Arc::new(WebSearchConfig::default()),
            client: OnceLock::new(),
        }
    }
    pub fn with_config(config: WebSearchConfig) -> Self {
        Self {
            config: Arc::new(config),
            client: OnceLock::new(),
        }
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebFetchTool {
    /// Lazily build (once) and return the shared HTTP client for this tool's
    /// config. A build failure is remembered and replayed on every call so the
    /// caller sees a consistent error instead of retrying the bad config.
    fn client(&self) -> Result<&reqwest::Client, String> {
        let built = self.client.get_or_init(|| http_client(&self.config));
        built.as_ref().map_err(|e| e.clone())
    }

    /// Observe a public URL for durable change tracking.
    ///
    /// Supplying a prior ETag or Last-Modified value enables a conditional
    /// request. Servers without validator support still work because callers
    /// can compare [`WebPageSnapshot::content_hash`]. Bodies above 8 MiB are
    /// rejected so a watched link cannot grow memory without bound.
    pub async fn snapshot(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<WebSnapshotResult, String> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("URL must start with http:// or https://".to_string());
        }
        crate::tools::ssrf::assert_public_url(url).await?;

        let mut request = self.client()?.get(url);
        if let Some(value) = etag.map(str::trim).filter(|value| !value.is_empty()) {
            request = request.header(reqwest::header::IF_NONE_MATCH, value);
        }
        if let Some(value) = last_modified
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            request = request.header(reqwest::header::IF_MODIFIED_SINCE, value);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("Request failed: {error}"))?;
        let checked_at_ms = unix_now_ms();
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(WebSnapshotResult::NotModified { checked_at_ms });
        }
        let status = response.status();
        if !status.is_success() {
            return Err(format!("HTTP {status} for {url}"));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_SNAPSHOT_BYTES as u64)
        {
            return Err(format!(
                "Response for {url} exceeds the {} MiB tracking limit",
                MAX_SNAPSHOT_BYTES / 1024 / 1024
            ));
        }

        let final_url = response.url().to_string();
        let headers = response.headers().clone();
        let body = response
            .bytes()
            .await
            .map_err(|error| format!("Failed to read body: {error}"))?;
        if body.len() > MAX_SNAPSHOT_BYTES {
            return Err(format!(
                "Response for {url} exceeds the {} MiB tracking limit",
                MAX_SNAPSHOT_BYTES / 1024 / 1024
            ));
        }
        let content_type = header_text(&headers, reqwest::header::CONTENT_TYPE)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let raw_text = String::from_utf8_lossy(&body);
        let title = if content_type.contains("html") {
            extract_html_title(&raw_text)
        } else {
            String::new()
        };
        let readable = if content_type.contains("html") {
            html_to_text(&raw_text)
        } else {
            raw_text.trim().to_string()
        };
        let text_preview = truncate_utf8(&readable, 800).to_string();
        let content_hash = format!("{:x}", Sha256::digest(&body));
        Ok(WebSnapshotResult::Modified(WebPageSnapshot {
            requested_url: url.to_string(),
            final_url,
            title,
            text_preview,
            content_hash,
            content_type,
            etag: header_text(&headers, reqwest::header::ETAG),
            last_modified: header_text(&headers, reqwest::header::LAST_MODIFIED),
            body_bytes: body.len(),
            checked_at_ms,
        }))
    }
}

fn header_text(
    headers: &reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn extract_html_title(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(open) = lower.find("<title") else {
        return String::new();
    };
    let Some(start_offset) = lower[open..].find('>') else {
        return String::new();
    };
    let start = open + start_offset + 1;
    let Some(end_offset) = lower[start..].find("</title>") else {
        return String::new();
    };
    html_to_text(&html[start..start + end_offset])
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

/// Build the shared HTTP client honoring the web tools' proxy and timeout.
///
/// Redirects are capped at a small, fixed number. The SSRF guard runs on the
/// *initial* URL (see [`crate::tools::ssrf::assert_public_url`]); bounding redirects is
/// defense-in-depth against a server bouncing the request across many internal
/// hops.
fn http_client(config: &WebSearchConfig) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(config.timeout_secs.max(1)))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("neenee/0.1 (+ai-coding-agent)");
    if let Some(proxy_url) = config
        .proxy
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let proxy = reqwest::Proxy::all(proxy_url)
            .map_err(|e| format!("Invalid proxy '{}': {}", proxy_url, e))?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))
}

/// Naive HTML → text conversion. Collapses whitespace and strips tags/scripts.
pub(crate) fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut skip = false;
    let lower = html.to_ascii_lowercase();
    let mut chars = html.char_indices().peekable();
    while let Some((byte_idx, c)) = chars.next() {
        if !in_tag && lower[byte_idx..].starts_with("<script") {
            skip = true;
        } else if skip && lower[byte_idx..].starts_with("</script") {
            skip = false;
            // jump to end of tag
            if let Some(idx) = lower[byte_idx..].find('>') {
                let next_byte = byte_idx + idx + 1;
                while chars
                    .peek()
                    .is_some_and(|(peek_byte, _)| *peek_byte < next_byte)
                {
                    chars.next();
                }
                continue;
            }
        } else if !in_tag && lower[byte_idx..].starts_with("<style") {
            skip = true;
        } else if skip && lower[byte_idx..].starts_with("</style") {
            skip = false;
            if let Some(idx) = lower[byte_idx..].find('>') {
                let next_byte = byte_idx + idx + 1;
                while chars
                    .peek()
                    .is_some_and(|(peek_byte, _)| *peek_byte < next_byte)
                {
                    chars.next();
                }
                continue;
            }
        }
        if skip {
            continue;
        }
        if c == '<' {
            in_tag = true;
        } else if c == '>' && in_tag {
            in_tag = false;
            out.push(' ');
        } else if !in_tag {
            out.push(c);
        }
    }
    // Decode a handful of common entities
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let mut collapsed = String::with_capacity(decoded.len());
    let mut prev_ws = false;
    for c in decoded.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                collapsed.push(' ');
                prev_ws = true;
            }
        } else {
            collapsed.push(c);
            prev_ws = false;
        }
    }
    collapsed.trim().to_string()
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "webfetch"
    }
    fn description(&self) -> &str {
        "Fetch the content of a web page or URL and return it as text. Use for reading \
         documentation, APIs, or any publicly accessible resource. HTML pages are converted to \
         plain text. Output is truncated for very large pages."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The fully-qualified URL to fetch (http/https)" },
                "raw": { "type": "boolean", "description": "If true, return raw content without HTML stripping (default false)" }
            },
            "required": ["url"]
        })
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let url = args["url"].as_str().ok_or("Missing 'url'")?;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("URL must start with http:// or https://".to_string());
        }
        // SSRF pre-flight: resolve the host and reject any non-public address
        // (metadata endpoint, loopback, RFC1918, link-local) before sending.
        crate::tools::ssrf::assert_public_url(url).await?;
        let raw = args["raw"].as_bool().unwrap_or(false);
        let client = self.client()?;
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("HTTP {} for {}", status, url));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let text = response
            .text()
            .await
            .map_err(|e| format!("Failed to read body: {}", e))?;
        let body = if raw || !content_type.contains("html") {
            text
        } else {
            html_to_text(&text)
        };
        if body.len() > 16_000 {
            return Ok(format!(
                "[Fetched {} chars from {}, truncated]\n{}\n\n[Use raw=true or a more specific URL for full content]",
                body.len(),
                url,
                truncate_utf8(&body, 8_000)
            ));
        }
        Ok(body)
    }
}

/// Search the web via a pluggable backend. The provider (and an optional
/// fallback) are selected from `[websearch]` config; see the `search` module
/// for the available backends. Default backend is Exa (hosted, anonymous,
/// reliable) with Parallel as fallback — mirroring other coding agents.
///
/// This struct is a thin shell: it only parses arguments, builds the shared
/// HTTP client (proxy/timeout), and delegates to the provider chain. All
/// backend-specific logic lives behind the `SearchProvider` trait so new
/// backends can be added without touching this tool.
/// Build the model-facing description once at construction time. The current
/// year is injected so the model biases time-sensitive queries correctly.
fn build_description() -> String {
    let year = chrono::Utc::now().format("%Y");
    format!(
        "Search the web and return results as text. Best for current information, \
documentation, or examples beyond your knowledge cutoff.

The current year is {year}. Use this year when searching for recent information \
or current events (e.g. search \"AI news {year}\", not last year).

WHEN TO SEARCH — bias toward searching when in doubt:
- Time-sensitive information that may have changed: news, prices, laws, \
schedules, release notes, software/library versions, exchange rates.
- The user wants recommendations involving time or money (products, travel, \
restaurants) or precise source attribution.
- Niche or emerging topics, or you suspect even a small (>10%) chance of \
misremembering a fact.
- High-stakes accuracy (medical, legal, financial) — search by default.
- A specific page, paper, or dataset is referenced and you lack its contents.
- The user explicitly asks to search, verify, or look something up.

Cite sources with Markdown links to the supporting page — link directly to the \
source, not to a search-result page. Place each citation near the claim it \
supports. Prefer primary and authoritative sources.

The backend is configurable via the `[websearch]` table in config.toml: `exa` \
(default; hosted, anonymous, reliable), `parallel` (hosted), `duckduckgo` \
(keyless scraping, frequently blocked), `searxng` (self-hosted, keyless), or \
`tavily` (hosted, needs key). A `fallback` backend is tried automatically if \
the primary fails."
    )
}

pub struct WebSearchTool {
    config: Arc<WebSearchConfig>,
    primary: Box<dyn SearchProvider>,
    fallback: Option<Box<dyn SearchProvider>>,
    description: String,
    /// Cached HTTP client, built once from `config` (see [`WebFetchTool`]'s
    /// rationale: connection pooling and keep-alive across searches).
    client: OnceLock<Result<reqwest::Client, String>>,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self::with_config(WebSearchConfig::default())
    }

    pub fn with_config(config: WebSearchConfig) -> Self {
        let primary = crate::tools::search::build_provider(&config, &config.provider);
        let fallback_name = config.fallback.trim();
        let fallback = if fallback_name.is_empty() {
            None
        } else {
            Some(crate::tools::search::build_provider(&config, fallback_name))
        };
        Self {
            config: Arc::new(config),
            primary,
            fallback,
            description: build_description(),
            client: OnceLock::new(),
        }
    }

    /// Lazily build (once) and return the shared HTTP client. Mirrors
    /// [`WebFetchTool::client`].
    fn client(&self) -> Result<&reqwest::Client, String> {
        let built = self.client.get_or_init(|| http_client(&self.config));
        built.as_ref().map_err(|e| e.clone())
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "websearch"
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The search query" }
            },
            "required": ["query"]
        })
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let query = args["query"].as_str().ok_or("Missing 'query'")?;
        let client = self.client()?;

        match self.primary.search(client, query).await {
            Ok(text) => Ok(text),
            Err(primary_err) => match &self.fallback {
                Some(fallback) => match fallback.search(client, query).await {
                    Ok(text) => Ok(text),
                    Err(fallback_err) => Err(format!(
                        "Primary backend {} failed: {}\nFallback backend {} also failed: {}",
                        self.primary.name(),
                        primary_err,
                        fallback.name(),
                        fallback_err
                    )),
                },
                None => Err(primary_err),
            },
        }
    }
}

neenee_core::register_tool!(WebFetchFactory => |ctx| {
    let cfg = ctx
        .get::<neenee_core::WebSearchConfig>()
        .cloned()
        .unwrap_or_default();
    WebFetchTool::with_config(cfg)
});
neenee_core::register_tool!(WebSearchFactory => |ctx| {
    let cfg = ctx
        .get::<neenee_core::WebSearchConfig>()
        .cloned()
        .unwrap_or_default();
    WebSearchTool::with_config(cfg)
});

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn html_title_is_normalized_for_watch_summaries() {
        let html = "<html><head><title>  Market &amp; Risk </title></head><body>x</body></html>";
        assert_eq!(extract_html_title(html), "Market & Risk");
        assert_eq!(extract_html_title("<html>untitled</html>"), "");
    }

    #[test]
    fn snapshot_shape_round_trips_through_json() {
        let snapshot = WebPageSnapshot {
            requested_url: "https://example.com/a".to_string(),
            final_url: "https://example.com/a".to_string(),
            title: "A".to_string(),
            text_preview: "preview".to_string(),
            content_hash: format!("{:x}", Sha256::digest(b"body")),
            content_type: "text/html".to_string(),
            etag: Some("v1".to_string()),
            last_modified: None,
            body_bytes: 4,
            checked_at_ms: 1,
        };
        let encoded = serde_json::to_string(&snapshot).expect("snapshot JSON");
        let decoded: WebPageSnapshot = serde_json::from_str(&encoded).expect("snapshot round trip");
        assert_eq!(decoded, snapshot);
    }
}
