//! Workspace trust vocabulary for project-supplied assets and configurations.
//!
//! Controls whether project-authored contributions (skills, MCP servers, hooks,
//! AGENTS.md instructions, and workspace config) are loaded into the runtime.
//!
//! Distinct and decoupled from AI runtime tool permissions, which are governed
//! purely by the Tool Hazard model (`HazardLevel` and `PermissionStore`).

use serde::{Deserialize, Serialize};

/// Trust state for one project-authored asset domain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum WorkspaceTrustState {
    /// The workspace declares no project-level contributions (skills, MCP, hooks, AGENTS.md).
    #[default]
    Absent,
    /// Contributions exist in the workspace, but have not been explicitly trusted by the user.
    Quarantined,
    /// The exact current content digest of contributions was explicitly trusted by the user.
    Trusted,
    /// Contributions were previously trusted, but their content/digest has changed.
    Changed,
}

impl WorkspaceTrustState {
    pub fn is_trusted(self) -> bool {
        matches!(self, Self::Trusted)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Quarantined => "quarantined",
            Self::Trusted => "trusted",
            Self::Changed => "changed",
        }
    }
}

/// Concrete domains for project asset trust.
///
/// `all` is deliberately not a domain. It is a command-layer selection that
/// expands to [`TrustDomain::ALL`]. Persisting an aggregate grant would create
/// a second source of truth and make a concrete domain impossible to revoke.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum TrustDomain {
    /// Trust project-level Model Context Protocol (MCP) server definitions.
    Mcp,
    /// Trust project-level custom skills.
    Skills,
    /// Trust project-level lifecycle hook definitions and hook assets.
    Hooks,
    /// Trust project-authored instructions and slash-command templates.
    Rules,
}

impl TrustDomain {
    pub const ALL: [Self; 4] = [Self::Mcp, Self::Skills, Self::Hooks, Self::Rules];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Skills => "skills",
            Self::Hooks => "hooks",
            Self::Rules => "rules",
        }
    }
}

/// First-class security state attached to every harness snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct WorkspaceSecuritySnapshot {
    /// Canonical exact workspace root used for persisted decisions.
    pub root: String,
    /// Trust status for MCP domain.
    #[serde(default)]
    pub mcp: WorkspaceTrustState,
    /// Trust status for Skills domain.
    #[serde(default)]
    pub skills: WorkspaceTrustState,
    /// Trust status for lifecycle hooks.
    #[serde(default)]
    pub hooks: WorkspaceTrustState,
    /// Trust status for project instructions and slash commands.
    #[serde(default)]
    pub rules: WorkspaceTrustState,
}

impl WorkspaceSecuritySnapshot {
    pub fn new(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            mcp: WorkspaceTrustState::Absent,
            skills: WorkspaceTrustState::Absent,
            hooks: WorkspaceTrustState::Absent,
            rules: WorkspaceTrustState::Absent,
        }
    }

    pub fn state(&self, domain: TrustDomain) -> WorkspaceTrustState {
        match domain {
            TrustDomain::Mcp => self.mcp,
            TrustDomain::Skills => self.skills,
            TrustDomain::Hooks => self.hooks,
            TrustDomain::Rules => self.rules,
        }
    }

    pub fn is_trusted(&self, domain: TrustDomain) -> bool {
        self.state(domain).is_trusted()
    }

    /// Aggregate state for display only. It never participates in admission.
    pub fn aggregate(&self) -> WorkspaceTrustState {
        let states = [self.mcp, self.skills, self.hooks, self.rules];
        let present = states
            .into_iter()
            .filter(|state| *state != WorkspaceTrustState::Absent)
            .collect::<Vec<_>>();
        if present.is_empty() {
            WorkspaceTrustState::Absent
        } else if present
            .iter()
            .all(|state| *state == WorkspaceTrustState::Trusted)
        {
            WorkspaceTrustState::Trusted
        } else if present.contains(&WorkspaceTrustState::Changed) {
            WorkspaceTrustState::Changed
        } else {
            WorkspaceTrustState::Quarantined
        }
    }
}
