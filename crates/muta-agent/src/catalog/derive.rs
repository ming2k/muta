//! Runtime derivation of connection entries — channels are derived, never
//! persisted.
//!
//! A connection declares *who* it connects to (preset + credential + client identity + optional
//! overrides). This module derives the concrete routes — one per model, each
//! with its transport/endpoint/credential/reasoning — from that declaration
//! plus the preset registry and the discovery cache. Nothing here is written
//! back; the stores stay the single source of truth and two connections of the
//! same preset never duplicate or drift a route set.
//!
//! Resolution precedence is the single source of truth shared by startup and
//! runtime switching (ADR-0002): env var (`api_key_env`) →
//! `credentials.toml` → empty. OAuth connections resolve their bearer from
//! `auth.toml` (refreshed by the runtime before building).

use muta_contracts::catalog::{Channel, ProviderEntry, Transport};
use muta_contracts::{ChannelAuth, Effort, RemoteModelEndpoint, SecretString, ThinkingMode};
use muta_persistence::config::{Credentials, DiscoveryCache, UserTransport};
use muta_persistence::connections::{Connection, Connections};
use muta_persistence::route_settings::RouteSettingsStore;
use muta_providers::{MUTA_USER_AGENT, provider_preset_spec, route_for_model as template_route};

pub(super) const CHATGPT_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// Derive every entry from the connections store, in declaration order.
pub fn derive_entries(
    connections: &Connections,
    cache: &DiscoveryCache,
    routes: &RouteSettingsStore,
    creds: &Credentials,
) -> Vec<ProviderEntry> {
    connections
        .connections
        .iter()
        .map(|conn| derive_entry(conn, cache, routes, creds))
        .collect()
}

/// Derive one entry from one connection.
pub fn derive_entry(
    connection: &Connection,
    cache: &DiscoveryCache,
    routes: &RouteSettingsStore,
    creds: &Credentials,
) -> ProviderEntry {
    let models = route_models(connection, cache);
    let channels = models
        .iter()
        .map(|model| derive_channel(connection, model, cache, routes, creds))
        .collect();
    ProviderEntry {
        id: connection.id.clone(),
        name: connection.display_name().to_string(),
        description: String::new(),
        channels,
        default_channel: 0,
        builtin: false,
    }
}

/// The model ids a connection serves, in picker order. Preset connections are
/// derived from the preset (live-discovered lists when the preset supports
/// discovery, else the compiled-in snapshot); pure-custom connections serve the
/// declared `models`.
pub fn route_models(connection: &Connection, cache: &DiscoveryCache) -> Vec<String> {
    if let Some(pid) = connection.preset_id.as_deref()
        && let Some(spec) = provider_preset_spec(pid)
    {
        if spec.discovery {
            // Prefer the last successful live list (already intersected /
            // fitted by discovery); fall back to the preset snapshot.
            if let Some(discovered) = cache.connection_models.get(&connection.id)
                && !discovered.is_empty()
            {
                return discovered.clone();
            }
        }
        return spec.models.iter().map(|m| (*m).to_string()).collect();
    }
    connection.models.clone()
}

