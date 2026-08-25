//! Workspace security vocabulary shared by the runtime and frontends.
//!
//! Workspace execution authority and project-authored extensions are separate
//! axes.  Opening a directory grants neither.  Autopilot is intentionally not
//! represented here: it controls whether a missing grant may be requested
//! interactively, never whether an operation is authorised.

use serde::{Deserialize, Serialize};

/// The authority a workspace grants to ordinary agent operations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum WorkspaceExecutionProfile {
    /// No decision has been made for this workspace. Model and direct-shell
    /// work must fail preflight until the operator selects a profile.
    #[default]
    Unknown,
    /// Read-oriented posture. Side effects require individual authority rules.
    Restricted,
    /// Ordinary development inside the workspace is pre-authorised. Hard bash
    /// denies and explicit high-risk confirmations remain in force.
    Development,
}

impl WorkspaceExecutionProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Restricted => "restricted",
            Self::Development => "development",
        }
    }
}

/// Trust state for project-authored control-plane contributions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum WorkspaceExtensionsState {
    /// The workspace declares no project MCP, hooks, skills, or commands.
    #[default]
    Absent,
    /// Contributions exist but their current content has never been trusted.
    Quarantined,
    /// The exact current contribution content was explicitly trusted.
    Trusted,
    /// Contributions changed after trust and have returned to quarantine.
    Changed,
}

/// Runtime enforcement state for the physical workspace sandbox.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum WorkspaceSandboxState {
    /// The runtime cannot provide the required containment and must fail closed.
    #[default]
    Unavailable,
    /// Filesystem and shell operations are physically confined to the workspace.
    Enforced,
}

impl WorkspaceExtensionsState {
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

impl WorkspaceSandboxState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Enforced => "enforced",
        }
    }
}

/// First-class security state attached to every harness snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct WorkspaceSecuritySnapshot {
    /// Canonical exact workspace root used for persisted decisions.
    pub root: String,
    pub execution: WorkspaceExecutionProfile,
    pub extensions: WorkspaceExtensionsState,
    #[serde(default)]
    pub sandbox: WorkspaceSandboxState,
}

impl WorkspaceSecuritySnapshot {
    pub fn unknown(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            execution: WorkspaceExecutionProfile::Unknown,
            extensions: WorkspaceExtensionsState::Absent,
            sandbox: WorkspaceSandboxState::Unavailable,
        }
    }
}
