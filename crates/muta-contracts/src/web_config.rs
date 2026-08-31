//! Shared configuration and connection schema for the web tools.
//!
//! Web search (breadth) and web fetch (depth) are decoupled into two orthogonal
//! sets of connections and presets:
//! - Search connections: declarations for search backends (Exa, Tavily, Bocha, SearXNG, DuckDuckGo, custom)
//! - Reader connections: declarations for page reader / scraper backends (Jina, Firecrawl, custom)

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A persisted Web Search Connection record (`search_connections` in `web_connections.toml`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields, export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct WebSearchConnection {
    /// Stable, unique connection identifier (e.g. "exa-default", "corp-searxng").
    pub id: String,
    /// Human-readable display name shown in pickers and UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// Builtin preset identifier (e.g. "exa", "parallel", "searxng", "tavily", "bocha", "duckduckgo").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preset_id: Option<String>,
    /// Optional environment variable name supplying the API key (12-factor override).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub api_key_env: Option<String>,
    /// Custom search base URL / endpoint (e.g. SearXNG endpoint or private search cluster).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_url: Option<String>,
    /// Optional custom HTTP headers sent with requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub custom_headers: Option<HashMap<String, String>>,
    /// Whether this connection is active and enabled for search routing.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// A persisted Web Reader Connection record (`reader_connections` in `web_connections.toml`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields, export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct WebReaderConnection {
    /// Stable, unique connection identifier (e.g. "my-jina", "corp-firecrawl").
    pub id: String,
    /// Human-readable display name shown in pickers and UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// Builtin preset identifier (e.g. "jina", "firecrawl").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preset_id: Option<String>,
    /// Optional environment variable name supplying the API key (12-factor override).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub api_key_env: Option<String>,
    /// Custom reader base URL / endpoint (e.g. self-hosted Firecrawl or Crawl4AI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_url: Option<String>,
    /// Optional custom HTTP headers sent with requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub custom_headers: Option<HashMap<String, String>>,
    /// Whether this connection is active and enabled for fetch routing.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Default for WebSearchConnection {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: None,
            preset_id: None,
            api_key_env: None,
            base_url: None,
            custom_headers: None,
            enabled: true,
        }
    }
}

impl WebSearchConnection {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    pub fn is_preset(&self) -> bool {
        self.preset_id.is_some()
    }
}

impl Default for WebReaderConnection {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: None,
            preset_id: None,
            api_key_env: None,
            base_url: None,
            custom_headers: None,
            enabled: true,
        }
    }
}

impl WebReaderConnection {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    pub fn is_preset(&self) -> bool {
        self.preset_id.is_some()
    }
}

/// Static template definition for a known builtin web search preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSearchPreset {
    pub id: &'static str,
    pub display_name: &'static str,
    pub default_endpoint: Option<&'static str>,
    pub requires_credential: bool,
    pub supports_anonymous: bool,
    pub default_env_var: Option<&'static str>,
    pub description: &'static str,
}

/// Registry of known builtin web search presets.
pub struct WebSearchPresets;

impl WebSearchPresets {
    pub const ALL: &'static [WebSearchPreset] = &[
        WebSearchPreset {
            id: "exa",
            display_name: "Exa Search",
            default_endpoint: Some("https://mcp.exa.ai"),
            requires_credential: false,
            supports_anonymous: true,
            default_env_var: Some("EXA_API_KEY"),
            description: "Hosted MCP AI Search · Keyless anonymous default or with Exa API key",
        },
        WebSearchPreset {
            id: "parallel",
            display_name: "Parallel Search",
            default_endpoint: Some("https://parallel-search.mcp.ai"),
            requires_credential: false,
            supports_anonymous: true,
            default_env_var: Some("PARALLEL_API_KEY"),
            description: "Hosted MCP Search · Keyless anonymous default or with Parallel API key",
        },
        WebSearchPreset {
            id: "tavily",
            display_name: "Tavily Search",
            default_endpoint: Some("https://api.tavily.com/search"),
            requires_credential: true,
            supports_anonymous: false,
            default_env_var: Some("TAVILY_API_KEY"),
            description: "Hosted AI search API tailored for LLM agents · Requires Tavily API key",
        },
        WebSearchPreset {
            id: "bocha",
            display_name: "Bocha AI Search",
            default_endpoint: Some("https://api.bochaai.com/v1/ai-search"),
            requires_credential: true,
            supports_anonymous: false,
            default_env_var: Some("BOCHA_API_KEY"),
            description: "Hosted AI search API · Directly reachable in mainland China without proxy",
        },
        WebSearchPreset {
            id: "searxng",
            display_name: "SearXNG",
            default_endpoint: None,
            requires_credential: false,
            supports_anonymous: true,
            default_env_var: None,
            description: "Self-hosted privacy meta-search engine · Requires JSON endpoint URL",
        },
        WebSearchPreset {
            id: "duckduckgo",
            display_name: "DuckDuckGo",
            default_endpoint: Some("https://html.duckduckgo.com/html"),
            requires_credential: false,
            supports_anonymous: true,
            default_env_var: None,
            description: "Keyless direct web scraping fallback",
        },
    ];

    pub fn find(id: &str) -> Option<&'static WebSearchPreset> {
        let norm = id.trim().to_ascii_lowercase();
        Self::ALL
            .iter()
            .find(|p| p.id.eq_ignore_ascii_case(&norm) || (norm == "ddg" && p.id == "duckduckgo"))
    }
}

