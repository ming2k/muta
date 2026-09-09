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

/// Root map of user-configured model providers in `model_providers.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProviders {
    #[serde(default)]
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
}
