//! Runtime derivation of connection entries — channels are derived, never
//! persisted.
//!
//! A connection declares *who* it connects to (model provider + credential +
//! client identity + optional overrides). This module derives the concrete
//! routes — one per model, each with its transport/endpoint/credential/
//! reasoning — from that declaration plus the model provider registry and the
//! discovery cache. Nothing here is written back; the stores stay the single
//! source of truth and two connections to the same provider never duplicate or
//! drift a route set.
//!
//! Resolution precedence is the single source of truth shared by startup and
//! runtime switching (ADR-0002): env var (`api_key_env`) →
//! `credentials.toml` → empty. OAuth connections resolve their bearer from
//! `auth.toml` through a dynamic, per-connection credential source.

use muta_contracts::catalog::{Channel, ProviderEntry, Transport};
use muta_contracts::model::CapabilityOverrides;
use muta_contracts::{
    AnthropicMessagesDialect, ClientProfile, ConnectionAuth, ConnectionFilterPolicy, Effort,
    GoogleGenerateContentDialect, NamedFilterPolicy, OpenAiChatDialect, OpenAiResponsesDialect,
    ReasoningMode, SecretString, WireProtocol,
};
use muta_persistence::config::{Credentials, DiscoveryCache};
use muta_persistence::connections::Connection;
use muta_persistence::connections::Connections;
use muta_persistence::model_providers::ModelProviders;
use muta_persistence::route_settings::RouteSettingsStore;
use muta_providers::{RemoteCatalogSource, model_provider_spec, route_for_model as provider_route};

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
        id: connection.name.clone(),
        name: connection.name.clone(),
        description: String::new(),
        channels,
        default_channel: 0,
        builtin: false,
    }
}

/// The model ids a connection serves, in picker order (ADR-0199, ADR-0201).
/// Evaluates the 3-step Set Delta Algebra:
/// 1. S_base = (Baseline ∩ Discovery) or snapshot for the provider.
/// 2. S_provider = (S_base ∪ Provider.include) \ Provider.exclude
/// 3. S_effective = (S_provider ∪ Connection.include) \ Connection.exclude
pub fn route_models(connection: &Connection, cache: &DiscoveryCache) -> Vec<String> {
    let providers = ModelProviders::load();
    route_models_with_providers(connection, cache, &providers)
}

