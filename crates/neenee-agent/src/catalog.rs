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

use neenee_core::catalog::{Channel, ProviderEntry, Transport, builtin_provider_metadata};
use neenee_core::{
    Effort, ProviderModelInfo, ProviderPickerRow, ProviderPickerSnapshot, ThinkingMode, WireFormat,
};
use neenee_providers::{
    ANTHROPIC_BUILTIN_MODELS, DEEPSEEK_BUILTIN_MODELS, GOOGLE_BUILTIN_MODELS, NEENEE_USER_AGENT,
    OPENAI_BUILTIN_MODELS, provider_template_spec,
};
use neenee_store::config::{
    Config, ModelSource, UserChannelConfig, UserProviderConfig, UserTransport,
};
use neenee_store::provider_usage::ProviderUsage;
use std::collections::HashSet;

#[cfg(test)]
use neenee_providers::{OPENAI_PROVIDER_SPECS, OPENAI_SUB2API_MODELS, OPENCODE_GO_MODELS};

/// The effective default provider id from `config.default_provider`.
pub fn default_provider_id(config: &Config) -> &str {
    &config.default_provider
}

/// Convert a user-defined channel config into a resolved [`Channel`].
///
/// Resolution rules mirror the built-in path: an `api_key_env` value wins over
/// an inline `api_key` (and empty env values fall through, just like built-ins);
/// the wire `model` falls back to the parent model's id; transport-specific
/// fields (`base_url`, `user_agent`) fall back to localhost defaults so a
/// minimal entry still builds.
fn user_channel_to_channel(uc: &UserChannelConfig, fallback_model: &str) -> Channel {
    // SuperGrok OAuth: bearer from auth.toml (key "xai"); activate/switch
    // refreshes via XaiOAuth::resolve_access_token before catalog snapshot.
    let api_key = match uc.auth {
        neenee_core::ChannelAuth::XaiOAuth => neenee_auth::AuthStore::load()
            .get(neenee_auth::AUTH_PROVIDER_ID)
            .map(|tokens| tokens.access.clone())
            .unwrap_or_default(),
        neenee_core::ChannelAuth::ApiKey => {
            env_or_config(uc.api_key_env.as_deref(), uc.api_key.clone()).unwrap_or_default()
        }
    };
    let model = uc
        .model
        .clone()
        .unwrap_or_else(|| fallback_model.to_string());
    let transport = match uc.transport {
        UserTransport::GeminiNative => Transport::GeminiNative {
            base_url: uc
                .base_url
                .clone()
                .unwrap_or_else(|| "http://localhost:8080/v1beta".to_string()),
            user_agent: uc
                .user_agent
                .clone()
                .unwrap_or_else(|| NEENEE_USER_AGENT.to_string()),
        },
        UserTransport::Anthropic => {
            // ADR-0046: reasoning is opt-in. A custom Anthropic relay channel
            // opts the model in to reasoning when the user has configured an
            // effort or an explicit thinking value for it: thinking defaults ON
            // (the recommended Claude mode) unless `thinking = false`, and a
            // set effort is parsed into the typed `Effort`. An untouched
            // channel (no effort, no thinking) stays off — same contract as a
            // built-in model with no `[model_reasoning]` entry.
            let effort = uc.effort.as_deref().and_then(Effort::parse);
            let configured = effort.is_some() || uc.thinking.is_some();
            let thinking = if configured {
                Some(match uc.thinking {
                    Some(false) => ThinkingMode::Off,
                    _ => ThinkingMode::Adaptive,
                })
            } else {
                None
            };
            Transport::Anthropic {
                base_url: uc
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "http://localhost:8080/v1/messages".to_string()),
                user_agent: uc
                    .user_agent
                    .clone()
                    .unwrap_or_else(|| NEENEE_USER_AGENT.to_string()),
                effort,
                thinking,
            }
        }
        UserTransport::OpenAiCompat => Transport::OpenAiCompat {
            base_url: uc
                .base_url
                .clone()
                .unwrap_or_else(|| "http://localhost:8080/v1/chat/completions".to_string()),
            user_agent: uc
                .user_agent
                .clone()
                .unwrap_or_else(|| NEENEE_USER_AGENT.to_string()),
            effort: uc.effort.as_deref().and_then(Effort::parse),
        },
    };
    Channel {
        id: uc.label.clone(),
        label: uc.label.clone(),
        transport,
        api_key,
        model,
    }
}

/// Convert a user-defined model config into a resolved [`ProviderEntry`]. Reuses
/// built-in display metadata (name / description / context window) when the id
/// matches a built-in, so overriding e.g. `gemini` inherits its friendly name
/// unless the user supplies their own. A model with no channels renders but is
/// not usable until the user supplies one.
fn user_provider_to_entry(um: &UserProviderConfig) -> ProviderEntry {
    let builtin = builtin_provider_metadata(&um.id);
    let name = um
        .name
        .clone()
        .or_else(|| builtin.map(|(n, _)| n.to_string()))
        .unwrap_or_else(|| um.id.clone());
    let description = builtin.map(|(_, d)| d.to_string()).unwrap_or_default();
    let fallback_model = um.id.clone();
    let channels: Vec<Channel> = um
        .channels
        .iter()
        .map(|c| user_channel_to_channel(c, &fallback_model))
        .collect();
    let default_channel = um.default_channel.min(channels.len().saturating_sub(1));
    ProviderEntry {
        id: um.id.clone(),
        name,
        description,
        channels,
        default_channel,
        builtin: false,
    }
}

