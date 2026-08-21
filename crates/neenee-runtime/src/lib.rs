//! The session runtime layer between the orchestration crate
//! (`neenee-agent`) and the frontends (the `neenee-cli` TUI and
//! `neenee --attach` clients today, a browser frontend tomorrow).
//!
//! # Why this crate exists
//!
//! Historically `neenee` was a single process: one TUI driving one agent
//! background task over a pair of `mpsc` channels. When the TUI process
//! exited, the agent task died with it. That model cannot serve a browser
//! frontend, which needs a long-running host holding multiple concurrent
//! sessions that several clients can subscribe to.
//!
//! This crate owns the per-session state that makes that possible — the
//! session driver, its handlers, and the `/serve` WebSocket bridge that
//! translates the wire protocol (`AgentRequest`/`AgentResponse`, both
//! `Serialize`/`Deserialize`) to and from the in-process channels.
//!
//! # Architecture today
//!
//! The ADR-0037 Step 6 assembly factory has landed as
//! [`bootstrap::assemble`]: it builds one frontend-neutral session harness
//! ([`session_driver::SessionDriver`] plus its channels) per call, and the
//! application binary (`neenee-cli`) goes through it. The
//! multi-session host of ADR-0089 and the unified session daemon of
//! ADR-0096 have landed on top of it — the "one session per process"
//! posture is gone:
//!
//! - [`registry::SessionRegistry`] owns every live session across every
//!   project, one [`registry::HostedSession`] per assembled harness, and
//!   lazily resumes persisted sessions on attach.
//! - [`host`] is the daemon runtime; the unified binary runs it via
//!   `neenee serve` or detached auto-spawn.
//! - Clients — the `neenee` TUI (attach mode), `neenee status`, the
//!   dashboard — talk to the daemon over the [`serve`] WebSocket control
//!   plane: owner-only native IPC by default (a Unix domain socket or Windows
//!   Named Pipe), plus TCP with a bearer token when started `--public`. The
//!   client side of that control plane lives here
//!   too, as [`client`] (ADR-0098): client and server speak the same
//!   [`serve::Wire`] protocol from one crate, so the two cannot drift.
//! - [`serve_discovery`] publishes the global `daemon.json` record clients
//!   use to find the daemon; on graceful shutdown the daemon tears every
//!   hosted session down through the registry, firing each one's
//!   SessionEnd hooks (ADR-0025).
//!
//! # Dependency posture
//!
//! `neenee-runtime` depends on `neenee-agent` (orchestration and the
//! built-in tools), `neenee-persistence` (persistence), `neenee-providers`,
//! `neenee-skills`, `neenee-mcp` (the MCP connector protocol; this crate owns
//! each live `McpRuntime` because it controls connection lifetime), and
//! `neenee-contracts` (vocabulary). Slash-command
//! discovery and project scaffolding live here as the `commands` and `project`
//! modules. Agent-owned stateful tools are assembled inside
//! `neenee-agent`. This crate does **not** depend on `neenee-cli` — frontends
//! depend on this crate, never the reverse.
//!
//! # Identity posture
//!
//! This crate is application-neutral: it holds no product name, mission, or
//! principal profile. The embedding binary supplies an
//! [`neenee_contracts::AgentIdentity`] to `Agent::new` / `from_toolset` and binds
//! a [`neenee_contracts::PrincipalProfile`] via `apply_principal_profile`.
//! `neenee-cli` keeps the coding identity. The `/btw` side-session reuses
//! the primary agent's identity (`Agent::identity()`) rather than naming a product here.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod agent_setup;
pub mod bootstrap;
pub mod client;
pub mod commands;
pub mod export;
pub mod handlers_chat;
pub mod handlers_permission;
pub mod handlers_provider;
pub mod handlers_session;
pub mod handlers_slash;
pub mod hooks;
pub mod host;
pub mod log_rotate;
pub mod monitor;
pub mod project;
pub mod registry;
pub mod search_lexical;
pub mod serve;
pub mod serve_discovery;
pub mod serve_http;
pub mod session_driver;
pub mod session_view;
pub mod shell;
pub mod shutdown;
pub mod side;
pub mod slash_handler;
pub mod startup;
pub mod supervise;
pub mod ui_bridge;
pub mod wip_tools;

pub use session_driver::SessionDriver;
pub use ui_bridge::{CopyOutcome, UiBridge};

// NOTE: identity (`NEENEE_NAME`/`NEENEE_MISSION`/`neenee_identity`/
// `principal_code`) used to live here. It has moved to the application layer
// (each binary's own `identity` module) so this crate stays application-neutral.
// The `/btw` side session reuses the primary agent's identity via
// `Agent::identity()` rather than naming a product here.