/// Route models evaluated with explicit model provider configurations.
pub fn route_models_with_providers(
    connection: &Connection,
    cache: &DiscoveryCache,
    providers: &ModelProviders,
) -> Vec<String> {
    let Some(spec) = model_provider_spec(&connection.provider) else {
        // The loader rejects an unknown provider before this runs (ADR-0201
        // INV-2); deriving nothing is the safe floor if one slips through.
        return Vec::new();
    };
    let mut models = if spec.catalog_source != RemoteCatalogSource::None {
        match cache.connection_models.get(&connection.name) {
            Some(discovered)
                if !discovered.is_empty() || cache.model_lists.contains_key(&connection.name) =>
            {
                discovered.clone()
            }
            None if cache.model_lists.contains_key(&connection.name) => Vec::new(),
            _ => spec.models.iter().map(|m| (*m).to_string()).collect(),
        }
    } else {
        spec.models.iter().map(|m| (*m).to_string()).collect()
    };

    // Connection pipe admission gate (ADR-0203):
    // 1. Filter candidates through the connection pipe valve
    let is_custom = connection.provider == muta_persistence::connections::CUSTOM_PROVIDER;
    let filter_policy = connection.models.filter.as_ref().cloned().unwrap_or(
        if is_custom || spec.catalog_source != RemoteCatalogSource::None {
            ConnectionFilterPolicy::Named(NamedFilterPolicy::All)
        } else {
            ConnectionFilterPolicy::Named(NamedFilterPolicy::Baseline)
        },
    );
    let baseline_ids: std::collections::HashSet<&str> =
        spec.baselines.iter().map(|m| m.id).collect();
    models.retain(|id| filter_policy.admits(id, baseline_ids.contains(id.as_str())));

    // 2. Provider delta application (from model_providers.toml)
    if let Some(provider_scope) = providers.get(&connection.provider) {
        for id in provider_scope.included_ids() {
            if !models.contains(&id) {
                models.push(id);
            }
        }
        models.retain(|m| !provider_scope.is_excluded(m));
    }

    // 3. Connection sovereign injection (inject bypasses filter unconditionally!)
    for id in connection.models.included_ids() {
        if !models.contains(&id) {
            models.push(id);
        }
    }

    // 4. Connection absolute block (block prunes unconditionally!)
    models.retain(|m| !connection.models.is_excluded(m));

    models
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
    // 4-layer descending capability cascade (ADR-0199):
    // Connection Overrides > Provider Overrides > Discovery Advertised Metadata > Baseline Registry Spec
    let providers = ModelProviders::load();
    let provider_scope = providers.get(&connection.provider);

    let mut remote = cache.remote_metadata_for(&connection.name, model).cloned();
    if let Some(provider_declared) = provider_scope.and_then(|p| p.find_included(model)) {
        let r = remote.get_or_insert_with(Default::default);
        r.context_window = provider_declared.context_window.or(r.context_window);
        r.max_output_tokens = provider_declared.max_output_tokens.or(r.max_output_tokens);
        r.thinking = provider_declared.thinking.or(r.thinking);
        r.vision = provider_declared.vision.or(r.vision);
        r.tool_call = provider_declared.tool_call.or(r.tool_call);
    }
    if let Some(declared) = connection.models.find_included(model) {
        let r = remote.get_or_insert_with(Default::default);
        r.context_window = declared.context_window.or(r.context_window);
        r.max_output_tokens = declared.max_output_tokens.or(r.max_output_tokens);
        r.thinking = declared.thinking.or(r.thinking);
        r.vision = declared.vision.or(r.vision);
        r.tool_call = declared.tool_call.or(r.tool_call);
    }

    let route_settings = routes.settings_for(&connection.name, model);

    let mut effective_overrides = CapabilityOverrides::default();
    if let Some(po) = provider_scope.and_then(|p| p.overrides.get(model)) {
        effective_overrides = effective_overrides.merge_with(po);
    }
    if let Some(io) = connection.models.overrides.get(model) {
        effective_overrides = effective_overrides.merge_with(io);
    }
    if let Some(ro) = route_settings.and_then(|r| r.capability_overrides.as_ref()) {
        effective_overrides = effective_overrides.merge_with(ro);
    }
    let user_overrides = (!effective_overrides.is_empty()).then_some(effective_overrides);
    let prompt_cache = model_provider_spec(&connection.provider)
        .map(|provider| (provider.prompt_cache)(model).materialize())
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
        Some(false) => ReasoningMode::Off,
        _ => ReasoningMode::Adaptive,
    });

    let credentials: std::sync::Arc<dyn muta_contracts::CredentialSource> =
        if connection.auth.is_oauth() {
            std::sync::Arc::new(muta_providers::oauth::OAuthCredentialSource::new(
                &connection.name,
                connection.auth,
            ))
        } else {
            let api_key = resolve_credential(connection, creds);
            muta_contracts::static_credential(api_key)
        };

    let transport = match connection.auth {
        ConnectionAuth::ChatGptOAuth => {
            let client_profile = effective_client_profile(connection);
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
            let client_profile = effective_client_profile(connection);
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
                WireProtocol::GoogleGemini => Transport::Google {
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
                WireProtocol::Responses => Transport::OpenAiResponses {
                    base_url,
                    client_profile,
                    effort,
                    dialect: if connection.provider == "deepseek" {
                        OpenAiResponsesDialect::DeepSeek
                    } else {
                        OpenAiResponsesDialect::Standard
                    },
                },
                WireProtocol::ChatCompletions => Transport::OpenAi {
                    base_url,
                    client_profile,
                    effort,
                    dialect: if connection.provider == "openrouter" {
                        OpenAiChatDialect::OpenRouter
                    } else {
                        OpenAiChatDialect::Standard
                    },
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
        user_overrides,
        prompt_cache,
        prompt_cache_preference,
    }
}

/// The base transport for a non-OAuth connection: the model provider's route
/// (derived from its hardcoded spec), with the connection's optional
/// `protocol` / `base_url` / `user_agent` overrides applied on top.
#[allow(clippy::expect_used)] // `provider` is validated at load (ADR-0201 INV-2).
fn base_route(connection: &Connection, model: &str) -> (WireProtocol, String, ClientProfile) {
    let spec = model_provider_spec(&connection.provider)
        .expect("connection provider is validated at load (ADR-0201 INV-2)");
    let (provider_protocol, provider_base_url, provider_ua) =
        provider_route(&connection.provider, model).unwrap_or((spec.protocol, "", spec.user_agent));
    let protocol = connection.protocol.unwrap_or(provider_protocol);
    let client_profile = if connection.user_agent.is_some()
        || connection.client_identity != ClientProfile::Native
        || spec.default_client_profile != muta_contracts::ClientPreset::Native
    {
        effective_client_profile(connection)
    } else if let Some(pua) = provider_ua {
        ClientProfile::from_user_agent(pua)
    } else {
        ClientProfile::Native
    };
    let base_url = if provider_base_url.is_empty() {
        connection
            .base_url
            .clone()
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| default_endpoint(protocol))
    } else {
        provider_base_url.to_string()
    };
    (protocol, base_url, client_profile)
}

/// Copilot OAuth routes select their wire family from the model's advertised
/// endpoint (which varies by plan and model), falling back to chat-completions.
fn copilot_route(
    connection: &Connection,
    remote: Option<&muta_contracts::RemoteModelMetadata>,
    effort: Option<Effort>,
    thinking: Option<ReasoningMode>,
) -> Transport {
    let client_profile = effective_client_profile(connection);
    match remote.and_then(|r| r.protocol) {
        Some(WireProtocol::Responses) => Transport::OpenAiResponses {
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
        Some(WireProtocol::ChatCompletions) | None => Transport::OpenAi {
            base_url: "https://api.githubcopilot.com/chat/completions".to_string(),
            client_profile,
            effort,
            dialect: OpenAiChatDialect::Copilot,
        },
        Some(WireProtocol::GoogleGemini) => {
            panic!("Copilot advertised unsupported Google generateContent protocol")
        }
    }
}

/// Resolve the sparse connection override over the provider's recommended
/// client profile. `Native` in legacy connection state is treated as omitted;
/// an explicit custom User-Agent remains the escape hatch for a native-like
/// override on sensitive providers.
fn effective_client_profile(connection: &Connection) -> ClientProfile {
    if let Some(ua) = connection.user_agent.as_deref() {
        return ClientProfile::from_user_agent(ua);
    }
    if connection.client_identity != ClientProfile::Native {
        return connection.client_identity.clone();
    }
    model_provider_spec(&connection.provider)
        .map(|spec| ClientProfile::from(spec.default_client_profile))
        .unwrap_or(ClientProfile::Native)
}

/// A transport's default endpoint when a `custom` connection omits one.
pub fn default_endpoint(protocol: WireProtocol) -> String {
    match protocol {
        WireProtocol::GoogleGemini => "http://localhost:8080/v1beta".to_string(),
        WireProtocol::AnthropicMessages => "http://localhost:8080/v1/messages".to_string(),
        WireProtocol::Responses => "http://localhost:8080/v1/responses".to_string(),
        WireProtocol::ChatCompletions => {
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
                    connection = %connection.name,
                    error = %error,
                    "could not read OAuth credentials"
                );
                error
            })
            .ok()
            .and_then(|store| {
                store
                    .get(&connection.name)
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
    creds.api_key(&connection.name).cloned().unwrap_or_default()
}