/// Resolve `env_var` first, then `config_value`. Empty and whitespace-only env
/// values are treated as unset and fall through to config, which unifies the
/// pre-catalog construction and readiness paths on one sensible rule: an empty
/// API key or model is never useful, so an empty env var never silently wins.
fn env_or_config(env_var: Option<&str>, config_value: Option<String>) -> Option<String> {
    env_var
        .and_then(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
        .or(config_value)
}

/// Build the catalog from configured provider instances only.
///
/// Provider kinds such as OpenAI, Anthropic, Gemini, and relay presets are now
/// templates in the add-provider UI. A concrete catalog row exists only after
/// the user adds a named instance.
pub fn build_catalog(config: &Config) -> Vec<ProviderEntry> {
    config
        .providers
        .iter()
        .map(user_provider_to_entry)
        .collect()
}

/// One-time hard migration from the legacy implicit built-in config fields to
/// explicit named provider instances. Returns true when `config` changed and
/// should be saved.
pub fn migrate_legacy_provider_instances(config: &mut Config) -> bool {
    let mut changed = false;
    let legacy_default = config.default_provider.clone();
    let legacy_model = config.default_model.clone();
    let openai_key = config.openai_api_key.take();
    let google_key = config.gemini_api_key.take();
    let google_base_url = config
        .gemini_base_url
        .as_deref()
        .unwrap_or("https://generativelanguage.googleapis.com/v1beta")
        .to_string();
    let kimi_key = config.moonshot_api_key.take();
    let deepseek_key = config.deepseek_api_key.take();
    let zai_key = config.zai_api_key.take();
    let anthropic_key = config.anthropic_api_key.take();
    let anthropic_base_url = config
        .anthropic_base_url
        .as_deref()
        .unwrap_or("https://api.anthropic.com/v1/messages")
        .to_string();
    let legacy_key_present = [
        openai_key.as_ref(),
        google_key.as_ref(),
        kimi_key.as_ref(),
        deepseek_key.as_ref(),
        zai_key.as_ref(),
        anthropic_key.as_ref(),
        config.opencode_go_api_key.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|key| !key.trim().is_empty());

    changed |= migrate_legacy_instance(
        config,
        "openai",
        "OpenAI",
        UserTransport::OpenAiCompat,
        "https://api.openai.com/v1/chat/completions",
        None,
        OPENAI_BUILTIN_MODELS,
        openai_key,
        legacy_model.as_deref(),
        Some("openai"),
    );
    changed |= migrate_legacy_instance(
        config,
        "google",
        "Google Gemini",
        UserTransport::GeminiNative,
        &google_base_url,
        None,
        GOOGLE_BUILTIN_MODELS,
        google_key,
        legacy_model.as_deref(),
        Some("google"),
    );
    changed |= migrate_legacy_instance(
        config,
        "kimi-code",
        "Kimi Code",
        UserTransport::OpenAiCompat,
        "https://api.kimi.com/coding/v1/chat/completions",
        Some("opencode/0.1.0"),
        &["kimi-k2.7-code"],
        kimi_key,
        legacy_model.as_deref(),
        Some("kimi-code"),
    );
    changed |= migrate_legacy_instance(
        config,
        "deepseek",
        "DeepSeek",
        UserTransport::OpenAiCompat,
        "https://api.deepseek.com/v1/chat/completions",
        None,
        DEEPSEEK_BUILTIN_MODELS,
        deepseek_key,
        legacy_model.as_deref(),
        Some("deepseek"),
    );
    changed |= migrate_legacy_instance(
        config,
        "zai-code",
        "ZAI Code",
        UserTransport::OpenAiCompat,
        "https://api.z.ai/api/coding/paas/v4/chat/completions",
        Some("opencode/1.17.10"),
        &["glm-5.2"],
        zai_key,
        legacy_model.as_deref(),
        Some("zai-code"),
    );
    changed |= migrate_legacy_instance(
        config,
        "anthropic",
        "Anthropic",
        UserTransport::Anthropic,
        &anthropic_base_url,
        None,
        ANTHROPIC_BUILTIN_MODELS,
        anthropic_key,
        legacy_model.as_deref(),
        Some("anthropic"),
    );

    if let Some(key) = config
        .opencode_go_api_key
        .take()
        .filter(|k| !k.trim().is_empty())
        && !config.providers.iter().any(|p| p.id == "opencode-go")
    {
        let channels = opencode_go_seed_channels(key);
        if !channels.is_empty() {
            let default_channel = legacy_model
                .as_deref()
                .and_then(|model| {
                    channels
                        .iter()
                        .position(|channel| channel.model.as_deref() == Some(model))
                })
                .unwrap_or(0);
            config.providers.push(UserProviderConfig {
                id: "opencode-go".to_string(),
                name: Some("OpenCode Go".to_string()),
                channels,
                default_channel,
                // The OpenCode Go seed derives its models from the KNOWN_MODELS
                // table at runtime, so it is NOT a 1:1 mirror of the
                // `opencode-go` template spec (which uses a fixed list). Leave
                // it untracked so a template edit does not clobber the curated
                // runtime-derived set.
                template_id: None,
                // A pure-custom (untracked) instance; model_source is ignored,
                // so the Fixed default is harmless.
                model_source: Default::default(),
            });
            changed = true;
        }
    }

    if config.openai_model.take().is_some()
        | config.moonshot_model.take().is_some()
        | config.zai_model.take().is_some()
        | config.default_model.take().is_some()
        | config.gemini_base_url.take().is_some()
        | config.anthropic_base_url.take().is_some()
        | config.anthropic_effort.take().is_some()
        | config.anthropic_thinking.take().is_some()
    {
        changed = true;
    }
    if legacy_key_present {
        changed = true;
    }

    if !config
        .providers
        .iter()
        .any(|provider| provider.id == config.default_provider)
    {
        config.default_provider = if config.providers.iter().any(|p| p.id == legacy_default) {
            legacy_default
        } else {
            config
                .providers
                .first()
                .map(|p| p.id.clone())
                .unwrap_or_default()
        };
        changed = true;
    }

    changed
}

/// Reconcile each template-sourced provider instance with the models the client
/// supports, then conservatively stamp a `template_id` onto legacy instances
/// that already match a template exactly.
///
/// A provider created from a template (`AddProvider` with a non-empty
/// `template_id`) records which template it came from. This walks those
/// instances and validates their channel sets. [`ModelSource::Fixed`] instances
/// mirror the template snapshot exactly. [`ModelSource::Api`] instances keep
/// their last discovered subset as long as each id remains in the client model
/// registry and supports the template's wire protocol; the asynchronous live
/// pass may add or remove registered models based on provider availability.
/// Pure-custom instances carry no `template_id` and are left untouched.
///
/// This function is **thin glue**: it resolves a `template_id` to a model list
/// plus transport (the bit that needs both `neenee-providers` and
/// `neenee-store`), then delegates the actual channel rebuild to
/// [`UserProviderConfig::reseed_channels_from_models`], which owns the
/// channel-level invariants. ADR-0005 forbids `neenee-store` from depending on
/// `neenee-providers`, so this resolution layer — and only it — lives in
/// `neenee-agent`, the one crate that sees both.
///
/// The conservative backfill is a one-way bridge for instances created before
/// `template_id` existed: if such an instance's current model set exactly equals
/// a current template's model set (same ids, same order), it is stamped with
/// that `template_id` and will track future template edits. An instance that has
/// drifted from every template is left as pure-custom (no `template_id`) and is
/// never re-seeded. Returns `true` when any instance was changed, so the caller
/// can persist only when necessary.
///
/// This is the **synchronous, offline** pass. Instances whose
/// [`ModelSource`](UserProviderConfig::model_source) is `Api` additionally get a
/// live `GET /models` fetch from [`discover_provider_models`] at startup; the
/// last valid persisted subset remains the fallback when that fetch fails.
pub fn reconcile_provider_models(config: &mut Config) -> bool {
    let mut changed = false;

    for provider in &mut config.providers {
        // A known template_id → reconcile against the client-supported set.
        // Fixed instances mirror the whole template. Api instances retain the
        // last successfully discovered subset of KNOWN_MODELS compatible with
        // the template protocol, while dropping unknown/incompatible ids.
        // Re-expanding Api instances here would overwrite the persisted
        // discovery result on every startup before the picker could display it.
        if let Some(tid) = provider.template_id.as_deref()
            && let Some(spec) = provider_template_spec(tid)
        {
            let target_models = if provider.model_source == ModelSource::Api {
                let current_models = provider
                    .channel_models()
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let known_models = supported_models_for_protocol(spec.protocol);
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

/// Fetch live model lists for every template-sourced instance whose
/// [`ModelSource`](UserProviderConfig::model_source) is `Api`.
///
/// The companion to [`reconcile_provider_models`] for the live-discovery path:
/// where reconcile validates the last known subset against the client model
/// registry synchronously, this hits each provider's actual `GET /models`
/// endpoint asynchronously and persists the intersection of those two sets.
/// Provider-advertised models unknown to the client or incompatible with the
/// template protocol are never materialized. The previous subset (or initial
/// template snapshot) is kept when fetching fails or the intersection is
/// empty, so a flaky network, a wrong endpoint, or an incompatible response
/// cannot blank a provider.
///
/// Per-instance semantics:
/// - `template_id` resolves to a known template **and** `spec.discovery` is on
///   **and** `model_source == Api` → fetch live, persist a non-empty supported
///   intersection, and leave the previous valid set alone otherwise.
/// - Otherwise → skipped. `Fixed` instances, discovery-disabled templates
///   (Kimi Code, Z.AI Code, opencode-go), and pure-custom instances keep what
///   the synchronous reconcile produced.
///
/// Returns `true` when any instance changed, so the caller can persist only
/// when necessary. Best-effort: a per-instance failure never aborts the pass —
/// the remaining instances are still fetched.
pub async fn discover_provider_models(config: &mut Config) -> bool {
    let mut changed = false;

    for provider in &mut config.providers {
        // Only template-sourced instances with discovery-enabled templates and
        // an explicit Api model source participate in live discovery.
        let Some(tid) = provider.template_id.as_deref() else {
            continue;
        };
        let Some(spec) = provider_template_spec(tid) else {
            continue;
        };
        if !spec.discovery || provider.model_source != ModelSource::Api {
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
        let api_key = channel.api_key.as_deref().unwrap_or("");
        let user_agent = channel.user_agent.as_deref();
        let protocol = neenee_providers::DiscoveryProtocol::from_template_protocol(spec.protocol);

        let discovery_req = neenee_providers::ModelDiscoveryRequest {
            protocol,
            base_url,
            api_key,
            user_agent,
        };

        match neenee_providers::list_models(discovery_req).await {
            Ok(models) => {
                // Only expose models both advertised by the provider and known
                // to the client for this wire protocol. Preserve registry order
                // so provider response ordering cannot churn the picker.
                let known_models = supported_models_for_protocol(spec.protocol);
                let supported = supported_model_intersection(&known_models, &models);
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
                if reseated {
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
                // place; a failed fetch never regresses the provider.
                tracing::warn!(
                    provider_id = %provider.id,
                    error = %error,
                    "live model discovery failed; keeping previous models"
                );
            }
        }
    }

    changed
}

/// Model ids known to the client and compatible with a provider protocol.
fn supported_models_for_protocol(protocol: &str) -> Vec<&'static str> {
    neenee_core::KNOWN_MODELS
        .iter()
        .filter(|model| {
            matches!(
                (protocol, model.format),
                ("openai", WireFormat::OpenAiCompat)
                    | ("anthropic", WireFormat::AnthropicCompat)
                    | ("gemini", WireFormat::Gemini)
            )
        })
        .map(|model| model.id)
        .collect()
}

/// Return `supported ∩ available` in the client-registry order.
///
/// The provider response is only an availability signal; it is not trusted as
/// a model registry. Restricting it to `KNOWN_MODELS` for the provider's wire
/// protocol guarantees every picker channel has client-side metadata and
/// request behavior.
fn supported_model_intersection<'a>(supported: &[&'a str], available: &[String]) -> Vec<&'a str> {
    let available = available.iter().map(String::as_str).collect::<HashSet<_>>();
    supported
        .iter()
        .copied()
        .filter(|model| available.contains(model))
        .collect()
}

/// The default [`ModelSource`] a template-sourced instance adopts when its
/// template supports live discovery. Maps `spec.discovery` to the model source:
/// `Api` when the template advertises a fetchable `GET /models` endpoint,
/// `Fixed` otherwise. Used by the add-provider flow and the legacy backfill so
/// a fresh instance starts from the right source without the caller
/// re-deriving the rule.
pub fn default_model_source_for_spec(spec: &neenee_providers::ProviderTemplateSpec) -> ModelSource {
    if spec.discovery {
        ModelSource::Api
    } else {
        ModelSource::Fixed
    }
}

/// The first template whose model set exactly equals the instance's current
/// channel model set (same ids, same order). Used by the conservative backfill:
/// a legacy instance that happens to already mirror a template gets stamped so
/// it tracks that template's future edits.
fn matching_template(
    provider: &UserProviderConfig,
) -> Option<&'static neenee_providers::ProviderTemplateSpec> {
    let current = provider.channel_models();
    neenee_providers::PROVIDER_TEMPLATE_SPECS
        .iter()
        .find(|spec| spec.models == current.as_slice())
}

/// Map a template wire protocol to its `UserTransport`. The template registry
/// speaks in protocol strings ("openai"/"anthropic"/"gemini"); channels carry
/// the richer `UserTransport` enum. This is the single bridge between the two.
fn transport_for_protocol(protocol: &str) -> UserTransport {
    match protocol {
        "anthropic" => UserTransport::Anthropic,
        "gemini" => UserTransport::GeminiNative,
        _ => UserTransport::OpenAiCompat,
    }
}

#[allow(clippy::too_many_arguments)]
fn migrate_legacy_instance(
    config: &mut Config,
    id: &str,
    name: &str,
    transport: UserTransport,
    base_url: &str,
    user_agent: Option<&str>,
    models: &[&str],
    api_key: Option<String>,
    active_model: Option<&str>,
    template_id: Option<&str>,
) -> bool {
    let Some(api_key) = api_key.filter(|k| !k.trim().is_empty()) else {
        return false;
    };
    if config.providers.iter().any(|p| p.id == id) {
        return false;
    }
    let channels: Vec<UserChannelConfig> = models
        .iter()
        .map(|model| UserChannelConfig {
            label: (*model).to_string(),
            transport,
            api_key_env: None,
            api_key: Some(api_key.clone()),
            model: Some((*model).to_string()),
            base_url: Some(base_url.to_string()),
            user_agent: user_agent.map(str::to_string),
            effort: None,
            thinking: None,
            auth: Default::default(),
        })
        .collect();
    let default_channel = active_model
        .and_then(|model| {
            channels
                .iter()
                .position(|channel| channel.model.as_deref() == Some(model))
        })
        .unwrap_or(0);
    // Stamp the template id so the migrated instance starts tracking the
    // template's model list (a legacy key migration seeds exactly the template
    // models, so the instance is a mirror of it). Only a known id is recorded.
    let template_id = template_id
        .filter(|tid| provider_template_spec(tid).is_some())
        .map(str::to_string);
    // A migrated template-sourced instance adopts the template's default model
    // source (Api where the template supports discovery, Fixed otherwise).
    let model_source = template_id
        .as_deref()
        .and_then(provider_template_spec)
        .map(default_model_source_for_spec)
        .unwrap_or_default();
    config.providers.push(UserProviderConfig {
        id: id.to_string(),
        name: Some(name.to_string()),
        channels,
        default_channel,
        template_id,
        model_source,
    });
    true
}

fn opencode_go_seed_channels(api_key: String) -> Vec<UserChannelConfig> {
    let mut models: Vec<_> = neenee_core::KNOWN_MODELS
        .iter()
        .filter(|m| {
            matches!(
                m.family,
                "glm" | "kimi" | "deepseek" | "mimo" | "minimax" | "qwen"
            )
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(b.id));
    models
        .into_iter()
        .map(|model| {
            let (transport, base_url) = match model.format {
                WireFormat::AnthropicCompat => (
                    UserTransport::Anthropic,
                    "https://opencode.ai/zen/go/v1/messages",
                ),
                WireFormat::Gemini => (
                    UserTransport::GeminiNative,
                    "https://opencode.ai/zen/go/v1beta",
                ),
                WireFormat::OpenAiCompat => (
                    UserTransport::OpenAiCompat,
                    "https://opencode.ai/zen/go/v1/chat/completions",
                ),
            };
            UserChannelConfig {
                label: model.id.to_string(),
                transport,
                api_key_env: None,
                api_key: Some(api_key.clone()),
                model: Some(model.id.to_string()),
                base_url: Some(base_url.to_string()),
                auth: Default::default(),
                user_agent: None,
                effort: None,
                thinking: None,
            }
        })
        .collect()
}

/// Resolve the active provider for a given provider id from `config`. Returns
/// the mock provider when the id is unknown or the entry has no usable channel,
/// so callers never have to branch on absence.
///
/// Channel selection honors `config.default_model`: for a multi-model provider
/// like opencode-go, the channel carrying that model (and thus the matching
/// transport) is chosen; otherwise the entry's default channel is used. This is
/// the single replacement for the resolution logic that used to be duplicated
/// at startup and in the `SwitchProvider` handler.
pub fn build_provider_for(config: &Config, id: &str) -> std::sync::Arc<dyn neenee_core::Provider> {
    build_provider_for_model(config, id, config.default_model.as_deref())
}

/// Resolve the provider for `provider_id`, selecting the channel that carries
/// `model_id` when given (falling back to `config.default_model`, then the
/// entry's default channel). Runtime switches that carry an explicit model
/// (e.g. selecting `minimax-m3` under opencode-go) route through here so the
/// per-model transport is picked correctly.
pub fn build_provider_for_model(
    config: &Config,
    provider_id: &str,
    model_id: Option<&str>,
) -> std::sync::Arc<dyn neenee_core::Provider> {
    let entries = build_catalog(config);
    let Some(entry) = entries.iter().find(|e| e.id == provider_id) else {
        return std::sync::Arc::new(neenee_providers::MockProvider);
    };
    let wanted = model_id.or(config.default_model.as_deref());
    let channel = wanted
        .and_then(|m| entry.channel_for_model(m))
        .or_else(|| entry.default_channel());
    match channel {
        Some(channel) => neenee_providers::build_provider_for_channel(channel, &entry.id),
        None => std::sync::Arc::new(neenee_providers::MockProvider),
    }
}

/// The display model name for a given provider id, as resolved from `config`.
/// Falls back to `"mock-model"` when the id is unknown. Replaces the former
/// `initial_m_name` block in `main.rs`.
///
/// For multi-model providers, the active model is `config.default_model` when
/// set (and served by the provider); otherwise the entry's default-channel
/// model. (Does not consult usage telemetry — startup paths that only know
/// the config use this; the picker uses [`active_model_id_for_entry`].)
pub fn resolved_model_name(config: &Config, id: &str) -> String {
    resolved_model_name_inner(config, id, &ProviderUsage::default())
}

/// The active model name resolved with usage telemetry so the last-activated
/// model per provider wins over the bare default-channel fallback. Used where
/// a live `usage` is available (status surfaces, re-activation).
pub fn resolved_model_name_with_usage(config: &Config, id: &str, usage: &ProviderUsage) -> String {
    resolved_model_name_inner(config, id, usage)
}

fn resolved_model_name_inner(config: &Config, id: &str, usage: &ProviderUsage) -> String {
    build_catalog(config)
        .iter()
        .find(|e| e.id == id)
        .map(|entry| active_model_id_for_entry(config, entry, usage))
        .unwrap_or_else(|| "mock-model".to_string())
}

/// The active wire model id for an already-built entry, resolved from usage
/// telemetry: `config.default_model` wins when the entry serves it, otherwise
/// the provider's last-activated model (so re-opening a provider lands on the
/// exact model it was left at), finally the entry's default-channel model.
/// Shared by [`resolved_model_name`] and [`build_picker_state`] so both pick
/// the same active model without rebuilding the catalog per row.
fn active_model_id_for_entry(
    config: &Config,
    entry: &ProviderEntry,
    usage: &ProviderUsage,
) -> String {
    config
        .default_model
        .as_deref()
        .filter(|m| entry.offers_model(m))
        .map(|m| m.to_string())
        .or_else(|| {
            usage
                .last_model_for(&entry.id)
                .filter(|m| entry.offers_model(m))
                .map(|m| m.to_string())
        })
        .or_else(|| entry.default_channel().map(|channel| channel.model.clone()))
        .unwrap_or_else(|| "mock-model".to_string())
}

/// The model ids a provider serves, in catalog order. Used by the picker to
/// render the second-stage model list for multi-model providers (opencode-go).
/// Empty for providers with no channels.
pub fn models_for_provider(config: &Config, provider_id: &str) -> Vec<String> {
    build_catalog(config)
        .iter()
        .find(|e| e.id == provider_id)
        .map(|entry| entry.channels.iter().map(|c| c.model.clone()).collect())
        .unwrap_or_default()
}

/// Build the full model-picker snapshot: the canonical default id plus one row
/// per catalog entry carrying the dynamic signals the picker renders and sorts
/// by (key readiness, favorite flag, last-used timestamp). Sent to the TUI on
/// startup and after any mutation so the picker always shows a consistent
/// picture
pub fn build_picker_state(config: &Config, usage: &ProviderUsage) -> ProviderPickerSnapshot {
    let entries = build_catalog(config);
    let default_id = default_provider_id(config).to_string();
    let rows = entries
        .iter()
        .map(|entry| {
            let (protocol, base_url) = entry
                .default_channel()
                .map(channel_protocol_and_base_url)
                .unwrap_or_default();
            let model = active_model_id_for_entry(config, entry, usage);
            let model_info = entry
                .channels
                .iter()
                .map(channel_model_info)
                .map(|mut info| {
                    info.last_used_ms = usage.model_last_used_ms(&info.model);
                    info
                })
                .collect();
            ProviderPickerRow {
                id: entry.id.clone(),
                name: entry.name.clone(),
                model,
                models: entry.channels.iter().map(|c| c.model.clone()).collect(),
                model_info,
                builtin: entry.builtin,
                protocol,
                base_url,
                key_ready: entry.key_ready(),
                favorite: config.favorites.iter().any(|fav| fav == &entry.id),
                last_used_ms: usage.last_used_ms(&entry.id),
                auth: provider_auth(config, &entry.id),
            }
        })
        .collect();
    ProviderPickerSnapshot { default_id, rows }
}

/// Auth mode of a user provider's default channel (`ApiKey` when unknown).
fn provider_auth(config: &Config, provider_id: &str) -> neenee_core::ChannelAuth {
    config
        .providers
        .iter()
        .find(|p| p.id == provider_id)
        .and_then(|p| {
            p.channels
                .get(p.default_channel.min(p.channels.len().saturating_sub(1)))
        })
        .map(|ch| ch.auth)
        .unwrap_or_default()
}

/// Map a channel's transport to the `(protocol_wire_id, base_url)` pair the TUI
/// edit form pre-fills from. `base_url` is empty for the keyless native Gemini
/// transport (it has no configurable endpoint).
fn channel_protocol_and_base_url(channel: &Channel) -> (String, String) {
    match &channel.transport {
        Transport::OpenAiCompat { base_url, .. } => ("openai".to_string(), base_url.clone()),
        Transport::Anthropic { base_url, .. } => ("anthropic".to_string(), base_url.clone()),
        Transport::GeminiNative { base_url, .. } => ("gemini".to_string(), base_url.clone()),
    }
}

fn channel_model_info(channel: &Channel) -> ProviderModelInfo {
    match &channel.transport {
        Transport::Anthropic {
            effort, thinking, ..
        } => {
            // ADR-0046: reasoning is opt-in. A channel's effective thinking
            // state is off unless it has an explicit on override. The info
            // surfaces both knobs so the picker can show a model's effort only
            // when it is actually opted in to reasoning (thinking on).
            let thinking_on = matches!(thinking, Some(ThinkingMode::Adaptive));
            ProviderModelInfo {
                model: channel.model.clone(),
                protocol: "anthropic".to_string(),
                effort: Some((*effort).unwrap_or(Effort::High).as_str().to_string()),
                thinking: Some(thinking_on),
                last_used_ms: None,
            }
        }
        Transport::OpenAiCompat { effort, .. } => {
            let model = neenee_core::model::resolve(&channel.model);
            let effective = if model.effort_levels.is_empty() {
                None
            } else {
                let default = if model.family == "gpt" {
                    Effort::Medium
                } else {
                    Effort::High
                };
                Some((*effort).unwrap_or(default).as_str().to_string())
            };
            ProviderModelInfo {
                model: channel.model.clone(),
                protocol: "openai".to_string(),
                effort: effective,
                thinking: None,
                last_used_ms: None,
            }
        }
        Transport::GeminiNative { .. } => ProviderModelInfo {
            model: channel.model.clone(),
            protocol: "gemini".to_string(),
            effort: None,
            thinking: None,
            last_used_ms: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Tests that mutate process-wide env vars (`*_API_KEY`, `*_MODEL`)
    /// must serialize against each other so the parallel test runner never
    /// observes a half-set environment. Mirrors the `ENV_GUARD` pattern in
    /// `paths.rs`.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    /// A config with no keys or model overrides set beyond the built-in
    /// defaults, so every field resolves predictably.
    fn bare_config() -> Config {
        Config::default()
    }

    #[test]
    fn empty_config_has_no_provider_instances() {
        let config = bare_config();
        assert!(build_catalog(&config).is_empty());
        assert_eq!(
            build_picker_state(&config, &ProviderUsage::default())
                .rows
                .len(),
            0
        );
        assert_eq!(
            build_provider_for(&config, default_provider_id(&config)).provider_id(),
            "mock"
        );
    }

    #[test]
    fn legacy_builtin_key_migrates_to_named_instance() {
        let mut config = bare_config();
        config.default_provider = "openai".to_string();
        config.default_model = Some("gpt-5.4-mini".to_string());
        config.openai_api_key = Some("sk-old".to_string());

        assert!(migrate_legacy_provider_instances(&mut config));
        assert!(config.openai_api_key.is_none());
        assert!(config.default_model.is_none());
        assert_eq!(config.default_provider, "openai");

        let entry = build_catalog(&config)
            .into_iter()
            .find(|entry| entry.id == "openai")
            .expect("migrated openai instance");
        assert_eq!(entry.name, "OpenAI");
        assert_eq!(entry.default_channel().unwrap().model, "gpt-5.4-mini");
        assert_eq!(entry.default_channel().unwrap().api_key, "sk-old");
        assert!(!entry.builtin);
    }

    /// A provider instance created from a template, pre-stamped with its
    /// `template_id`. Used to exercise model reconciliation without depending
    /// on the live `PROVIDER_TEMPLATE_SPECS` model lists (which evolve).
    fn template_instance(tid: &str, models: &[&str]) -> UserProviderConfig {
        UserProviderConfig {
            id: "test-instance".to_string(),
            name: Some("Test".to_string()),
            channels: models
                .iter()
                .map(|m| UserChannelConfig {
                    label: m.to_string(),
                    transport: UserTransport::OpenAiCompat,
                    api_key_env: None,
                    api_key: Some("sk-test".to_string()),
                    model: Some(m.to_string()),
                    base_url: Some("https://relay.example.com/v1/chat/completions".to_string()),
                    user_agent: None,
                    effort: None,
                    thinking: None,
                    auth: Default::default(),
                })
                .collect(),
            default_channel: 0,
            template_id: Some(tid.to_string()),
            model_source: Default::default(),
        }
    }

    /// The exact current model ids a known template seeds — read from the live
    /// registry so this test tracks template evolution rather than a snapshot.
    fn current_template_models(tid: &str) -> Vec<String> {
        provider_template_spec(tid)
            .expect("known template id")
            .models
            .iter()
            .map(|m| m.to_string())
            .collect()
    }

    #[test]
    fn discovery_intersection_keeps_only_supported_models_in_registry_order() {
        let supported = &["model-a", "model-b", "model-c"];
        let available = vec![
            "unknown-cloud-model".to_string(),
            "model-c".to_string(),
            "model-a".to_string(),
        ];

        assert_eq!(
            supported_model_intersection(supported, &available),
            vec!["model-a", "model-c"]
        );
        assert!(supported_model_intersection(supported, &["unknown".to_string()]).is_empty());
    }

    #[test]
    fn protocol_supported_models_come_from_the_client_registry() {
        let openai = supported_models_for_protocol("openai");
        assert!(openai.contains(&"gpt-4o"));
        assert!(openai.contains(&"gpt-5.6"));
        assert!(!openai.contains(&"claude-opus-4-8"));

        let anthropic = supported_models_for_protocol("anthropic");
        assert!(anthropic.contains(&"claude-opus-4-8"));
        assert!(!anthropic.contains(&"gpt-4o"));
    }

    #[test]
    fn reconcile_noops_when_instance_already_mirrors_template() {
        // An instance whose channels exactly equal the current template models
        // must not be churned (no change reported, channels untouched).
        let models = current_template_models("openai-sub2api");
        let mut config = bare_config();
        config.providers.push(UserProviderConfig {
            id: "relay".to_string(),
            name: Some("Relay".to_string()),
            channels: models
                .iter()
                .map(|m| UserChannelConfig {
                    label: m.clone(),
                    transport: UserTransport::OpenAiCompat,
                    api_key_env: None,
                    api_key: Some("sk".to_string()),
                    model: Some(m.clone()),
                    base_url: Some("https://relay.example.com".to_string()),
                    user_agent: None,
                    effort: None,
                    thinking: None,
                    auth: Default::default(),
                })
                .collect(),
            default_channel: 0,
            template_id: Some("openai-sub2api".to_string()),
            model_source: Default::default(),
        });
        let before_models: Vec<String> = config.providers[0]
            .channels
            .iter()
            .map(|c| c.model.clone().unwrap_or_default())
            .collect();

        assert!(!reconcile_provider_models(&mut config));
        let after_models: Vec<String> = config.providers[0]
            .channels
            .iter()
            .map(|c| c.model.clone().unwrap_or_default())
            .collect();
        assert_eq!(after_models, before_models);
    }

    #[test]
    fn reconcile_drops_models_removed_from_template() {
        // Start with the current template models plus one extra user-added
        // model. After reconcile, the extra is gone — pure-mirror semantics.
        let mut models = current_template_models("openai-sub2api");
        models.push("stale-user-model".to_string());
        let mut config = bare_config();
        config.providers.push(template_instance("openai-sub2api", &{
            let refs: Vec<&str> = models.iter().map(|s| s.as_str()).collect();
            refs
        }));

        assert!(reconcile_provider_models(&mut config));
        let got: Vec<String> = config.providers[0]
            .channels
            .iter()
            .map(|c| c.model.clone().unwrap_or_default())
            .collect();
        assert_eq!(got, current_template_models("openai-sub2api"));
        assert!(
            !got.iter().any(|m| m == "stale-user-model"),
            "extra user model must be dropped on reconcile"
        );
    }

    #[test]
    fn reconcile_adds_new_models_introduced_by_template() {
        // An instance seeded with a strict subset of the template models picks
        // up the missing ones after reconcile — proving template edits propagate
        // forward to existing instances.
        let full = current_template_models("deepseek");
        let subset: Vec<&str> = full.iter().take(1).map(|s| s.as_str()).collect();
        let mut config = bare_config();
        config
            .providers
            .push(template_instance("deepseek", &subset));

        assert!(reconcile_provider_models(&mut config));
        let got: Vec<String> = config.providers[0]
            .channels
            .iter()
            .map(|c| c.model.clone().unwrap_or_default())
            .collect();
        assert_eq!(got, full, "missing template models are added");
        // The shared key configured on the surviving channel is copied onto the
        // newly added channels so the instance keeps working.
        assert!(
            config.providers[0]
                .channels
                .iter()
                .all(|c| c.api_key.as_deref() == Some("sk-test")),
            "shared key is preserved across the reseed"
        );
    }

    #[test]
    fn reconcile_api_instance_keeps_last_discovered_supported_subset() {
        let known = supported_models_for_protocol("openai");
        let subset = [known[1], known[3]];
        let mut instance = template_instance("openai-sub2api", &subset);
        instance.model_source = neenee_store::config::ModelSource::Api;
        let mut config = bare_config();
        config.providers.push(instance);

        assert!(
            !reconcile_provider_models(&mut config),
            "startup reconciliation must not expand a persisted Api subset"
        );
        assert_eq!(config.providers[0].channel_models(), subset);
    }

    #[test]
    fn reconcile_api_instance_drops_unsupported_without_expanding_subset() {
        let known = supported_models_for_protocol("openai");
        let kept = known[2];
        let mut instance = template_instance("openai-sub2api", &[kept, "removed-or-unknown-model"]);
        instance.model_source = neenee_store::config::ModelSource::Api;
        let mut config = bare_config();
        config.providers.push(instance);

        assert!(reconcile_provider_models(&mut config));
        assert_eq!(config.providers[0].channel_models(), vec![kept]);
        let channel = &config.providers[0].channels[0];
        assert_eq!(channel.api_key.as_deref(), Some("sk-test"));
        assert_eq!(
            channel.base_url.as_deref(),
            Some("https://relay.example.com/v1/chat/completions")
        );
    }

    #[test]
    fn reconcile_preserves_per_model_reasoning_for_surviving_models() {
        // A model that survives the reseed keeps its effort/thinking knobs; a
        // newly added model starts with reasoning off (ADR-0046).
        let full = current_template_models("anthropic");
        let kept: Vec<&str> = full.iter().take(1).map(|s| s.as_str()).collect();
        let mut config = bare_config();
        let mut inst = template_instance("anthropic", &kept);
        inst.channels[0].transport = UserTransport::Anthropic;
        inst.channels[0].effort = Some("high".to_string());
        inst.channels[0].thinking = Some(true);
        config.providers.push(inst);

        assert!(reconcile_provider_models(&mut config));
        let channels = &config.providers[0].channels;
        let survived = channels
            .iter()
            .find(|c| c.model.as_deref() == Some(kept[0]))
            .expect("surviving model present");
        assert_eq!(survived.effort.as_deref(), Some("high"));
        assert_eq!(survived.thinking, Some(true));
        let added = channels
            .iter()
            .find(|c| c.model.as_deref() != Some(kept[0]))
            .expect("a newly added model exists");
        assert!(added.effort.is_none(), "new model starts with no effort");
        assert!(
            added.thinking.is_none(),
            "new model starts with thinking off"
        );
    }

    #[test]
    fn reconcile_leaves_unknown_template_id_untouched() {
        // A template_id that no longer resolves (template removed from the
        // codebase) must NOT blank out a working instance — the dangling
        // pointer is ignored so the provider keeps serving its models.
        let mut config = bare_config();
        config.providers.push(UserProviderConfig {
            id: "orphan".to_string(),
            name: Some("Orphan".to_string()),
            channels: vec![UserChannelConfig {
                label: "only-model".to_string(),
                transport: UserTransport::OpenAiCompat,
                api_key_env: None,
                api_key: Some("sk".to_string()),
                model: Some("only-model".to_string()),
                base_url: Some("https://x.example.com".to_string()),
                user_agent: None,
                effort: None,
                thinking: None,
                auth: Default::default(),
            }],
            default_channel: 0,
            template_id: Some("removed-template".to_string()),
            model_source: Default::default(),
        });
        let before_models: Vec<String> = config.providers[0]
            .channels
            .iter()
            .map(|c| c.model.clone().unwrap_or_default())
            .collect();

        assert!(!reconcile_provider_models(&mut config));
        // Channels, model, key, and the (dangling) template_id are all
        // unchanged — a dangling pointer must not blank a working provider.
        let after_models: Vec<String> = config.providers[0]
            .channels
            .iter()
            .map(|c| c.model.clone().unwrap_or_default())
            .collect();
        assert_eq!(after_models, before_models);
        assert_eq!(config.providers[0].channels.len(), 1);
        assert_eq!(
            config.providers[0].template_id.as_deref(),
            Some("removed-template")
        );
    }

    #[test]
    fn reconcile_leaves_pure_custom_instance_untouched() {
        // A pure-custom instance (no template_id) whose model set does NOT match
        // any template is never re-seeded — user customizations are preserved.
        let mut config = bare_config();
        config
            .providers
            .push(template_instance("", &["alpha", "beta"]));
        config.providers[0].template_id = None;
        let before_models: Vec<String> = config.providers[0]
            .channels
            .iter()
            .map(|c| c.model.clone().unwrap_or_default())
            .collect();

        assert!(!reconcile_provider_models(&mut config));
        // The user's custom models and keys are intact; no template_id stamped.
        let after_models: Vec<String> = config.providers[0]
            .channels
            .iter()
            .map(|c| c.model.clone().unwrap_or_default())
            .collect();
        assert_eq!(after_models, before_models);
        assert_eq!(config.providers[0].template_id, None);
    }

    #[test]
    fn reconcile_backfills_template_id_for_legacy_matching_instance() {
        // A pre-template_id instance whose model set exactly equals a current
        // template gets stamped, so it will track future template edits. The
        // stamp itself is the change.
        let models = current_template_models("openai-sub2api");
        let refs: Vec<&str> = models.iter().map(|s| s.as_str()).collect();
        let mut inst = template_instance("", &refs);
        inst.template_id = None;
        let mut config = bare_config();
        config.providers.push(inst);

        assert!(reconcile_provider_models(&mut config));
        assert_eq!(
            config.providers[0].template_id.as_deref(),
            Some("openai-sub2api"),
            "legacy matching instance is stamped"
        );
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn catalog_contains_every_builtin_preset() {
        let entries = build_catalog(&bare_config());
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"kimi-code"), "missing kimi-code: {ids:?}");
        assert!(ids.contains(&"openai"));
        assert!(ids.contains(&"google"), "missing google: {ids:?}");
        assert!(ids.contains(&"deepseek"), "missing deepseek: {ids:?}");
        assert!(ids.contains(&"opencode-go"), "missing opencode-go: {ids:?}");
        assert!(ids.contains(&"anthropic"), "missing anthropic: {ids:?}");
        // Every registry preset is present.
        for spec in OPENAI_PROVIDER_SPECS {
            assert!(
                entries.iter().find(|e| e.id == spec.id).is_some(),
                "registry preset {} missing",
                spec.id
            );
        }
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn opencode_go_hosts_both_wire_formats() {
        let entries = build_catalog(&bare_config());
        let entry = entries
            .iter()
            .find(|e| e.id == "opencode-go")
            .expect("opencode-go entry");
        // Every served model has its own channel.
        assert!(!entry.channels.is_empty());
        // An OpenAI-format model routes through the OpenAiCompat transport.
        let glm = entry
            .channel_for_model("glm-5.2")
            .expect("glm-5.2 served by opencode-go");
        assert!(
            matches!(
                glm.transport,
                neenee_core::catalog::Transport::OpenAiCompat { .. }
            ),
            "glm-5.2 must use OpenAiCompat"
        );
        // An Anthropic-format model routes through the Anthropic transport —
        // the load-bearing detail: one provider, two wire formats.
        let mm = entry
            .channel_for_model("minimax-m3")
            .expect("minimax-m3 served by opencode-go");
        assert!(
            matches!(
                mm.transport,
                neenee_core::catalog::Transport::Anthropic { .. }
            ),
            "minimax-m3 must use Anthropic /messages"
        );
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn anthropic_relay_hosts_claude_family_over_messages() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("ANTHROPIC_BASE_URL");
        }
        let entries = build_catalog(&bare_config());
        let entry = entries
            .iter()
            .find(|e| e.id == "anthropic")
            .expect("anthropic entry");
        // Every Claude model is a channel, all on the Anthropic /messages
        // transport pointed at the configured endpoint.
        assert!(!entry.channels.is_empty());
        let opus = entry
            .channel_for_model("claude-opus-4-8")
            .expect("claude-opus-4-8 served");
        match &opus.transport {
            Transport::Anthropic { base_url, .. } => {
                // Default endpoint is Anthropic's official API.
                assert_eq!(base_url, "https://api.anthropic.com/v1/messages");
            }
            other => panic!("anthropic must use the Anthropic transport, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn anthropic_relay_base_url_is_configurable() {
        // A custom relay address (e.g. a self-hosted proxy) flows through config
        // with no code change — the load-bearing requirement for users whose
        // relay URL differs.
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("ANTHROPIC_BASE_URL");
        }
        let mut config = bare_config();
        config.anthropic_base_url = Some("https://relay.example.com/v1/messages".to_string());
        let entries = build_catalog(&config);
        let entry = entries.iter().find(|e| e.id == "anthropic").unwrap();
        let channel = entry.default_channel().expect("default channel");
        match &channel.transport {
            Transport::Anthropic { base_url, .. } => {
                assert_eq!(base_url, "https://relay.example.com/v1/messages");
            }
            other => panic!("expected Anthropic transport, got {other:?}"),
        }
    }

    #[test]
    fn custom_anthropic_model_rows_carry_channel_settings() {
        let mut config = bare_config();
        config
            .providers
            .push(neenee_store::config::UserProviderConfig {
                id: "example".to_string(),
                name: Some("Example Claude".to_string()),
                channels: vec![neenee_store::config::UserChannelConfig {
                    label: "claude-sonnet-4-6".to_string(),
                    transport: neenee_store::config::UserTransport::Anthropic,
                    model: Some("claude-sonnet-4-6".to_string()),
                    base_url: Some("https://relay.example.com/v1/messages".to_string()),
                    effort: Some("high".to_string()),
                    thinking: Some(true),
                    ..Default::default()
                }],
                default_channel: 0,
                ..Default::default()
            });

        let picker = build_picker_state(&config, &ProviderUsage::default());
        let row = picker.rows.iter().find(|row| row.id == "example").unwrap();
        let info = row
            .model_info
            .iter()
            .find(|info| info.model == "claude-sonnet-4-6")
            .unwrap();
        assert_eq!(info.protocol, "anthropic");
        assert_eq!(info.effort.as_deref(), Some("high"));
        assert_eq!(info.thinking, Some(true));
    }

    #[test]
    fn resolved_model_honors_per_provider_last_used_model() {
        // A multi-model custom provider: with no config `default_model` and no
        // usage telemetry, the active model is the default channel. After a
        // model is recorded as used under that provider, resolving the active
        // model (via usage) lands on it, and the picker row mirrors it — so a
        // provider re-opens on the exact model it was left at.
        use neenee_store::config::{UserChannelConfig, UserProviderConfig, UserTransport};
        let mut config = bare_config();
        config.providers.push(UserProviderConfig {
            id: "relay".to_string(),
            name: Some("Relay".to_string()),
            channels: vec![
                UserChannelConfig {
                    label: "alpha".to_string(),
                    transport: UserTransport::OpenAiCompat,
                    model: Some("alpha".to_string()),
                    ..Default::default()
                },
                UserChannelConfig {
                    label: "beta".to_string(),
                    transport: UserTransport::OpenAiCompat,
                    model: Some("beta".to_string()),
                    ..Default::default()
                },
            ],
            default_channel: 0,
            ..Default::default()
        });
        config.default_provider = "relay".to_string();

        // No usage → default channel model (alpha).
        assert_eq!(
            resolved_model_name_with_usage(&config, "relay", &ProviderUsage::default()),
            "alpha"
        );

        // Record `beta` under `relay`: it becomes the resolved active model.
        let mut usage = ProviderUsage::default();
        usage.record_model("relay", "beta");
        assert_eq!(
            resolved_model_name_with_usage(&config, "relay", &usage),
            "beta"
        );

        // The picker row's `model` (the displayed active model) mirrors this.
        let picker = build_picker_state(&config, &usage);
        let row = picker.rows.iter().find(|r| r.id == "relay").unwrap();
        assert_eq!(row.model, "beta");
        // And the stage-2 model list surfaces beta's recency on its info row.
        let beta_info = row.model_info.iter().find(|i| i.model == "beta").unwrap();
        assert!(beta_info.last_used_ms.is_some());
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn built_in_anthropic_applies_per_model_reasoning_overrides() {
        // ADR-0046: reasoning is opt-in per model. A `[model_reasoning]` entry
        // keyed by model id opts that model in; an explicit `thinking = false`
        // keeps it off even with an entry. A sibling model with no entry stays
        // at the default (thinking off, no explicit effort) — it does not
        // reason on its own.
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("ANTHROPIC_BASE_URL");
        }
        let mut config = bare_config();
        config
            .model_reasoning
            .for_model_mut("claude-opus-4-8")
            .effort = Some("max".to_string());
        config
            .model_reasoning
            .for_model_mut("claude-opus-4-8")
            .thinking = Some(false);

        let entries = build_catalog(&config);
        let entry = entries.iter().find(|e| e.id == "anthropic").unwrap();
        // The configured model carries max effort + thinking off (explicit).
        let opus = entry.channel_for_model("claude-opus-4-8").unwrap();
        match &opus.transport {
            Transport::Anthropic {
                effort, thinking, ..
            } => {
                assert_eq!(*effort, Some(Effort::Max), "opus per-model effort");
                assert_eq!(
                    *thinking,
                    Some(ThinkingMode::Off),
                    "opus per-model thinking off"
                );
            }
            other => panic!("expected Anthropic transport, got {other:?}"),
        }
        // A sibling model with no entry keeps the opt-in default (effort None,
        // thinking None → off on the wire).
        let sonnet = entry.channel_for_model("claude-sonnet-4-6").unwrap();
        match &sonnet.transport {
            Transport::Anthropic {
                effort, thinking, ..
            } => {
                assert!(effort.is_none(), "sonnet untouched effort");
                assert!(thinking.is_none(), "sonnet untouched thinking");
            }
            other => panic!("expected Anthropic transport, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn per_model_entry_presence_defaults_thinking_on() {
        // ADR-0046 opt-in contract: a `[model_reasoning]` entry's mere presence
        // opts the model in to reasoning — thinking defaults ON unless the
        // entry explicitly sets `thinking = false`. So an entry with only an
        // effort still turns thinking on. This is "写的默认有 think 且为对应
        // effort".
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("ANTHROPIC_BASE_URL");
        }
        let mut config = bare_config();
        // Entry with effort only (no thinking key) → thinking defaults on.
        config
            .model_reasoning
            .for_model_mut("claude-opus-4-8")
            .effort = Some("xhigh".to_string());

        let entries = build_catalog(&config);
        let entry = entries.iter().find(|e| e.id == "anthropic").unwrap();
        let opus = entry.channel_for_model("claude-opus-4-8").unwrap();
        match &opus.transport {
            Transport::Anthropic {
                effort, thinking, ..
            } => {
                assert_eq!(*effort, Some(Effort::Xhigh), "effort honored");
                assert_eq!(
                    *thinking,
                    Some(ThinkingMode::Adaptive),
                    "entry presence defaults thinking on"
                );
            }
            other => panic!("expected Anthropic transport, got {other:?}"),
        }
        // A bare entry with NO knobs at all (an empty `[model_reasoning."m"]`)
        // still counts as opted in → thinking on, effort None (model default).
        config.model_reasoning.for_model_mut("claude-sonnet-4-6");
        let entries = build_catalog(&config);
        let entry = entries.iter().find(|e| e.id == "anthropic").unwrap();
        let sonnet = entry.channel_for_model("claude-sonnet-4-6").unwrap();
        match &sonnet.transport {
            Transport::Anthropic {
                effort, thinking, ..
            } => {
                assert!(effort.is_none(), "no effort → model default, omitted");
                assert_eq!(
                    *thinking,
                    Some(ThinkingMode::Adaptive),
                    "bare entry still opts in to thinking"
                );
            }
            other => panic!("expected Anthropic transport, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn anthropic_default_model_selects_its_channel_and_builds() {
        let mut config = bare_config();
        config.default_model = Some("claude-sonnet-4-6".to_string());
        assert_eq!(
            resolved_model_name(&config, "anthropic"),
            "claude-sonnet-4-6"
        );
        let provider = build_provider_for_model(&config, "anthropic", Some("claude-sonnet-4-6"));
        assert_eq!(provider.model(), "claude-sonnet-4-6");
        assert_eq!(provider.provider_id(), "anthropic");
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn opencode_go_default_model_selects_its_channel() {
        let mut config = bare_config();
        config.default_model = Some("minimax-m3".to_string());
        // resolved_model_name honors default_model when the provider serves it.
        assert_eq!(resolved_model_name(&config, "opencode-go"), "minimax-m3");
        // models_for_provider lists every served model for the picker.
        let models = models_for_provider(&config, "opencode-go");
        assert!(models.contains(&"glm-5.2".to_string()));
        assert!(models.contains(&"minimax-m3".to_string()));
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn build_provider_for_model_picks_anthropic_transport_for_minimax() {
        // Selecting minimax-m3 under opencode-go must build a provider whose
        // model id is minimax-m3 (the Anthropic /messages path), proving the
        // per-model transport routing reaches construction.
        let config = bare_config();
        let provider = build_provider_for_model(&config, "opencode-go", Some("minimax-m3"));
        assert_eq!(provider.model(), "minimax-m3");
        assert_eq!(provider.provider_id(), "opencode-go");
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn kimi_code_uses_kimi_code_platform() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("MOONSHOT_MODEL");
        }
        let config = bare_config();
        let entries = build_catalog(&config);
        let entry = entries
            .iter()
            .find(|e| e.id == "kimi-code")
            .expect("kimi-code entry");
        let channel = entry.default_channel().expect("default channel");
        // The Kimi Code platform pins the model id to kimi-k2.7-code.
        assert_eq!(
            channel.model, "kimi-k2.7-code",
            "model must be the pinned kimi-k2.7-code alias"
        );
        let (base_url, user_agent) = match &channel.transport {
            Transport::OpenAiCompat {
                base_url,
                user_agent,
                ..
            } => (base_url.clone(), user_agent.clone()),
            other => panic!("kimi-code must be OpenAiCompat, got {other:?}"),
        };
        assert_eq!(base_url, "https://api.kimi.com/coding/v1/chat/completions");
        // The Kimi Code platform requires a recognized coding-agent UA.
        assert_eq!(user_agent, "opencode/0.1.0");
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn google_default_model_selects_its_gemini_channel() {
        // google is multi-model: default_model picks which Gemini channel is
        // active; every channel uses the native Gemini transport. ENV_GUARD is
        // held because the built-in entry reads `GEMINI_BASE_URL` (and other
        // `GEMINI_*` vars) — a parallel test mutating them must not leak in.
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut config = bare_config();
        config.default_model = Some("gemini-2.0-flash".to_string());
        let entries = build_catalog(&config);
        let entry = entries
            .iter()
            .find(|e| e.id == "google")
            .expect("google entry");
        assert_eq!(entry.default_channel().unwrap().model, "gemini-2.0-flash");
        assert!(matches!(
            entry.default_channel().unwrap().transport,
            Transport::GeminiNative { .. }
        ));
        // The built-in default base URL resolves to Google's official endpoint.
        if let Transport::GeminiNative { base_url, .. } =
            &entry.default_channel().unwrap().transport
        {
            assert_eq!(base_url, "https://generativelanguage.googleapis.com/v1beta");
        }
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn deepseek_hosts_flash_and_pro_as_one_provider() {
        // The two DeepSeek models are now channels of one `deepseek` provider,
        // both over the OpenAI-compatible transport at the DeepSeek endpoint.
        let entries = build_catalog(&bare_config());
        let entry = entries
            .iter()
            .find(|e| e.id == "deepseek")
            .expect("deepseek entry");
        assert!(entry.offers_model("deepseek-v4-flash"));
        assert!(entry.offers_model("deepseek-v4-pro"));
        let flash = entry.channel_for_model("deepseek-v4-flash").unwrap();
        match &flash.transport {
            Transport::OpenAiCompat { base_url, .. } => {
                assert_eq!(base_url, "https://api.deepseek.com/v1/chat/completions");
            }
            other => panic!("deepseek must be OpenAiCompat, got {other:?}"),
        }
    }

    #[test]
    fn resolved_model_name_falls_back_for_unknown_id() {
        assert_eq!(resolved_model_name(&bare_config(), "nope"), "mock-model");
    }

    #[test]
    fn build_provider_for_unknown_id_returns_mock() {
        let provider = build_provider_for(&bare_config(), "does-not-exist");
        assert_eq!(provider.provider_id(), "mock");
    }

    #[test]
    fn split_deepseek_ids_no_longer_resolve_as_providers() {
        // The pre-merge provider ids are gone; only the merged `deepseek` id is a
        // provider now, so the old ids fall back to mock.
        let provider = build_provider_for(&bare_config(), "deepseek-v4-flash");
        assert_eq!(provider.provider_id(), "mock");
        let provider = build_provider_for(&bare_config(), "deepseek-v4-pro");
        assert_eq!(provider.provider_id(), "mock");
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn cloud_providers_report_not_ready_without_key() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }
        let entries = build_catalog(&bare_config());
        let openai = entries
            .iter()
            .find(|e| e.id == "openai")
            .expect("openai entry");
        assert!(
            !openai.key_ready(),
            "openai without a key must not be ready"
        );
    }

    /// Build a user model override on `gemini` with two channels.
    fn gemini_two_channel_config() -> Config {
        let mut config = bare_config();
        config.providers = vec![UserProviderConfig {
            id: "gemini".to_string(),
            name: Some("Gemini (custom)".to_string()),
            channels: vec![
                UserChannelConfig {
                    label: "Studio".to_string(),
                    transport: UserTransport::GeminiNative,
                    api_key_env: Some("GEMINI_STUDIO_KEY".to_string()),
                    model: Some("gemini-2.5-flash".to_string()),
                    base_url: Some("https://relay.example.com/v1beta".to_string()),
                    ..Default::default()
                },
                UserChannelConfig {
                    label: "Relay".to_string(),
                    transport: UserTransport::OpenAiCompat,
                    base_url: Some("https://relay.example.com/v1/chat/completions".to_string()),
                    api_key: Some("inline-key".to_string()),
                    model: Some("gemini-2.5-flash".to_string()),
                    ..Default::default()
                },
            ],
            default_channel: 1,
            ..Default::default()
        }];
        config
    }

    #[test]
    fn user_model_overrides_builtin_by_id() {
        let entries = build_catalog(&gemini_two_channel_config());
        let gemini = entries
            .iter()
            .find(|e| e.id == "gemini")
            .expect("overridden gemini entry");
        // The user-supplied name wins over the built-in "Gemini 2.5 Flash".
        assert_eq!(gemini.name, "Gemini (custom)");
        assert!(!gemini.builtin, "an override is user-owned, not read-only");
        // Two channels, with the user's default index honored.
        assert_eq!(gemini.channels.len(), 2);
        assert_eq!(gemini.default_channel, 1);
        assert_eq!(gemini.default_channel().unwrap().label, "Relay");
    }

    #[test]
    fn user_channel_resolves_env_key_over_inline() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("GEMINI_STUDIO_KEY", "env-key");
        }
        let entries = build_catalog(&gemini_two_channel_config());
        let entry = entries.iter().find(|e| e.id == "gemini").unwrap();
        // Studio names an env var → the env value wins.
        let studio = entry.channels.iter().find(|c| c.label == "Studio").unwrap();
        assert_eq!(studio.api_key, "env-key");
        // Relay uses an inline key (no env var named) → inline wins.
        let relay = entry.channels.iter().find(|c| c.label == "Relay").unwrap();
        assert_eq!(relay.api_key, "inline-key");
        unsafe {
            std::env::remove_var("GEMINI_STUDIO_KEY");
        }
    }

    #[test]
    fn openai_reasoning_effort_surfaces_in_picker_and_transport() {
        let mut config = bare_config();
        config.providers = vec![UserProviderConfig {
            id: "openai-relay".to_string(),
            name: Some("OpenAI Relay".to_string()),
            channels: vec![
                UserChannelConfig {
                    label: "default".to_string(),
                    transport: UserTransport::OpenAiCompat,
                    api_key: Some("k".to_string()),
                    model: Some("gpt-5.5".to_string()),
                    ..Default::default()
                },
                UserChannelConfig {
                    label: "xhigh".to_string(),
                    transport: UserTransport::OpenAiCompat,
                    api_key: Some("k".to_string()),
                    model: Some("gpt-5.2".to_string()),
                    effort: Some("xhigh".to_string()),
                    ..Default::default()
                },
            ],
            default_channel: 0,
            ..Default::default()
        }];

        let picker = build_picker_state(&config, &ProviderUsage::default());
        let row = picker
            .rows
            .iter()
            .find(|row| row.id == "openai-relay")
            .expect("openai relay row");
        let gpt55 = row
            .model_info
            .iter()
            .find(|info| info.model == "gpt-5.5")
            .expect("gpt-5.5 info");
        assert_eq!(gpt55.protocol, "openai");
        assert_eq!(gpt55.effort.as_deref(), Some("medium"));
        assert_eq!(gpt55.thinking, None);

        let entries = build_catalog(&config);
        let entry = entries
            .iter()
            .find(|entry| entry.id == "openai-relay")
            .expect("openai relay entry");
        let gpt52 = entry.channel_for_model("gpt-5.2").expect("gpt-5.2");
        match &gpt52.transport {
            Transport::OpenAiCompat { effort, .. } => assert_eq!(*effort, Some(Effort::Xhigh)),
            other => panic!("expected OpenAiCompat, got {other:?}"),
        }
    }

    #[test]
    fn user_gemini_native_channel_carries_relay_base_url() {
        // A 中转站 wired onto a native-Gemini channel supplies the versioned
        // base URL; it must land on the transport verbatim (the provider
        // appends the `/models/{id}:generateContent` path itself).
        let entries = build_catalog(&gemini_two_channel_config());
        let entry = entries.iter().find(|e| e.id == "gemini").unwrap();
        let studio = entry.channels.iter().find(|c| c.label == "Studio").unwrap();
        match &studio.transport {
            Transport::GeminiNative { base_url, .. } => {
                assert_eq!(base_url, "https://relay.example.com/v1beta");
            }
            other => panic!("Studio must be native Gemini, got {other:?}"),
        }
    }

    #[test]
    fn user_gemini_native_channel_defaults_base_url_when_unset() {
        // A native-Gemini channel with no base_url falls back to the localhost
        // relay default (mirrors the OpenAI/Anthropic unset-channel contract),
        // never to Google's official endpoint — only the built-in `google`
        // preset resolves the official default.
        let mut config = bare_config();
        config.providers = vec![UserProviderConfig {
            id: "gemini".to_string(),
            name: None,
            channels: vec![UserChannelConfig {
                label: "default".to_string(),
                transport: UserTransport::GeminiNative,
                api_key: Some("k".to_string()),
                model: Some("gemini-2.5-flash".to_string()),
                ..Default::default()
            }],
            default_channel: 0,
            ..Default::default()
        }];
        let entries = build_catalog(&config);
        let entry = entries.iter().find(|e| e.id == "gemini").unwrap();
        match &entry.default_channel().unwrap().transport {
            Transport::GeminiNative { base_url, .. } => {
                assert_eq!(base_url, "http://localhost:8080/v1beta");
            }
            other => panic!("expected native Gemini, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn gemini_base_url_env_overrides_official_default() {
        // The built-in `google` preset reads GEMINI_BASE_URL first, then the
        // config slot, falling back to the official endpoint — same contract as
        // the anthropic relay (ADR for the configurable Claude relay).
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("GEMINI_BASE_URL", "https://relay.example.com/v1beta");
        }
        let mut config = bare_config();
        config.gemini_base_url = Some("https://from-config.example.com/v1beta".to_string());
        let entries = build_catalog(&config);
        unsafe {
            std::env::remove_var("GEMINI_BASE_URL");
        }
        let entry = entries.iter().find(|e| e.id == "google").unwrap();
        match &entry.default_channel().unwrap().transport {
            Transport::GeminiNative { base_url, .. } => {
                // env wins over config.
                assert_eq!(base_url, "https://relay.example.com/v1beta");
            }
            other => panic!("google must be native Gemini, got {other:?}"),
        }
    }

    #[test]
    fn user_model_appends_when_id_is_new() {
        let mut config = bare_config();
        config.providers = vec![UserProviderConfig {
            id: "my-relay".to_string(),
            name: Some("My Relay".to_string()),
            channels: vec![UserChannelConfig {
                label: "default".to_string(),
                transport: UserTransport::OpenAiCompat,
                base_url: Some("https://my.example.com/v1/chat/completions".to_string()),
                api_key: Some("k".to_string()),
                model: Some("my-model".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }];
        let entries = build_catalog(&config);
        let relay = entries
            .iter()
            .find(|e| e.id == "my-relay")
            .expect("appended user model");
        assert_eq!(relay.name, "My Relay");
        assert_eq!(relay.default_channel().unwrap().model, "my-model");
    }

    #[test]
    fn default_provider_id_reads_config() {
        let mut config = bare_config();
        config.default_provider = "zai-code".to_string();
        assert_eq!(default_provider_id(&config), "zai-code");
    }

    #[test]
    fn picker_state_reflects_user_default_and_channels() {
        let mut config = gemini_two_channel_config();
        config.default_provider = "gemini".to_string();
        let usage = ProviderUsage::default();
        let snapshot = build_picker_state(&config, &usage);
        assert_eq!(snapshot.default_id, "gemini");
        let gemini_row = snapshot
            .rows
            .iter()
            .find(|r| r.id == "gemini")
            .expect("gemini row present");
        assert!(gemini_row.key_ready, "Relay channel has an inline key");
        // The picker row is fully self-describing: a user-defined provider shows
        // its display name, served models, active model, and builtin=false — the
        // fields the snapshot-driven TUI renders directly (no static table).
        assert_eq!(gemini_row.name, "Gemini (custom)");
        assert!(!gemini_row.builtin, "user-defined provider is not builtin");
        assert_eq!(gemini_row.models.len(), 2, "both channels' models listed");
        assert!(gemini_row.models.iter().all(|m| m == "gemini-2.5-flash"));
        assert_eq!(gemini_row.model, "gemini-2.5-flash");
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn openai_is_a_multi_model_builtin_with_gpt_4o_default() {
        // OpenAI is now a multi-model provider: its picker row lists every
        // OPENAI_BUILTIN_MODELS entry and defaults to gpt-4o.
        let config = bare_config();
        let usage = ProviderUsage::default();
        let snapshot = build_picker_state(&config, &usage);
        let openai = snapshot
            .rows
            .iter()
            .find(|r| r.id == "openai")
            .expect("openai row present");
        assert_eq!(openai.name, "OpenAI");
        assert!(openai.builtin);
        assert!(openai.models.contains(&"gpt-4o".to_string()));
        assert!(openai.models.contains(&"gpt-4o-mini".to_string()));
        assert_eq!(openai.model, "gpt-4o");
        // Llama no longer appears as a built-in provider.
        assert!(snapshot.rows.iter().all(|r| r.id != "llama"));
    }

    // ── live model discovery (discover_provider_models) ────────────────────

    #[test]
    fn default_model_source_maps_discovery_flag() {
        // A discovery-enabled template → Api; a fixed-list template → Fixed.
        let openai_spec = provider_template_spec("openai").expect("openai template");
        assert_eq!(
            default_model_source_for_spec(openai_spec),
            neenee_store::config::ModelSource::Api
        );
        let opencode_spec = provider_template_spec("opencode-go").expect("opencode-go template");
        assert_eq!(
            default_model_source_for_spec(opencode_spec),
            neenee_store::config::ModelSource::Fixed
        );
    }

    #[tokio::test]
    async fn discover_filters_to_supported_intersection_and_keeps_provider_settings() {
        let spec = provider_template_spec("openai-sub2api").unwrap();
        let kept_a = spec.models[1];
        let kept_b = spec.models[4];
        let known_outside_seed = "gpt-4o";
        assert!(!spec.models.contains(&known_outside_seed));
        let advertised = vec![
            "cloud-only-model".to_string(),
            kept_b.to_string(),
            known_outside_seed.to_string(),
            kept_a.to_string(),
        ];
        let expected = supported_model_intersection(
            &supported_models_for_protocol(spec.protocol),
            &advertised,
        )
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let mut server = mockito::Server::new_async().await;
        let body = format!(
            r#"{{"data":[{{"id":"cloud-only-model"}},{{"id":"{kept_b}"}},{{"id":"{known_outside_seed}"}},{{"id":"{kept_a}"}}]}}"#
        );
        let _mock = server
            .mock("GET", "/v1/models")
            .match_header("authorization", "Bearer sk-test")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let mut instance = template_instance("openai-sub2api", spec.models);
        instance.model_source = neenee_store::config::ModelSource::Api;
        let chat_url = format!("{}/v1/chat/completions", server.url());
        for channel in &mut instance.channels {
            channel.base_url = Some(chat_url.clone());
            channel.api_key_env = Some("RELAY_API_KEY".to_string());
            channel.user_agent = Some("relay-client/1.0".to_string());
        }
        let mut config = bare_config();
        config.providers.push(instance);

        assert!(discover_provider_models(&mut config).await);
        assert_eq!(config.providers[0].channel_models(), expected);
        assert!(config.providers[0].channels.iter().all(|channel| {
            channel.api_key.as_deref() == Some("sk-test")
                && channel.api_key_env.as_deref() == Some("RELAY_API_KEY")
                && channel.base_url.as_deref() == Some(chat_url.as_str())
                && channel.user_agent.as_deref() == Some("relay-client/1.0")
        }));
    }

    #[tokio::test]
    async fn discover_empty_supported_intersection_keeps_previous_provider() {
        let spec = provider_template_spec("openai-sub2api").unwrap();
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/v1/models")
            .match_header("authorization", "Bearer sk-test")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":[{"id":"cloud-only-model"}]}"#)
            .create_async()
            .await;

        let mut instance = template_instance("openai-sub2api", spec.models);
        instance.model_source = neenee_store::config::ModelSource::Api;
        let chat_url = format!("{}/v1/chat/completions", server.url());
        for channel in &mut instance.channels {
            channel.base_url = Some(chat_url.clone());
        }
        let before = instance
            .channel_models()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let mut config = bare_config();
        config.providers.push(instance);

        assert!(!discover_provider_models(&mut config).await);
        assert_eq!(config.providers[0].channel_models(), before);
        assert!(config.providers[0].channels.iter().all(|channel| {
            channel.api_key.as_deref() == Some("sk-test")
                && channel.base_url.as_deref() == Some(chat_url.as_str())
        }));
    }

    #[tokio::test]
    async fn discover_skips_fixed_instances_without_hitting_network() {
        // A Fixed template-sourced instance must be skipped entirely — the
        // snapshot from reconcile is authoritative. Because discover returns
        // `false` (no change) and never attempts a fetch, this also confirms
        // the gating is correct.
        let mut config = bare_config();
        let mut instance = template_instance("openai-sub2api", OPENAI_SUB2API_MODELS);
        instance.model_source = neenee_store::config::ModelSource::Fixed;
        config.providers.push(instance);
        let before: Vec<String> = config.providers[0]
            .channel_models()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let changed = discover_provider_models(&mut config).await;

        assert!(!changed, "Fixed instance must not be discovered");
        let after: Vec<String> = config.providers[0]
            .channel_models()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(after, before, "Fixed instance models untouched");
    }

    #[tokio::test]
    async fn discover_falls_back_to_snapshot_when_fetch_fails() {
        // An Api instance whose endpoint is unreachable (relay.example.com does
        // not resolve within the request timeout) must keep the template
        // snapshot — the live fetch only ever improves, never regresses.
        let mut config = bare_config();
        let mut instance = template_instance("openai-sub2api", OPENAI_SUB2API_MODELS);
        instance.model_source = neenee_store::config::ModelSource::Api;
        config.providers.push(instance);
        let before: Vec<String> = config.providers[0]
            .channel_models()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let changed = discover_provider_models(&mut config).await;

        assert!(
            !changed,
            "a failed fetch must not report a change (snapshot kept as-is)"
        );
        let after: Vec<String> = config.providers[0]
            .channel_models()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            after, before,
            "snapshot must be preserved when the live fetch fails"
        );
    }

    #[tokio::test]
    async fn discover_skips_discovery_disabled_template_even_when_api() {
        // opencode-go is discovery=false; even with model_source=Api it must be
        // skipped (the template does not expose a usable /models endpoint).
        let mut config = bare_config();
        let mut instance = template_instance("opencode-go", OPENCODE_GO_MODELS);
        instance.model_source = neenee_store::config::ModelSource::Api;
        config.providers.push(instance);
        let before: Vec<String> = config.providers[0]
            .channel_models()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let changed = discover_provider_models(&mut config).await;

        assert!(!changed, "discovery-disabled template must be skipped");
        let after: Vec<String> = config.providers[0]
            .channel_models()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn discover_skips_pure_custom_instance() {
        // A pure-custom instance (no template_id) must never be discovered.
        let mut config = bare_config();
        let mut instance = template_instance("openai-sub2api", OPENAI_SUB2API_MODELS);
        instance.template_id = None;
        instance.model_source = neenee_store::config::ModelSource::Api;
        config.providers.push(instance);
        let before: Vec<String> = config.providers[0]
            .channel_models()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let changed = discover_provider_models(&mut config).await;

        assert!(!changed, "pure-custom instance must not be discovered");
        let after: Vec<String> = config.providers[0]
            .channel_models()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(after, before);
    }

    #[test]
    fn reconcile_backfill_sets_api_model_source_for_discovery_template() {
        // A legacy instance that exactly matches a discovery-enabled template
        // (openai-sub2api) gets stamped AND adopts model_source=Api, so it
        // starts benefiting from live discovery on the next startup.
        let models = current_template_models("openai-sub2api");
        let mut config = bare_config();
        config.providers.push(UserProviderConfig {
            id: "relay".to_string(),
            name: Some("Relay".to_string()),
            channels: models
                .iter()
                .map(|m| UserChannelConfig {
                    label: m.clone(),
                    transport: UserTransport::OpenAiCompat,
                    api_key_env: None,
                    api_key: Some("sk".to_string()),
                    model: Some(m.clone()),
                    base_url: Some("https://relay.example.com".to_string()),
                    user_agent: None,
                    effort: None,
                    thinking: None,
                    auth: Default::default(),
                })
                .collect(),
            default_channel: 0,
            template_id: None,
            model_source: Default::default(),
        });

        assert!(reconcile_provider_models(&mut config));
        assert_eq!(
            config.providers[0].template_id.as_deref(),
            Some("openai-sub2api")
        );
        assert_eq!(
            config.providers[0].model_source,
            neenee_store::config::ModelSource::Api,
            "backfilled discovery-template instance adopts Api source"
        );
    }

    #[test]
    fn reconcile_backfill_sets_fixed_model_source_for_nondiscovery_template() {
        // A legacy instance that exactly matches a discovery-disabled template
        // (kimi-code) gets stamped but keeps model_source=Fixed.
        let models = current_template_models("kimi-code");
        let mut config = bare_config();
        config.providers.push(UserProviderConfig {
            id: "kimi".to_string(),
            name: Some("Kimi".to_string()),
            channels: models
                .iter()
                .map(|m| UserChannelConfig {
                    label: m.clone(),
                    transport: UserTransport::OpenAiCompat,
                    api_key_env: None,
                    api_key: Some("sk".to_string()),
                    model: Some(m.clone()),
                    base_url: Some("https://kimi.example.com".to_string()),
                    user_agent: None,
                    effort: None,
                    thinking: None,
                    auth: Default::default(),
                })
                .collect(),
            default_channel: 0,
            template_id: None,
            model_source: Default::default(),
        });

        assert!(reconcile_provider_models(&mut config));
        assert_eq!(
            config.providers[0].template_id.as_deref(),
            Some("kimi-code")
        );
        assert_eq!(
            config.providers[0].model_source,
            neenee_store::config::ModelSource::Fixed,
            "backfilled nondiscovery-template instance keeps Fixed source"
        );
    }
}
