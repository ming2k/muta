//! Durable workspace trust for project-supplied assets and configurations.
//!
//! Governs whether project-authored skills, MCP servers, hooks, configuration,
//! and AGENTS.md instructions are trusted to load for a given workspace.
//!
//! Trust is strictly content-bound via SHA-256 digests over all project contribution paths.
//! If project assets change (e.g. via git pull/checkout), trust drops back to
//! Quarantined until explicitly reviewed again.

use crate::{fsutil, paths};
use muta_contracts::{WorkspaceSecuritySnapshot, WorkspaceTrustState};
use serde::{Deserialize, Serialize};

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 1;
const EXTENSION_PATHS: &[&str] = &[
    // Hash the whole control-plane tree, skills, hooks, MCP definitions, and project instructions
    ".muta",
    ".agents/skills",
    ".claude/skills",
    "skills",
    "AGENTS.md",
    ".cursorrules",
    ".windsurfrules",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WorkspaceRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extensions_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedWorkspaceSecurity {
    version: u32,
    #[serde(default)]
    workspaces: BTreeMap<String, WorkspaceRecord>,
}

impl Default for PersistedWorkspaceSecurity {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            workspaces: BTreeMap::new(),
        }
    }
}

/// Durable store for workspace trust decisions.
#[derive(Debug)]
pub struct WorkspaceSecurityStore {
    file: PathBuf,
}

impl WorkspaceSecurityStore {
    pub fn load() -> Self {
        Self::load_from(paths::get().workspace_security_file())
    }

    pub fn load_from(file: PathBuf) -> Self {
        Self { file }
    }

    /// Compute the current, content-aware trust state for a workspace.
    pub fn snapshot(&self, workspace: &Path) -> WorkspaceSecuritySnapshot {
        let root = workspace_identity(workspace);
        let key = canonical_string(&root);
        let state = self.read_state().unwrap_or_else(|error| {
            tracing::warn!(%error, path = %self.file.display(), "workspace security state is unreadable; failing closed");
            PersistedWorkspaceSecurity::default()
        });
        let record = state.workspaces.get(&key).cloned().unwrap_or_default();
        let current_digest = match extension_digest(&root) {
            Ok(digest) => digest,
            Err(error) => {
                tracing::warn!(%error, workspace = %root.display(), "cannot attest project contribution content; quarantining it");
                return WorkspaceSecuritySnapshot {
                    root: key,
                    trust: if record.extensions_digest.is_some() {
                        WorkspaceTrustState::Changed
                    } else {
                        WorkspaceTrustState::Quarantined
                    },
                    extensions: if record.extensions_digest.is_some() {
                        WorkspaceTrustState::Changed
                    } else {
                        WorkspaceTrustState::Quarantined
                    },
                };
            }
        };
        let trust = match (current_digest, record.extensions_digest.as_deref()) {
            (None, _) => WorkspaceTrustState::Absent,
            (Some(current), Some(trusted)) if current == trusted => {
                WorkspaceTrustState::Trusted
            }
            (Some(_), Some(_)) => WorkspaceTrustState::Changed,
            (Some(_), None) => WorkspaceTrustState::Quarantined,
        };
        WorkspaceSecuritySnapshot {
            root: key,
            trust,
            extensions: trust,
        }
    }

    /// Trust the exact current project contribution content (skills, MCP, hooks, AGENTS.md).
    /// Returns `false` when the workspace declares no contributions.
    pub fn trust_workspace(&self, workspace: &Path) -> Result<bool, String> {
        let root = workspace_identity(workspace);
        let Some(digest) = extension_digest(&root)? else {
            return Ok(false);
        };
        let key = canonical_string(&root);
        let _lock = fsutil::FileLock::acquire(&self.file).map_err(|error| {
            format!(
                "cannot lock workspace security state '{}': {error}",
                self.file.display()
            )
        })?;
        let mut state = self.read_state()?;
        state.workspaces.entry(key).or_default().extensions_digest = Some(digest);
        self.persist(&state)?;
        Ok(true)
    }

    /// Alias for `trust_workspace`.
    pub fn trust_extensions(&self, workspace: &Path) -> Result<bool, String> {
        self.trust_workspace(workspace)
    }

    /// Revoke trust for the project contributions of a workspace.
    pub fn untrust_workspace(&self, workspace: &Path) -> Result<bool, String> {
        let key = canonical_string(&workspace_identity(workspace));
        let _lock = fsutil::FileLock::acquire(&self.file).map_err(|error| {
            format!(
                "cannot lock workspace security state '{}': {error}",
                self.file.display()
            )
        })?;
        let mut state = self.read_state()?;
        let changed = state
            .workspaces
            .get_mut(&key)
            .is_some_and(|record| record.extensions_digest.take().is_some());
        if changed {
            self.persist(&state)?;
        }
        Ok(changed)
    }

    /// Alias for `untrust_workspace`.
    pub fn untrust_extensions(&self, workspace: &Path) -> Result<bool, String> {
        self.untrust_workspace(workspace)
    }

