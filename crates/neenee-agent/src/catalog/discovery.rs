//! Live model discovery and template reconciliation: mirror template model
//! lists into instances, fetch `GET /models` from discovery-capable
//! templates, fit advertised capabilities, and sync the fitted-model
//! overlay that model resolution reads.

use super::migrate::{matching_template, transport_for_protocol};
use neenee_contracts::{Effort, SecretString, WireFormat};
use neenee_persistence::config::{
    Config, DiscoveryCache, FittedModelInfo, ModelSource, UserProviderConfig,
};
use neenee_providers::{
    OPENCODE_USER_AGENT, ProviderTemplateSpec, ZCODE_USER_AGENT, provider_template_spec,
};
use std::collections::HashSet;

pub fn reconcile_provider_models(config: &mut Config) -> bool {
    let mut changed = false;

    for provider in &mut config.providers {
        // A known template_id → reconcile against the client-supported set.
        if let Some(tid) = provider.template_id.as_deref()
            && let Some(spec) = provider_template_spec(tid)
        {
            // Fixed → Api upgrade for fitting templates (see the fn docs).
            if spec.discovery && spec.fitting && provider.model_source == ModelSource::Fixed {
                provider.model_source = ModelSource::Api;
                changed = true;
            }
            if !spec.discovery && provider.model_source == ModelSource::Api {
                provider.model_source = ModelSource::Fixed;
                changed = true;
            }
            // Migrate zai-code instances from api.z.ai to the domestic open.bigmodel.cn endpoint
            // and refresh outdated user-agent strings.
            if tid == "zai-code" {
                for channel in &mut provider.channels {
                    if channel.base_url.as_deref()
                        == Some("https://api.z.ai/api/coding/paas/v4/chat/completions")
                    {
                        channel.base_url = Some(
                            "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions"
                                .to_string(),
                        );
                        changed = true;
                    }
                    if channel.user_agent.as_deref() == Some("opencode/1.17.10")
                        || channel.user_agent.as_deref() == Some("opencode/0.1.0")
                        || channel.user_agent.as_deref() == Some(OPENCODE_USER_AGENT)
                    {
                        channel.user_agent = Some(ZCODE_USER_AGENT.to_string());
                        changed = true;
                    }
                }
            }
            if tid == "kimi-code" {
                for channel in &mut provider.channels {
                    if channel.user_agent.as_deref() == Some("opencode/0.1.0") {
                        channel.user_agent = Some(OPENCODE_USER_AGENT.to_string());
                        changed = true;
                    }
                }
            }
            if (!spec.discovery
                || provider.channels.iter().any(|c| {
                    c.auth == neenee_contracts::ChannelAuth::AntigravityOAuth
                        || c.base_url
                            .as_deref()
                            .is_some_and(|u| u.contains("cloudcode-pa.googleapis.com"))
                }))
                && provider.model_source == ModelSource::Api
            {
                provider.model_source = ModelSource::Fixed;
                changed = true;
            }
            // Fitted ids from the last live fetch are as retainable as
            // registry ids — intersecting against the static registry alone
            // would undo the fitting on every startup. Owned up front so the
            // borrow of `provider` ends before the reseed below, and declared
            // outside the branch so `target_models` may borrow from it.
            let fitted_ids: Vec<String> = if spec.fitting {
                provider.fitted_models.keys().cloned().collect()
            } else {
                Vec::new()
            };
            let target_models = if provider.model_source == ModelSource::Api {
                let current_models = provider
                    .channel_models()
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let mut known_models: Vec<&str> = supported_models_for_template(spec);
                known_models.extend(fitted_ids.iter().map(String::as_str));
                let supported = supported_model_intersection(&known_models, &current_models);
                // A malformed/obsolete instance with no supported channels
                // falls back to the snapshot rather than becoming unusable.
                if supported.is_empty() {
                    spec.models.to_vec()
                } else {
                    supported
                }
            } else {
                spec.models.to_vec()
            };
            changed |= provider
                .reseed_channels_from_models(&target_models, transport_for_protocol(spec.protocol));
            continue;
        }

        // Conservative backfill for legacy (pre-template_id) instances: if the
        // instance's model set already matches a template exactly, stamp it so
        // it starts tracking future edits. Anything that does not match stays a
        // pure-custom instance.
        if provider.template_id.is_none()
            && let Some(spec) = matching_template(provider)
        {
            // Stamp the id (always a change), then re-seed. The reseed is a
            // no-op when the set already matches exactly, so this only writes
            // the new pointer without churning the channels.
            provider.template_id = Some(spec.id.to_string());
            // A legacy instance that exactly matches a template adopts the
            // template's default model source (Api where discovery is
            // supported, Fixed otherwise) so it starts benefiting from live
            // discovery on the next startup.
            provider.model_source = default_model_source_for_spec(spec);
            changed = true;
            provider
                .reseed_channels_from_models(spec.models, transport_for_protocol(spec.protocol));
        }
    }

    changed
}

