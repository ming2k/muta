//! Live model discovery and the fitted-model overlay.
//!
//! Discovery fetches each discovery-capable template instance's `GET /models`
//! list live, intersects it against the client registry (or, for trusted
//! fitting templates, materializes every advertised id), and records the
//! result in the per-instance discovery cache. Routes are *derived* from that
//! cache at catalog-build time — nothing here mutates config or the instance
//! store. On an error or empty result the last valid subset is retained, so a
//! broken endpoint never regresses a working instance.

use super::Stores;
use super::derive::{resolve_credential, route_models};
use neenee_contracts::{ChannelAuth, WireFormat};
use neenee_persistence::config::{DiscoveryCache, FittedModelInfo};
use neenee_providers::{
    DiscoveryProtocol, ModelDiscoveryRequest, ProviderTemplateSpec, provider_template_spec,
    route_for_model,
};
use std::collections::HashSet;

/// The result of a live model-discovery pass ([`discover_provider_models`]).
///
/// Discovery is best-effort across every template-sourced instance: one
/// provider failing to fetch never aborts the others. This struct carries both
/// signals back so the caller can persist only when something changed *and*
/// surface a per-provider failure to the user instead of letting a silently
/// stale seed list read as "the account just has these models".
#[derive(Debug, Default)]
pub struct DiscoveryOutcome {
    /// Whether any instance changed its cached model list or fitted metadata.
    pub changed: bool,
    /// Per-provider fetch failures: `(instance_id, error_message)`. Empty when
    /// every discovered instance succeeded.
    pub failures: Vec<(String, String)>,
}

/// Fetch every discovery-capable instance's live model list and update the
/// discovery cache. Called at startup (best-effort, non-blocking) and by the
/// TUI's refresh action.
pub async fn discover_provider_models() -> DiscoveryOutcome {
    let mut stores = Stores::load();
    let mut changed = false;
    let mut failures: Vec<(String, String)> = Vec::new();

    for instance in &stores.instances.providers {
        let Some(tid) = instance.template_id.as_deref() else {
            continue;
        };
        let Some(spec) = provider_template_spec(tid) else {
            continue;
        };
        if !spec.discovery
            || instance.auth == ChannelAuth::AntigravityOAuth
            || instance
                .base_url
                .as_deref()
                .is_some_and(|u| u.contains("cloudcode-pa.googleapis.com"))
        {
            continue;
        }

        // The discovery request mirrors what a chat request would send: the
        // route's endpoint/key/user-agent. Build it from the first derived
        // route of the instance.
        let first_model = route_models(instance, &stores.cache)
            .into_iter()
            .next()
            .unwrap_or_default();
        let Some((protocol, template_base, tpl_ua)) = route_for_model(tid, &first_model) else {
            continue;
        };
        let base_url = instance
            .base_url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| template_base.to_string());
        let user_agent = instance
            .user_agent
            .clone()
            .or_else(|| tpl_ua.map(str::to_string));

        let discovery_req = ModelDiscoveryRequest {
            protocol: DiscoveryProtocol::from_template_protocol(protocol),
            base_url: &base_url,
            api_key: &resolve_credential(instance, &stores.creds),
            user_agent: user_agent.as_deref(),
            extra_headers: &[],
        };

        match neenee_providers::list_models(discovery_req).await {
            Ok(models) => {
                let supported: Vec<String> = if spec.fitting {
                    // Trusted endpoint: every advertised id is kept, and ids the
                    // static registry does not know have their advertised
                    // capability metadata fitted (registry-known ids keep the
                    // vetted entry, so a provider can never downgrade a known
                    // model).
                    let fitted: std::collections::BTreeMap<String, FittedModelInfo> = models
                        .iter()
                        .filter(|model| neenee_contracts::model::model_by_id(&model.id).is_none())
                        .map(|model| (model.id.clone(), fitted_model_info(model)))
                        .collect();
                    if stores.cache.fitted_models.get(&instance.id) != Some(&fitted) {
                        stores
                            .cache
                            .fitted_models
                            .insert(instance.id.clone(), fitted);
                        changed = true;
                    }
                    models
                        .iter()
                        .filter(|model| model.picker_enabled != Some(false))
                        .map(|model| model.id.clone())
                        .collect()
                } else {
                    // Only expose models both advertised and known to the
                    // client for this wire protocol. Preserve registry order so
                    // provider response ordering cannot churn the picker.
                    let ids: Vec<String> = models.iter().map(|model| model.id.clone()).collect();
                    supported_model_intersection(&supported_models_for_template(spec), &ids)
                        .into_iter()
                        .map(str::to_string)
                        .collect()
                };
                if supported.is_empty() {
                    tracing::warn!(
                        instance_id = %instance.id,
                        discovered_count = models.len(),
                        "live model discovery had no supported intersection; keeping previous models"
                    );
                    continue;
                }
                // Persist the per-route remote metadata (Kimi/Copilot advertise
                // endpoint + capability fields) for the derived routes.
                let remote_metadata: std::collections::BTreeMap<String, _> = models
                    .iter()
                    .filter(|model| model.picker_enabled != Some(false))
                    .map(|model| (model.id.clone(), model.remote_metadata()))
                    .collect();
                let prev_remote = stores
                    .cache
                    .remote_metadata
                    .get(&instance.id)
                    .cloned()
                    .unwrap_or_default();
                if prev_remote != remote_metadata {
                    stores
                        .cache
                        .remote_metadata
                        .insert(instance.id.clone(), remote_metadata);
                    changed = true;
                }
                if stores.cache.provider_models.get(&instance.id) != Some(&supported) {
                    stores
                        .cache
                        .provider_models
                        .insert(instance.id.clone(), supported);
                    changed = true;
                }
                if changed {
                    tracing::info!(
                        instance_id = %instance.id,
                        discovered_count = models.len(),
                        "live model discovery updated instance"
                    );
                }
            }
            Err(error) => {
                // The previous valid subset (or initial snapshot) remains in
                // place; a failed fetch never regresses the provider. Report it
                // back so the caller can surface the cause to the user rather
                // than letting a silently-stale list read as "login worked, the
                // account just has one model".
                tracing::warn!(
                    instance_id = %instance.id,
                    error = %error,
                    "live model discovery failed; keeping previous models"
                );
                failures.push((instance.id.clone(), error.to_string()));
            }
        }
    }

    if changed {
        let _ = stores.cache.save();
    }

    DiscoveryOutcome { changed, failures }
}

