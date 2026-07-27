//! The orchestration layer between the pure domain (`neenee-core`) and the
//! application services (`neenee-persistence`) on one side, and the frontends on the
//! other.
//!
//! # What lives here
//!
//! - **The `Agent` struct** (`agent.rs`) — holds the provider, tool set, mode,
//!   and skill registry; runs the streaming ReAct loop
//!   (`run_streaming_with_events`).
//! - **Model-request assembly** (`model_request/`) — immutable request
//!   projection and system-prompt policy. Durable harness-authored messages
//!   live separately under `conversation_context/`.
//! - **Extension integration** — consumes an optional `neenee-skills`
//!   registry for model-context injection and accepts connector tools through
//!   a protocol-neutral dynamic-tool port. Discovery and transport stay in
//!   their dedicated capability crates.
//! - **Turn orchestration** (`orchestration.rs`) — the policy that wraps every
//!   agent turn: compaction, mid-turn pruning, retries with backoff, and the
//!   `/repeat` cron scheduler. Frontends drive the harness
//!   through [`orchestration::execute_round`] and friends; they own only the
//!   UI-specific input path (slash commands for the CLI, menus/dialogs for a
//!   future GUI).
//!
//! # Dependency posture
//!
//! `neenee-agent` is the wiring layer: it depends on `neenee-core`
//! (domain vocabulary), `neenee-persistence` (durable state: `SessionStore`,
//! `Config`, `EmbeddingStore`), and `neenee-providers` (the
//! `build_provider_for_channel` factory plus the user-agent / spec
//! constants the catalog uses when constructing concrete impls). The
//! concrete coding-tool implementations live in this crate's [`tools`]
//! module; skill capability comes from `neenee-skills`, and tools are
//! dispatched through the
//! core [`Tool`] and [`ToolSet`] contracts. These dependencies point downward
//! (`agent -> skills`); orchestration-native tools that
//! construct or control agents remain in this crate.
//!
//! ## Why catalog and EnvoyTool live here (not in store / tools)
//!
//! Both got relocated here from their intuitive homes to keep the
//! dependency graph strictly layered (see ADR-0005):
//!
//! - **`catalog`** builds concrete `Provider` impls from a `Config`. It
//!   used to live in `neenee-persistence`, which forced store to depend on
//!   `neenee-providers` — an inversion, since store is otherwise a peer
//!   of providers. The catalog is fundamentally a factory consumed by
//!   orchestration, so it lives where orchestration lives.
//! - **`EnvoyTool`** spawns envoys via `Agent::new`. It used to live
//!   in the former `neenee-tools` crate, which forced tools to depend on
//!   this crate —
//!   another inversion, since tools are below the agent layer. The
//!   envoy tool is fundamentally an orchestration primitive that
//!   happens to satisfy the `Tool` trait, so it lives here too.
//!
//! Everything `neenee-core` exports is re-exported here so consumers can
//! `use neenee_agent::*` and get the full domain vocabulary alongside the
//! orchestration API.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub use neenee_core::*;

// Persistence-backed types that lived in core pre-refactor now live in store
// (ADR-0005: core is zero-I/O). Re-exported here so consumers keep reaching
// them through `neenee_agent::` unchanged.
pub use neenee_persistence::RepeatStore;

// Explicit re-exports of core's top-level re-exports. `pub use X::*` does
// not propagate through X's own `pub use` re-exports in Rust, so the items
// the Agent struct expects at the crate root have to be listed here by name.
// Keep this list in sync with `neenee_core`'s lib.rs re-exports.
pub use neenee_core::{
    AgentEvent, AgentOp, AgentRequest, AgentResponse, Channel, EXPLORE, EnvoyEvent, EnvoyProfile,
    HarnessError, HarnessSnapshot, ImagePart, InputReply, InputRequest, McpConnectionStatus,
    McpServerConfig, Message, ModelRequest, PRUNED_TOOL_PLACEHOLDER, PatchOp, PermissionDecision,
    PermissionRequest, Provider, ProviderEntry, ProviderPickerRow, ProviderPickerSnapshot,
    ProviderStreamEvent, PruneOutcome, RetryableError, Role, SessionOverview,
    ShellTermination, SkillsConfig, StdinPolicy, TITLE, TokenUsage, Tool, ToolCall, ToolOutput,
    ToolPolicy, ToolResult, ToolStream, Transport, UserQuestion, UserQuestionOption,
    UserQuestionReply, UserQuestionRequest, WebSearchConfig, estimate_bytes, estimate_tokens,
    is_context_overflow, parse_retryable_error, prune_tool_results, public_error_message,
    retryable_error, truncate_utf8,
};

