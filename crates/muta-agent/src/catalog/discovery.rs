//! Live model discovery and the fitted-model overlay.
//!
//! Discovery fetches each discovery-capable preset connection's `GET /models`
//! list live, intersects it against the client registry (or, for trusted
//! fitting presets, materializes every advertised id), and records the
//! result in the per-connection discovery cache. Routes are *derived* from that
//! cache at catalog-build time — nothing here mutates config or the connection
//! store. On an error or empty result the last valid subset is retained, so a
//! broken endpoint never regresses a working connection.

use super::Stores;
use super::derive::{resolve_credential, route_models};
use muta_contracts::{ConnectionAuth, WireProtocol};
use muta_persistence::config::{DiscoveryCache, FittedModelInfo, ModelListCacheState};
use muta_persistence::connections::Connections;
use muta_providers::{
    DiscoveryProtocol, ModelDiscoveryOptions, ModelDiscoveryRequest, ModelDiscoveryUpdate,
    ProviderPresetSpec, provider_preset_spec, route_for_model,
};
use std::collections::HashSet;

const MODEL_LIST_CACHE_TTL_MS: i64 = 5 * 60 * 1000;
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The result of a live model-discovery pass ([`discover_provider_models`]).
#[derive(Debug, Default)]
pub struct DiscoveryOutcome {
    /// Whether any connection changed its cached model list or fitted metadata.
    pub changed: bool,
    /// Per-connection fetch failures: `(connection_id, error_message)`.
    pub failures: Vec<(String, String)>,
}

/// Fetch every discovery-capable connection's live model list and update the
/// discovery cache.
pub async fn discover_provider_models(force: bool) -> DiscoveryOutcome {
    let mut stores = Stores::load();
    let mut changed = false;
    let mut cache_dirty = false;
    let mut failures: Vec<(String, String)> = Vec::new();
    let now_ms = chrono::Utc::now().timestamp_millis();

    for connection in &stores.connections.connections {
        let Some(pid) = connection.preset_id.as_deref() else {
            continue;
        };
        let Some(spec) = provider_preset_spec(pid) else {
            continue;
        };
        if !spec.discovery
            || connection.auth == ConnectionAuth::AntigravityOAuth
            || connection
                .base_url
                .as_deref()
                .is_some_and(|u| u.contains("cloudcode-pa.googleapis.com"))
        {
            continue;
        }
        if !force
            && stores
                .cache
                .model_lists
                .get(&connection.id)
                .is_some_and(|state| {
                    state.client_version == CLIENT_VERSION
                        && now_ms.saturating_sub(state.refreshed_at_ms) < MODEL_LIST_CACHE_TTL_MS
                })
            && stores
                .cache
                .connection_models
                .get(&connection.id)
                .is_some_and(|models| !models.is_empty())
        {
            continue;
        }

        let first_model = route_models(connection, &stores.cache)
            .into_iter()
            .next()
            .unwrap_or_default();
        let Some((protocol, preset_base, preset_ua)) = route_for_model(pid, &first_model) else {
            continue;
        };
        let base_url = connection
            .base_url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| preset_base.to_string());
        let user_agent = connection.user_agent.clone().or_else(|| {
            if connection.client_identity != muta_contracts::ClientIdentity::Native {
                Some(connection.client_identity.user_agent().to_string())
            } else {
                preset_ua.map(str::to_string)
            }
        });

        let protocol = DiscoveryProtocol::for_connection(
            connection.preset_id.as_deref(),
            connection.auth,
            protocol,
        );
        let auth = if connection.auth.is_oauth() {
            let source = muta_providers::oauth::OAuthCredentialSource::new(
                &connection.id,
                connection.preset_id.as_deref(),
                connection.auth,
            );
            match muta_contracts::CredentialSource::resolve_auth(&source).await {
                Ok(auth) => auth,
                Err(error) => {
                    tracing::warn!(
                        connection_id = %connection.id,
                        error = %error,
                        "could not resolve OAuth model-catalog authentication"
                    );
                    failures.push((connection.id.clone(), error));
                    continue;
                }
            }
        } else {
            let key = resolve_credential(connection, &stores.creds);
            muta_contracts::ResolvedAuth::new(key)
        };
        let cached_etag = stores
            .cache
            .model_lists
            .get(&connection.id)
            .and_then(|state| state.etag.as_deref());
        let discovery_req = ModelDiscoveryRequest {
            protocol,
            base_url: &base_url,
            api_key: &auth.token,
            account_id: auth.account_id.as_deref(),
            user_agent: user_agent.as_deref(),
            extra_headers: &[],
        };
        let options = ModelDiscoveryOptions { etag: cached_etag };

        match muta_providers::discover_models(discovery_req, options).await {
            Ok(ModelDiscoveryUpdate::Modified { models, etag }) => {
                let mut connection_changed = false;
                let supported: Vec<String> = if spec.fitting {
                    let fitted: std::collections::BTreeMap<String, FittedModelInfo> = models
                        .iter()
                        .filter(|model| model.picker_enabled != Some(false))
                        .filter(|model| muta_contracts::model::model_by_id(&model.id).is_none())
                        .map(|model| (model.id.clone(), fitted_model_info(model)))
                        .collect();
                    if stores.cache.fitted_models.get(&connection.id) != Some(&fitted) {
                        stores
                            .cache
                            .fitted_models
                            .insert(connection.id.clone(), fitted);
                        connection_changed = true;
                    }
                    models
                        .iter()
                        .filter(|model| model.picker_enabled != Some(false))
                        .map(|model| model.id.clone())
                        .collect()
                } else {
                    let ids: Vec<String> = models.iter().map(|model| model.id.clone()).collect();
                    supported_model_intersection(&supported_models_for_preset(spec), &ids)
                        .into_iter()
                        .map(str::to_string)
                        .collect()
                };
                if supported.is_empty() {
                    tracing::warn!(
                        connection_id = %connection.id,
                        discovered_count = models.len(),
                        "live model discovery had no supported intersection; keeping previous models"
                    );
                    continue;
                }
                let remote_metadata: std::collections::BTreeMap<String, _> = models
                    .iter()
                    .filter(|model| model.picker_enabled != Some(false))
                    .map(|model| (model.id.clone(), model.remote_metadata()))
                    .collect();
                let prev_remote = stores
                    .cache
                    .remote_metadata
                    .get(&connection.id)
                    .cloned()
                    .unwrap_or_default();
                if prev_remote != remote_metadata {
                    stores
                        .cache
                        .remote_metadata
                        .insert(connection.id.clone(), remote_metadata);
                    connection_changed = true;
                }
                if stores.cache.connection_models.get(&connection.id) != Some(&supported) {
                    stores
                        .cache
                        .connection_models
                        .insert(connection.id.clone(), supported);
                    connection_changed = true;
                }
                let state = ModelListCacheState {
                    etag,
                    client_version: CLIENT_VERSION.to_string(),
                    refreshed_at_ms: now_ms,
                };
                if stores.cache.model_lists.get(&connection.id) != Some(&state) {
                    stores
                        .cache
                        .model_lists
                        .insert(connection.id.clone(), state);
                    cache_dirty = true;
                }
                if connection_changed {
                    changed = true;
                    cache_dirty = true;
                    tracing::info!(
                        connection_id = %connection.id,
                        discovered_count = models.len(),
                        "live model discovery updated connection"
                    );
                }
            }
            Ok(ModelDiscoveryUpdate::NotModified { etag }) => {
                stores.cache.model_lists.insert(
                    connection.id.clone(),
                    ModelListCacheState {
                        etag,
                        client_version: CLIENT_VERSION.to_string(),
                        refreshed_at_ms: now_ms,
                    },
                );
                cache_dirty = true;
                tracing::debug!(
                    connection_id = %connection.id,
                    "live model catalog revalidated without changes"
                );
            }
            Err(error) => {
                tracing::warn!(
                    connection_id = %connection.id,
                    error = %error,
                    "live model discovery failed; keeping previous models"
                );
                failures.push((connection.id.clone(), error.to_string()));
            }
        }
    }

    if cache_dirty {
        let _ = stores.cache.save();
    }

    DiscoveryOutcome { changed, failures }
}

