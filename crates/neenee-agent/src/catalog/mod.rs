//! Materializes the runtime `Catalog` from the instance store, the template
//! registry, and the discovery cache — never from `config.toml`, which holds
//! behavior only.
//!
//! Provider instances are persisted in `providers.toml` (see
//! `neenee_persistence::instances`); their routes (one channel per model) are
//! **derived** here at startup and on every switch from the instance's
//! template plus the per-route facts in `models_discovery.json`. Every
//! [`neenee_contracts::catalog::Channel`] produced here carries fully resolved
//! credentials and model id, so provider construction
//! (`build_provider_for_channel` in `neenee-providers`) never touches the
//! environment, config, or stores again.

mod derive;
mod discovery;
mod legacy;
mod picker;

pub use derive::{derive_channel, derive_entries, route_models, transport_for_protocol};
pub use discovery::{discover_provider_models, sync_fitted_model_registry};
pub use legacy::migrate_legacy_state;
use picker::active_model_id_for_entry;
pub use picker::build_picker_state;

use neenee_contracts::catalog::ProviderEntry;
use neenee_persistence::config::{Config, Credentials, DiscoveryCache};
use neenee_persistence::instances::Instances;

#[cfg(test)]
mod tests;

/// The three stores the catalog derives from, loaded together so a caller
/// that builds an entry and then mutates the stores stays consistent.
pub struct Stores {
    pub instances: Instances,
    pub cache: DiscoveryCache,
    pub creds: Credentials,
}

impl Stores {
    pub fn load() -> Self {
        Self {
            instances: Instances::load(),
            cache: DiscoveryCache::load(),
            creds: Credentials::load(),
        }
    }
}

pub fn default_provider_id(config: &Config) -> &str {
    &config.default_provider
}

/// The effective default provider id: `config.default_provider` when it names
/// a live instance, else the first instance, else empty.
pub fn effective_default_provider_id(config: &Config, stores: &Stores) -> String {
    stores
        .instances
        .effective_default(&config.default_provider)
        .map(|p| p.id.clone())
        .unwrap_or_default()
}

pub fn build_catalog() -> Vec<ProviderEntry> {
    let stores = Stores::load();
    derive_entries(&stores.instances, &stores.cache, &stores.creds)
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
    let stores = Stores::load();
    let entry = derive_entries(&stores.instances, &stores.cache, &stores.creds)
        .into_iter()
        .find(|e| e.id == provider_id)?;
    let wanted = model_id.or(config.default_model.as_deref());
    let channel = wanted
        .and_then(|m| entry.channel_for_model(m))
        .or_else(|| entry.default_channel());
    channel
        .map(|channel| neenee_providers::build_provider_for_channel(channel, &entry.id, session_id))
}

pub fn resolved_model_name(config: &Config, id: &str) -> Option<String> {
    resolved_model_name_inner(
        config,
        id,
        &neenee_persistence::provider_usage::ProviderUsage::default(),
    )
}

pub fn resolved_model_name_with_usage(
    config: &Config,
    id: &str,
    usage: &neenee_persistence::provider_usage::ProviderUsage,
) -> Option<String> {
    resolved_model_name_inner(config, id, usage)
}

fn resolved_model_name_inner(
    config: &Config,
    id: &str,
    usage: &neenee_persistence::provider_usage::ProviderUsage,
) -> Option<String> {
    build_catalog()
        .iter()
        .find(|e| e.id == id)
        .and_then(|entry| active_model_id_for_entry(config, entry, usage))
}

pub fn models_for_provider(_config: &Config, provider_id: &str) -> Vec<String> {
    build_catalog()
        .iter()
        .find(|e| e.id == provider_id)
        .map(|entry| entry.channels.iter().map(|c| c.model.clone()).collect())
        .unwrap_or_default()
}
