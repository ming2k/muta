//! Provider facade and factory consumed by the orchestration layer.
//!
//! Protocol-specific implementation lives in per-protocol SDK crates:
//! `neenee-ai-sdk-openai`, `neenee-ai-sdk-anthropic`, and
//! `neenee-ai-sdk-google`. This crate keeps the app-facing registry,
//! `build_provider_for_channel`, and the in-memory mock provider, while
//! re-exporting the concrete SDK provider types for compatibility.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod mock;
mod registry;

pub use mock::MockProvider;
pub use neenee_ai_sdk_anthropic::{
    AnthropicMessagesProvider, Effort, ThinkingConfig, ThinkingMode,
};
pub use neenee_ai_sdk_core::{Endpoint, NEENEE_USER_AGENT, TurnState};
pub use neenee_ai_sdk_google::{GOOGLE_DEFAULT_BASE_URL, GoogleProvider};
pub use neenee_ai_sdk_openai::OpenAiCompatProvider;
pub use registry::{
    ANTHROPIC_BUILTIN_MODELS, DEEPSEEK_BUILTIN_MODELS, GOOGLE_BUILTIN_MODELS,
    OPENAI_BUILTIN_MODELS, OPENAI_PROVIDER_SPECS, OpenAiProviderSpec, XAI_BUILTIN_MODELS,
    build_provider_for_channel, openai_provider_spec,
};
