//! Multi-protocol HTTP client for LLM backends.
//!
//! One crate speaks the wire protocols neenee supports — OpenAI
//! chat-completions, the OpenAI Responses API, Anthropic Messages, and Google
//! Gemini — over a shared pooled HTTP transport. The crate is organised in two
//! layers:
//!
//! - **Transport** (`endpoint`, `sse`, `transport`, `client`, `json`): the
//!   connection configuration, the pooled [`reqwest::Client`] wrapper, SSE byte
//!   reassembly, retry/error classification, and JSON framing helpers shared by
//!   every protocol.
//! - **Protocols** (`protocol::{openai, anthropic, google}`): per-vendor
//!   request construction and response/stream parsing. Each protocol is a thin
//!   executor over its pure `request`/`response` modules plus the shared
//!   transport; it owns no cross-protocol surface of its own.
//!
//! The crate is consumed by `neenee-providers`, the channel registry/factory
//! that decides *which* backend to talk to; this crate only knows *how*.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod client;
pub mod endpoint;
pub mod json;
pub mod protocol;
pub mod sse;
pub mod transport;

// Re-export the shared substrate at the crate root so protocol modules and the
// facade reach it as `crate::{Endpoint, ensure_success, …}` rather than through
// the owning module.
pub use client::Client;
pub use endpoint::{COPILOT_CLIENT_HEADERS, Endpoint, NEENEE_USER_AGENT, TurnState};
pub use transport::{decode_response_json, ensure_success, retry_after_ms, transport_error};

// Re-export the concrete provider types at the crate root for ergonomic access
// and stable intra-doc links.
pub use protocol::anthropic::{AnthropicMessagesProvider, Effort, ThinkingConfig, ThinkingMode};
pub use protocol::google::{GOOGLE_DEFAULT_BASE_URL, GoogleProvider};
pub use protocol::openai::{OpenAiChatCompletionsProvider, OpenAiResponsesProvider};

/// Test-only model baselines.
///
/// This crate's protocol tests exercise request-building logic (effort
/// clamping, thinking-field construction, vision stripping) against specific
/// real model ids. The metadata for those ids lives in `neenee-providers`'
/// per-provider files, but `neenee-llm-client` cannot depend on
/// `neenee-providers` (the dependency points the other way). These
/// `#[cfg(test)]` registrations mirror the vendor metadata so `resolve()`
/// finds the same data inside this crate's standalone test binary. They are
/// never compiled into the library or linked by downstream crates, so there
/// is no collision with providers' own inventory submissions.
#[cfg(test)]
mod test_baselines {
    use neenee_core::thinking::ThinkingSupport;
    use neenee_core::{Model, WireFormat};
    use neenee_core::model::BaselineModels;

    const CLAUDE_BASELINES: &[Model] = &[
        Model {
            id: "claude-opus-4-8",
            name: "Claude Opus 4.8",
            family: "claude",
            context_window: 1_000_000,
            thinking: ThinkingSupport::AnthropicAdaptive,
            tool_call: true,
            vision: true,
            format: WireFormat::AnthropicCompat,
            model_guidance: "",
            effort_levels: neenee_core::effort::EFFORT_CLAUDE_FULL,
        },
        Model {
            id: "claude-sonnet-4-6",
            name: "Claude Sonnet 4.6",
            family: "claude",
            context_window: 1_000_000,
            thinking: ThinkingSupport::AnthropicAdaptive,
            tool_call: true,
            vision: true,
            format: WireFormat::AnthropicCompat,
            model_guidance: "",
            effort_levels: neenee_core::effort::EFFORT_CLAUDE_NO_XHIGH,
        },
        Model {
            id: "claude-haiku-4-5-20251001",
            name: "Claude Haiku 4.5",
            family: "claude",
            context_window: 200_000,
            thinking: ThinkingSupport::AnthropicManual,
            tool_call: true,
            vision: true,
            format: WireFormat::AnthropicCompat,
            model_guidance: "",
            effort_levels: &[],
        },
    ];

    const OPENAI_BASELINES: &[Model] = &[
        Model {
            id: "gpt-5.5",
            name: "GPT-5.5",
            family: "gpt",
            context_window: 1_000_000,
            thinking: ThinkingSupport::ReasoningSummary,
            tool_call: true,
            vision: true,
            format: WireFormat::OpenAi,
            model_guidance: "",
            effort_levels: neenee_core::effort::EFFORT_OPENAI_GPT,
        },
        Model {
            id: "gpt-5.6-sol",
            name: "GPT-5.6 Sol",
            family: "gpt",
            context_window: 1_000_000,
            thinking: ThinkingSupport::ReasoningSummary,
            tool_call: true,
            vision: true,
            format: WireFormat::OpenAi,
            model_guidance: "",
            effort_levels: neenee_core::effort::EFFORT_OPENAI_GPT_5_6,
        },
    ];

    inventory::submit!(BaselineModels(CLAUDE_BASELINES));
    inventory::submit!(BaselineModels(OPENAI_BASELINES));
}
