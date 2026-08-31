//! Multi-protocol HTTP client for LLM backends.
//!
//! One crate speaks the wire protocols muta supports — OpenAI
//! chat-completions, the OpenAI Responses API, Anthropic Messages, and Google
//! — over a shared pooled HTTP transport. The crate is organised in two
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
//! The crate is consumed by `muta-providers`, the channel registry/factory
//! that decides *which* backend to talk to; this crate only knows *how*.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod client;
pub mod endpoint;
pub mod json;
pub mod prompt_cache;
pub mod protocol;
pub mod sse;
pub mod transport;

// Re-export the shared substrate at the crate root so protocol modules and the
// facade reach it as `crate::{Endpoint, ensure_success, …}` rather than through
// the owning module.
pub use client::Client;
pub use endpoint::{
    COPILOT_CLIENT_HEADERS, ClientIdentity, Endpoint, MUTA_USER_AGENT, OPENCODE_USER_AGENT,
    OPENCODE_VERSION, ZCODE_CLIENT_HEADERS, ZCODE_USER_AGENT,
};
pub use transport::{decode_response_json, ensure_success, retry_after_ms, transport_error};

// Re-export the concrete provider types at the crate root for ergonomic access
// and stable intra-doc links.
pub use prompt_cache::PromptCacheConfig;
pub use protocol::anthropic::{AnthropicMessagesProvider, Effort, ThinkingConfig, ThinkingMode};
pub use protocol::google::{GOOGLE_DEFAULT_BASE_URL, GoogleProvider};
pub use protocol::openai::{OpenAiChatCompletionsProvider, OpenAiResponsesProvider};

/// Test-only model baselines.
///
/// This crate's protocol tests exercise request-building logic (effort
/// clamping, thinking-field construction, vision stripping) against specific
/// real model ids. The metadata for those ids lives in `muta-providers`'
/// per-provider files, but `muta-llm-client` cannot depend on
/// `muta-providers` (the dependency points the other way). These
/// `#[cfg(test)]` registrations mirror the vendor metadata so `resolve()`
/// finds the same data inside this crate's standalone test binary. They are
/// never compiled into the library or linked by downstream crates, so there
/// is no collision with providers' own inventory submissions.
#[cfg(test)]
mod test_baselines {
    use muta_contracts::model::BaselineModels;
    use muta_contracts::thinking::ThinkingSupport;
    use muta_contracts::{Model, WireProtocol};

    const CLAUDE_BASELINES: &[Model] = &[
        Model {
            id: "claude-opus-4-8",
            family: "claude",
            context_window: 1_000_000,
            thinking: ThinkingSupport::AnthropicAdaptive,
            tool_call: true,
            vision: true,
            protocol: WireProtocol::AnthropicMessages,
            model_guidance: "",
            effort_levels: muta_contracts::effort::EFFORT_CLAUDE_FULL,
        },
        Model {
            id: "claude-sonnet-4-6",
            family: "claude",
            context_window: 1_000_000,
            thinking: ThinkingSupport::AnthropicAdaptive,
            tool_call: true,
            vision: true,
            protocol: WireProtocol::AnthropicMessages,
            model_guidance: "",
            effort_levels: muta_contracts::effort::EFFORT_CLAUDE_NO_XHIGH,
        },
        Model {
            id: "claude-haiku-4-5-20251001",
            family: "claude",
            context_window: 200_000,
            thinking: ThinkingSupport::AnthropicManual,
            tool_call: true,
            vision: true,
            protocol: WireProtocol::AnthropicMessages,
            model_guidance: "",
            effort_levels: &[],
        },
    ];

    const OPENAI_BASELINES: &[Model] = &[
        Model {
            id: "gpt-5.5",
            family: "gpt",
            context_window: 1_000_000,
            thinking: ThinkingSupport::ReasoningSummary,
            tool_call: true,
            vision: true,
            protocol: WireProtocol::OpenAiChatCompletions,
            model_guidance: "",
            effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT,
        },
        Model {
            id: "gpt-5.6-sol",
            family: "gpt",
            context_window: 1_000_000,
            thinking: ThinkingSupport::ReasoningSummary,
            tool_call: true,
            vision: true,
            protocol: WireProtocol::OpenAiChatCompletions,
            model_guidance: "",
            effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT_5_6,
        },
    ];

    inventory::submit!(BaselineModels(CLAUDE_BASELINES));
    inventory::submit!(BaselineModels(OPENAI_BASELINES));
}
