//! Provider facade and factory consumed by the orchestration layer.
//!
//! Protocol-specific implementation lives in `muta-llm-client`, the
//! multi-protocol HTTP client crate (`protocol::{openai, anthropic, google}`).
//! This crate keeps the app-facing registry, `build_provider_for_channel`,
//! and the OAuth2/PKCE credential-acquisition flows for subscription providers
//! ([`oauth`]), while re-exporting the concrete provider types for
//! compatibility.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod http;
mod list_models;
pub mod oauth;
mod registry;
pub mod usage;
pub use registry::effort_ladders;

pub use registry::{QoderCatalogSigning, build_catalog_signer};

pub use list_models::{
    CatalogParser, CatalogShape, CatalogSignature, CatalogSigning, DiscoveredModel, ModelListError,
    RemoteCatalogOptions, RemoteCatalogRequest, RemoteCatalogUpdate, fetch_remote_catalog,
    list_models, models_endpoint_for, parser_for,
};
pub use muta_llm_client::{
    AnthropicMessagesProvider, COPILOT_CLIENT_HEADERS, ChatCompletionsProvider, ClientIdentity,
    Effort, Endpoint, GOOGLE_DEFAULT_BASE_URL, GoogleGeminiProvider, GoogleProvider,
    MUTA_USER_AGENT, OPENCODE_USER_AGENT, OPENCODE_VERSION, OpenAiChatCompletionsProvider,
    OpenAiResponsesProvider, ReasoningMode, ResponsesProvider, ThinkingConfig,
    ZCODE_CLIENT_HEADERS, ZCODE_USER_AGENT,
};
pub use oauth::OAuthCredentialSource;
pub use registry::{
    ANTHROPIC_BUILTIN_MODELS, ANTIGRAVITY_OAUTH_MODELS, CHATGPT_BUILTIN_MODELS,
    COPILOT_SEED_MODELS, DEEPSEEK_BUILTIN_MODELS, GOOGLE_BUILTIN_MODELS, KIMI_CODE_MODELS,
    MODEL_PROVIDER_SPECS, ModelProviderSpec, OPENAI_BUILTIN_MODELS, OPENCODE_CONSOLE_MODELS,
    OPENCODE_GO_MODELS, OPENCODE_ZEN_MODELS, OPENROUTER_BUILTIN_MODELS, PromptCachePolicy,
    RemoteCatalogSource, XAI_BUILTIN_MODELS,
    ZAI_CODE_MODELS, build_provider_for_channel, endpoint_for, model_provider_spec,
    register_user_declared_provider, route_for_model, sync_user_declared_providers_from_disk,
    unsupported_prompt_cache, user_declared_provider_spec,
};
/// Public: the Qoder dialect's wire surface, for golden-wire integration
/// tests ([INV-WIRE-01], ADR-0271) and downstream dialect tooling.
pub use registry::qoder;
pub use usage::{
    AntigravityUsageFetcher, DeepSeekUsageFetcher, KimiUsageFetcher, OpenRouterUsageFetcher,
    ProviderUsageFetcher, SiliconFlowUsageFetcher, fetch_provider_usage,
};

/// Build a dynamic or static credential source for a connection (ADR-0267).
pub fn build_credential_source(
    connection: &muta_persistence::connections::Connection,
    api_key: muta_contracts::SecretString,
    dialect: muta_contracts::ProviderDialect,
) -> std::sync::Arc<dyn muta_contracts::CredentialSource> {
    if connection.auth.is_oauth() {
        std::sync::Arc::new(oauth::OAuthCredentialSource::new(
            &connection.name,
            connection.auth.clone(),
        ))
    } else if dialect == muta_contracts::ProviderDialect::Qoder {
        std::sync::Arc::new(oauth::qoder::QoderApiKeyCredentialSource::new(
            &connection.name,
            api_key,
        ))
    } else {
        muta_contracts::static_credential(api_key)
    }
}
