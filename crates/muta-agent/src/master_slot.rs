//! Legacy alias for `agent_slot` (ADR-0183).
//!
//! Superseded by [`crate::agent_slot`]. Under the homogeneous agent model,
//! a session hosts an [`crate::AgentSlot`] rather than a "master" slot.

pub use crate::agent_slot::*;
