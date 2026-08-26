//! Workspace trust vocabulary for project-supplied assets and configurations.
//!
//! Controls whether project-authored contributions (skills, MCP servers, hooks,
//! AGENTS.md instructions, and workspace config) are loaded into the runtime.
//!
//! Distinct and decoupled from AI runtime tool permissions, which are governed
//! purely by the Tool Hazard model (`HazardLevel` and `PermissionStore`).

use serde::{Deserialize, Serialize};

/// Trust state for project-authored contributions (skills, MCP, hooks, AGENTS.md, config).
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

/// Alias for compatibility during migration.
pub type WorkspaceExtensionsState = WorkspaceTrustState;

/// First-class security state attached to every harness snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct WorkspaceSecuritySnapshot {
    /// Canonical exact workspace root used for persisted decisions.
    pub root: String,
    /// Trust status of workspace contributions (skills, MCP, hooks, AGENTS.md).
    #[serde(default)]
    pub trust: WorkspaceTrustState,
    /// Alias field for extensions trust.
    #[serde(default)]
    pub extensions: WorkspaceTrustState,
}

impl WorkspaceSecuritySnapshot {
    pub fn new(root: impl Into<String>, trust: WorkspaceTrustState) -> Self {
        let r = root.into();
        Self {
            root: r,
            trust,
            extensions: trust,
        }
    }

    pub fn unknown(root: impl Into<String>) -> Self {
        Self::new(root, WorkspaceTrustState::Quarantined)
    }

    pub fn is_trusted(&self) -> bool {
        self.trust.is_trusted() || self.extensions.is_trusted()
    }
}

