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
pub use protocol::openai::{OpenAiProvider, ResponsesProvider};
