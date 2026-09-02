use std::sync::RwLock;

use async_trait::async_trait;
use muta_contracts::{SharedWebSearchConfig, Tool, WebSearchConfig};
use muta_tool_derive::ToolSchema;
use serde::Deserialize;

use crate::tools::web::client::{UNTRUSTED_PREFIX, UNTRUSTED_SUFFIX, http_client};
use crate::tools::web::snapshot::{WebSnapshotResult, take_snapshot};

pub const WEB_READER_MAX_TOKENS: usize = 4_000;

#[derive(ToolSchema, Deserialize)]
struct WebReaderArgs {
    #[tool(desc = "The fully-qualified URL to read (http/https)")]
    url: String,
    #[tool(desc = "If true, return raw content without HTML stripping (default false)")]
    raw: Option<bool>,
}

/// Read a web page URL and extract its clean Markdown content via the configured Reader.
pub struct WebReaderTool {
    config: SharedWebSearchConfig,
    client: RwLock<Option<(String, Result<reqwest::Client, String>)>>,
}

impl WebReaderTool {
    pub fn new() -> Self {
        Self::with_config(WebSearchConfig::default())
    }
    pub fn with_config(config: WebSearchConfig) -> Self {
        Self::with_shared_config(SharedWebSearchConfig::new(config))
    }
    pub fn with_shared_config(config: SharedWebSearchConfig) -> Self {
        Self {
            config,
            client: RwLock::new(None),
        }
    }

    pub fn client(&self) -> Result<reqwest::Client, String> {
        let snapshot = self.config.get();
        let sig = snapshot.signature();
        {
            let guard = self
                .client
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some((cached_sig, built)) = guard.as_ref()
                && *cached_sig == sig
            {
                return built.clone().map_err(|e| e.clone());
            }
        }
        let built = http_client(&snapshot);
        *self
            .client
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((sig, built.clone()));
        built.map_err(|e| e.clone())
    }

    pub async fn snapshot(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<WebSnapshotResult, String> {
        let client = self.client()?;
        take_snapshot(&client, url, etag, last_modified).await
    }
}

impl Default for WebReaderTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebReaderTool {
    fn name(&self) -> &str {
        "read_url"
    }
    fn is_available(&self) -> bool {
        let snapshot = self.config.get();
        let reader = snapshot.reader.trim();
        if reader.is_empty() || reader == "none" || reader == "disabled" || reader == "(none)" {
            return false;
        }
        match reader {
            "jina" => snapshot
                .jina_api_key
                .as_ref()
                .map(|k| !k.expose_secret().trim().is_empty())
                .unwrap_or(false),
            _ => true,
        }
    }
    fn description(&self) -> &str {
        "Read a web page and return its text content as clean Markdown."
    }
    fn parameters(&self) -> serde_json::Value {
        WebReaderArgs::parameters_schema()
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: WebReaderArgs =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let url = &args.url;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("URL must start with http:// or https://".to_string());
        }
        crate::tools::ssrf::assert_public_url(url).await?;
        let raw = args.raw.unwrap_or(false);
        let client = self.client()?;
        let snapshot = self.config.get();
        let reader = crate::tools::reader::build_reader(&snapshot);
        let reader_name = reader.name();
        let output = reader.read(&client, url, raw).await?;
        let body = output.text;
        let content_type = output.content_type;
        let tokens = muta_contracts::tokenizer::count_tokens(&body);
        if tokens > WEB_READER_MAX_TOKENS {
            let (keep, _kept) =
                muta_contracts::tokenizer::truncate_to_tokens(&body, WEB_READER_MAX_TOKENS / 2);
            return Ok(format!(
                "{UNTRUSTED_PREFIX}[Read {tokens} tokens from {url} (reader: {reader_name}, \
content-type: {content_type}); kept the first {}/{} tokens — the page is longer than the tool's \
context budget. Request a more specific URL/anchor or a section link for the part you need.]\n{keep}\
{UNTRUSTED_SUFFIX}",
                WEB_READER_MAX_TOKENS / 2,
                WEB_READER_MAX_TOKENS
            ));
        }
        Ok(format!("{UNTRUSTED_PREFIX}{body}{UNTRUSTED_SUFFIX}"))
    }
}
