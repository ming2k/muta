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
//! `auth.toml` through a dynamic, per-connection credential source.

use muta_contracts::catalog::{Channel, ProviderEntry, Transport};
use muta_contracts::{
    AnthropicMessagesDialect, ClientProfile, ConnectionAuth, Effort, GoogleGenerateContentDialect,
    OpenAiChatDialect, OpenAiResponsesDialect, SecretString, ThinkingMode, WireProtocol,
};
use muta_persistence::config::{Credentials, DiscoveryCache};
use muta_persistence::connections::{Connection, Connections};
use muta_persistence::route_settings::RouteSettingsStore;
use muta_providers::{provider_preset_spec, route_for_model as preset_route};

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
    if let Some(pid) = connection.preset_id.as_deref() {
        let Some(spec) = provider_preset_spec(pid) else {
            return Vec::new();
        };
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
    let prompt_cache = connection
        .preset_id
        .as_deref()
        .and_then(provider_preset_spec)
        .map(|preset| (preset.prompt_cache)(model).materialize())
        .unwrap_or_else(muta_contracts::PromptCacheCapabilities::unsupported);
    let prompt_cache_preference = route_settings
        .and_then(|settings| settings.prompt_cache)
        .unwrap_or_default();

    // Effort applies to OpenAI/Anthropic/Google alike; thinking is an
    // Anthropic-protocol switch.
    let effort = route_settings
        .and_then(|r| r.effort.as_deref())
        .and_then(Effort::parse);
    let thinking = route_settings.map(|r| match r.thinking {
        Some(false) => ThinkingMode::Off,
        _ => ThinkingMode::Adaptive,
    });

    let credentials: std::sync::Arc<dyn muta_contracts::CredentialSource> =
        if connection.auth.is_oauth() {
            std::sync::Arc::new(muta_providers::oauth::OAuthCredentialSource::new(
                &connection.id,
                connection.auth,
            ))
        } else {
            let api_key = resolve_credential(connection, creds);
            muta_contracts::static_credential(api_key)
        };

    let transport = match connection.auth {
        ConnectionAuth::ChatGptOAuth => {
            let client_profile = if let Some(ua) = connection.user_agent.as_deref() {
                ClientProfile::from_user_agent(ua)
            } else if connection.client_identity != ClientProfile::Native {
                connection.client_identity.clone()
            } else {
                ClientProfile::Native
            };
            Transport::OpenAiResponses {
                base_url: connection
                    .base_url
                    .clone()
                    .unwrap_or_else(|| CHATGPT_RESPONSES_URL.to_string()),
                client_profile,
                effort,
                dialect: OpenAiResponsesDialect::ChatGpt,
            }
        }
        ConnectionAuth::CopilotOAuth => {
            copilot_route(connection, remote.as_ref(), effort, thinking)
        }
        ConnectionAuth::AntigravityOAuth => {
            let client_profile = if let Some(ua) = connection.user_agent.as_deref() {
                ClientProfile::from_user_agent(ua)
            } else if connection.client_identity != ClientProfile::Native {
                connection.client_identity.clone()
            } else {
                ClientProfile::Antigravity
            };
            Transport::Google {
                base_url: connection
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://daily-cloudcode-pa.googleapis.com".to_string()),
                client_profile,
                effort,
                dialect: GoogleGenerateContentDialect::Antigravity,
            }
        }
        _ => {
            let (protocol, base_url, client_profile) = base_route(connection, model);
            match protocol {
                WireProtocol::GoogleGenerateContent => Transport::Google {
                    base_url,
                    client_profile,
                    effort,
                    dialect: GoogleGenerateContentDialect::GenerativeLanguage,
                },
                WireProtocol::AnthropicMessages => Transport::Anthropic {
                    base_url,
                    client_profile,
                    effort,
                    thinking,
                    dialect: AnthropicMessagesDialect::Standard,
                },
                WireProtocol::OpenAiResponses => Transport::OpenAiResponses {
                    base_url,
                    client_profile,
                    effort,
                    dialect: if connection.preset_id.as_deref() == Some("deepseek") {
                        OpenAiResponsesDialect::DeepSeek
                    } else {
                        OpenAiResponsesDialect::Standard
                    },
                },
                WireProtocol::OpenAiChatCompletions => Transport::OpenAi {
                    base_url,
                    client_profile,
                    effort,
                    dialect: OpenAiChatDialect::Standard,
                },
            }
        }
    };

    Channel {
        id: model.to_string(),
        label: model.to_string(),
        transport,
        credentials,
        model: model.to_string(),
        remote,
        user_overrides: route_settings
            .and_then(|r| r.capability_overrides.clone())
            .filter(|o| !o.is_empty()),
        prompt_cache,
        prompt_cache_preference,
    }
}

