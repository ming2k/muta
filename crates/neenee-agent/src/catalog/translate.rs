//! Config-to-[`Channel`] translation: the typed mapping from a persisted
//! [`UserChannelConfig`] / [`UserProviderConfig`] to the runtime
//! [`Channel`] carrying a fully resolved transport and credential.
//!
//! Resolution here is the single source of truth shared by startup and
//! runtime provider switching (ADR-0002): built-in presets produce one
//! `"default"` channel per entry; user-defined entries may declare several
//! channels with `default_channel` selecting one.

use neenee_contracts::catalog::{Channel, ProviderEntry, Transport, builtin_provider_metadata};
use neenee_contracts::{Effort, RemoteModelEndpoint, SecretString, ThinkingMode};
use neenee_persistence::config::{UserChannelConfig, UserProviderConfig, UserTransport};
use neenee_providers::NEENEE_USER_AGENT;

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

pub(super) fn user_channel_to_channel(
    uc: &UserChannelConfig,
    fallback_model: &str,
    provider_id: &str,
    template_id: Option<&str>,
) -> Channel {
    // OAuth channels resolve their bearer from auth.toml. ChatGPT also yields
    // the chatgpt_account_id (carried on the Responses transport); Google Antigravity yields
    // project_id; xAI has none. Activate/switch refreshes the token first (handlers_provider).
    let (api_key, account_id, project_id) = if uc.auth.is_oauth() {
        let store = neenee_providers::oauth::AuthStore::load();
        let tokens = store.get_for_provider(provider_id, template_id, uc.auth);
        (
            tokens.map(|t| t.access.clone()).unwrap_or_default(),
            tokens.and_then(|t| t.account_id.clone()),
            tokens
                .and_then(|t| t.project_id.clone())
                .or_else(|| tokens.and_then(|t| t.account_id.clone())),
        )
    } else {
        (
            env_or_config(uc.api_key_env.as_deref(), uc.api_key.clone()).unwrap_or_default(),
            None,
            None,
        )
    };
    let model = uc
        .model
        .clone()
        .unwrap_or_else(|| fallback_model.to_string());
    let transport = match uc.auth {
        // ChatGPT OAuth always speaks the Responses transport, regardless of the
        // stored `UserTransport`, with the bearer + account id resolved above.
        neenee_contracts::ChannelAuth::ChatGptOAuth => Transport::OpenAiResponses {
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
        neenee_contracts::ChannelAuth::CopilotOAuth => copilot_transport(uc),
        neenee_contracts::ChannelAuth::AntigravityOAuth => Transport::Google {
            base_url: uc
                .base_url
                .clone()
                .unwrap_or_else(|| "https://cloudcode-pa.googleapis.com".to_string()),
            user_agent: uc
                .user_agent
                .clone()
                .unwrap_or_else(|| "antigravity/1.23.2 windows/amd64".to_string()),
            effort: uc.effort.as_deref().and_then(Effort::parse),
            project_id,
        },
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
                effort: uc.effort.as_deref().and_then(Effort::parse),
                project_id,
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
            // API-key Responses channel (e.g. DeepSeek V4): same shape as the
            // ChatGPT OAuth Responses transport minus the account-id header.
            UserTransport::OpenAiResponses => Transport::OpenAiResponses {
                base_url: uc
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "http://localhost:8080/v1/responses".to_string()),
                user_agent: uc
                    .user_agent
                    .clone()
                    .unwrap_or_else(|| NEENEE_USER_AGENT.to_string()),
                effort: uc.effort.as_deref().and_then(Effort::parse),
                account_id: None,
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

pub(super) fn copilot_transport(uc: &UserChannelConfig) -> Transport {
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

pub(super) fn user_provider_to_entry(um: &UserProviderConfig) -> ProviderEntry {
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
        .map(|c| user_channel_to_channel(c, &fallback_model, &um.id, um.template_id.as_deref()))
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

pub(super) fn env_or_config(
    env_var: Option<&str>,
    config_value: Option<SecretString>,
) -> Option<SecretString> {
    env_var
        .and_then(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
        .map(SecretString::from)
        .or(config_value)
}
