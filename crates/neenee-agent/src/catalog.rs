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
    Effort, ProviderModelInfo, ProviderPickerRow, ProviderPickerSnapshot, RemoteModelEndpoint,
    SecretString, ThinkingMode, WireFormat,
};
use neenee_persistence::config::{
    Config, FittedModelInfo, ModelSource, UserChannelConfig, UserProviderConfig, UserTransport,
};
use neenee_persistence::provider_usage::ProviderUsage;
use neenee_providers::{
    ANTHROPIC_BUILTIN_MODELS, DEEPSEEK_BUILTIN_MODELS, GOOGLE_BUILTIN_MODELS, KIMI_CODE_MODELS,
    NEENEE_USER_AGENT, OPENAI_BUILTIN_MODELS, OPENCODE_GO_SERVED_MODELS, provider_template_spec,
};
use std::collections::HashSet;

#[cfg(test)]
use neenee_providers::{OPENAI_PROVIDER_SPECS, OPENAI_SUB2API_MODELS, OPENCODE_GO_MODELS};

/// The effective default provider id from `config.default_provider`.
pub fn default_provider_id(config: &Config) -> &str {
    &config.default_provider
}

/// The ChatGPT subscription backend (Codex Responses API). A ChatGPT OAuth
/// channel routes here rather than to chat completions, sending the OAuth
/// access token as a bearer plus the `ChatGPT-Account-Id` header.
pub const CHATGPT_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// The GitHub Copilot subscription backend (chat-completions surface). A
/// Copilot OAuth channel routes here, sending the GitHub OAuth access token as
/// a bearer plus Copilot's required request headers (`x-initiator`,
/// `Openai-Intent`, `X-GitHub-Api-Version`). This is the universally available
/// endpoint — every Copilot plan (incl. Free/Student, which only unlock the
/// GPT-4o chat family) can speak chat-completions; the Responses API and
/// GPT-5 require Pro+ and are not assumed.
pub const COPILOT_CHAT_URL: &str = "https://api.githubcopilot.com/chat/completions";

/// Convert a user-defined channel config into a resolved [`Channel`].
///
/// Resolution rules mirror the built-in path: an `api_key_env` value wins over
/// an inline `api_key` (and empty env values fall through, just like built-ins);
/// the wire `model` falls back to the parent model's id; transport-specific
/// fields (`base_url`, `user_agent`) fall back to localhost defaults so a
/// minimal entry still builds.
fn user_channel_to_channel(uc: &UserChannelConfig, fallback_model: &str) -> Channel {
    // OAuth channels resolve their bearer from auth.toml. ChatGPT also yields
    // the chatgpt_account_id (carried on the Responses transport); xAI has none.
    // Activate/switch refreshes the token first (handlers_provider).
    let (api_key, account_id) = match uc.auth {
        neenee_core::ChannelAuth::ChatGptOAuth => {
            let store = neenee_oauth::AuthStore::load();
            let tokens = store.get("chatgpt");
            (
                tokens.map(|t| t.access.clone()).unwrap_or_default(),
                tokens.and_then(|t| t.account_id.clone()),
            )
        }
        neenee_core::ChannelAuth::CopilotOAuth => {
            // Copilot's bearer is the GitHub OAuth access token; there is no
            // account id (unlike ChatGPT's chatgpt_account_id claim).
            let store = neenee_oauth::AuthStore::load();
            (
                store
                    .get("copilot")
                    .map(|tokens| tokens.access.clone())
                    .unwrap_or_default(),
                None,
            )
        }
        neenee_core::ChannelAuth::XaiOAuth => {
            let store = neenee_oauth::AuthStore::load();
            (
                store
                    .get("xai")
                    .map(|tokens| tokens.access.clone())
                    .unwrap_or_default(),
                None,
            )
        }
        neenee_core::ChannelAuth::ApiKey => (
            env_or_config(uc.api_key_env.as_deref(), uc.api_key.clone()).unwrap_or_default(),
            None,
        ),
    };
    let model = uc
        .model
        .clone()
        .unwrap_or_else(|| fallback_model.to_string());
    let transport = match uc.auth {
        // ChatGPT OAuth always speaks the Responses transport, regardless of the
        // stored `UserTransport`, with the bearer + account id resolved above.
        neenee_core::ChannelAuth::ChatGptOAuth => Transport::OpenAiResponses {
            base_url: uc
                .base_url
                .clone()
                .unwrap_or_else(|| CHATGPT_RESPONSES_URL.to_string()),
            user_agent: uc
                .user_agent
                .clone()
                .unwrap_or_else(|| NEENEE_USER_AGENT.to_string()),
            effort: uc.effort.as_deref().and_then(Effort::parse),
            account_id,
            copilot: false,
        },
        neenee_core::ChannelAuth::CopilotOAuth => copilot_transport(uc),
        _ => match uc.transport {
            UserTransport::Google => Transport::Google {
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
                    copilot: false,
                }
            }
            UserTransport::OpenAi => Transport::OpenAi {
                base_url: uc
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "http://localhost:8080/v1/chat/completions".to_string()),
                user_agent: uc
                    .user_agent
                    .clone()
                    .unwrap_or_else(|| NEENEE_USER_AGENT.to_string()),
                effort: uc.effort.as_deref().and_then(Effort::parse),
                copilot: false,
            },
        },
    };
    Channel {
        id: uc.label.clone(),
        label: uc.label.clone(),
        transport,
        api_key,
        model,
        remote: uc.remote.clone(),
    }
}