/// Rebuild the fitted-model overlay (`neenee_contracts::model`) from the
/// discovery cache. Called at startup after discovery so model resolution sees
/// platform-fitted ids.
pub fn sync_fitted_model_registry() {
    let cache = DiscoveryCache::load();
    let instances = neenee_persistence::instances::Instances::load();
    let fitted: Vec<neenee_contracts::model::FittedModel> = instances
        .providers
        .iter()
        .flat_map(|instance| {
            let spec = instance
                .template_id
                .as_deref()
                .and_then(provider_template_spec);
            let fitted_map = cache.fitted_models.get(&instance.id);
            fitted_map.map(|map| {
                let (format, family) = match spec {
                    Some(spec) => (wire_format_for_protocol(spec.protocol), spec.id.to_string()),
                    None => (WireFormat::OpenAi, instance.id.clone()),
                };
                map.iter()
                    .map(move |(id, info)| neenee_contracts::model::FittedModel {
                        id: id.clone(),
                        family: family.clone(),
                        context_window: info.context_window,
                        reasoning: info.reasoning,
                        vision: info.vision,
                        format,
                        effort_levels: info
                            .efforts
                            .iter()
                            .filter_map(|level| match neenee_contracts::Effort::parse(level) {
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
    neenee_contracts::model::register_fitted_models(fitted);
}

/// The model ids a template serves over its protocol's wire format.
fn supported_models_for_template(spec: &ProviderTemplateSpec) -> Vec<&'static str> {
    spec.baselines
        .iter()
        .filter(|model| {
            matches!(
                (spec.protocol, model.format),
                ("openai", WireFormat::OpenAi)
                    | ("openai-responses", WireFormat::OpenAi)
                    | ("anthropic", WireFormat::AnthropicCompat)
                    | ("google", WireFormat::Google)
                    | ("gemini", WireFormat::Google) // legacy label
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

fn fitted_model_info(model: &neenee_providers::DiscoveredModel) -> FittedModelInfo {
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
