//! Shared configuration schema for the web tools.
//!
//! Lives in `neenee-contracts` (not with the tool implementations) because both the
//! app-layer
//! `Config` (which owns the `[websearch]` table) and the tool implementations
//! need the type, and we do not want `neenee-persistence` to depend on
//! `neenee-agent`. It is plain serialisable data; the tool implementations
//! live in `neenee-agent::tools::web` and read this struct as input.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// User-tunable web-tool configuration, deserialized from the `[websearch]`
/// table of `config.toml`. All fields default sensibly, so a `config.toml`
/// with no `[websearch]` table (or a partially specified one) is valid.
///
/// # Where the keys live
///
/// `config.toml` is behavior-only and shareable; the six API keys are
/// secrets and are **not** serialized here. They persist in
/// `credentials.toml` under `[websearch]` (`neenee-persistence::config`
/// performs the load-time merge and the one-shot migration from the
/// historical in-`[websearch]` spelling). The fields remain plain
/// `Option<SecretString>` members so the in-memory shape every consumer
/// reads is unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebSearchConfig {
    /// Primary search backend. One of: `"exa"` (default; hosted MCP, anonymous
    /// or an Exa key in `credentials.toml`), `"parallel"` (hosted MCP), `"duckduckgo"`
    /// (best-effort scraping, frequently blocked), `"searxng"` (self-hosted,
    /// keyless), or `"tavily"` (hosted API, requires a Tavily key), or `"bocha"`
    /// (hosted AI search API, requires a Bocha key; directly reachable from
    /// mainland China without a proxy).
    pub provider: String,
    /// Fallback backend tried when `provider` fails. Empty string disables it.
    /// Default `"parallel"`.
    pub fallback: String,
    /// Optional proxy URL applied to both `webfetch` and `websearch`.
    /// Supports `http://`, `https://`, `socks5://`, and `socks5h://`. Takes
    /// precedence over the `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` env vars.
    pub proxy: Option<String>,
    /// Per-request timeout in seconds (default 20).
    pub timeout_secs: u64,
    /// Exa API key (optional; anonymous use works without it).
    /// Persisted in `credentials.toml [websearch]`, never in `config.toml`.
    #[serde(skip_serializing)]
    pub exa_api_key: Option<crate::SecretString>,
    /// Parallel Search API key (optional; anonymous use works without it).
    /// Persisted in `credentials.toml [websearch]`, never in `config.toml`.
    #[serde(skip_serializing)]
    pub parallel_api_key: Option<crate::SecretString>,
    /// SearXNG JSON search endpoint, e.g. `http://localhost:8080/search`.
    /// Required when `provider = "searxng"`.
    pub searxng_url: Option<String>,
    /// Tavily API key. Required when `provider = "tavily"`.
    /// Persisted in `credentials.toml [websearch]`, never in `config.toml`.
    #[serde(skip_serializing)]
    pub tavily_api_key: Option<crate::SecretString>,
    /// Bocha AI Search API key (api.bochaai.com). Required when
    /// `provider = "bocha"`. Directly reachable from mainland China networks,
    /// so it works without a proxy. Persisted in `credentials.toml
    /// [websearch]`, never in `config.toml`.
    #[serde(skip_serializing)]
    pub bocha_api_key: Option<crate::SecretString>,
    /// Jina Reader API key (r.jina.ai). Optional — the reader works
    /// anonymously with a lower rate limit; a key raises the quota.
    /// Persisted in `credentials.toml [websearch]`, never in `config.toml`.
    #[serde(skip_serializing)]
    pub jina_api_key: Option<crate::SecretString>,
    /// Page-content backend used by `webfetch` for HTML pages. One of:
    /// `"builtin"` (default; direct fetch + local HTML stripping — zero
    /// third-party dependency, but naive extraction that keeps boilerplate),
    /// or `"jina"` (r.jina.ai Reader: server-side rendering including
    /// JavaScript, readability-style extraction, Markdown output; sends the
    /// URL to a third party and adds one network hop).
    ///
    /// This is the "depth" half of the two-stage research pipeline
    /// (websearch = breadth, webfetch = depth); see ADR-0117.
    pub reader: String,
}

impl WebSearchConfig {
    /// Extract the six secret keys from this table, leaving every field at
    /// its default. Used by the persistence layer to move keys found in the
    /// historical in-`config.toml` location into `credentials.toml`.
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
            fallback: "parallel".to_string(),
            proxy: None,
            timeout_secs: 20,
            exa_api_key: None,
            parallel_api_key: None,
            searxng_url: None,
            tavily_api_key: None,
            bocha_api_key: None,
            jina_api_key: None,
            reader: "builtin".to_string(),
        }
    }
}

/// A process-wide, shared, hot-reloadable handle to the effective
/// `[websearch]` configuration.
///
/// The web tools snapshot [`WebSearchConfig`] at construction time
/// (bootstrap), but the runtime can now mutate the configuration live
/// (`AgentRequest::UpdateWebSearchConfig`, and `/settings reload`). Rather
/// than rebuilding the toolset, the tools hold this shared handle and
/// re-derive their provider chain / HTTP client whenever the config's
/// *signature* ([`WebSearchConfig::signature`]) changes — a cheap string
/// comparison on the call path instead of a toolset rebuild.
///
/// The handle is intentionally tiny (`Arc<RwLock<...>>`) and lives in
/// `neenee-contracts` so both `neenee-persistence` (config load) and
/// `neenee-agent` (tool construction) can share it without new crate edges.
#[derive(Debug, Clone, Default)]
pub struct SharedWebSearchConfig(Arc<std::sync::RwLock<WebSearchConfig>>);

impl SharedWebSearchConfig {
    /// Wrap an initial effective configuration.
    pub fn new(initial: WebSearchConfig) -> Self {
        Self(Arc::new(std::sync::RwLock::new(initial)))
    }

    /// Replace the effective configuration. Wakes every holders; the web
    /// tools notice on their next call via the signature check.
    pub fn set(&self, config: WebSearchConfig) {
        *self
            .0
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = config;
    }

    /// Clone the effective configuration out.
    pub fn get(&self) -> WebSearchConfig {
        self.0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl WebSearchConfig {
    /// A canonical fingerprint of every field that changes how the web tools
    /// behave (backends, keys presence, proxy, timeout, reader). Two configs
    /// with equal signatures are interchangeable for tool construction, so
    /// the tools compare signatures instead of whole structs and rebuild
    /// their cached provider chain / HTTP client only when something actually
    /// moved.
    ///
    /// Key material enters the signature only through its **presence** (and a
    /// content hash, so changing a key is picked up) — the plaintext is
    /// never part of the string.
    pub fn signature(&self) -> String {
        fn key(sig: Option<&crate::SecretString>) -> String {
            sig.map(|k| {
                // Fingerprint, not the value: enough to detect a change.
                let hash = std::collections::hash_map::DefaultHasher::new();
                let mut hasher = hash;
                std::hash::Hash::hash_slice(k.expose_secret().as_bytes(), &mut hasher);
                format!("{:016x}", std::hash::Hasher::finish(&hasher))
            })
            .unwrap_or_else(|| "-".to_string())
        }
        format!(
            "v1|provider={}|fallback={}|reader={}|proxy={}|timeout={}|searxng_url={}|\
             exa={}|parallel={}|tavily={}|bocha={}|jina={}",
            self.provider,
            self.fallback,
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
