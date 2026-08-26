use std::sync::{OnceLock, RwLock};

use async_trait::async_trait;
use muta_contracts::{SharedWebSearchConfig, Tool, WebSearchConfig};
use muta_tool_derive::ToolSchema;
use serde::Deserialize;

use crate::tools::search::SearchProvider;
use crate::tools::web::client::http_client;

#[derive(ToolSchema, Deserialize)]
struct WebSearchArgs {
    #[tool(desc = "The search query")]
    query: String,
}

pub struct WebSearchTool {
    config: SharedWebSearchConfig,
    chain: RwLock<Option<ChainCache>>,
}

struct ChainCache {
    sig: String,
    primary: Box<dyn SearchProvider>,
    fallback: Option<Box<dyn SearchProvider>>,
    client: Result<reqwest::Client, String>,
}

type ProviderChain = (
    Box<dyn SearchProvider>,
    Option<Box<dyn SearchProvider>>,
    reqwest::Client,
);

impl WebSearchTool {
    pub fn new() -> Self {
        Self::with_config(WebSearchConfig::default())
    }

    pub fn with_config(config: WebSearchConfig) -> Self {
        Self::with_shared_config(SharedWebSearchConfig::new(config))
    }

    pub fn with_shared_config(config: SharedWebSearchConfig) -> Self {
        Self {
            config,
            chain: RwLock::new(None),
        }
    }

    pub(crate) fn current_chain(&self) -> Result<ProviderChain, String> {
        let snapshot = self.config.get();
        let sig = snapshot.signature();
        {
            let guard = self
                .chain
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cache) = guard.as_ref()
                && cache.sig == sig
            {
                return Ok((
                    clone_provider(cache.primary.as_ref()),
                    cache.fallback.as_deref().map(clone_provider),
                    cache.client.clone().map_err(|e| e.clone())?,
                ));
            }
        }
        let primary = crate::tools::search::build_provider(&snapshot, &snapshot.provider);
        let fallback_name = snapshot.fallback.trim();
        let fallback = if fallback_name.is_empty() {
            None
        } else {
            Some(crate::tools::search::build_provider(
                &snapshot,
                fallback_name,
            ))
        };
        let client = http_client(&snapshot);
        *self
            .chain
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(ChainCache {
            sig,
            primary: clone_provider(primary.as_ref()),
            fallback: fallback.as_deref().map(clone_provider),
            client: client.clone(),
        });
        Ok((primary, fallback, client.map_err(|e| e.clone())?))
    }

    fn description_text() -> String {
        let year = chrono::Utc::now().format("%Y");
        format!(
            "Search the web for current information, documentation, or events. Current year is {year}."
        )
    }
}

fn clone_provider(p: &dyn SearchProvider) -> Box<dyn SearchProvider> {
    p.clone_box()
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "search_web"
    }
    fn is_available(&self) -> bool {
        let snapshot = self.config.get();
        let provider = snapshot.provider.trim();
        if provider.is_empty()
            || provider == "none"
            || provider == "disabled"
            || provider == "(none)"
        {
            return false;
        }
        match provider {
            "tavily" => snapshot
                .tavily_api_key
                .as_ref()
                .map(|k| !k.expose_secret().trim().is_empty())
                .unwrap_or(false),
            "bocha" => snapshot
                .bocha_api_key
                .as_ref()
                .map(|k| !k.expose_secret().trim().is_empty())
                .unwrap_or(false),
            "searxng" => snapshot
                .searxng_url
                .as_ref()
                .map(|u| !u.trim().is_empty())
                .unwrap_or(false),
            _ => true,
        }
    }
    fn description(&self) -> &str {
        static DESC: OnceLock<String> = OnceLock::new();
        DESC.get_or_init(Self::description_text)
    }
    fn parameters(&self) -> serde_json::Value {
        WebSearchArgs::parameters_schema()
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: WebSearchArgs =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let query = &args.query;
        let (primary, fallback, client) = self.current_chain()?;

        let output = match primary.search(&client, query).await {
            Ok(output) => output,
            Err(primary_err) => match &fallback {
                Some(fallback) => match fallback.search(&client, query).await {
                    Ok(output) => output,
                    Err(fallback_err) => {
                        return Err(format!(
                            "Primary backend {} failed: {}\nFallback backend {} also failed: {}",
                            primary.name(),
                            primary_err,
                            fallback.name(),
                            fallback_err
                        ));
                    }
                },
                None => return Err(primary_err),
            },
        };
        let body = match output {
            crate::tools::search::ProviderOutput::Results(results) => {
                crate::tools::search::format_results(query, primary.name(), results)
            }
            crate::tools::search::ProviderOutput::Blob(text) => {
                format!(
                    "Search results for '{query}' (via {}):\n\n{text}",
                    primary.name()
                )
            }
        };
        Ok(crate::tools::search::cap_output(&body))
    }
}