/// Build a Copilot channel from the endpoint advertised for this model. The
/// initial seed has no remote metadata and deliberately falls back to Chat
/// Completions; the first live `/models` refresh replaces it with the exact
/// route for every plan-unlocked model.
fn copilot_transport(uc: &UserChannelConfig) -> Transport {
    let user_agent = uc
        .user_agent
        .clone()
        .unwrap_or_else(|| NEENEE_USER_AGENT.to_string());
    let effort = uc.effort.as_deref().and_then(Effort::parse);
    match uc.remote.as_ref().and_then(|remote| remote.endpoint) {
        Some(RemoteModelEndpoint::Responses) => Transport::OpenAiResponses {
            base_url: "https://api.githubcopilot.com/responses".to_string(),
            user_agent,
            effort,
            account_id: None,
            copilot: true,
        },
        Some(RemoteModelEndpoint::Messages) => Transport::Anthropic {
            base_url: "https://api.githubcopilot.com/v1/messages".to_string(),
            user_agent,
            effort,
            thinking: uc.thinking.map(|on| {
                if on {
                    ThinkingMode::Adaptive
                } else {
                    ThinkingMode::Off
                }
            }),
            copilot: true,
        },
        Some(RemoteModelEndpoint::ChatCompletions) | None => Transport::OpenAi {
            base_url: uc
                .base_url
                .clone()
                .unwrap_or_else(|| COPILOT_CHAT_URL.to_string()),
            user_agent,
            effort,
            copilot: true,
        },
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
fn env_or_config(
    env_var: Option<&str>,
    config_value: Option<SecretString>,
) -> Option<SecretString> {
    env_var
        .and_then(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
        .map(SecretString::from)
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
    .any(|key| !key.expose_secret().trim().is_empty());

    changed |= migrate_legacy_instance(
        config,
        "openai",
        "OpenAI",
        UserTransport::OpenAi,
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
        UserTransport::Google,
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
        UserTransport::OpenAi,
        "https://api.kimi.com/coding/v1/chat/completions",
        Some("opencode/0.1.0"),
        KIMI_CODE_MODELS,
        kimi_key,
        legacy_model.as_deref(),
        Some("kimi-code"),
    );
    changed |= migrate_legacy_instance(
        config,
        "deepseek",
        "DeepSeek",
        UserTransport::OpenAi,
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
        UserTransport::OpenAi,
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
        .filter(|k| !k.expose_secret().trim().is_empty())
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
                // The OpenCode Go seed mirrors the relay's served catalogue
                // (OPENCODE_GO_SERVED_MODELS), so it is NOT a 1:1 mirror of
                // the `opencode-go` template spec (which uses a fixed list).
                // Leave it untracked so a template edit does not clobber the
                // curated served set.
                template_id: None,
                // A pure-custom (untracked) instance; model_source is ignored,
                // so the Fixed default is harmless.
                model_source: Default::default(),
                fitted_models: Default::default(),
            });
            changed = true;
        }
    }

    // Strip the legacy per-provider fields the migration above consumes.
    // `default_model` is NOT legacy: the switch handler persists it as the
    // global model pointer and the runtime honors it as the active model when
    // the default provider serves it — taking it would erase the persisted
    // selection on every startup.
    if config.openai_model.take().is_some()
        | config.moonshot_model.take().is_some()
        | config.zai_model.take().is_some()
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
/// `neenee-persistence`), then delegates the actual channel rebuild to
/// [`UserProviderConfig::reseed_channels_from_models`], which owns the
/// channel-level invariants. ADR-0005 forbids `neenee-persistence` from depending on
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
///
/// Per-instance semantics:
/// - **Fixed** instances mirror the whole template snapshot.
/// - **Api** instances retain their last discovered subset of ids known to the
///   client: registry ids compatible with the template protocol, plus — for
///   fitting-enabled templates — the persisted fitted ids (ADR-0065).
///   Re-expanding Api instances to the snapshot here would overwrite the
///   persisted discovery result on every startup before the picker could
///   display it.
/// - A **Fixed** instance of a fitting template upgrades to **Api**: its
///   Fixed source predates the template's discovery support and cannot have
///   been a deliberate opt-out.
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
                let mut known_models: Vec<&str> = supported_models_for_protocol(spec.protocol);
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

/// Fetch live model lists for every template-sourced instance whose
/// [`ModelSource`](UserProviderConfig::model_source) is `Api`.
///
/// The companion to [`reconcile_provider_models`] for the live-discovery path:
/// where reconcile validates the last known subset synchronously, this hits
/// each provider's actual `GET /models` endpoint asynchronously and persists
/// the result. Two shapes, decided by the template's `fitting` flag
/// (ADR-0065):
///
/// - **Registry-intersected** (default): only ids both advertised and known
///   to the client for the template protocol are materialized. An arbitrary
///   relay is an availability signal only, never a metadata source.
/// - **Fitted** (trusted first-party templates): every advertised picker model
///   is materialized. Its provider-scoped capability snapshot is persisted on
///   the channel, so exact endpoints and explicit remote values override the
///   static baseline; registry-unknown ids are also mirrored to `fitted_models`
///   for legacy id-only resolution outside a channel.
///
/// Either way, the previous subset (or initial template snapshot) is kept
/// when fetching fails or the result is empty, so a flaky network, a wrong
/// endpoint, or an incompatible response cannot blank a provider.
///
/// Per-instance semantics:
/// - `template_id` resolves to a known template **and** `spec.discovery` is on
///   **and** `model_source == Api` → fetch live as above.
/// - Otherwise → skipped. `Fixed` instances, discovery-disabled templates
///   (Z.AI Code, opencode-go), and pure-custom instances keep what the
///   synchronous reconcile produced.
///
/// Returns a [`DiscoveryOutcome`] so the caller knows whether to persist and
/// whether any instance failed to fetch. Best-effort: a per-instance failure
/// never aborts the pass — the remaining instances are still fetched, and every
/// failure is reported back so the UI can tell the user *why* their model list
/// did not update instead of silently keeping the stale seed.
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
        // OAuth channels (xAI / ChatGPT / Copilot) store no api_key — their
        // bearer lives in auth.toml and is resolved at runtime. Discovery must
        // read the same token a chat request would send, so resolve it here for
        // OAuth auth modes; API-key channels keep using the stored key.
        let resolved_bearer: SecretString;
        let no_key = SecretString::default();
        let api_key: &SecretString = match channel.auth {
            neenee_core::ChannelAuth::ApiKey => channel.api_key.as_ref().unwrap_or(&no_key),
            neenee_core::ChannelAuth::CopilotOAuth => {
                resolved_bearer = neenee_oauth::AuthStore::load()
                    .get("copilot")
                    .map(|tokens| tokens.access.clone())
                    .unwrap_or_default();
                &resolved_bearer
            }
            neenee_core::ChannelAuth::ChatGptOAuth => {
                resolved_bearer = neenee_oauth::AuthStore::load()
                    .get("chatgpt")
                    .map(|tokens| tokens.access.clone())
                    .unwrap_or_default();
                &resolved_bearer
            }
            neenee_core::ChannelAuth::XaiOAuth => {
                resolved_bearer = neenee_oauth::AuthStore::load()
                    .get("xai")
                    .map(|tokens| tokens.access.clone())
                    .unwrap_or_default();
                &resolved_bearer
            }
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
                        .filter(|model| neenee_core::model::model_by_id(&model.id).is_none())
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
                    let known_models = supported_models_for_protocol(spec.protocol);
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

/// Model ids known to the client and compatible with a provider protocol.
fn supported_models_for_protocol(protocol: &str) -> Vec<&'static str> {
    neenee_core::KNOWN_MODELS
        .iter()
        .filter(|model| {
            matches!(
                (protocol, model.format),
                ("openai", WireFormat::OpenAi)
                    | ("anthropic", WireFormat::AnthropicCompat)
                    | ("gemini", WireFormat::Google)
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

/// Convert a live-discovered model entry into its persisted fitted form.
/// Absent capability hints degrade to the conservative zero values — the
/// overlay then behaves exactly like the static fallback for that aspect.
fn fitted_model_info(model: &neenee_providers::DiscoveredModel) -> FittedModelInfo {
    FittedModelInfo {
        context_window: model.context_window.unwrap_or(0),
        reasoning: model.reasoning.unwrap_or(false),
        vision: model.vision.unwrap_or(false),
        efforts: model.effort_levels.clone().unwrap_or_default(),
        display_name: model.display_name.clone(),
    }
}

/// Persist trusted remote metadata on each currently materialized channel.
///
/// A channel is the ownership boundary for a remote model description: the same
/// id can expose different endpoints and capability values at another provider
/// or under another Copilot plan. Clearing metadata for models that disappeared
/// is handled naturally by reseeding before this function runs.
fn persist_remote_model_metadata(
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

/// Feed every instance's persisted fitted-model metadata into the dynamic
/// model overlay ([`neenee_core::model::register_fitted_models`]), so a model
/// id the static registry does not know still resolves with the capabilities
/// its (trusted) provider advertised — context window, reasoning, vision, and
/// effort tiers flow through the same `model::resolve` every consumer uses.
///
/// Idempotent and additive: the static registry always wins for ids it knows.
/// Called at startup after reconciliation and again after a live discovery
/// refresh.
pub fn sync_fitted_model_registry(config: &Config) {
    let fitted: Vec<neenee_core::model::FittedModel> = config
        .providers
        .iter()
        .flat_map(|provider| {
            let spec = provider
                .template_id
                .as_deref()
                .and_then(provider_template_spec);
            provider.fitted_models.iter().map(move |(id, info)| {
                let (format, family) = match spec {
                    Some(spec) => (wire_format_for_protocol(spec.protocol), spec.id.to_string()),
                    // A pure-custom instance should never carry fitted data
                    // (only fitting templates write it); degrade to the most
                    // common shape if one somehow does.
                    None => (WireFormat::OpenAi, provider.id.clone()),
                };
                neenee_core::model::FittedModel {
                    id: id.clone(),
                    display_name: info.display_name.clone(),
                    family,
                    context_window: info.context_window,
                    reasoning: info.reasoning,
                    vision: info.vision,
                    format,
                    effort_levels: info
                        .efforts
                        .iter()
                        .filter_map(|level| Effort::parse(level))
                        .collect(),
                }
            })
        })
        .collect();
    neenee_core::model::register_fitted_models(fitted);
}

/// Map a template wire protocol to the registry's wire format. Mirrors
/// [`transport_for_protocol`], which produces the channel-level enum.
fn wire_format_for_protocol(protocol: &str) -> WireFormat {
    match protocol {
        "anthropic" => WireFormat::AnthropicCompat,
        "gemini" => WireFormat::Google,
        _ => WireFormat::OpenAi,
    }
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
        "gemini" => UserTransport::Google,
        _ => UserTransport::OpenAi,
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
    api_key: Option<SecretString>,
    active_model: Option<&str>,
    template_id: Option<&str>,
) -> bool {
    let Some(api_key) = api_key.filter(|k| !k.expose_secret().trim().is_empty()) else {
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
            remote: None,
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
        fitted_models: Default::default(),
    });
    true
}

/// Seed one channel per model the opencode-go relay actually serves
/// ([`OPENCODE_GO_SERVED_MODELS`], mirroring models.dev), taking the wire
/// format and metadata from the client registry. A model registered for
/// another provider (e.g. Kimi `k3`, `glm-4.7`) must not leak in here — an
/// unserved channel only ever answers "model not found".
fn opencode_go_seed_channels(api_key: SecretString) -> Vec<UserChannelConfig> {
    let mut models: Vec<_> = neenee_core::KNOWN_MODELS
        .iter()
        .filter(|m| OPENCODE_GO_SERVED_MODELS.contains(&m.id))
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
                WireFormat::Google => (UserTransport::Google, "https://opencode.ai/zen/go/v1beta"),
                WireFormat::OpenAi => (
                    UserTransport::OpenAi,
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
                remote: None,
            }
        })
        .collect()
}

/// Resolve the active provider for a given provider id from `config`. Returns
/// `None` when the id is unknown or the entry has no usable channel — the
/// caller is expected to refuse the action and surface a notification rather
/// than silently fall back to a placeholder.
///
/// Channel selection honors `config.default_model`: for a multi-model provider
/// like opencode-go, the channel carrying that model (and thus the matching
/// transport) is chosen; otherwise the entry's default channel is used. This is
/// the single replacement for the resolution logic that used to be duplicated
/// at startup and in the `SwitchProvider` handler.
pub fn build_provider_for(
    config: &Config,
    id: &str,
) -> Option<std::sync::Arc<dyn neenee_core::Provider>> {
    build_provider_for_model(config, id, config.default_model.as_deref(), None)
}

/// Resolve the provider for `provider_id`, selecting the channel that carries
/// `model_id` when given (falling back to `config.default_model`, then the
/// entry's default channel). Runtime switches that carry an explicit model
/// (e.g. selecting `minimax-m3` under opencode-go) route through here so the
/// per-model transport is picked correctly.
///
/// Returns `None` when the provider id is unknown or the entry has no resolvable
/// channel. Callers must surface a user-facing notification in that case rather
/// than silently installing a placeholder — see [`crate::NoProvider`] for the
/// explicit sentinel installed only at startup so the shared holder satisfies
/// its `Arc<dyn Provider>` type.
///
/// `session_id` flows into prompt-cache control (ADR-0067): when the active
/// model's [`neenee_core::CachePolicy`] is `SessionKey` (Moonshot / Kimi), the
/// session id becomes the provider's `prompt_cache_key`. Pass `None` at shared
/// bootstrap; pass the live session id on session create / model switch.
pub fn build_provider_for_model(
    config: &Config,
    provider_id: &str,
    model_id: Option<&str>,
    session_id: Option<&str>,
) -> Option<std::sync::Arc<dyn neenee_core::Provider>> {
    let entries = build_catalog(config);
    let entry = entries.iter().find(|e| e.id == provider_id)?;
    let wanted = model_id.or(config.default_model.as_deref());
    let channel = wanted
        .and_then(|m| entry.channel_for_model(m))
        .or_else(|| entry.default_channel());
    channel
        .map(|channel| neenee_providers::build_provider_for_channel(channel, &entry.id, session_id))
}

/// The display model name for a given provider id, as resolved from `config`.
/// Returns `None` when the id is unknown or no model can be resolved, so
/// callers can distinguish "no provider configured" from a real (possibly
/// empty-string) model id. Replaces the former `initial_m_name` block in
/// `main.rs`.
///
/// For multi-model providers, the active model is `config.default_model` when
/// set (and served by the provider); otherwise the entry's default-channel
/// model. (Does not consult usage telemetry — startup paths that only know
/// the config use this; the picker uses [`active_model_id_for_entry`].)
pub fn resolved_model_name(config: &Config, id: &str) -> Option<String> {
    resolved_model_name_inner(config, id, &ProviderUsage::default())
}

/// The active model name resolved with usage telemetry so the last-activated
/// model per provider wins over the bare default-channel fallback. Used where
/// a live `usage` is available (status surfaces, re-activation). Returns
/// `None` when the provider id is unknown or has no resolvable model.
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

/// The active wire model id for an already-built entry, resolved from usage
/// telemetry: `config.default_model` wins when the entry serves it, otherwise
/// the provider's last-activated model (so re-opening a provider lands on the
/// exact model it was left at), finally the entry's default-channel model.
/// Shared by [`resolved_model_name`] and [`build_picker_state`] so both pick
/// the same active model without rebuilding the catalog per row. Returns
/// `None` when no model can be resolved (the entry has no channels, or every
/// candidate fails the `offers_model` filter).
fn active_model_id_for_entry(
    config: &Config,
    entry: &ProviderEntry,
    usage: &ProviderUsage,
) -> Option<String> {
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
            let model = active_model_id_for_entry(config, entry, usage).unwrap_or_default();
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
        Transport::OpenAi { base_url, .. } => ("openai".to_string(), base_url.clone()),
        Transport::OpenAiResponses { base_url, .. } => ("openai".to_string(), base_url.clone()),
        Transport::Anthropic { base_url, .. } => ("anthropic".to_string(), base_url.clone()),
        Transport::Google { base_url, .. } => ("gemini".to_string(), base_url.clone()),
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
        Transport::OpenAi { effort, .. } => {
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
        Transport::OpenAiResponses { effort, .. } => {
            let model = neenee_core::model::resolve(&channel.model);
            let effective = if model.effort_levels.is_empty() {
                None
            } else {
                Some((*effort).unwrap_or(Effort::Medium).as_str().to_string())
            };
            ProviderModelInfo {
                model: channel.model.clone(),
                protocol: "openai".to_string(),
                effort: effective,
                thinking: None,
                last_used_ms: None,
            }
        }
        Transport::Google { .. } => ProviderModelInfo {
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
        assert!(build_provider_for(&config, default_provider_id(&config)).is_none());
    }

    #[test]
    fn legacy_builtin_key_migrates_to_named_instance() {
        let mut config = bare_config();
        config.default_provider = "openai".to_string();
        config.default_model = Some("gpt-5.4-mini".to_string());
        config.openai_api_key = Some("sk-old".into());

        assert!(migrate_legacy_provider_instances(&mut config));
        assert!(config.openai_api_key.is_none());
        // `default_model` is a live field (the switch handler persists it), so
        // the migration must NOT strip it — only seed the instance's default
        // channel from it.
        assert_eq!(config.default_model.as_deref(), Some("gpt-5.4-mini"));
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

    #[test]
    fn migration_strips_legacy_model_slots_but_preserves_default_model() {
        let mut config = bare_config();
        config.default_provider = "kimi-code".to_string();
        config.default_model = Some("k3".to_string());
        config.moonshot_model = Some("k3".to_string());
        // An existing kimi-code instance (created by an earlier migration or
        // the add-provider flow) — the migration has nothing to create, only
        // legacy fields to strip.
        config.providers.push(UserProviderConfig {
            id: "kimi-code".to_string(),
            channels: vec![UserChannelConfig {
                label: "k3".to_string(),
                model: Some("k3".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });

        assert!(migrate_legacy_provider_instances(&mut config));
        // Legacy per-provider model slots are consumed…
        assert!(config.moonshot_model.is_none());
        // …but the persisted global model pointer survives, so a fresh
        // session lands on the model the user last switched to.
        assert_eq!(config.default_model.as_deref(), Some("k3"));
        assert_eq!(config.default_provider, "kimi-code");
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
                    transport: UserTransport::OpenAi,
                    api_key_env: None,
                    api_key: Some("sk-test".into()),
                    model: Some(m.to_string()),
                    base_url: Some("https://relay.example.com/v1/chat/completions".to_string()),
                    user_agent: None,
                    effort: None,
                    thinking: None,
                    auth: Default::default(),
                    remote: None,
                })
                .collect(),
            default_channel: 0,
            template_id: Some(tid.to_string()),
            model_source: Default::default(),
            fitted_models: Default::default(),
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
                    transport: UserTransport::OpenAi,
                    api_key_env: None,
                    api_key: Some("sk".into()),
                    model: Some(m.clone()),
                    base_url: Some("https://relay.example.com".to_string()),
                    user_agent: None,
                    effort: None,
                    thinking: None,
                    auth: Default::default(),
                    remote: None,
                })
                .collect(),
            default_channel: 0,
            template_id: Some("openai-sub2api".to_string()),
            model_source: Default::default(),
            fitted_models: Default::default(),
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
            config.providers[0].channels.iter().all(|c| c
                .api_key
                .as_ref()
                .map(SecretString::expose_secret)
                == Some("sk-test")),
            "shared key is preserved across the reseed"
        );
    }

    #[test]
    fn reconcile_api_instance_keeps_last_discovered_supported_subset() {
        let known = supported_models_for_protocol("openai");
        let subset = [known[1], known[3]];
        let mut instance = template_instance("openai-sub2api", &subset);
        instance.model_source = neenee_persistence::config::ModelSource::Api;
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
        instance.model_source = neenee_persistence::config::ModelSource::Api;
        let mut config = bare_config();
        config.providers.push(instance);

        assert!(reconcile_provider_models(&mut config));
        assert_eq!(config.providers[0].channel_models(), vec![kept]);
        let channel = &config.providers[0].channels[0];
        assert_eq!(
            channel.api_key.as_ref().map(SecretString::expose_secret),
            Some("sk-test")
        );
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
                transport: UserTransport::OpenAi,
                api_key_env: None,
                api_key: Some("sk".into()),
                model: Some("only-model".to_string()),
                base_url: Some("https://x.example.com".to_string()),
                user_agent: None,
                effort: None,
                thinking: None,
                auth: Default::default(),
                remote: None,
            }],
            default_channel: 0,
            template_id: Some("removed-template".to_string()),
            model_source: Default::default(),
            fitted_models: Default::default(),
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
    fn opencode_go_seed_channels_only_include_models_the_relay_serves() {
        let channels = opencode_go_seed_channels("go-key".into());
        let ids: Vec<&str> = channels.iter().filter_map(|c| c.model.as_deref()).collect();
        // Models registered for other providers but not served by the relay
        // must not be seeded: an unserved channel only answers "model not
        // found" (Kimi k3 is kimi-code-only; glm-4.7 is not on go).
        assert!(!ids.contains(&"k3"), "k3 must not be seeded: {ids:?}");
        assert!(!ids.contains(&"glm-4.7"), "glm-4.7 must not be seeded");
        // Served models in the registry are seeded, each with the transport
        // its wire format implies (one provider, two wire formats).
        for (id, is_anthropic) in [
            ("glm-5.2", false),
            ("kimi-k2.7-code", false),
            ("minimax-m3", true),
        ] {
            let channel = channels
                .iter()
                .find(|c| c.model.as_deref() == Some(id))
                .unwrap_or_else(|| panic!("{id} served by opencode-go"));
            let want_anthropic = matches!(channel.transport, UserTransport::Anthropic);
            assert_eq!(want_anthropic, is_anthropic, "{id} transport");
        }
        // The seed set is exactly the served catalogue the registry knows.
        let mut expected: Vec<&str> = OPENCODE_GO_SERVED_MODELS.to_vec();
        expected.sort_unstable();
        let mut got = ids;
        got.sort_unstable();
        assert_eq!(got, expected);
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
        // An OpenAI-format model routes through the OpenAi transport.
        let glm = entry
            .channel_for_model("glm-5.2")
            .expect("glm-5.2 served by opencode-go");
        assert!(
            matches!(
                glm.transport,
                neenee_core::catalog::Transport::OpenAi { .. }
            ),
            "glm-5.2 must use OpenAi"
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
            .push(neenee_persistence::config::UserProviderConfig {
                id: "example".to_string(),
                name: Some("Example Claude".to_string()),
                channels: vec![neenee_persistence::config::UserChannelConfig {
                    label: "claude-sonnet-4-6".to_string(),
                    transport: neenee_persistence::config::UserTransport::Anthropic,
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
        use neenee_persistence::config::{UserChannelConfig, UserProviderConfig, UserTransport};
        let mut config = bare_config();
        config.providers.push(UserProviderConfig {
            id: "relay".to_string(),
            name: Some("Relay".to_string()),
            channels: vec![
                UserChannelConfig {
                    label: "alpha".to_string(),
                    transport: UserTransport::OpenAi,
                    model: Some("alpha".to_string()),
                    ..Default::default()
                },
                UserChannelConfig {
                    label: "beta".to_string(),
                    transport: UserTransport::OpenAi,
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
            resolved_model_name_with_usage(&config, "relay", &ProviderUsage::default()).as_deref(),
            Some("alpha")
        );

        // Record `beta` under `relay`: it becomes the resolved active model.
        let mut usage = ProviderUsage::default();
        usage.record_model("relay", "beta");
        assert_eq!(
            resolved_model_name_with_usage(&config, "relay", &usage).as_deref(),
            Some("beta")
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
    fn startup_model_recording_restores_boot_model_on_next_launch() {
        // Regression for "recently-used model not restored on startup". The
        // OAuth GPT (`chatgpt`) provider is multi-model: a user who boots into
        // a non-default model (e.g. selects `gpt-5.6-terra` while the catalog's
        // default channel is `gpt-5.6-sol`) must, on the *next* launch, reopen
        // on that same model. Restoration works only if startup records the
        // boot model via `record_model` — previously startup recorded only the
        // provider, leaving `last_models` empty, so the next launch fell back
        // to the default-channel model.
        //
        // Modeled here with a generic multi-model "relay" provider (two
        // channels, default channel = first = "alpha"), exactly mirroring the
        // `chatgpt` shape from `CHATGPT_BUILTIN_MODELS`.
        use neenee_persistence::config::{UserChannelConfig, UserProviderConfig, UserTransport};
        let mut config = bare_config();
        config.providers.push(UserProviderConfig {
            id: "relay".to_string(),
            name: Some("Relay".to_string()),
            channels: vec![
                UserChannelConfig {
                    label: "alpha".to_string(),
                    transport: UserTransport::OpenAi,
                    model: Some("alpha".to_string()),
                    ..Default::default()
                },
                UserChannelConfig {
                    label: "beta".to_string(),
                    transport: UserTransport::OpenAi,
                    model: Some("beta".to_string()),
                    ..Default::default()
                },
            ],
            default_channel: 0,
            ..Default::default()
        });
        config.default_provider = "relay".to_string();
        // Boot into the non-default-channel model "beta" (analogous to a
        // session pin or `default_model` selecting gpt-5.6-terra).
        config.default_model = Some("beta".to_string());

        // The model the startup provider is actually built with — same
        // config-only precedence `build_provider_for` uses, and what
        // `SessionDriver::run` now records via `record_model`.
        let boot_model = resolved_model_name(&config, "relay");
        assert_eq!(
            boot_model.as_deref(),
            Some("beta"),
            "boot resolves to the pinned model"
        );

        let mut usage = ProviderUsage::default();
        usage.record("relay");
        usage.record_model("relay", boot_model.as_deref().unwrap());

        // ── Next launch: a fresh session with no `default_model` pin. ──
        // (Session pins live in `SessionData`, not config.toml, so a fresh
        // session sees only the global default — here empty.) Restoration must
        // come from the recorded `last_models` entry, not the default channel.
        let mut next_config = config.clone();
        next_config.default_model = None;
        assert_eq!(
            resolved_model_name_with_usage(&next_config, "relay", &usage).as_deref(),
            Some("beta"),
            "next launch must reopen on the recorded boot model"
        );

        // Counter-assertion: the pre-fix behavior — recording only the
        // provider, never the model — leaves `last_models` empty, so the next
        // launch wrongly reopens on the default-channel model "alpha".
        let provider_only_usage = {
            let mut u = ProviderUsage::default();
            u.record("relay");
            u
        };
        assert_eq!(
            resolved_model_name_with_usage(&next_config, "relay", &provider_only_usage).as_deref(),
            Some("alpha"),
            "without record_model the default-channel model wins (the bug)"
        );
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
            resolved_model_name(&config, "anthropic").as_deref(),
            Some("claude-sonnet-4-6")
        );
        let provider =
            build_provider_for_model(&config, "anthropic", Some("claude-sonnet-4-6"), None)
                .expect("anthropic provider should build");
        assert_eq!(provider.model(), "claude-sonnet-4-6");
        assert_eq!(provider.provider_id(), "anthropic");
    }

    #[test]
    #[ignore = "legacy behavior: built-in providers are now user-added templates"]
    fn opencode_go_default_model_selects_its_channel() {
        let mut config = bare_config();
        config.default_model = Some("minimax-m3".to_string());
        // resolved_model_name honors default_model when the provider serves it.
        assert_eq!(
            resolved_model_name(&config, "opencode-go").as_deref(),
            Some("minimax-m3")
        );
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
        let provider = build_provider_for_model(&config, "opencode-go", Some("minimax-m3"), None)
            .expect("opencode-go minimax-m3 channel should build");
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
        // The Kimi Code platform pins the model id to k3.
        assert_eq!(channel.model, "k3", "model must be the pinned k3 alias");
        let (base_url, user_agent) = match &channel.transport {
            Transport::OpenAi {
                base_url,
                user_agent,
                ..
            } => (base_url.clone(), user_agent.clone()),
            other => panic!("kimi-code must be OpenAi, got {other:?}"),
        };
        assert_eq!(base_url, "https://api.kimi.com/coding/v1/chat/completions");
        // The preset borrows a recognized coding-agent UA as the zero-risk
        // default (the endpoint tolerates any UA under OAuth, untested for
        // API-key auth).
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
            Transport::Google { .. }
        ));
        // The built-in default base URL resolves to Google's official endpoint.
        if let Transport::Google { base_url, .. } = &entry.default_channel().unwrap().transport {
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
            Transport::OpenAi { base_url, .. } => {
                assert_eq!(base_url, "https://api.deepseek.com/v1/chat/completions");
            }
            other => panic!("deepseek must be OpenAi, got {other:?}"),
        }
    }

    #[test]
    fn resolved_model_name_falls_back_for_unknown_id() {
        assert!(resolved_model_name(&bare_config(), "nope").is_none());
    }

    #[test]
    fn build_provider_for_unknown_id_returns_none() {
        assert!(build_provider_for(&bare_config(), "does-not-exist").is_none());
    }

    #[test]
    fn split_deepseek_ids_no_longer_resolve_as_providers() {
        // The pre-merge provider ids are gone; only the merged `deepseek` id is a
        // provider now, so the old ids no longer resolve.
        assert!(build_provider_for(&bare_config(), "deepseek-v4-flash").is_none());
        assert!(build_provider_for(&bare_config(), "deepseek-v4-pro").is_none());
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
                    transport: UserTransport::Google,
                    api_key_env: Some("GEMINI_STUDIO_KEY".to_string()),
                    model: Some("gemini-2.5-flash".to_string()),
                    base_url: Some("https://relay.example.com/v1beta".to_string()),
                    ..Default::default()
                },
                UserChannelConfig {
                    label: "Relay".to_string(),
                    transport: UserTransport::OpenAi,
                    base_url: Some("https://relay.example.com/v1/chat/completions".to_string()),
                    api_key: Some("inline-key".into()),
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
                    transport: UserTransport::OpenAi,
                    api_key: Some("k".into()),
                    model: Some("gpt-5.5".to_string()),
                    ..Default::default()
                },
                UserChannelConfig {
                    label: "xhigh".to_string(),
                    transport: UserTransport::OpenAi,
                    api_key: Some("k".into()),
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
            Transport::OpenAi { effort, .. } => assert_eq!(*effort, Some(Effort::Xhigh)),
            other => panic!("expected OpenAi, got {other:?}"),
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
            Transport::Google { base_url, .. } => {
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
                transport: UserTransport::Google,
                api_key: Some("k".into()),
                model: Some("gemini-2.5-flash".to_string()),
                ..Default::default()
            }],
            default_channel: 0,
            ..Default::default()
        }];
        let entries = build_catalog(&config);
        let entry = entries.iter().find(|e| e.id == "gemini").unwrap();
        match &entry.default_channel().unwrap().transport {
            Transport::Google { base_url, .. } => {
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
            Transport::Google { base_url, .. } => {
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
                transport: UserTransport::OpenAi,
                base_url: Some("https://my.example.com/v1/chat/completions".to_string()),
                api_key: Some("k".into()),
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
            neenee_persistence::config::ModelSource::Api
        );
        let opencode_spec = provider_template_spec("opencode-go").expect("opencode-go template");
        assert_eq!(
            default_model_source_for_spec(opencode_spec),
            neenee_persistence::config::ModelSource::Fixed
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
        instance.model_source = neenee_persistence::config::ModelSource::Api;
        let chat_url = format!("{}/v1/chat/completions", server.url());
        for channel in &mut instance.channels {
            channel.base_url = Some(chat_url.clone());
            channel.api_key_env = Some("RELAY_API_KEY".to_string());
            channel.user_agent = Some("relay-client/1.0".to_string());
        }
        let mut config = bare_config();
        config.providers.push(instance);

        assert!(discover_provider_models(&mut config).await.changed);
        assert_eq!(config.providers[0].channel_models(), expected);
        assert!(config.providers[0].channels.iter().all(|channel| {
            channel.api_key.as_ref().map(SecretString::expose_secret) == Some("sk-test")
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
        instance.model_source = neenee_persistence::config::ModelSource::Api;
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

        assert!(!discover_provider_models(&mut config).await.changed);
        assert_eq!(config.providers[0].channel_models(), before);
        assert!(config.providers[0].channels.iter().all(|channel| {
            channel.api_key.as_ref().map(SecretString::expose_secret) == Some("sk-test")
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
        instance.model_source = neenee_persistence::config::ModelSource::Fixed;
        config.providers.push(instance);
        let before: Vec<String> = config.providers[0]
            .channel_models()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let changed = discover_provider_models(&mut config).await.changed;

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
        instance.model_source = neenee_persistence::config::ModelSource::Api;
        config.providers.push(instance);
        let before: Vec<String> = config.providers[0]
            .channel_models()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let changed = discover_provider_models(&mut config).await.changed;

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
        instance.model_source = neenee_persistence::config::ModelSource::Api;
        config.providers.push(instance);
        let before: Vec<String> = config.providers[0]
            .channel_models()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let changed = discover_provider_models(&mut config).await.changed;

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
        instance.model_source = neenee_persistence::config::ModelSource::Api;
        config.providers.push(instance);
        let before: Vec<String> = config.providers[0]
            .channel_models()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let changed = discover_provider_models(&mut config).await.changed;

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
                    transport: UserTransport::OpenAi,
                    api_key_env: None,
                    api_key: Some("sk".into()),
                    model: Some(m.clone()),
                    base_url: Some("https://relay.example.com".to_string()),
                    user_agent: None,
                    effort: None,
                    thinking: None,
                    auth: Default::default(),
                    remote: None,
                })
                .collect(),
            default_channel: 0,
            template_id: None,
            model_source: Default::default(),
            fitted_models: Default::default(),
        });

        assert!(reconcile_provider_models(&mut config));
        assert_eq!(
            config.providers[0].template_id.as_deref(),
            Some("openai-sub2api")
        );
        assert_eq!(
            config.providers[0].model_source,
            neenee_persistence::config::ModelSource::Api,
            "backfilled discovery-template instance adopts Api source"
        );
    }

    #[test]
    fn reconcile_backfill_sets_fixed_model_source_for_nondiscovery_template() {
        // A legacy instance that exactly matches a discovery-disabled template
        // (zai-code) gets stamped but keeps model_source=Fixed.
        let models = current_template_models("zai-code");
        let mut config = bare_config();
        config.providers.push(UserProviderConfig {
            id: "zai".to_string(),
            name: Some("ZAI".to_string()),
            channels: models
                .iter()
                .map(|m| UserChannelConfig {
                    label: m.clone(),
                    transport: UserTransport::OpenAi,
                    api_key_env: None,
                    api_key: Some("sk".into()),
                    model: Some(m.clone()),
                    base_url: Some("https://zai.example.com".to_string()),
                    user_agent: None,
                    effort: None,
                    thinking: None,
                    auth: Default::default(),
                    remote: None,
                })
                .collect(),
            default_channel: 0,
            template_id: None,
            model_source: Default::default(),
            fitted_models: Default::default(),
        });

        assert!(reconcile_provider_models(&mut config));
        assert_eq!(config.providers[0].template_id.as_deref(), Some("zai-code"));
        assert_eq!(
            config.providers[0].model_source,
            neenee_persistence::config::ModelSource::Fixed,
            "backfilled nondiscovery-template instance keeps Fixed source"
        );
    }

    #[test]
    fn reconcile_upgrades_fixed_to_api_for_fitting_templates() {
        // kimi-code gained discovery+fitting after existing instances had been
        // stamped Fixed by the backfill — that Fixed was never a deliberate
        // opt-out (the template offered no Api source at the time), so the
        // instance follows the template to Api and starts live discovery.
        let mut config = bare_config();
        let mut instance = template_instance("kimi-code", KIMI_CODE_MODELS);
        instance.model_source = neenee_persistence::config::ModelSource::Fixed;
        config.providers.push(instance);

        assert!(reconcile_provider_models(&mut config));
        assert_eq!(
            config.providers[0].model_source,
            neenee_persistence::config::ModelSource::Api,
            "Fixed instance of a fitting template upgrades to Api"
        );
        // The upgrade itself does not touch the channel set.
        assert_eq!(
            config.providers[0].channel_models(),
            KIMI_CODE_MODELS.to_vec()
        );
    }

    #[test]
    fn reconcile_api_instance_retains_fitted_channels() {
        // An Api instance whose channels came from a previous live fetch keeps
        // its fitted ids across reconciles — intersecting against the static
        // registry alone would drop them and undo the fitting.
        let mut config = bare_config();
        let mut instance = template_instance("kimi-code", &["k3", "kimi-for-coding"]);
        instance.model_source = neenee_persistence::config::ModelSource::Api;
        instance.fitted_models.insert(
            "kimi-for-coding".to_string(),
            FittedModelInfo {
                context_window: 262_144,
                reasoning: true,
                vision: true,
                efforts: Vec::new(),
                display_name: None,
            },
        );
        config.providers.push(instance);

        // The channel set already equals the retainable set → no-op.
        assert!(!reconcile_provider_models(&mut config));
        assert_eq!(
            config.providers[0].channel_models(),
            vec!["k3".to_string(), "kimi-for-coding".to_string()]
        );
    }

    #[tokio::test]
    async fn discover_fitting_template_materializes_and_fits_advertised_models() {
        // Recorded 2026-07 from GET https://api.kimi.com/coding/v1/models.
        let body = r#"{"data":[
            {"id":"kimi-for-coding","created":1761264000,"created_at":"2025-10-24T00:00:00Z","object":"model","display_name":"kimi-for-coding","type":"model","context_length":262144,"supports_reasoning":true,"supports_image_in":true,"supports_video_in":true,"supports_thinking_type":"only"},
            {"id":"kimi-for-coding-highspeed","created":1761264000,"created_at":"2025-10-24T00:00:00Z","object":"model","display_name":"kimi-for-coding-highspeed","type":"model","context_length":262144,"supports_reasoning":true,"supports_image_in":true,"supports_video_in":true,"supports_thinking_type":"only"},
            {"id":"k3","created":1761264000,"created_at":"2025-10-24T00:00:00Z","object":"model","display_name":"k3","type":"model","context_length":1048576,"supports_reasoning":true,"supports_image_in":true,"supports_video_in":true,"supports_thinking_type":"only","think_efforts":{"support":true,"valid_efforts":["max"],"default_effort":"max"}}
        ],"object":"list","first_id":"kimi-for-coding","last_id":"k3","has_more":false}"#;
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/v1/models")
            .match_header("authorization", "Bearer sk-test")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let mut instance = template_instance("kimi-code", KIMI_CODE_MODELS);
        instance.model_source = neenee_persistence::config::ModelSource::Api;
        let chat_url = format!("{}/v1/chat/completions", server.url());
        for channel in &mut instance.channels {
            channel.base_url = Some(chat_url.clone());
        }
        let mut config = bare_config();
        config.providers.push(instance);

        assert!(discover_provider_models(&mut config).await.changed);
        // Every advertised id is materialized (sorted by id), including the
        // platform-native ids the static registry does not know.
        assert_eq!(
            config.providers[0].channel_models(),
            vec![
                "k3".to_string(),
                "kimi-for-coding".to_string(),
                "kimi-for-coding-highspeed".to_string()
            ]
        );
        // Fitted metadata is persisted only for registry-unknown ids — k3 is
        // registered, so its vetted static entry stays authoritative.
        let fitted = &config.providers[0].fitted_models;
        assert!(!fitted.contains_key("k3"));
        let kimi_for_coding = &fitted["kimi-for-coding"];
        assert_eq!(kimi_for_coding.context_window, 262_144);
        assert!(kimi_for_coding.reasoning);
        assert!(kimi_for_coding.vision);
        assert_eq!(
            fitted["kimi-for-coding-highspeed"].display_name.as_deref(),
            Some("kimi-for-coding-highspeed")
        );
    }

    #[tokio::test]
    async fn discover_copilot_uses_remote_picker_models_and_persists_routes() {
        let body = r#"{"data":[
            {
                "id":"gpt-5",
                "name":"GPT-5",
                "model_picker_enabled":true,
                "supported_endpoints":["/responses"],
                "capabilities":{
                    "type":"chat",
                    "family":"gpt-5",
                    "limits":{"max_context_window_tokens":200000,"max_output_tokens":16384},
                    "supports":{"tool_calls":true,"vision":true,"reasoning_effort":["low","high"]}
                }
            },
            {
                "id":"claude-opus-4.7",
                "name":"Claude Opus 4.7",
                "model_picker_enabled":true,
                "supported_endpoints":["/v1/messages"],
                "capabilities":{
                    "type":"chat",
                    "family":"claude-opus",
                    "limits":{"max_context_window_tokens":144000,"max_output_tokens":64000},
                    "supports":{"adaptive_thinking":true,"tool_calls":true,"vision":true}
                }
            },
            {
                "id":"internal-title",
                "name":"Internal title model",
                "model_picker_enabled":false,
                "supported_endpoints":["/responses"],
                "capabilities":{
                    "type":"chat",
                    "family":"internal",
                    "limits":{"max_output_tokens":1024},
                    "supports":{"tool_calls":false}
                }
            }
        ]}"#;
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/models")
            .match_header("authorization", "Bearer copilot-token")
            .match_header("copilot-integration-id", "vscode-chat")
            .match_header("x-initiator", "user")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let mut instance = template_instance("copilot-oauth", &["gpt-4o-mini"]);
        instance.model_source = ModelSource::Api;
        for channel in &mut instance.channels {
            channel.api_key = Some("copilot-token".into());
            channel.base_url = Some(format!("{}/chat/completions", server.url()));
        }
        let mut config = bare_config();
        config.providers.push(instance);

        let changed = discover_provider_models(&mut config).await.changed;

        assert!(changed);
        assert_eq!(
            config.providers[0].channel_models(),
            vec!["claude-opus-4.7".to_string(), "gpt-5".to_string()]
        );
        let gpt = config.providers[0]
            .channels
            .iter()
            .find(|channel| channel.model.as_deref() == Some("gpt-5"))
            .unwrap();
        assert_eq!(
            gpt.remote.as_ref().and_then(|remote| remote.endpoint),
            Some(RemoteModelEndpoint::Responses)
        );
        let claude = config.providers[0]
            .channels
            .iter()
            .find(|channel| channel.model.as_deref() == Some("claude-opus-4.7"))
            .unwrap();
        assert_eq!(
            claude.remote.as_ref().and_then(|remote| remote.endpoint),
            Some(RemoteModelEndpoint::Messages)
        );
    }

    #[test]
    fn sync_fitted_model_registry_populates_the_resolution_overlay() {
        let mut config = bare_config();
        let mut instance = template_instance("kimi-code", &["k3", "fitted-sync-k9"]);
        instance.fitted_models.insert(
            "fitted-sync-k9".to_string(),
            FittedModelInfo {
                context_window: 512_000,
                reasoning: true,
                vision: true,
                // Unsorted: the overlay stores levels ascending.
                efforts: vec!["high".to_string(), "low".to_string()],
                display_name: Some("Sync K9".to_string()),
            },
        );
        config.providers.push(instance);

        sync_fitted_model_registry(&config);

        let model = neenee_core::model::resolve("fitted-sync-k9");
        assert_eq!(model.name, "Sync K9");
        assert_eq!(model.family, "kimi-code");
        assert_eq!(model.context_window, 512_000);
        assert!(model.reasoning());
        assert!(model.vision);
        assert_eq!(model.effort_levels, &[Effort::Low, Effort::High]);
    }

    #[test]
    fn copilot_remote_endpoint_selects_the_advertised_transport() {
        use neenee_core::{RemoteModelMetadata, ThinkingSupport};

        let base = UserChannelConfig {
            label: "remote-model".to_string(),
            model: Some("remote-model".to_string()),
            auth: neenee_core::ChannelAuth::CopilotOAuth,
            remote: Some(RemoteModelMetadata {
                endpoint: Some(RemoteModelEndpoint::Messages),
                max_output_tokens: Some(64_000),
                thinking: Some(ThinkingSupport::AnthropicAdaptive),
                ..Default::default()
            }),
            ..Default::default()
        };
        let messages = user_channel_to_channel(&base, "remote-model");
        assert!(matches!(
            messages.transport,
            Transport::Anthropic { copilot: true, .. }
        ));

        let mut responses = base.clone();
        responses.remote.as_mut().unwrap().endpoint = Some(RemoteModelEndpoint::Responses);
        let responses = user_channel_to_channel(&responses, "remote-model");
        assert!(matches!(
            responses.transport,
            Transport::OpenAiResponses { copilot: true, .. }
        ));

        let mut chat = base;
        chat.remote.as_mut().unwrap().endpoint = Some(RemoteModelEndpoint::ChatCompletions);
        let chat = user_channel_to_channel(&chat, "remote-model");
        assert!(matches!(
            chat.transport,
            Transport::OpenAi { copilot: true, .. }
        ));
    }

    #[test]
    fn trusted_remote_metadata_is_persisted_only_for_picker_models() {
        let mut provider = template_instance("copilot-oauth", &["gpt-5", "internal-title"]);
        let discovered = vec![
            neenee_providers::DiscoveredModel {
                id: "gpt-5".to_string(),
                picker_enabled: Some(true),
                endpoint: Some(RemoteModelEndpoint::Responses),
                family: Some("gpt-5".to_string()),
                context_window: Some(200_000),
                max_output_tokens: Some(16_384),
                reasoning: Some(true),
                thinking: Some(neenee_core::ThinkingSupport::ReasoningSummary),
                tool_call: Some(true),
                vision: Some(true),
                effort_levels: Some(vec!["low".to_string(), "high".to_string()]),
                display_name: Some("GPT-5".to_string()),
            },
            neenee_providers::DiscoveredModel {
                id: "internal-title".to_string(),
                picker_enabled: Some(false),
                endpoint: Some(RemoteModelEndpoint::Responses),
                ..Default::default()
            },
        ];

        assert!(persist_remote_model_metadata(
            &mut provider,
            &discovered,
            true
        ));
        let gpt5 = provider
            .channels
            .iter()
            .find(|channel| channel.model.as_deref() == Some("gpt-5"))
            .unwrap();
        assert_eq!(
            gpt5.remote.as_ref().and_then(|remote| remote.endpoint),
            Some(RemoteModelEndpoint::Responses)
        );
        let internal = provider
            .channels
            .iter()
            .find(|channel| channel.model.as_deref() == Some("internal-title"))
            .unwrap();
        assert!(internal.remote.is_none());
    }
}