/// The base transport for a non-OAuth connection: preset route (always derived
/// from the hardcoded preset spec) or the pure-custom declaration.
#[allow(clippy::expect_used)] // `route_models` validates preset IDs before this derivation step.
fn base_route(connection: &Connection, model: &str) -> (WireProtocol, String, ClientProfile) {
    if let Some(pid) = connection.preset_id.as_deref() {
        let preset = provider_preset_spec(pid)
            .expect("route_models rejects unknown provider presets before route derivation");
        let (protocol, preset_base_url, preset_ua) =
            preset_route(pid, model).unwrap_or((preset.protocol, "", preset.user_agent));
        let client_profile = if let Some(ua) = connection.user_agent.as_deref() {
            ClientProfile::from_user_agent(ua)
        } else if connection.client_identity != ClientProfile::Native {
            connection.client_identity.clone()
        } else if let Some(pua) = preset_ua {
            ClientProfile::from_user_agent(pua)
        } else {
            connection.client_identity.clone()
        };
        let base_url = if preset_base_url.is_empty() {
            connection
                .base_url
                .clone()
                .filter(|url| !url.trim().is_empty())
                .unwrap_or_else(|| default_endpoint(protocol))
        } else {
            preset_base_url.to_string()
        };
        (protocol, base_url, client_profile)
    } else {
        let protocol = connection
            .protocol
            .unwrap_or(WireProtocol::OpenAiChatCompletions);
        let base_url = connection
            .base_url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| default_endpoint(protocol));
        let client_profile = if let Some(ua) = connection.user_agent.as_deref() {
            ClientProfile::from_user_agent(ua)
        } else {
            connection.client_identity.clone()
        };
        (protocol, base_url, client_profile)
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
    let client_profile = if let Some(ua) = connection.user_agent.as_deref() {
        ClientProfile::from_user_agent(ua)
    } else if connection.client_identity != ClientProfile::Native {
        connection.client_identity.clone()
    } else {
        ClientProfile::Copilot
    };
    match remote.and_then(|r| r.protocol) {
        Some(WireProtocol::OpenAiResponses) => Transport::OpenAiResponses {
            base_url: "https://api.githubcopilot.com/responses".to_string(),
            client_profile,
            effort,
            dialect: OpenAiResponsesDialect::Copilot,
        },
        Some(WireProtocol::AnthropicMessages) => Transport::Anthropic {
            base_url: "https://api.githubcopilot.com/v1/messages".to_string(),
            client_profile,
            effort,
            thinking,
            dialect: AnthropicMessagesDialect::Copilot,
        },
        Some(WireProtocol::OpenAiChatCompletions) | None => Transport::OpenAi {
            base_url: "https://api.githubcopilot.com/chat/completions".to_string(),
            client_profile,
            effort,
            dialect: OpenAiChatDialect::Copilot,
        },
        Some(WireProtocol::GoogleGenerateContent) => {
            panic!("Copilot advertised unsupported Google generateContent protocol")
        }
    }
}

/// A transport's default endpoint when a custom connection omits one.
pub fn default_endpoint(protocol: WireProtocol) -> String {
    match protocol {
        WireProtocol::GoogleGenerateContent => "http://localhost:8080/v1beta".to_string(),
        WireProtocol::AnthropicMessages => "http://localhost:8080/v1/messages".to_string(),
        WireProtocol::OpenAiResponses => "http://localhost:8080/v1/responses".to_string(),
        WireProtocol::OpenAiChatCompletions => {
            "http://localhost:8080/v1/chat/completions".to_string()
        }
    }
}

/// The resolved credential for a connection: env var (`api_key_env`) →
/// `credentials.toml` → empty. OAuth connections resolve their live access token
/// from the exact connection namespace in `auth.toml` instead.
pub fn resolve_credential(connection: &Connection, creds: &Credentials) -> SecretString {
    if connection.auth.is_oauth() {
        return muta_providers::oauth::AuthStore::load()
            .map_err(|error| {
                tracing::error!(
                    connection_id = %connection.id,
                    error = %error,
                    "could not read OAuth credentials"
                );
                error
            })
            .ok()
            .and_then(|store| {
                store
                    .get(&connection.id)
                    .map(|tokens| tokens.access.clone())
            })
            .unwrap_or_default();
    }
    if let Some(env) = connection.api_key_env.as_deref()
        && let Ok(value) = std::env::var(env)
        && !value.trim().is_empty()
    {
        return SecretString::from(value);
    }
    creds.api_key(&connection.id).cloned().unwrap_or_default()
}