/// Rebuild the fitted-model overlay (`muta_contracts::model`) from the
/// discovery cache.
pub fn sync_fitted_model_registry() {
    let cache = DiscoveryCache::load();
    let connections = Connections::load();
    let fitted: Vec<muta_contracts::model::FittedModel> = connections
        .connections
        .iter()
        .flat_map(|connection| {
            let spec = connection
                .preset_id
                .as_deref()
                .and_then(provider_preset_spec);
            let fitted_map = cache.fitted_models.get(&connection.id);
            fitted_map.map(|map| {
                let (format, family) = match spec {
                    Some(spec) => (spec.protocol, spec.id.to_string()),
                    None => (WireProtocol::OpenAiChatCompletions, connection.id.clone()),
                };
                map.iter()
                    .map(move |(id, info)| muta_contracts::model::FittedModel {
                        id: id.clone(),
                        family: family.clone(),
                        context_window: info.context_window,
                        reasoning: info.reasoning,
                        vision: info.vision,
                        protocol: format,
                        effort_levels: info
                            .efforts
                            .iter()
                            .filter_map(|level| match muta_contracts::Effort::parse(level) {
                                Some(e) => Some(e),
                                None => {
                                    tracing::warn!(
                                        level = level,
                                        model = %id,
                                        "effort tier outside the known vocabulary; \
                                         preserved on the channel but not the static \
                                         baseline"
                                    );
                                    None
                                }
                            })
                            .collect(),
                    })
            })
        })
        .flatten()
        .collect();
    muta_contracts::model::register_fitted_models(fitted);
}

/// The model ids explicitly owned by a preset.
fn supported_models_for_preset(spec: &ProviderPresetSpec) -> Vec<&'static str> {
    spec.baselines.iter().map(|model| model.id).collect()
}

/// Preserve `supported` order, keeping only ids present in `available`.
fn supported_model_intersection<'a>(supported: &[&'a str], available: &[String]) -> Vec<&'a str> {
    let available = available.iter().map(String::as_str).collect::<HashSet<_>>();
    supported
        .iter()
        .copied()
        .filter(|model| available.contains(model))
        .collect()
}

fn fitted_model_info(model: &muta_providers::DiscoveredModel) -> FittedModelInfo {
    FittedModelInfo {
        context_window: model.context_window.unwrap_or(0),
        reasoning: model.reasoning.unwrap_or(false),
        vision: model.vision.unwrap_or(false),
        efforts: model.effort_levels.clone().unwrap_or_default(),
    }
}
