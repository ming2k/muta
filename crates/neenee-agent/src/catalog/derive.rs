//! Runtime derivation of provider entries — channels are derived, never
//! persisted.
//!
//! An instance declares *who* it connects to (template + credential + optional
//! overrides). This module derives the concrete routes — one per model, each
//! with its transport/endpoint/credential/reasoning — from that declaration
//! plus the template registry and the discovery cache. Nothing here is written
//! back; the stores stay the single source of truth and two instances of the
//! same template never duplicate or drift a route set.
//!
//! Resolution precedence is the single source of truth shared by startup and
//! runtime provider switching (ADR-0002): env var (`api_key_env`) →
//! `credentials.toml` → empty. OAuth instances resolve their bearer from
//! `auth.toml` (refreshed by the runtime before building).

use neenee_contracts::catalog::{Channel, ProviderEntry, Transport};
use neenee_contracts::{ChannelAuth, Effort, RemoteModelEndpoint, SecretString, ThinkingMode};
use neenee_persistence::config::{Credentials, DiscoveryCache, UserTransport};
use neenee_persistence::instances::{Instances, ProviderInstance};
use neenee_persistence::route_settings::RouteSettingsStore;
use neenee_providers::{
    NEENEE_USER_AGENT, provider_template_spec, route_for_model as template_route,
};

pub(super) const CHATGPT_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// Derive every entry from the instance store, in declaration order.
pub fn derive_entries(
    instances: &Instances,
    cache: &DiscoveryCache,
    routes: &RouteSettingsStore,
    creds: &Credentials,
) -> Vec<ProviderEntry> {
    instances
        .providers
        .iter()
        .map(|instance| derive_entry(instance, cache, routes, creds))
        .collect()
}

/// Derive one entry from one instance.
pub fn derive_entry(
    instance: &ProviderInstance,
    cache: &DiscoveryCache,
    routes: &RouteSettingsStore,
    creds: &Credentials,
) -> ProviderEntry {
    let models = route_models(instance, cache);
    let channels = models
        .iter()
        .map(|model| derive_channel(instance, model, cache, routes, creds))
        .collect();
    ProviderEntry {
        id: instance.id.clone(),
        name: instance.display_name().to_string(),
        description: String::new(),
        channels,
        default_channel: 0,
        // Every instance is user-managed through the Connections surface
        // (including template-created ones), so none is a locked preset.
        builtin: false,
    }
}

/// The model ids an instance serves, in picker order. Template instances are
/// derived from the template (live-discovered lists when the template supports
/// discovery, else the compiled-in snapshot); pure-custom instances serve the
/// declared `models`.
pub fn route_models(instance: &ProviderInstance, cache: &DiscoveryCache) -> Vec<String> {
    if let Some(tid) = instance.template_id.as_deref()
        && let Some(spec) = provider_template_spec(tid)
    {
        if spec.discovery {
            // Prefer the last successful live list (already intersected /
            // fitted by discovery); fall back to the template snapshot.
            if let Some(discovered) = cache.provider_models.get(&instance.id)
                && !discovered.is_empty()
            {
                return discovered.clone();
            }
        }
        return spec.models.iter().map(|m| (*m).to_string()).collect();
        // Unknown template id: nothing to derive from — fall through to the
        // declared models so the instance stays usable.
    }
    instance.models.clone()
}

/// Resolve one model's route: transport/endpoint/user-agent plus the resolved
/// credential, reasoning knobs, and remote capability metadata. The reasoning
/// knobs come from `routes` — the user's state store, not the cache
/// (see [`RouteSettingsStore`]).
pub fn derive_channel(
    instance: &ProviderInstance,
    model: &str,
    cache: &DiscoveryCache,
    routes: &RouteSettingsStore,
    creds: &Credentials,
) -> Channel {
    let remote = cache.remote_metadata_for(&instance.id, model).cloned();
    let route_settings = routes.settings_for(&instance.id, model);

    // Effort applies to OpenAI/Anthropic/Google alike; thinking is an
    // Anthropic-protocol switch (ADR-0046: entry presence opts in, default on
    // unless explicitly off).
    let effort = route_settings
        .and_then(|r| r.effort.as_deref())
        .and_then(Effort::parse);
    let thinking = route_settings.map(|r| match r.thinking {
        Some(false) => ThinkingMode::Off,
        _ => ThinkingMode::Adaptive,
    });

    let api_key = resolve_credential(instance, creds);
    let oauth = oauth_ids(instance);

    let transport = match instance.auth {
        ChannelAuth::ChatGptOAuth => Transport::OpenAiResponses {
            base_url: instance
                .base_url
                .clone()
                .unwrap_or_else(|| CHATGPT_RESPONSES_URL.to_string()),
            user_agent: instance
                .user_agent
                .clone()
                .unwrap_or_else(|| NEENEE_USER_AGENT.to_string()),
            effort,
            account_id: oauth.0,
            copilot: false,
        },
        ChannelAuth::CopilotOAuth => copilot_route(instance, remote.as_ref(), effort, thinking),
        ChannelAuth::AntigravityOAuth => Transport::Google {
            base_url: instance
                .base_url
                .clone()
                .unwrap_or_else(|| "https://cloudcode-pa.googleapis.com".to_string()),
            user_agent: instance
                .user_agent
                .clone()
                .unwrap_or_else(|| "antigravity/1.23.2 windows/amd64".to_string()),
            effort,
            project_id: oauth.1,
        },
        _ => {
            let (transport, base_url, user_agent) = base_route(instance, model);
            match transport {
                UserTransport::Google => Transport::Google {
                    base_url,
                    user_agent,
                    effort,
                    project_id: None,
                },
                UserTransport::Anthropic => Transport::Anthropic {
                    base_url,
                    user_agent,
                    effort,
                    thinking,
                    copilot: false,
                },
                UserTransport::OpenAiResponses => Transport::OpenAiResponses {
                    base_url,
                    user_agent,
                    effort,
                    account_id: None,
                    copilot: false,
                },
                UserTransport::OpenAi => Transport::OpenAi {
                    base_url,
                    user_agent,
                    effort,
                    copilot: false,
                },
            }
        }
    };

    Channel {
        id: model.to_string(),
        label: model.to_string(),
        transport,
        api_key,
        model: model.to_string(),
        remote,
    }
}

