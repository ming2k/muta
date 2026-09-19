//! User-declared model provider customizations (`model_providers.toml`,
//! ADR-0199, ADR-0201).
//!
//! Stores provider-level model scoping and capability customizations keyed by
//! model provider id (e.g. `deepseek`, `anthropic`, `openai`). Applied across
//! every connection that points at that provider — model semantics belong to
//! the provider, not to a credential binding.

use muta_contracts::model::ModelScopeConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::fsutil;
use crate::paths;

/// A first-class user-declared model provider surface (ADR-0258).
///
/// Declares the physical transport endpoint, default protocol, catalog discovery,
/// dialect, and client identity preset for a custom LLM service surface (e.g.
/// corporate relay, local vLLM, or self-hosted gateway).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserDeclaredProvider {
    /// Optional human-readable display label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Root API URL (e.g. `https://relay.example.com/v1`, without trailing slash).
    pub root_url: String,
    /// Default wire transport protocol (e.g. `chat-completions`, `responses`, `anthropic-messages`, `google-gemini`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_protocol: Option<muta_contracts::WireProtocol>,
    /// Default client profile preset for User-Agent / client headers emulation (ADR-0164, ADR-0258).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_profile: Option<muta_contracts::ClientPreset>,
    /// Optional User-Agent override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Catalog discovery format: `openai`, `anthropic`, `google`, `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_format: Option<String>,
    /// Dialect kind: `standard`, `deepseek`, `openrouter`, `copilot`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialect: Option<String>,
}

/// Root map of user-configured model providers in `model_providers.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProviders {
    /// Declarative user-defined provider services (ADR-0258).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, UserDeclaredProvider>,
    /// Scoped model inclusions, exclusions, and capability overrides per provider (ADR-0199, ADR-0201).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_providers: BTreeMap<String, ModelScopeConfig>,
}

impl ModelProviders {
    fn path() -> std::path::PathBuf {
        paths::get().model_providers_file()
    }

    /// Read the store, returning empty when missing or unparseable.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str(&content) {
            Ok(providers) => providers,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not parse model_providers.toml; ignoring it",
                );
                Self::default()
            }
        }
    }

    /// Persist atomically.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let bytes = toml::to_string_pretty(self)?.into_bytes();
        fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    /// Look up a user-declared provider definition by id.
    pub fn get_provider(&self, provider_id: &str) -> Option<&UserDeclaredProvider> {
        self.providers.get(provider_id)
    }

    /// Set or update a user-declared provider definition.
    pub fn set_provider(&mut self, provider_id: impl Into<String>, provider: UserDeclaredProvider) {
        self.providers.insert(provider_id.into(), provider);
    }

    /// Remove a user-declared provider definition.
    pub fn remove_provider(&mut self, provider_id: &str) -> Option<UserDeclaredProvider> {
        self.providers.remove(provider_id)
    }

    /// Get the scope config for a model provider.
    pub fn get(&self, provider_id: &str) -> Option<&ModelScopeConfig> {
        self.model_providers.get(provider_id)
    }

    /// Get mutable scope config for a model provider, creating it if absent.
    pub fn get_or_create_mut(&mut self, provider_id: &str) -> &mut ModelScopeConfig {
        self.model_providers
            .entry(provider_id.to_string())
            .or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::model::DeclaredModel;

    #[test]
    fn model_providers_roundtrip_toml() {
        let mut providers = ModelProviders::default();
        let config = providers.get_or_create_mut("deepseek");
        config.include.push(DeclaredModel {
            id: "deepseek-v4-preview".to_string(),
            context_window: Some(1_000_000),
            ..DeclaredModel::default()
        });
        config.exclude.push("deepseek-chat-deprecated".to_string());

        let text = toml::to_string_pretty(&providers).unwrap();
        assert!(text.contains("[model_providers.deepseek]"));
        let parsed: ModelProviders = toml::from_str(&text).unwrap();
        assert_eq!(providers, parsed);
        let deepseek = parsed.get("deepseek").unwrap();
        assert_eq!(deepseek.include.len(), 1);
        assert_eq!(deepseek.include[0].id, "deepseek-v4-preview");
        assert_eq!(deepseek.exclude, vec!["deepseek-chat-deprecated"]);
    }

    #[test]
    fn user_declared_provider_roundtrip_toml() {
        let mut store = ModelProviders::default();
        store.set_provider(
            "corp-relay",
            UserDeclaredProvider {
                label: Some("Corporate Relay".to_string()),
                root_url: "https://relay.corp.example/v1".to_string(),
                default_protocol: Some(muta_contracts::WireProtocol::ChatCompletions),
                client_profile: Some(muta_contracts::ClientPreset::Cursor),
                user_agent: None,
                catalog_format: Some("openai".to_string()),
                dialect: Some("deepseek".to_string()),
            },
        );

        let text = toml::to_string_pretty(&store).unwrap();
        assert!(text.contains("[providers.corp-relay]"));
        assert!(text.contains("root_url = \"https://relay.corp.example/v1\""));
        assert!(text.contains("client_profile = \"cursor\""));

        let parsed: ModelProviders = toml::from_str(&text).unwrap();
        assert_eq!(store, parsed);
        let prov = parsed.get_provider("corp-relay").unwrap();
        assert_eq!(prov.root_url, "https://relay.corp.example/v1");
        assert_eq!(prov.default_protocol, Some(muta_contracts::WireProtocol::ChatCompletions));
        assert_eq!(prov.client_profile, Some(muta_contracts::ClientPreset::Cursor));
        assert_eq!(prov.dialect.as_deref(), Some("deepseek"));
    }
}
