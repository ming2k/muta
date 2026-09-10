//! Schema-neutral types mirroring the models.dev catalog JSON
//! (`https://models.dev/api.json`). These deliberately depend on nothing in
//! `muta-providers`; the mapping to the client's `DiscoveredModel` lives there
//! so this crate stays a removable, low-level data source.

use serde::Deserialize;
use std::collections::BTreeMap;

/// A provider entry in the models.dev catalog.
#[derive(Debug, Clone, Deserialize)]
pub struct DevProvider {
    /// Provider id (also the map key).
    pub id: String,
    /// Display name.
    pub name: String,
    /// The provider's base API url, when advertised.
    #[serde(default)]
    pub api: Option<String>,
    /// The AI-SDK package that speaks the provider's wire format (e.g.
    /// `@ai-sdk/openai-compatible`, `@ai-sdk/anthropic`).
    #[serde(default)]
    pub npm: Option<String>,
    /// Model ids → model entries.
    pub models: BTreeMap<String, DevModel>,
}

/// A model entry within a provider.
#[derive(Debug, Clone, Deserialize)]
pub struct DevModel {
    /// Canonical model id.
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Provider model family, when advertised.
    #[serde(default)]
    pub family: Option<String>,
    /// Whether the model advertises reasoning.
    #[serde(default)]
    pub reasoning: bool,
    /// Reasoning-effort configuration, when present.
    #[serde(default)]
    pub reasoning_options: Vec<DevReasoningOption>,
    /// Whether the model advertises tool/function calling.
    #[serde(default)]
    pub tool_call: bool,
    /// Token limits (context window, output).
    #[serde(default)]
    pub limit: DevLimit,
    /// Input/output modalities.
    #[serde(default)]
    pub modalities: DevModalities,
    /// Whether the model accepts image input.
    #[serde(default)]
    pub attachment: bool,
    /// Catalog lifecycle status (`active`/`beta`/`deprecated`/`alpha`).
    #[serde(default)]
    pub status: Option<String>,
}

/// A single reasoning-effort option (`{type, values}`).
#[derive(Debug, Clone, Deserialize)]
pub struct DevReasoningOption {
    /// `"effort"`, `"toggle"`, or `"budget_tokens"`.
    pub r#type: String,
    /// Effort rung names (present for `type: "effort"`). Entries may be
    /// `null` in the wild (some providers emit a `null` rung).
    #[serde(default)]
    pub values: Vec<Option<String>>,
}

/// Token limits.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DevLimit {
    /// Context window in tokens.
    #[serde(default)]
    pub context: Option<u64>,
    /// Maximum output tokens.
    #[serde(default)]
    pub output: Option<u64>,
}

/// Input/output modality lists.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DevModalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}
