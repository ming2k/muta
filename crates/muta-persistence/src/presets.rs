//! User-declared preset configurations (`presets.toml`, ADR-0199).
//!
//! Stores preset-level model scoping and capability customizations
//! keyed by preset id (e.g. `deepseek`, `anthropic`, `openai`).
//! Applied across all connection instances that share that preset.

use std::collections::BTreeMap;
use muta_contracts::model::ModelScopeConfig;
use serde::{Deserialize, Serialize};

use crate::fsutil;
use crate::paths;

/// Root map of user-configured presets in `presets.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presets {
    #[serde(default)]
    pub presets: BTreeMap<String, ModelScopeConfig>,
}

impl Presets {
    fn path() -> std::path::PathBuf {
        paths::get().presets_file()
    }

    /// Read the presets store, returning empty when missing or unparseable.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str(&content) {
            Ok(presets) => presets,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not parse presets.toml; ignoring it",
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

    /// Get the scope config for a preset.
    pub fn get(&self, preset_id: &str) -> Option<&ModelScopeConfig> {
        self.presets.get(preset_id)
    }

    /// Get mutable scope config for a preset, creating it if absent.
    pub fn get_or_create_mut(&mut self, preset_id: &str) -> &mut ModelScopeConfig {
        self.presets.entry(preset_id.to_string()).or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::model::DeclaredModel;

    #[test]
    fn presets_roundtrip_toml() {
        let mut presets = Presets::default();
        let config = presets.get_or_create_mut("deepseek");
        config.include.push(DeclaredModel {
            id: "deepseek-v4-preview".to_string(),
            context_window: Some(1_000_000),
            ..DeclaredModel::default()
        });
        config.exclude.push("deepseek-chat-deprecated".to_string());

        let text = toml::to_string_pretty(&presets).unwrap();
        let parsed: Presets = toml::from_str(&text).unwrap();
        assert_eq!(presets, parsed);
        let deepseek = parsed.get("deepseek").unwrap();
        assert_eq!(deepseek.include.len(), 1);
        assert_eq!(deepseek.include[0].id, "deepseek-v4-preview");
        assert_eq!(deepseek.exclude, vec!["deepseek-chat-deprecated"]);
    }
}