pub async fn discover_provider_models(config: &mut Config) -> DiscoveryOutcome {
    let mut changed = false;
    let mut failures: Vec<(String, String)> = Vec::new();

    for provider in &mut config.providers {
        // Only template-sourced instances with discovery-enabled templates and
        // an explicit Api model source participate in live discovery.
        let Some(tid) = provider.template_id.as_deref() else {
            continue;
        };
        let Some(spec) = provider_template_spec(tid) else {
            continue;
        };
        if !spec.discovery
            || provider.model_source != ModelSource::Api
            || tid == "antigravity-oauth"
            || provider.channels.iter().any(|c| {
                c.auth == neenee_contracts::ChannelAuth::AntigravityOAuth
                    || c.base_url
                        .as_deref()
                        .is_some_and(|u| u.contains("cloudcode-pa.googleapis.com"))
            })
        {
            continue;
        }

        // Build the discovery request from the instance's first channel — the
        // channel's endpoint/key is what a chat request would actually use, so
        // auth matches exactly. A channel-less instance cannot be discovered
        // (and the snapshot reconcile has nothing to improve on either).
        let Some(channel) = provider.channels.first() else {
            continue;
        };
        let Some(base_url) = channel.base_url.as_deref() else {
            tracing::debug!(
                provider_id = %provider.id,
                "skipping live discovery: channel has no base_url"
            );
            continue;
        };
        // OAuth channels (xAI / ChatGPT / Copilot) store no api_key — their
        // bearer lives in auth.toml and is resolved at runtime. Discovery must
        // read the same token a chat request would send, so resolve it here for
        // OAuth auth modes; API-key channels keep using the stored key.
        let resolved_bearer: SecretString;
        let no_key = SecretString::default();
        let api_key: &SecretString = if channel.auth.is_oauth() {
            resolved_bearer = neenee_providers::oauth::AuthStore::load()
                .get_for_provider(&provider.id, provider.template_id.as_deref(), channel.auth)
                .map(|tokens| tokens.access.clone())
                .unwrap_or_default();
            &resolved_bearer
        } else {
            channel.api_key.as_ref().unwrap_or(&no_key)
        };
        let user_agent = channel.user_agent.as_deref();
        let protocol = neenee_providers::DiscoveryProtocol::from_template_protocol(spec.protocol);

        // Copilot's /models requires the same headers a chat request sends —
        // the client-identity headers (`Copilot-Integration-Id` and friends)
        // so the backend resolves the account's actual plan entitlements
        // instead of falling back to the always-available GPT-4o family, plus
        // the per-turn headers chat requests also send. Other OAuth providers
        // send standard auth only, so the slice stays empty for them.
        let copilot_headers: [(&str, &str); 6] = [
            neenee_llm_client::COPILOT_CLIENT_HEADERS[0],
            neenee_llm_client::COPILOT_CLIENT_HEADERS[1],
            neenee_llm_client::COPILOT_CLIENT_HEADERS[2],
            ("x-initiator", "user"),
            ("Openai-Intent", "conversation-edits"),
            ("X-GitHub-Api-Version", "2026-06-01"),
        ];
        let extra_headers: &[(&str, &str)] = if spec.id == "copilot-oauth" {
            &copilot_headers
        } else {
            &[]
        };

        let discovery_req = neenee_providers::ModelDiscoveryRequest {
            protocol,
            base_url,
            api_key,
            user_agent,
            extra_headers,
        };

        match neenee_providers::list_models(discovery_req).await {
            Ok(models) => {
                let supported: Vec<&str> = if spec.fitting {
                    // Trusted endpoint: every advertised id is materialized,
                    // and ids the static registry does not know have their
                    // advertised capability metadata persisted for the dynamic
                    // overlay (registry-known ids keep the vetted entry, so a
                    // provider can never downgrade a known model).
                    let fitted: std::collections::BTreeMap<String, FittedModelInfo> = models
                        .iter()
                        .filter(|model| neenee_contracts::model::model_by_id(&model.id).is_none())
                        .map(|model| (model.id.clone(), fitted_model_info(model)))
                        .collect();
                    if provider.fitted_models != fitted {
                        provider.fitted_models = fitted;
                        changed = true;
                    }
                    models
                        .iter()
                        .filter(|model| model.picker_enabled != Some(false))
                        .map(|model| model.id.as_str())
                        .collect()
                } else {
                    // Only expose models both advertised by the provider and
                    // known to the client for this wire protocol. Preserve
                    // registry order so provider response ordering cannot
                    // churn the picker.
                    let ids: Vec<String> = models.iter().map(|model| model.id.clone()).collect();
                    let known_models = supported_models_for_template(spec);
                    supported_model_intersection(&known_models, &ids)
                };
                if supported.is_empty() {
                    tracing::warn!(
                        provider_id = %provider.id,
                        discovered_count = models.len(),
                        "live model discovery had no supported intersection; keeping previous models"
                    );
                    continue;
                }
                let reseated = provider
                    .reseed_channels_from_models(&supported, transport_for_protocol(spec.protocol));
                let metadata_updated = if spec.fitting {
                    persist_remote_model_metadata(provider, &models, spec.id == "copilot-oauth")
                } else {
                    false
                };
                if reseated || metadata_updated {
                    tracing::info!(
                        provider_id = %provider.id,
                        discovered_count = models.len(),
                        supported_count = supported.len(),
                        "live model discovery updated instance"
                    );
                    changed = true;
                }
            }
            Err(error) => {
                // The previous valid subset (or initial snapshot) remains in
                // place; a failed fetch never regresses the provider. Report it
                // back so the caller can surface the cause to the user rather
                // than letting a silently-stale list read as "login worked, the
                // account just has one model".
                tracing::warn!(
                    provider_id = %provider.id,
                    error = %error,
                    "live model discovery failed; keeping previous models"
                );
                failures.push((provider.id.clone(), error.to_string()));
            }
        }
    }

    if changed {
        let mut cache = DiscoveryCache::load();
        for provider in &config.providers {
            cache.provider_models.insert(
                provider.id.clone(),
                provider
                    .channels
                    .iter()
                    .filter_map(|c| c.model.clone())
                    .collect(),
            );
            if !provider.fitted_models.is_empty() {
                cache
                    .fitted_models
                    .insert(provider.id.clone(), provider.fitted_models.clone());
            }
        }
        let _ = cache.save();
    }

    DiscoveryOutcome { changed, failures }
}

