//! Materializes the runtime `Catalog` from the connection store, the preset
//! registry, and the discovery cache — never from `config.toml`, which holds
//! behavior only.

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
use neenee_persistence::connection_usage::ConnectionUsage;
use neenee_persistence::connections::Connections;
use neenee_persistence::route_settings::RouteSettingsStore;

#[cfg(test)]
mod tests;

/// The stores the catalog derives from.
pub struct Stores {
    pub connections: Connections,
    pub instances: Connections,
    pub cache: DiscoveryCache,
    pub routes: RouteSettingsStore,
    pub creds: Credentials,
}

impl Stores {
    pub fn load() -> Self {
        let connections = Connections::load();
        Self {
            instances: connections.clone(),
            connections,
            cache: DiscoveryCache::load(),
            routes: RouteSettingsStore::load(),
            creds: Credentials::load(),
        }
    }
}

pub fn default_connection_id(config: &Config) -> &str {
    &config.default_connection
}

pub fn default_provider_id(config: &Config) -> &str {
    default_connection_id(config)
}

/// The effective default connection id.
pub fn effective_default_connection_id(config: &Config, stores: &Stores) -> String {
    stores
        .connections
        .effective_default(&config.default_connection)
        .map(|p| p.id.clone())
        .unwrap_or_default()
}

pub fn effective_default_provider_id(config: &Config, stores: &Stores) -> String {
    effective_default_connection_id(config, stores)
}

pub fn build_catalog() -> Vec<ProviderEntry> {
    let stores = Stores::load();
    derive_entries(
        &stores.connections,
        &stores.cache,
        &stores.routes,
        &stores.creds,
    )
}

pub fn build_provider_for(
    config: &Config,
    id: &str,
) -> Option<std::sync::Arc<dyn neenee_contracts::Provider>> {
    build_provider_for_model(config, id, config.default_model.as_deref(), None)
}

pub fn build_provider_for_model(
    config: &Config,
    connection_id: &str,
    model_id: Option<&str>,
    session_id: Option<&str>,
) -> Option<std::sync::Arc<dyn neenee_contracts::Provider>> {
    let stores = Stores::load();
    let entry = derive_entries(
        &stores.connections,
        &stores.cache,
        &stores.routes,
        &stores.creds,
    )
    .into_iter()
    .find(|e| e.id == connection_id)?;
    let wanted = model_id.or(config.default_model.as_deref());
    let channel = wanted
        .and_then(|m| entry.channel_for_model(m))
        .or_else(|| entry.default_channel());
    channel
        .map(|channel| neenee_providers::build_provider_for_channel(channel, &entry.id, session_id))
}

pub fn resolved_model_name(config: &Config, id: &str) -> Option<String> {
    resolved_model_name_inner(config, id, &ConnectionUsage::default())
}

pub fn resolved_model_name_with_usage(
    config: &Config,
    id: &str,
    usage: &ConnectionUsage,
) -> Option<String> {
    resolved_model_name_inner(config, id, usage)
}

fn resolved_model_name_inner(
    config: &Config,
    id: &str,
    usage: &ConnectionUsage,
) -> Option<String> {
    build_catalog()
        .iter()
        .find(|e| e.id == id)
        .and_then(|entry| active_model_id_for_entry(config, entry, usage))
}

pub fn models_for_connection(_config: &Config, connection_id: &str) -> Vec<String> {
    build_catalog()
        .iter()
        .find(|e| e.id == connection_id)
        .map(|entry| entry.channels.iter().map(|c| c.model.clone()).collect())
        .unwrap_or_default()
}

pub fn models_for_provider(config: &Config, provider_id: &str) -> Vec<String> {
    models_for_connection(config, provider_id)
}
