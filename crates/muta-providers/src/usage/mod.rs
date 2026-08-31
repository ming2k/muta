//! Provider usage and quota querying.
//!
//! Providers differ in their quota policies, billing models, and balance API endpoints:
//! - DeepSeek exposes `GET /user/balance` with currency, total, granted, and topped-up balances.
//! - Kimi (Moonshot) exposes `GET /v1/users/me/balance` with available, cash, and voucher balances.
//! - OpenRouter exposes `GET /api/v1/auth/key` with usage, credit limit, and rate limits.
//! - SiliconFlow exposes `GET /v1/user/info` with total and charge balances.
//!
//! This module defines an extensible trait [`ProviderUsageFetcher`] and a unified dispatcher
//! [`fetch_provider_usage`] that translates provider-specific responses into the generic
//! [`muta_contracts::ProviderUsage`] model.

use muta_contracts::async_trait;
use muta_contracts::{ConnectionUsageState, ProviderUsage};

mod deepseek;
mod kimi;
mod openrouter;
mod siliconflow;

pub use deepseek::DeepSeekUsageFetcher;
pub use kimi::KimiUsageFetcher;
pub use openrouter::OpenRouterUsageFetcher;
pub use siliconflow::SiliconFlowUsageFetcher;

/// Trait implemented by provider-specific usage / quota fetchers.
#[async_trait]
pub trait ProviderUsageFetcher: Send + Sync {
    /// Whether this fetcher handles the given preset or base URL.
    fn matches(&self, preset_id: Option<&str>, base_url: &str) -> bool;

    /// Fetch and normalize usage/quota data from the provider endpoint.
    async fn fetch_usage(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        api_key: &str,
    ) -> Result<ProviderUsage, String>;
}

/// Registry of built-in provider usage fetchers.
pub fn registered_fetchers() -> &'static [&'static dyn ProviderUsageFetcher] {
    &[
        &DeepSeekUsageFetcher,
        &KimiUsageFetcher,
        &OpenRouterUsageFetcher,
        &SiliconFlowUsageFetcher,
    ]
}

/// Query provider usage for a connection based on preset or endpoint URL.
pub async fn fetch_provider_usage(
    preset_id: Option<&str>,
    base_url: &str,
    api_key: &str,
) -> ConnectionUsageState {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    fetch_provider_usage_with_client(&client, preset_id, base_url, api_key).await
}

/// Query provider usage using an explicit reqwest client.
pub async fn fetch_provider_usage_with_client(
    client: &reqwest::Client,
    preset_id: Option<&str>,
    base_url: &str,
    api_key: &str,
) -> ConnectionUsageState {
    let key = api_key.trim();
    if key.is_empty() {
        return ConnectionUsageState::Error("API key is not configured".to_string());
    }

    for fetcher in registered_fetchers() {
        if fetcher.matches(preset_id, base_url) {
            return match fetcher.fetch_usage(client, base_url, key).await {
                Ok(usage) => ConnectionUsageState::Available(usage),
                Err(err) => ConnectionUsageState::Error(err),
            };
        }
    }

    ConnectionUsageState::Unsupported
}
