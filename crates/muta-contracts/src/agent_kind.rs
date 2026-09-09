//! Agent archetypes and mesh stations: the Worker-Station Model (ADR-0167).
//!
//! Two agent archetypes ([`AgentKind`]) — `Root` (the driving brain) and
//! `Subagent` (the mission worker).
//!
//! Three operational stations ([`MeshStation`]) — `Hypervisor` (the daemon-level
//! coordinator), `Session` (the user-facing conversation host), and `Subtask`
//! (the task execution slot).

use serde::{Deserialize, Serialize};

/// Archetype / classification of an agent entity.
///
/// In Muta's homogeneous agent model (ADR-0183), an agent entity runs either in
/// a top-level root posture or a delegated child posture:
/// - [`AgentKind::Root`]: Full cognitive loop, tool execution, session/daemon orchestrator.
/// - [`AgentKind::Subagent`]: Isolated, sandboxed, short-lived task execution worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    /// Root agent: full cognitive loop, intent driver, conversation & tool authority.
    #[serde(alias = "master")]
    Root,
    /// Subagent: mission-scoped worker, isolated/sandboxed, single-task lifecycle.
    Subagent,
}

impl AgentKind {
    pub const ALL: &'static [AgentKind] = &[AgentKind::Root, AgentKind::Subagent];

    pub fn is_root(self) -> bool {
        matches!(self, Self::Root)
    }

    pub fn is_subagent(self) -> bool {
        matches!(self, Self::Subagent)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Subagent => "subagent",
        }
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// The operational station / host slot where an agent is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshStation {
    /// Daemon-level Hypervisor station (staffed by a Root agent).
    Hypervisor,
    /// Session-level primary conversation station (staffed by a Root agent).
    Session,
    /// Session subtask execution station (staffed by a Subagent).
    Subtask,
}

impl MeshStation {
    pub const ALL: &'static [MeshStation] = &[
        MeshStation::Hypervisor,
        MeshStation::Session,
        MeshStation::Subtask,
    ];

    /// Depth in the hierarchy: hypervisor `0`, session `1`, subtask `2`.
    pub fn depth(self) -> usize {
        match self {
            MeshStation::Hypervisor => 0,
            MeshStation::Session => 1,
            MeshStation::Subtask => 2,
        }
    }

    /// Parent station for routing (None for Hypervisor root).
    pub fn parent_station(self) -> Option<MeshStation> {
        match self {
            MeshStation::Hypervisor => None,
            MeshStation::Session => Some(MeshStation::Hypervisor),
            MeshStation::Subtask => Some(MeshStation::Session),
        }
    }

    /// Whether `self` is the direct parent of `child`.
    pub fn is_parent_of(self, child: MeshStation) -> bool {
        child.parent_station() == Some(self)
    }

    /// Whether `self` may command `other`.
    pub fn may_command(self, other: MeshStation) -> bool {
        self.is_parent_of(other)
    }

    /// Human label used in UI copy and log lines.
    pub fn label(self) -> &'static str {
        match self {
            MeshStation::Hypervisor => "hypervisor",
            MeshStation::Session => "session",
            MeshStation::Subtask => "subtask",
        }
    }
}

impl std::fmt::Display for MeshStation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn station_hierarchy_and_depth() {
        assert!(MeshStation::Hypervisor < MeshStation::Session);
        assert!(MeshStation::Session < MeshStation::Subtask);
        assert_eq!(MeshStation::Hypervisor.depth(), 0);
        assert_eq!(MeshStation::Session.depth(), 1);
        assert_eq!(MeshStation::Subtask.depth(), 2);
    }

    #[test]
    fn station_parenting() {
        assert!(MeshStation::Hypervisor.is_parent_of(MeshStation::Session));
        assert!(MeshStation::Session.is_parent_of(MeshStation::Subtask));
        assert!(!MeshStation::Hypervisor.is_parent_of(MeshStation::Subtask));
        assert_eq!(MeshStation::Hypervisor.parent_station(), None);
    }

    #[test]
    fn station_command_permissions() {
        assert!(MeshStation::Hypervisor.may_command(MeshStation::Session));
        assert!(MeshStation::Session.may_command(MeshStation::Subtask));
        assert!(!MeshStation::Subtask.may_command(MeshStation::Session));
        assert!(!MeshStation::Subtask.may_command(MeshStation::Subtask));
    }

    #[test]
    fn agent_kind_and_station_serde() {
        for kind in AgentKind::ALL {
            let s = serde_json::to_string(kind).unwrap();
            let back: AgentKind = serde_json::from_str(&s).unwrap();
            assert_eq!(back, *kind);
        }
        assert_eq!(
            serde_json::from_str::<AgentKind>("\"master\"").unwrap(),
            AgentKind::Root
        );
        for station in MeshStation::ALL {
            let s = serde_json::to_string(station).unwrap();
            let back: MeshStation = serde_json::from_str(&s).unwrap();
            assert_eq!(back, *station);
        }
    }
}
