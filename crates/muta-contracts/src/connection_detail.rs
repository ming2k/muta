//! Connection detail and provider usage / quota models.

use serde::{Deserialize, Serialize};

/// Generic normalized provider usage / quota / balance info.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct ProviderUsage {
    /// High-level plan / account status (e.g. "Active", "Available", "Free Tier", "Tier 2", "Pay-as-you-go").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Primary balance / credit summary (e.g. "¥10.00", "$5.20", "10.00 CNY").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_balance: Option<String>,
    /// Structured usage metrics / breakdown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<UsageMetric>,
    /// Unix epoch milliseconds of when this usage data was retrieved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u64>,
}

/// One named metric in a provider's usage / quota report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct UsageMetric {
    /// Metric label, e.g. "Total Balance", "Granted Balance", "Rate Limit".
    pub label: String,
    /// Metric value, e.g. "¥10.00", "200 req / 10s", "0.50".
    pub value: String,
    /// Optional unit or currency, e.g. "CNY", "USD", "requests".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// State of a connection's usage / quota retrieval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum ConnectionUsageState {
    /// Provider does not support remote usage querying.
    Unsupported,
    /// Usage query is currently in progress.
    Fetching,
    /// Usage data retrieved successfully.
    Available(ProviderUsage),
    /// Usage query failed with an error message.
    Error(String),
}

impl Default for ConnectionUsageState {
    fn default() -> Self {
        Self::Unsupported
    }
}

/// Full inspection detail for one connection in the `/connections` modal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct ConnectionDetail {
    /// Canonical connection id.
    pub id: String,
    /// User-visible connection name.
    pub name: String,
    /// Preset id if created from a preset (e.g. "deepseek", "anthropic").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<String>,
    /// Human-friendly preset label (e.g. "DeepSeek", "Anthropic").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_label: Option<String>,
    /// Wire protocol label (e.g. "openai", "anthropic", "google").
    pub protocol: String,
    /// Base URL endpoint.
    pub base_url: String,
    /// Authentication type description (e.g. "API Key", "OAuth").
    pub auth_type: String,
    /// Masked API key preview for security (e.g. "sk-12...abcd"), or None if no key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_masked: Option<String>,
    /// Where the credential originates (e.g. "credentials.toml", "Environment (DEEPSEEK_API_KEY)").
    pub api_key_source: String,
    /// Client identity configuration.
    pub client_identity: crate::ClientIdentity,
    /// Effective User-Agent string sent with requests.
    pub user_agent: String,
    /// Every model served by this connection.
    #[serde(default)]
    pub models: Vec<String>,
    /// Active / default model for this connection if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_model: Option<String>,
    /// Remote provider usage / quota / balance state.
    pub usage: ConnectionUsageState,
}
