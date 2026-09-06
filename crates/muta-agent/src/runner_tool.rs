//! Legacy alias for `subagent_tool` (ADR-0183).
//!
//! Superseded by [`crate::subagent_tool`]. Homogeneous agents use recursive
//! delegation (`SubAgentTool`, `spawn_agent`) rather than a distinct "runner"
//! species. Re-exported here for migration compatibility.

pub use crate::subagent_tool::*;
