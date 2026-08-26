//! Agent tiers: the Supervisor / Master / Runner lattice (ADR-0144).
//!
//! One word — *agent* — three tiers. A tier is not a kind of thing: all
//! tiers run the same engine, own a conversation, and declare tools from the
//! shared [`crate::ToolPool`]. A tier is **position in the hierarchy**:
//!
//! - [`AgentTier::Supervisor`] (tier 0): exactly one per daemon. Hosts,
//!   tracks, and jointly debugs (联调) sessions. Root of the mesh.
//! - [`AgentTier::Master`] (tier 1): exactly one per session. The
//!   user-facing conversation. Owns the session's runners.
//! - [`AgentTier::Runner`] (tier 2): many per master. Specific, narrow
//!   missions; exists to keep the master's conversation clean.
//!
//! The old vocabulary maps as: `Principal` → `Master`, `Envoy` → `Runner`,
//! `SessionRegistry`-as-host → `Supervisor`.

use serde::{Deserialize, Serialize};

/// Position of an agent in the daemon's hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTier {
    /// Tier 0 — the single daemon-level agent hosting sessions.
    Supervisor,
    /// Tier 1 — the single session-level agent the user talks to.
    Master,
    /// Tier 2 — a mission-scoped agent owned by a master.
    Runner,
}

impl AgentTier {
    /// All tiers, root first.
    pub const ALL: &'static [AgentTier] = &[AgentTier::Supervisor, AgentTier::Master, AgentTier::Runner];

    /// Depth from the root: supervisor `0`, master `1`, runner `2`.
    pub fn depth(self) -> usize {
        match self {
            AgentTier::Supervisor => 0,
            AgentTier::Master => 1,
            AgentTier::Runner => 2,
        }
    }

    /// The tier that owns instances of `self` (`None` for the supervisor —
    /// nothing owns the root).
    pub fn parent_tier(self) -> Option<AgentTier> {
        match self {
            AgentTier::Supervisor => None,
            AgentTier::Master => Some(AgentTier::Supervisor),
            AgentTier::Runner => Some(AgentTier::Master),
        }
    }

    /// Whether `self` may own agents of `child`: strictly one step down.
    pub fn is_parent_of(self, child: AgentTier) -> bool {
        child.parent_tier() == Some(self)
    }

    /// Whether `self` may issue `Instruction`-class mesh messages to
    /// `other`: strictly elder, one step up in the other direction.
    pub fn may_command(self, other: AgentTier) -> bool {
        self.is_parent_of(other)
    }

    /// Human label used in UI copy and log lines.
    pub fn label(self) -> &'static str {
        match self {
            AgentTier::Supervisor => "supervisor",
            AgentTier::Master => "master",
            AgentTier::Runner => "runner",
        }
    }
}

impl std::fmt::Display for AgentTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_depth_ordered() {
        assert!(AgentTier::Supervisor < AgentTier::Master);
        assert!(AgentTier::Master < AgentTier::Runner);
        assert_eq!(AgentTier::Runner.depth(), 2);
    }

    #[test]
    fn parenting_is_strictly_one_step() {
        assert!(AgentTier::Supervisor.is_parent_of(AgentTier::Master));
        assert!(AgentTier::Master.is_parent_of(AgentTier::Runner));
        assert!(!AgentTier::Supervisor.is_parent_of(AgentTier::Runner));
        assert_eq!(AgentTier::Supervisor.parent_tier(), None);
    }

    #[test]
    fn command_flow_is_elder_only() {
        assert!(AgentTier::Master.may_command(AgentTier::Runner));
        assert!(!AgentTier::Runner.may_command(AgentTier::Master));
        assert!(!AgentTier::Runner.may_command(AgentTier::Runner));
    }

    #[test]
    fn tier_round_trips_serde() {
        for tier in AgentTier::ALL {
            let s = serde_json::to_string(tier).unwrap();
            let back: AgentTier = serde_json::from_str(&s).unwrap();
            assert_eq!(back, *tier);
        }
    }
}
