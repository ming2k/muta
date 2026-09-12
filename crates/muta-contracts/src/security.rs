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
    /// Trust project-authored instructions and rules (`AGENTS.md`, rules).
    Instructions,
    /// Trust project-level external workspace roots (`[workspace].additional_roots`).
    #[serde(rename = "ex_workspace", alias = "ex-workspace")]
    ExWorkspace,
}

impl TrustDomain {
    pub const ALL: [Self; 5] = [
        Self::Mcp,
        Self::Skills,
        Self::Hooks,
        Self::Instructions,
        Self::ExWorkspace,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Skills => "skills",
            Self::Hooks => "hooks",
            Self::Instructions => "instructions",
            Self::ExWorkspace => "ex-workspace",
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
    /// Trust status for project instructions (AGENTS.md).
    #[serde(default, alias = "rules")]
    pub instructions: WorkspaceTrustState,
    /// Trust status for project-declared external workspace roots.
    #[serde(default)]
    pub ex_workspace: WorkspaceTrustState,
}

impl WorkspaceSecuritySnapshot {
    pub fn new(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            mcp: WorkspaceTrustState::Absent,
            skills: WorkspaceTrustState::Absent,
            hooks: WorkspaceTrustState::Absent,
            instructions: WorkspaceTrustState::Absent,
            ex_workspace: WorkspaceTrustState::Absent,
        }
    }

    pub fn state(&self, domain: TrustDomain) -> WorkspaceTrustState {
        match domain {
            TrustDomain::Mcp => self.mcp,
            TrustDomain::Skills => self.skills,
            TrustDomain::Hooks => self.hooks,
            TrustDomain::Instructions => self.instructions,
            TrustDomain::ExWorkspace => self.ex_workspace,
        }
    }

    pub fn is_trusted(&self, domain: TrustDomain) -> bool {
        self.state(domain).is_trusted()
    }

    /// Aggregate state for display only. It never participates in admission.
    pub fn aggregate(&self) -> WorkspaceTrustState {
        let states = [
            self.mcp,
            self.skills,
            self.hooks,
            self.instructions,
            self.ex_workspace,
        ];
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

/// Specification of an external capability unit subject to attestation (ADR-0243).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ts_rs::TS,
)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum AssetSpec {
    /// Physical OS child process (e.g. Stdio MCP server, lifecycle hooks).
    Process {
        command: Vec<String>,
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        env: std::collections::BTreeMap<String, String>,
    },
    /// Remote network service endpoint (e.g. Streamable HTTP/SSE MCP server).
    RemoteEndpoint {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        headers: std::collections::BTreeMap<String, String>,
    },
}

impl AssetSpec {
    /// Compute the canonical SHA-256 cryptographic fingerprint of this specification.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        match self {
            Self::Process { command, env } => {
                hasher.update(b"process\0");
                for arg in command {
                    hasher.update(arg.as_bytes());
                    hasher.update(b"\0");
                }
                for (k, v) in env {
                    hasher.update(k.as_bytes());
                    hasher.update(b"=");
                    hasher.update(v.as_bytes());
                    hasher.update(b"\0");
                }
            }
            Self::RemoteEndpoint { url, headers } => {
                hasher.update(b"endpoint\0");
                hasher.update(url.as_bytes());
                hasher.update(b"\0");
                for (k, v) in headers {
                    hasher.update(k.as_bytes());
                    hasher.update(b":");
                    hasher.update(v.as_bytes());
                    hasher.update(b"\0");
                }
            }
        }
        format!("{:x}", hasher.finalize())
    }

    /// User-friendly one-line summary of this asset.
    pub fn summary(&self) -> String {
        match self {
            Self::Process { command, .. } => command.join(" "),
            Self::RemoteEndpoint { url, .. } => url.clone(),
        }
    }
}

/// Attestation status for an asset in the universal ledger (ADR-0243).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum AttestationStatus {
    /// Asset is quarantined and blocked from physical execution until user attestation.
    #[default]
    Quarantined,
    /// Exact asset fingerprint has been attested and trusted by the user.
    Trusted,
    /// Asset is temporarily allowed for the active session lifetime only.
    SessionEphemeral,
}

impl AttestationStatus {
    pub fn is_trusted(self) -> bool {
        matches!(self, Self::Trusted | Self::SessionEphemeral)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quarantined => "quarantined",
            Self::Trusted => "trusted",
            Self::SessionEphemeral => "session_ephemeral",
        }
    }
}
