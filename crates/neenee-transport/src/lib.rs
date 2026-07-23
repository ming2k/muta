//! The session/transport layer between the orchestration crate
//! (`neenee-agent`) and the frontends (`neenee` TUI today, a browser
//! frontend tomorrow).
//!
//! # Why this crate exists
//!
//! Historically `neenee` was a single process: one TUI driving one agent
//! background task over a pair of `mpsc` channels. When the TUI process
//! exited, the agent task died with it. That model cannot serve a browser
//! frontend, which needs a long-running daemon holding multiple concurrent
//! sessions that several clients can subscribe to.
//!
//! This crate owns the per-session state that makes that possible — the
//! session driver, its handlers, and the `/serve` WebSocket bridge that
//! translates the wire protocol (`AgentRequest`/`AgentResponse`, both
//! `Serialize`/`Deserialize`) to and from the in-process channels.
//!
//! # Migration posture
//!
//! The frontend-neutral session driver and its handlers live in this crate.
//! `neenee` still assembles one [`session_driver::SessionDriver`] during
//! startup. The multi-session server scaffolding sketched in ADR-0037 §6
//! (`SessionRegistry` / `SessionHandle` / `SharedState`) was removed as
//! dormant — every method returned `Err("not yet populated")`. Reintroduce
//! it when the server move resumes.
//!
//! # Dependency posture
//!
//! `neenee-transport` depends on `neenee-agent` (orchestration), `neenee-persistence`
//! (persistence), `neenee-providers`, `neenee-tools` (shell, project, and
//! command services), `neenee-skills`, `neenee-mcp`, and `neenee-core`
//! (vocabulary). It owns each live MCP runtime while the protocol remains in
//! its dedicated crate. Agent-owned stateful tools are assembled inside
//! `neenee-agent`. This crate does **not** depend on `neenee` — frontends
//! depend on this crate, never the reverse.
//!
//! # Identity posture
//!
//! This crate is application-neutral: it holds no product name, mission, or
//! principal profile. The embedding binary supplies an
//! [`neenee_agent::AgentIdentity`] to `Agent::new` / `from_toolset` and binds
//! a [`neenee_agent::PrincipalProfile`] via `apply_principal_profile`.
//! `neenee` keeps the coding identity; a future sibling binary would bring
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
pub mod review;
pub mod serve;
pub mod session_driver;
pub mod session_view;
pub mod shell;
pub mod side;
pub mod slash_handler;
pub mod startup;
pub mod ui_bridge;

pub use session_driver::SessionDriver;
pub use ui_bridge::{CopyOutcome, UiBridge};

// NOTE: identity (`NEENEE_NAME`/`NEENEE_MISSION`/`neenee_identity`/
// `principal_code`) used to live here. It has moved to the application layer
// (`neenee`'s `identity` module) so this crate stays application-neutral.
// The `/btw` side session reuses the primary agent's identity via
// `Agent::identity()` rather than naming a product here.