/// Static template definition for a known builtin web reader preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebReaderPreset {
    pub id: &'static str,
    pub display_name: &'static str,
    pub default_endpoint: Option<&'static str>,
    pub requires_credential: bool,
    pub supports_anonymous: bool,
    pub default_env_var: Option<&'static str>,
    pub description: &'static str,
}

/// Registry of known builtin web reader presets.
pub struct WebReaderPresets;

impl WebReaderPresets {
    pub const ALL: &'static [WebReaderPreset] = &[
        WebReaderPreset {
            id: "jina",
            display_name: "Jina Reader",
            default_endpoint: Some("https://r.jina.ai"),
            requires_credential: false,
            supports_anonymous: true,
            default_env_var: Some("JINA_API_KEY"),
            description: "Server-side JavaScript rendering, readability extraction, and Markdown conversion",
        },
        WebReaderPreset {
            id: "firecrawl",
            display_name: "Firecrawl",
            default_endpoint: Some("https://api.firecrawl.dev/v1/scrape"),
            requires_credential: true,
            supports_anonymous: false,
            default_env_var: Some("FIRECRAWL_API_KEY"),
            description: "Hosted or self-hosted web scraping engine for LLMs",
        },
    ];

    pub fn find(id: &str) -> Option<&'static WebReaderPreset> {
        let norm = id.trim().to_ascii_lowercase();
        Self::ALL.iter().find(|p| p.id.eq_ignore_ascii_case(&norm))
    }
}

/// User-tunable web-tool configuration, deserialized from `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebSearchConfig {
    /// Primary search backend or connection id. Default is `"exa"`.
    pub provider: String,
    /// Optional proxy URL applied to both `fetch_url` and `search_web`.
    pub proxy: Option<String>,
    /// Per-request timeout in seconds (default 20).
    pub timeout_secs: u64,
    /// Exa API key (optional). Persisted in `credentials.toml [websearch]`.
    #[serde(skip_serializing)]
    pub exa_api_key: Option<crate::SecretString>,
    /// Parallel Search API key (optional). Persisted in `credentials.toml [websearch]`.
    #[serde(skip_serializing)]
    pub parallel_api_key: Option<crate::SecretString>,
    /// SearXNG JSON search endpoint. Required when `provider = "searxng"`.
    pub searxng_url: Option<String>,
    /// Tavily API key. Required when `provider = "tavily"`.
    #[serde(skip_serializing)]
    pub tavily_api_key: Option<crate::SecretString>,
    /// Bocha AI Search API key. Required when `provider = "bocha"`.
    #[serde(skip_serializing)]
    pub bocha_api_key: Option<crate::SecretString>,
    /// Jina Reader API key (r.jina.ai). Optional.
    #[serde(skip_serializing)]
    pub jina_api_key: Option<crate::SecretString>,
    /// Page-content backend used by `fetch_url`. Default is `"none"` (disabled).
    pub reader: String,
}

impl WebSearchConfig {
    pub fn secret_keys_only(&self) -> Self {
        Self {
            exa_api_key: self.exa_api_key.clone(),
            parallel_api_key: self.parallel_api_key.clone(),
            tavily_api_key: self.tavily_api_key.clone(),
            bocha_api_key: self.bocha_api_key.clone(),
            jina_api_key: self.jina_api_key.clone(),
            ..Self::default()
        }
    }
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            provider: "exa".to_string(),
            proxy: None,
            timeout_secs: 20,
            exa_api_key: None,
            parallel_api_key: None,
            searxng_url: None,
            tavily_api_key: None,
            bocha_api_key: None,
            jina_api_key: None,
            reader: "none".to_string(),
        }
    }
}

/// A process-wide, shared, hot-reloadable handle to the effective web configuration.
#[derive(Debug, Clone, Default)]
pub struct SharedWebSearchConfig(Arc<std::sync::RwLock<WebSearchConfig>>);

impl SharedWebSearchConfig {
    pub fn new(initial: WebSearchConfig) -> Self {
        Self(Arc::new(std::sync::RwLock::new(initial)))
    }

    pub fn set(&self, config: WebSearchConfig) {
        *self
            .0
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = config;
    }

    pub fn get(&self) -> WebSearchConfig {
        self.0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl WebSearchConfig {
    pub fn signature(&self) -> String {
        fn key(sig: Option<&crate::SecretString>) -> String {
            sig.map(|k| {
                let hash = std::collections::hash_map::DefaultHasher::new();
                let mut hasher = hash;
                std::hash::Hash::hash_slice(k.expose_secret().as_bytes(), &mut hasher);
                format!("{:016x}", std::hash::Hasher::finish(&hasher))
            })
            .unwrap_or_else(|| "-".to_string())
        }
        format!(
            "v1|provider={}|reader={}|proxy={}|timeout={}|searxng_url={}|\
             exa={}|parallel={}|tavily={}|bocha={}|jina={}",
            self.provider,
            self.reader,
            self.proxy.as_deref().unwrap_or("-"),
            self.timeout_secs,
            self.searxng_url.as_deref().unwrap_or("-"),
            key(self.exa_api_key.as_ref()),
            key(self.parallel_api_key.as_ref()),
            key(self.tavily_api_key.as_ref()),
            key(self.bocha_api_key.as_ref()),
            key(self.jina_api_key.as_ref()),
        )
    }
}