    fn read_state(&self) -> Result<PersistedWorkspaceSecurity, String> {
        let text = match std::fs::read_to_string(&self.file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PersistedWorkspaceSecurity::default());
            }
            Err(error) => {
                return Err(format!(
                    "cannot read workspace security state '{}': {error}",
                    self.file.display()
                ));
            }
        };
        let state = serde_json::from_str::<PersistedWorkspaceSecurity>(&text).map_err(|error| {
            format!(
                "workspace security state '{}' is invalid JSON: {error}",
                self.file.display()
            )
        })?;
        if state.version != CURRENT_VERSION {
            return Err(format!(
                "workspace security state '{}' has unsupported version {}; expected {}",
                self.file.display(),
                state.version,
                CURRENT_VERSION
            ));
        }
        Ok(state)
    }

    fn persist(&self, state: &PersistedWorkspaceSecurity) -> Result<(), String> {
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "cannot create parent directory '{}': {error}",
                    parent.display()
                )
            })?;
        }
        let text = serde_json::to_string_pretty(state).map_err(|error| {
            format!(
                "cannot serialize workspace security state '{}': {error}",
                self.file.display()
            )
        })?;
        let temp = format!(
            "{}.tmp.{}",
            self.file.display(),
            uuid::Uuid::new_v4().simple()
        );
        std::fs::write(&temp, text).map_err(|error| {
            format!(
                "cannot write temporary workspace security state '{temp}': {error}"
            )
        })?;
        std::fs::rename(&temp, &self.file).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            format!(
                "cannot replace workspace security state '{}': {error}",
                self.file.display()
            )
        })?;
        Ok(())
    }
}

fn workspace_identity(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn canonical_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn extension_digest(workspace: &Path) -> Result<Option<String>, String> {
    let mut files = Vec::new();
    for relative in EXTENSION_PATHS {
        let entry_path = workspace.join(relative);
        collect_extension_files(workspace, &entry_path, &mut files)?;
    }
    if files.is_empty() {
        return Ok(None);
    }
    files.sort();
    let mut hasher = Sha256::new();
    for (rel, abs) in files {
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        let meta = std::fs::symlink_metadata(&abs).map_err(|error| {
            format!(
                "cannot inspect extension path '{}': {error}",
                abs.display()
            )
        })?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "workspace extension path '{}' is a symlink; symlinked extension content cannot be trusted",
                abs.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode();
            hasher.update(mode.to_le_bytes());
            hasher.update([0]);
        }
        let bytes = std::fs::read(&abs).map_err(|error| {
            format!(
                "cannot read extension file content '{}': {error}",
                abs.display()
            )
        })?;
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update([0]);
        hasher.update(&bytes);
        hasher.update([0xff]);
    }
    Ok(Some(format!("{:x}", hasher.finalize())))
}

fn collect_extension_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    let meta = match std::fs::symlink_metadata(current) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "cannot inspect extension path '{}': {error}",
                current.display()
            ));
        }
    };
    if meta.file_type().is_symlink() {
        return Err(format!(
            "workspace extension path '{}' is a symlink; symlinked extension content cannot be trusted",
            current.display()
        ));
    }
    if meta.is_file() {
        let rel = current
            .strip_prefix(root)
            .map_err(|error| {
                format!(
                    "extension path '{}' escaped root '{}': {error}",
                    current.display(),
                    root.display()
                )
            })?
            .to_string_lossy()
            .to_string();
        out.push((rel, current.to_path_buf()));
        return Ok(());
    }
    if meta.is_dir() {
        let entries = std::fs::read_dir(current).map_err(|error| {
            format!(
                "cannot read extension directory '{}': {error}",
                current.display()
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "cannot read extension directory entry in '{}': {error}",
                    current.display()
                )
            })?;
            collect_extension_files(root, &entry.path(), out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "muta-test-trust-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extensions_are_content_bound_and_quarantined_by_default() {
        let root = scratch();
        let file = root.join("state/workspace_security.json");
        let store = WorkspaceSecurityStore::load_from(file);
        assert_eq!(
            store.snapshot(&root).trust,
            WorkspaceTrustState::Absent
        );

        std::fs::create_dir_all(root.join(".muta/skills/demo")).unwrap();
        std::fs::write(root.join(".muta/skills/demo/SKILL.md"), "one").unwrap();
        assert_eq!(
            store.snapshot(&root).trust,
            WorkspaceTrustState::Quarantined
        );
        assert!(store.trust_workspace(&root).unwrap());
        assert_eq!(
            store.snapshot(&root).trust,
            WorkspaceTrustState::Trusted
        );

        std::fs::write(root.join(".muta/skills/demo/SKILL.md"), "two").unwrap();
        let snapshot = store.snapshot(&root);
        assert_eq!(snapshot.trust, WorkspaceTrustState::Changed);
    }

    #[cfg(unix)]
    #[test]
    fn executable_mode_change_revokes_trust() {
        use std::os::unix::fs::PermissionsExt;

        let root = scratch();
        let file = root.join("state/workspace_security.json");
        let hook = root.join(".muta/hooks/check.sh");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&hook, permissions).unwrap();

        let store = WorkspaceSecurityStore::load_from(file);
        assert!(store.trust_workspace(&root).unwrap());
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&hook, permissions).unwrap();
        assert_eq!(
            store.snapshot(&root).trust,
            WorkspaceTrustState::Changed
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_extension_content_cannot_be_trusted() {
        let root = scratch();
        let file = root.join("state/workspace_security.json");
        let outside = root.join("outside-skill.md");
        std::fs::write(&outside, "mutable target").unwrap();
        std::fs::create_dir_all(root.join(".muta/skills/demo")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join(".muta/skills/demo/SKILL.md")).unwrap();

        let store = WorkspaceSecurityStore::load_from(file);
        let error = store.trust_workspace(&root).unwrap_err();
        assert!(error.contains("symlink"));
        assert_eq!(
            store.snapshot(&root).trust,
            WorkspaceTrustState::Quarantined
        );
    }
}
