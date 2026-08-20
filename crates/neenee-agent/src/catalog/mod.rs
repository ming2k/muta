//! Materializes a `Catalog` from the host crate's [`Config`].
//!
//! This is the single source of truth for the environment-variable-then-config
//! resolution rules that startup and runtime provider switching share. Every
//! [`Channel`] produced here carries fully resolved credentials and model id, so
//! provider construction (`build_provider_for_channel` in `neenee-providers`)
//! never touches the environment or config again.
//!
//! ADR-0002: built-in presets produce one `"default"` channel per entry from
//! the per-provider fields, while user-defined entries may declare several
//! channels (with `default_channel` selecting one). Favorites and recency are
//! layered on top via the provider-usage telemetry.

mod discovery;
mod migrate;
mod picker;
mod translate;

pub use discovery::{
    DiscoveryOutcome, default_model_source_for_spec, discover_provider_models,
    reconcile_provider_models, sync_fitted_model_registry,
};
pub use migrate::{
    DEEPSEEK_RESPONSES_URL, migrate_deepseek_channels_to_responses,
    migrate_legacy_provider_instances,
};
use picker::active_model_id_for_entry;
pub use picker::build_picker_state;
use translate::user_provider_to_entry;

use neenee_contracts::catalog::ProviderEntry;
use neenee_persistence::config::Config;
use neenee_persistence::provider_usage::ProviderUsage;

#[cfg(test)]
mod tests;

pub fn default_provider_id(config: &Config) -> &str {
    &config.default_provider
}

pub fn build_catalog(config: &Config) -> Vec<ProviderEntry> {
    config
        .providers
        .iter()
        .map(user_provider_to_entry)
        .collect()
}

pub fn build_provider_for(
    config: &Config,
    id: &str,
) -> Option<std::sync::Arc<dyn neenee_contracts::Provider>> {
    build_provider_for_model(config, id, config.default_model.as_deref(), None)
}

pub fn build_provider_for_model(
    config: &Config,
    provider_id: &str,
    model_id: Option<&str>,
    session_id: Option<&str>,
) -> Option<std::sync::Arc<dyn neenee_contracts::Provider>> {
    let entries = build_catalog(config);
    let entry = entries.iter().find(|e| e.id == provider_id)?;
    let wanted = model_id.or(config.default_model.as_deref());
    let channel = wanted
        .and_then(|m| entry.channel_for_model(m))
        .or_else(|| entry.default_channel());
    channel
        .map(|channel| neenee_providers::build_provider_for_channel(channel, &entry.id, session_id))
}

pub fn resolved_model_name(config: &Config, id: &str) -> Option<String> {
    resolved_model_name_inner(config, id, &ProviderUsage::default())
}

pub fn resolved_model_name_with_usage(
    config: &Config,
    id: &str,
    usage: &ProviderUsage,
) -> Option<String> {
    resolved_model_name_inner(config, id, usage)
}

fn resolved_model_name_inner(config: &Config, id: &str, usage: &ProviderUsage) -> Option<String> {
    build_catalog(config)
        .iter()
        .find(|e| e.id == id)
        .and_then(|entry| active_model_id_for_entry(config, entry, usage))
}

pub fn models_for_provider(config: &Config, provider_id: &str) -> Vec<String> {
    build_catalog(config)
        .iter()
        .find(|e| e.id == provider_id)
        .map(|entry| entry.channels.iter().map(|c| c.model.clone()).collect())
        .unwrap_or_default()
}