/// The base transport for a non-OAuth instance: template route (with
/// instance-level overrides) or the pure-custom declaration.
fn base_route(instance: &ProviderInstance, model: &str) -> (UserTransport, String, String) {
    if let Some(tid) = instance.template_id.as_deref() {
        let (protocol, base_url, tpl_ua) =
            template_route(tid, model).unwrap_or(("openai", "", None));
        let base_url = instance
            .base_url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| base_url.to_string());
        let user_agent = instance
            .user_agent
            .clone()
            .or_else(|| tpl_ua.map(str::to_string))
            .unwrap_or_else(|| NEENEE_USER_AGENT.to_string());
        (transport_for_protocol(protocol), base_url, user_agent)
    } else {
        let transport = instance.transport.unwrap_or(UserTransport::OpenAi);
        let base_url = instance
            .base_url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| default_endpoint(transport));
        let user_agent = instance
            .user_agent
            .clone()
            .unwrap_or_else(|| NEENEE_USER_AGENT.to_string());
        (transport, base_url, user_agent)
    }
}

/// Copilot OAuth routes select their wire family from the model's advertised
/// endpoint (which varies by plan and model), falling back to chat-completions.
fn copilot_route(
    instance: &ProviderInstance,
    remote: Option<&neenee_contracts::RemoteModelMetadata>,
    effort: Option<Effort>,
    thinking: Option<ThinkingMode>,
) -> Transport {
    let ua = || {
        instance
            .user_agent
            .clone()
            .unwrap_or_else(|| NEENEE_USER_AGENT.to_string())
    };
    match remote.and_then(|r| r.endpoint) {
        Some(RemoteModelEndpoint::Responses) => Transport::OpenAiResponses {
            base_url: "https://api.githubcopilot.com/responses".to_string(),
            user_agent: ua(),
            effort,
            account_id: None,
            copilot: true,
        },
        Some(RemoteModelEndpoint::Messages) => Transport::Anthropic {
            base_url: "https://api.githubcopilot.com/v1/messages".to_string(),
            user_agent: ua(),
            effort,
            thinking,
            copilot: true,
        },
        Some(RemoteModelEndpoint::ChatCompletions) | None => Transport::OpenAi {
            base_url: instance
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.githubcopilot.com/chat/completions".to_string()),
            user_agent: ua(),
            effort,
            copilot: true,
        },
    }
}

/// The `(account_id, project_id)` claims an OAuth instance's token carries, if
/// it is currently logged in. Empty for API-key instances.
fn oauth_ids(instance: &ProviderInstance) -> (Option<String>, Option<String>) {
    if !instance.auth.is_oauth() {
        return (None, None);
    }
    let store = neenee_providers::oauth::AuthStore::load();
    store
        .get_for_provider(&instance.id, instance.template_id.as_deref(), instance.auth)
        .map(|t| {
            (
                t.account_id.clone(),
                t.project_id.clone().or_else(|| t.account_id.clone()),
            )
        })
        .unwrap_or_default()
}

/// Map a wire-protocol label to the persisted transport enum.
pub fn transport_for_protocol(protocol: &str) -> UserTransport {
    match protocol {
        "anthropic" => UserTransport::Anthropic,
        "google" | "gemini" => UserTransport::Google,
        "openai-responses" => UserTransport::OpenAiResponses,
        _ => UserTransport::OpenAi,
    }
}

/// A transport's default endpoint when a custom instance omits one (legacy
/// local defaults).
pub fn default_endpoint(transport: UserTransport) -> String {
    match transport {
        UserTransport::Google => "http://localhost:8080/v1beta".to_string(),
        UserTransport::Anthropic => "http://localhost:8080/v1/messages".to_string(),
        UserTransport::OpenAiResponses => "http://localhost:8080/v1/responses".to_string(),
        UserTransport::OpenAi => "http://localhost:8080/v1/chat/completions".to_string(),
    }
}

/// The resolved credential for an instance: env var (`api_key_env`) →
/// `credentials.toml` → empty. OAuth instances resolve their live access token
/// from `auth.toml` instead (refreshed by the runtime before building).
pub fn resolve_credential(instance: &ProviderInstance, creds: &Credentials) -> SecretString {
    if instance.auth.is_oauth() {
        let store = neenee_providers::oauth::AuthStore::load();
        let tokens =
            store.get_for_provider(&instance.id, instance.template_id.as_deref(), instance.auth);
        return tokens.map(|t| t.access.clone()).unwrap_or_default();
    }
    if let Some(env) = instance.api_key_env.as_deref()
        && let Ok(value) = std::env::var(env)
        && !value.trim().is_empty()
    {
        return SecretString::from(value);
    }
    creds.api_key(&instance.id).cloned().unwrap_or_default()
}
