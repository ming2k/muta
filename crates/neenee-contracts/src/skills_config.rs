//! Shared configuration schema for skills.
//!
//! Lives in `neenee-contracts` for the same reason [`crate::WebSearchConfig`] and
//! [`crate::McpServerConfig`] do: the app-layer `Config` owns the `[skills]`
//! table and the loader in `neenee-skills` needs to read it, while the store
//! does not depend on that implementation crate.

use serde::{Deserialize, Serialize};

/// Skill configuration stored under `[skills]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SkillsConfig {
    /// Additional local directories to scan for skills.
    pub paths: Vec<String>,
    /// Remote skill repositories to fetch and cache.
    pub urls: Vec<String>,
    /// Skill names to disable (case-sensitive).
    pub disabled: Vec<String>,
}

impl SkillsConfig {
    /// True when no skill configuration is present.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.urls.is_empty() && self.disabled.is_empty()
    }

    /// True when the given skill name is disabled.
    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled.iter().any(|n| n == name)
    }
}
