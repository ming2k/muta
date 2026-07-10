//! The session/transport layer between the orchestration crate
//! (`neenee-agent`) and the frontends (`neenee-code` TUI today, a browser
//! frontend tomorrow).
//!
//! # Why this crate exists
//!
//! Historically `neenee-code` was a single process: one TUI driving one agent
//! background task over a pair of `mpsc` channels. When the TUI process
//! exited, the agent task died with it. That model cannot serve a browser
//! frontend, which needs a long-running daemon holding multiple concurrent
//! sessions that several clients can subscribe to.
//!
//! This crate owns the three things that makes that possible:
//!
//! - **[`SharedState`]** — process-level singletons constructed once at
//!   bootstrap (the provider holder, skills registry, MCP catalog, config,
//!   embedding store, repeat store). Every session borrows from it; nothing
//!   here is session-scoped.
//! - **[`SessionRegistry`]** — a map of `session_id → Arc<SessionHandle>`. Each
//!   [`SessionHandle`] owns its own `Agent`, `SessionStore`, request channel,
//!   and a `broadcast` channel for its responses, so multiple clients can
//!   subscribe to the same session's event stream.
//! - **the transport bridge** — (future) WebSocket / SSE adapters that
//!   translate the wire protocol (`AgentRequest`/`AgentResponse`, now
//!   `Serialize`/`Deserialize`) to and from the in-process channels.
//!
//! # Migration posture
//!
//! This crate is being populated incrementally. The driver logic currently
//! living in `neenee-code` (`agent_loop`, `handlers/*`, `side`,
//! `agent_setup`, `session_view`, …) is pure of TUI dependencies except for a
//! single `/export` + clipboard call path, and is slated to move here. Until
//! then the types below define the target shape and the TUI continues to drive
//! `neenee-agent` directly. The shapes are intentionally close to the existing
//! `agent_loop::Harness` fields so the eventual move is mechanical.
//!
//! # Dependency posture
//!
//! `neenee-server` depends on `neenee-agent` (orchestration), `neenee-store`
//! (persistence), `neenee-providers` + `neenee-tools` (concrete impls the
//! session assembles), and `neenee-core` (vocabulary). It does **not** depend
//! on `neenee-code` — frontends depend on this crate, never the reverse.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod agent_loop;
pub mod agent_setup;
pub mod export;
pub mod handlers_chat;
pub mod handlers_permission;
pub mod handlers_provider;
pub mod handlers_session;
pub mod handlers_slash;
pub mod hooks;
pub mod mcp_catalog;
pub mod mcp_runtime;
pub mod pursuits;
pub mod registry;
pub mod review;
pub mod serve;
pub mod session_view;
pub mod shared;
pub mod shell;
pub mod side;
pub mod startup;
pub mod ui_bridge;

pub use registry::{SessionHandle, SessionRegistry};
pub use shared::SharedState;
pub use ui_bridge::{CopyOutcome, UiBridge};

/// The default agent identity: name + mission. Lives in the server layer (the
/// crate that constructs agents) so both the TUI and a future web frontend
/// share one identity. A frontend that wants a different persona can construct
/// its own [`neenee_agent::AgentIdentity`] and pass it to the session builder.
/// `neenee` is the **product's default instance name**, not "the principal's
/// name" — it is a self-reference anchor the model uses in the system prompt
/// (intro line, responding when called by name). The *role* ("code",
/// "research", …) is carried by a [`neenee_agent::PrincipalProfile`]; the
/// product name + mission live here on the identity. Private: only
/// [`neenee_identity`] composes them, and embeddings thread the whole
/// [`neenee_agent::AgentIdentity`] through a profile rather than naming these
/// constants directly.
const NEENEE_NAME: &str = "neenee";
const NEENEE_MISSION: &str = "an expert AI coding assistant with tool access";

/// The composed identity: name + mission, default tone (no persona override).
pub fn neenee_identity() -> neenee_agent::AgentIdentity {
    neenee_agent::AgentIdentity::new(NEENEE_NAME, NEENEE_MISSION)
}

/// The built-in **coding principal** profile (ADR-0053): the declarative form
/// of the role `neenee-code` historically assembled inline. The binary binds it
/// via `agent.apply_principal_profile(&principal_code())` after construction.
///
/// Scope and operation boundary are unrestricted (a coding principal may use
/// every capability and write anywhere in the workspace) and the runtime config
/// is the default — the binary still overlays the live `[principal]` config
/// table afterwards so per-installation knobs win. A future quant/research/ops
/// principal is another [`neenee_agent::PrincipalProfile`] value.
pub fn principal_code() -> neenee_agent::PrincipalProfile {
    neenee_agent::PrincipalProfile::with_identity("code", neenee_identity())
}