/// Resolve one model's route: transport/endpoint/user-agent plus the resolved
/// credential, reasoning knobs, and remote capability metadata.
pub fn derive_channel(
    connection: &Connection,
    model: &str,
    cache: &DiscoveryCache,
    routes: &RouteSettingsStore,
    creds: &Credentials,
) -> Channel {
    let remote = cache.remote_metadata_for(&connection.id, model).cloned();
    let route_settings = routes.settings_for(&connection.id, model);

    // Effort applies to OpenAI/Anthropic/Google alike; thinking is an
    // Anthropic-protocol switch.
    let effort = route_settings
        .and_then(|r| r.effort.as_deref())
        .and_then(Effort::parse);
    let thinking = route_settings.map(|r| match r.thinking {
        Some(false) => ThinkingMode::Off,
        _ => ThinkingMode::Adaptive,
    });

    let api_key = resolve_credential(connection, creds);
    let oauth = oauth_ids(connection);

    let transport = match connection.auth {
        ChannelAuth::ChatGptOAuth => Transport::OpenAiResponses {
            base_url: connection
                .base_url
                .clone()
                .unwrap_or_else(|| CHATGPT_RESPONSES_URL.to_string()),
            user_agent: connection
                .user_agent
                .clone()
                .unwrap_or_else(|| connection.client_identity.user_agent().to_string()),
            effort,
            account_id: oauth.0,
            copilot: false,
        },
        ChannelAuth::CopilotOAuth => copilot_route(connection, remote.as_ref(), effort, thinking),
        ChannelAuth::AntigravityOAuth => Transport::Google {
            base_url: connection
                .base_url
                .clone()
                .unwrap_or_else(|| "https://daily-cloudcode-pa.googleapis.com".to_string()),
            user_agent: connection.user_agent.clone().unwrap_or_else(|| {
                if connection.client_identity != muta_contracts::ClientIdentity::Native {
                    connection.client_identity.user_agent().to_string()
                } else {
                    muta_contracts::client_identity::ANTIGRAVITY_USER_AGENT.to_string()
                }
            }),
            effort,
            project_id: oauth.1,
        },
        _ => {
            let (transport, base_url, user_agent) = base_route(connection, model);
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
        user_overrides: route_settings
            .and_then(|r| r.capability_overrides.clone())
            .filter(|o| !o.is_empty()),
    }
}

/// The base transport for a non-OAuth connection: preset route (always derived
/// from the hardcoded preset spec) or the pure-custom declaration.
fn base_route(connection: &Connection, model: &str) -> (UserTransport, String, String) {
    if let Some(pid) = connection.preset_id.as_deref() {
        let (protocol, base_url, tpl_ua) =
            template_route(pid, model).unwrap_or(("openai", "", None));
        let user_agent = connection
            .user_agent
            .clone()
            .or_else(|| {
                if connection.client_identity != muta_contracts::ClientIdentity::Native {
                    Some(connection.client_identity.user_agent().to_string())
                } else {
                    tpl_ua.map(str::to_string)
                }
            })
            .unwrap_or_else(|| connection.client_identity.user_agent().to_string());
        (
            transport_for_protocol(protocol),
            base_url.to_string(),
            user_agent,
        )
    } else {
        let transport = connection.transport.unwrap_or(UserTransport::OpenAi);
        let base_url = connection
            .base_url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| default_endpoint(transport));
        let user_agent = connection
            .user_agent
            .clone()
            .unwrap_or_else(|| connection.client_identity.user_agent().to_string());
        (transport, base_url, user_agent)
    }
}

/// Copilot OAuth routes select their wire family from the model's advertised
/// endpoint (which varies by plan and model), falling back to chat-completions.
fn copilot_route(
    connection: &Connection,
    remote: Option<&muta_contracts::RemoteModelMetadata>,
    effort: Option<Effort>,
    thinking: Option<ThinkingMode>,
) -> Transport {
    let ua = || {
        connection
            .user_agent
            .clone()
            .unwrap_or_else(|| MUTA_USER_AGENT.to_string())
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
            base_url: "https://api.githubcopilot.com/chat/completions".to_string(),
            user_agent: ua(),
            effort,
            copilot: true,
        },
    }
}

/// The `(account_id, project_id)` claims an OAuth connection's token carries, if
/// it is currently logged in. Empty for API-key connections.
fn oauth_ids(connection: &Connection) -> (Option<String>, Option<String>) {
    if !connection.auth.is_oauth() {
        return (None, None);
    }
    let store = muta_providers::oauth::AuthStore::load();
    store
        .get_for_provider(
            &connection.id,
            connection.preset_id.as_deref(),
            connection.auth,
        )
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

/// A transport's default endpoint when a custom connection omits one.
pub fn default_endpoint(transport: UserTransport) -> String {
    match transport {
        UserTransport::Google => "http://localhost:8080/v1beta".to_string(),
        UserTransport::Anthropic => "http://localhost:8080/v1/messages".to_string(),
        UserTransport::OpenAiResponses => "http://localhost:8080/v1/responses".to_string(),
        UserTransport::OpenAi => "http://localhost:8080/v1/chat/completions".to_string(),
    }
}

/// The resolved credential for a connection: env var (`api_key_env`) →
/// `credentials.toml` → empty. OAuth connections resolve their live access token
/// from `auth.toml` instead (refreshed by the runtime before building).
pub fn resolve_credential(connection: &Connection, creds: &Credentials) -> SecretString {
    if connection.auth.is_oauth() {
        let store = muta_providers::oauth::AuthStore::load();
        let tokens = store.get_for_provider(
            &connection.id,
            connection.preset_id.as_deref(),
            connection.auth,
        );
        return tokens.map(|t| t.access.clone()).unwrap_or_default();
    }
    if let Some(env) = connection.api_key_env.as_deref()
        && let Ok(value) = std::env::var(env)
        && !value.trim().is_empty()
    {
        return SecretString::from(value);
    }
    creds.api_key(&connection.id).cloned().unwrap_or_default()
}