/// The result of a live model-discovery pass ([`discover_provider_models`]).
///
/// Discovery is best-effort across every template-sourced instance: one
/// provider failing to fetch never aborts the others. This struct carries both
/// signals back so the caller can persist only when something changed *and*
/// surface a per-provider failure to the user instead of letting a silently
/// stale seed list read as "the account just has these models".
#[derive(Debug, Default)]
pub struct DiscoveryOutcome {
    /// Whether any provider instance changed its model list (or fitted
    /// metadata). The caller persists config only when this is `true`.
    pub changed: bool,
    /// Per-provider fetch failures: `(provider_id, error_message)`. Empty when
    /// every discovered instance succeeded.
    pub failures: Vec<(String, String)>,
}

pub(super) fn supported_models_for_template(spec: &ProviderTemplateSpec) -> Vec<&'static str> {
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

pub(super) fn supported_model_intersection<'a>(supported: &[&'a str], available: &[String]) -> Vec<&'a str> {
    let available = available.iter().map(String::as_str).collect::<HashSet<_>>();
    supported
        .iter()
        .copied()
        .filter(|model| available.contains(model))
        .collect()
}

pub(super) fn fitted_model_info(model: &neenee_providers::DiscoveredModel) -> FittedModelInfo {
    FittedModelInfo {
        context_window: model.context_window.unwrap_or(0),
        reasoning: model.reasoning.unwrap_or(false),
        vision: model.vision.unwrap_or(false),
        efforts: model.effort_levels.clone().unwrap_or_default(),
    }
}

