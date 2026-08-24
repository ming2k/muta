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
use muta_contracts::{ChannelAuth, WireFormat};
use muta_persistence::config::{DiscoveryCache, FittedModelInfo};
use muta_persistence::connections::Connections;
use muta_providers::{
    DiscoveryProtocol, ModelDiscoveryRequest, ProviderPresetSpec, provider_preset_spec,
    route_for_model,
};
use std::collections::HashSet;

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
pub async fn discover_provider_models() -> DiscoveryOutcome {
    let mut stores = Stores::load();
    let mut changed = false;
    let mut failures: Vec<(String, String)> = Vec::new();

    for connection in &stores.connections.connections {
        let Some(pid) = connection.preset_id.as_deref() else {
            continue;
        };
        let Some(spec) = provider_preset_spec(pid) else {
            continue;
        };
        if !spec.discovery
            || connection.auth == ChannelAuth::AntigravityOAuth
            || connection
                .base_url
                .as_deref()
                .is_some_and(|u| u.contains("cloudcode-pa.googleapis.com"))
        {
            continue;
        }

        let first_model = route_models(connection, &stores.cache)
            .into_iter()
            .next()
            .unwrap_or_default();
        let Some((protocol, template_base, tpl_ua)) = route_for_model(pid, &first_model) else {
            continue;
        };
        let base_url = connection
            .base_url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| template_base.to_string());
        let user_agent = connection.user_agent.clone().or_else(|| {
            if connection.client_identity != muta_contracts::ClientIdentity::Native {
                Some(connection.client_identity.user_agent().to_string())
            } else {
                tpl_ua.map(str::to_string)
            }
        });

        let discovery_req = ModelDiscoveryRequest {
            protocol: DiscoveryProtocol::from_template_protocol(protocol),
            base_url: &base_url,
            api_key: &resolve_credential(connection, &stores.creds),
            user_agent: user_agent.as_deref(),
            extra_headers: &[],
        };

        match muta_providers::list_models(discovery_req).await {
            Ok(models) => {
                let supported: Vec<String> = if spec.fitting {
                    let fitted: std::collections::BTreeMap<String, FittedModelInfo> = models
                        .iter()
                        .filter(|model| muta_contracts::model::model_by_id(&model.id).is_none())
                        .map(|model| (model.id.clone(), fitted_model_info(model)))
                        .collect();
                    if stores.cache.fitted_models.get(&connection.id) != Some(&fitted) {
                        stores
                            .cache
                            .fitted_models
                            .insert(connection.id.clone(), fitted);
                        changed = true;
                    }
                    models
                        .iter()
                        .filter(|model| model.picker_enabled != Some(false))
                        .map(|model| model.id.clone())
                        .collect()
                } else {
                    let ids: Vec<String> = models.iter().map(|model| model.id.clone()).collect();
                    supported_model_intersection(&supported_models_for_template(spec), &ids)
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
                    changed = true;
                }
                if stores.cache.connection_models.get(&connection.id) != Some(&supported) {
                    stores
                        .cache
                        .connection_models
                        .insert(connection.id.clone(), supported);
                    changed = true;
                }
                if changed {
                    tracing::info!(
                        connection_id = %connection.id,
                        discovered_count = models.len(),
                        "live model discovery updated connection"
                    );
                }
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

    if changed {
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
                    Some(spec) => (wire_format_for_protocol(spec.protocol), spec.id.to_string()),
                    None => (WireFormat::OpenAi, connection.id.clone()),
                };
                map.iter()
                    .map(move |(id, info)| muta_contracts::model::FittedModel {
                        id: id.clone(),
                        family: family.clone(),
                        context_window: info.context_window,
                        reasoning: info.reasoning,
                        vision: info.vision,
                        format,
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

/// The model ids a preset serves over its protocol's wire format.
fn supported_models_for_template(spec: &ProviderPresetSpec) -> Vec<&'static str> {
    spec.baselines
        .iter()
        .filter(|model| {
            matches!(
                (spec.protocol, model.format),
                ("openai", WireFormat::OpenAi)
                    | ("openai-responses", WireFormat::OpenAi)
                    | ("anthropic", WireFormat::AnthropicCompat)
                    | ("google", WireFormat::Google)
                    | ("gemini", WireFormat::Google)
            )
        })
        .map(|model| model.id)
        .collect()
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

fn wire_format_for_protocol(protocol: &str) -> WireFormat {
    match protocol {
        "anthropic" => WireFormat::AnthropicCompat,
        "google" | "gemini" => WireFormat::Google,
        _ => WireFormat::OpenAi,
    }
}
