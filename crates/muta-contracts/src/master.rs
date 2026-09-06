//! Legacy aliases for agent presets (ADR-0183).
//!
//! Superseded by [`crate::agent_preset`]. The homogeneous agent architecture
//! eliminates the artificial "master" vs "runner" species in favor of unified
//! [`crate::AgentPreset`] and [`crate::DelegationPolicy`].

pub use crate::agent_preset::*;
