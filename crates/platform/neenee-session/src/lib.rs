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
//! The frontend-neutral session driver and its handlers live in this crate.
//! `neenee-code` still assembles one [`session_driver::SessionDriver`] during
//! startup; the remaining migration step is to move that assembly behind
//! [`SessionRegistry::create_session`] so a server process can own multiple
//! sessions.
//!
//! # Dependency posture
//!
//! `neenee-session` depends on `neenee-agent` (orchestration), `neenee-store`
//! (persistence), `neenee-providers`, `neenee-tools` (shell, project, and
//! command services), `neenee-skills`, `neenee-mcp`, and `neenee-core`
//! (vocabulary). It owns each live MCP runtime while the protocol remains in
//! its dedicated crate. Agent-owned stateful tools are assembled inside
//! `neenee-agent`. This crate does **not** depend on `neenee-code` — frontends
//! depend on this crate, never the reverse.
//!
//! # Identity posture
//!
//! This crate is application-neutral: it holds no product name, mission, or
//! principal profile. The embedding binary supplies an
//! [`neenee_agent::AgentIdentity`] to `Agent::new` / `from_toolset` and binds
//! a [`neenee_agent::PrincipalProfile`] via `apply_principal_profile`.
//! `neenee-code` keeps the coding identity; a future `neenee-quant` binary
//! brings its own. The `/btw` side-session reuses the primary agent's
//! identity (`Agent::identity()`) rather than naming a product here.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod agent_setup;
pub mod export;
pub mod handlers_chat;
pub mod handlers_permission;
pub mod handlers_provider;
pub mod handlers_session;
pub mod handlers_slash;
pub mod hooks;
pub mod pursuits;
pub mod registry;
pub mod review;
pub mod serve;
pub mod session_driver;
pub mod session_view;
pub mod shared;
pub mod shell;
pub mod side;
pub mod slash_handler;
pub mod startup;
pub mod ui_bridge;

pub use registry::{SessionHandle, SessionRegistry};
pub use session_driver::SessionDriver;
pub use shared::SharedState;
pub use ui_bridge::{CopyOutcome, UiBridge};

// NOTE: identity (`NEENEE_NAME`/`NEENEE_MISSION`/`neenee_identity`/
// `principal_code`) used to live here. It has moved to the application layer
// (`neenee-code`'s `identity` module) so this crate stays application-neutral.
// The `/btw` side session reuses the primary agent's identity via
// `Agent::identity()` rather than naming a product here.