// Same ambient std/tokio prelude the Agent struct used to inherit from
// `neenee-core`'s lib.rs (`use super::*`).
use futures::{StreamExt, future::join_all};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Maximum interval between consecutive stream events (text/reasoning/tool-call
/// deltas) before the stream is considered stalled. All LLM providers use
/// `reqwest::Client::new()` which sets no read timeout, so without this guard a
/// reasoning model whose SSE connection hangs mid-generation (server stops
/// sending but keeps the TCP connection alive) blocks the turn loop
/// indefinitely — the UI spins "running · responding" forever and only a user
/// interrupt can break it. The bound is generous: reasoning models stream
/// deltas frequently and SSE keepalives arrive every 15–30 s, so two full
/// minutes of total silence is a genuine stall. On timeout the harness
/// surfaces a retryable error so the turn retries with backoff instead of
/// hanging.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Overall timeout for a single non-streaming `provider.chat()` call. The
/// non-streaming ReAct path ([`Agent::run_with_events`]) and context-
/// compaction summarization both call `chat()`, which blocks until the model
/// returns the complete response. Without a bound, a stalled or overloaded
/// endpoint hangs the turn (and, for compaction, the entire frontend) forever.
/// Five minutes is generous enough for a reasoning model generating a full
/// non-streaming response, while still catching a genuine stall. On timeout
/// the caller surfaces a retryable / fallback error instead of hanging.
const CHAT_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

pub mod agent;
pub use agent::{Agent, AgentBuilder, RequestTokenEstimate, RoundOutcome};

mod bash_policy;
pub mod catalog;
mod context_projection;
mod conversation_context;
pub mod doom_guard;
pub mod dynamic;
mod dynamic_tools;
pub mod hooks;
pub use hooks::{HookRegistry, UserPromptVerdict, matcher_matches};
pub mod envoy_tool;
mod hook_runner;
pub mod loop_guard;
mod model_request;
pub mod no_provider;
pub mod orchestration;
mod permission_store;
#[allow(dead_code)] // machinery: permission policies are wired; the async-gate policy types
                    // (HookPolicy/BashPolicy/AskUserPolicy) stay as documented placeholders.
mod permission_policy;
#[allow(dead_code)] // machinery: the four-stage pipeline types await the full dispatch rewrite.
mod dispatch_pipeline;
#[allow(dead_code)] // machinery: disclosure ledger awaits select_tools + tool_specs wiring.
mod disclosure_ledger;
#[allow(dead_code)] // machinery: disclosure bridge awaits the select_tools switchover.
mod disclosure_bridge;
pub mod round_lifecycle;
pub use round_lifecycle::{RoundBegin, RoundLifecycle};
pub mod session_review;
pub mod session_title;
mod shell_input;
use neenee_skills as skills;
mod tool_call;
mod tool_integration;
#[allow(dead_code)] // machinery: ToolManager logic is reused via Agent methods; the standalone
                    // struct stays as a tested unit pending the resolved_tools field restructure.
mod tool_manager;
#[allow(dead_code)] // machinery: ToolScheduler is tested but not yet the dispatch driver
                    // (execute_tools_concurrent uses group_by_conflict batching instead).
mod tool_scheduler;
/// MCP connector (formerly the standalone neenee-mcp crate).
pub mod mcp;
pub mod tools;

pub use context_projection::ContextProjectionGate;
pub use envoy_tool::{EnvoyRegistry, EnvoyTool};
pub use model_request::system_prompt::{
    SystemPromptContext, SystemPromptRegistry, SystemPromptRegistryError, SystemPromptSection,
};
pub use no_provider::{NO_PROVIDER_ID, NoProvider};
pub use session_review::{LoopingReview, default_reviews};

#[cfg(test)]
mod tests;
