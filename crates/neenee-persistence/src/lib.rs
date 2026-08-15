//! Durable state and configuration for the neenee agent stack.
//!
//! `neenee-contracts` holds the pure domain (types & traits), zero I/O. This
//! crate sits one layer above it: the durable state and configuration a
//! frontend needs to actually run a session — config loading, path
//! resolution, the event-sourced session store (which carries the `/repeat`
//! cron schedule as session-scoped state), blob storage, the embedding index,
//! the per-project advisory lock, and model-usage telemetry.
//!
//! This is the **local agent** persistence layer. It assumes a
//! single-user workstation: paths resolve via XDG `ProjectDirs`, sessions
//! are keyed by project root, and a process-level `flock` enforces
//! single-instance-per-project. Other scenarios the project may grow
//! (group-chat with multi-tenancy, always-on quant trading) will not fit
//! this layer and should spawn sibling crates (`neenee-chat-store`,
//! `neenee-trading-store`) sharing only `neenee-contracts`. See ADR-0005.
//!
//! Frontends depend on `neenee-contracts` + `neenee-persistence` and add their own
//! presentation layer. They must never need to reach into a sibling
//! frontend's crate; this is what keeps the CLI self-contained today and
//! a GUI reachable tomorrow.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod blobs;
pub mod cache;
pub mod config;
pub mod embedding;
pub mod events;
pub mod fsutil;
pub mod lock;
pub mod paths;
pub mod provider_usage;
pub mod session;
pub mod trusted_projects;