pub(super) fn persist_remote_model_metadata(
    provider: &mut UserProviderConfig,
    discovered: &[neenee_providers::DiscoveredModel],
    use_remote_endpoint: bool,
) -> bool {
    let discovered = discovered
        .iter()
        .filter(|model| model.picker_enabled != Some(false))
        .map(|model| (model.id.as_str(), model.remote_metadata()))
        .collect::<std::collections::HashMap<_, _>>();
    let mut changed = false;
    for channel in &mut provider.channels {
        let Some(model) = channel.model.as_deref() else {
            continue;
        };
        let Some(remote) = discovered.get(model) else {
            continue;
        };
        let mut remote = remote.clone();
        // Kimi advertises capabilities but its configured coding endpoint owns
        // routing. Copilot's supported_endpoints are authoritative per model.
        if !use_remote_endpoint {
            remote.endpoint = None;
        }
        if channel.remote.as_ref() != Some(&remote) {
            channel.remote = Some(remote);
            changed = true;
        }
    }
    changed
}

pub fn sync_fitted_model_registry(config: &Config) {
    let cache = DiscoveryCache::load();
    let fitted: Vec<neenee_contracts::model::FittedModel> = config
        .providers
        .iter()
        .flat_map(|provider| {
            let spec = provider
                .template_id
                .as_deref()
                .and_then(provider_template_spec);
            let cached_fitted = cache.fitted_models.get(&provider.id);
            let fitted_map = if !provider.fitted_models.is_empty() {
                &provider.fitted_models
            } else if let Some(cf) = cached_fitted {
                cf
            } else {
                &provider.fitted_models
            };
            fitted_map.iter().map(move |(id, info)| {
                let (format, family) = match spec {
                    Some(spec) => (wire_format_for_protocol(spec.protocol), spec.id.to_string()),
                    // A pure-custom instance should never carry fitted data
                    // (only fitting templates write it); degrade to the most
                    // common shape if one somehow does.
                    None => (WireFormat::OpenAi, provider.id.clone()),
                };
                neenee_contracts::model::FittedModel {
                    id: id.clone(),
                    family,
                    context_window: info.context_window,
                    reasoning: info.reasoning,
                    vision: info.vision,
                    format,
                    // The fitted overlay feeds the `Copy` static `Model`
                    // registry, which can hold only the known `Effort`
                    // vocabulary — so extract the known rungs and warn about
                    // any provider-advertised tier outside the vocabulary.
                    // Unknown tiers are NOT lost: they survive in the
                    // per-channel `RemoteModelMetadata` runtime path
                    // (`ModelCapabilities`), which carries `EffortLevel` and
                    // stamps `Other` tiers through to the wire. This bridge
                    // only narrows for the vetted static baseline.
                    effort_levels: info
                        .efforts
                        .iter()
                        .filter_map(|level| match Effort::parse(level) {
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
                }
            })
        })
        .collect();
    neenee_contracts::model::register_fitted_models(fitted);
}

pub(super) fn wire_format_for_protocol(protocol: &str) -> WireFormat {
    match protocol {
        "anthropic" => WireFormat::AnthropicCompat,
        "google" | "gemini" => WireFormat::Google,
        _ => WireFormat::OpenAi,
    }
}

pub fn default_model_source_for_spec(spec: &neenee_providers::ProviderTemplateSpec) -> ModelSource {
    if spec.discovery {
        ModelSource::Api
    } else {
        ModelSource::Fixed
    }
}
