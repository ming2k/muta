//! Provider facade and factory consumed by the orchestration layer.
//!
//! Protocol-specific implementation lives in `muta-llm-client`, the
//! multi-protocol HTTP client crate (`protocol::{openai, anthropic, google}`).
//! This crate keeps the app-facing registry, `build_provider_for_channel`,
//! and the OAuth2/PKCE credential-acquisition flows for subscription providers
//! ([`oauth`]), while re-exporting the concrete provider types for
//! compatibility.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod list_models;
pub mod oauth;
mod registry;

pub use list_models::{
    DiscoveredModel, DiscoveryProtocol, ModelDiscoveryOptions, ModelDiscoveryRequest,
    ModelDiscoveryUpdate, ModelListError, discover_models, list_models, models_endpoint_for,
};
pub use muta_llm_client::{
    AnthropicMessagesProvider, COPILOT_CLIENT_HEADERS, ClientIdentity, Effort, Endpoint,
    GOOGLE_DEFAULT_BASE_URL, GoogleProvider, MUTA_USER_AGENT, OPENCODE_USER_AGENT,
    OPENCODE_VERSION, OpenAiChatCompletionsProvider, OpenAiResponsesProvider, ThinkingConfig,
    ThinkingMode, TurnState, ZCODE_CLIENT_HEADERS, ZCODE_USER_AGENT,
};
pub use registry::{
    ANTHROPIC_BUILTIN_MODELS, ANTIGRAVITY_OAUTH_MODELS, CHATGPT_BUILTIN_MODELS,
    COPILOT_SEED_MODELS, DEEPSEEK_BUILTIN_MODELS, GOOGLE_BUILTIN_MODELS, KIMI_CODE_MODELS,
    OPENAI_BUILTIN_MODELS, OPENAI_PROVIDER_SPECS, OPENCODE_GO_MODELS, OPENCODE_GO_SERVED_MODELS,
    OpenAiProviderSpec, PROVIDER_PRESET_SPECS, ProviderPresetSpec, XAI_BUILTIN_MODELS,
    ZAI_CODE_MODELS, build_provider_for_channel, openai_provider_spec, provider_preset_spec,
    route_for_model,
};
