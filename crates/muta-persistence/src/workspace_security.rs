//! Durable workspace authority and project-extension trust.
//!
//! The two decisions are deliberately independent:
//! - execution authority controls ordinary workspace operations;
//! - extension trust controls project-authored MCP servers, hooks, skills, and
//!   slash commands.
//!
//! Extension trust is content-bound. A changed contribution digest is
//! quarantined automatically instead of inheriting an old path-only grant.

use crate::{fsutil, paths};
use muta_contracts::{
    WorkspaceExecutionProfile, WorkspaceExtensionsState, WorkspaceSandboxState,
    WorkspaceSecuritySnapshot,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 1;
const EXTENSION_PATHS: &[&str] = &[
    // Hash the whole control-plane tree, not only the declaration file: a
    // trusted hook/MCP command may point at `.muta/hooks/run.sh`, and changing
    // that executable must revoke the effective trust too.
    ".muta",
    ".agents/skills",
    ".claude/skills",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WorkspaceRecord {
    #[serde(default)]
    execution: WorkspaceExecutionProfile,
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

/// Durable handle used by bootstrap and the workspace/extension commands.
///
/// There is deliberately no per-handle state cache. Each mutation takes the
/// cross-process companion lock and reloads the latest file before applying
/// its change, so independent sessions cannot silently overwrite one another.
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

    /// Compute the current, content-aware security state for a workspace.
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
                tracing::warn!(%error, workspace = %root.display(), "cannot attest project extension content; quarantining it");
                return WorkspaceSecuritySnapshot {
                    root: key,
                    execution: record.execution,
                    extensions: if record.extensions_digest.is_some() {
                        WorkspaceExtensionsState::Changed
                    } else {
                        WorkspaceExtensionsState::Quarantined
                    },
                    sandbox: WorkspaceSandboxState::Unavailable,
                };
            }
        };
        let extensions = match (current_digest, record.extensions_digest.as_deref()) {
            (None, _) => WorkspaceExtensionsState::Absent,
            (Some(current), Some(trusted)) if current == trusted => {
                WorkspaceExtensionsState::Trusted
            }
            (Some(_), Some(_)) => WorkspaceExtensionsState::Changed,
            (Some(_), None) => WorkspaceExtensionsState::Quarantined,
        };
        WorkspaceSecuritySnapshot {
            root: key,
            execution: record.execution,
            extensions,
            sandbox: WorkspaceSandboxState::Unavailable,
        }
    }

    /// Set the ordinary execution profile independently of extension trust.
    pub fn set_execution(
        &self,
        workspace: &Path,
        profile: WorkspaceExecutionProfile,
    ) -> Result<WorkspaceSecuritySnapshot, String> {
        let root = workspace_identity(workspace);
        let key = canonical_string(&root);
        let _lock = fsutil::FileLock::acquire(&self.file).map_err(|error| {
            format!(
                "cannot lock workspace security state '{}': {error}",
                self.file.display()
            )
        })?;
        let mut state = self.read_state()?;
        state.workspaces.entry(key).or_default().execution = profile;
        self.persist(&state)?;
        Ok(self.snapshot(workspace))
    }

    /// Trust the exact current project contribution content. Returns `false`
    /// when the workspace declares no contributions.
    pub fn trust_extensions(&self, workspace: &Path) -> Result<bool, String> {
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

    pub fn untrust_extensions(&self, workspace: &Path) -> Result<bool, String> {
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
        fsutil::atomic_write_json(&self.file, state).map_err(|error| {
            format!(
                "cannot persist workspace security state '{}': {error}",
                self.file.display()
            )
        })
    }
}

/// Stable least-privilege workspace identity. Worktrees and explicitly opened
/// subdirectories remain separate authority masters; a grant must never
/// widen itself to a larger repository merely because `.git` exists above it.
pub fn workspace_identity(start: &Path) -> PathBuf {
    std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf())
}

fn canonical_string(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Digest paths and bytes in deterministic order without following symlinks.
fn extension_digest(root: &Path) -> Result<Option<String>, String> {
    let mut entries = Vec::<PathBuf>::new();
    for relative in EXTENSION_PATHS {
        collect_entries(root, &root.join(relative), &mut entries, true)?;
    }
    entries.sort();
    entries.dedup();
    if entries.is_empty() {
        return Ok(None);
    }
    let mut digest = Sha256::new();
    for path in entries {
        let relative = path.strip_prefix(root).unwrap_or(&path);
        digest.update(relative.to_string_lossy().as_bytes());
        digest.update([0]);
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "cannot inspect extension contribution '{}': {error}",
                path.display()
            )
        })?;
        digest.update([u8::from(metadata.permissions().readonly())]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            digest.update(metadata.permissions().mode().to_le_bytes());
        }
        match metadata {
            metadata if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "project extension contribution '{}' is a symlink; content-bound trust requires regular in-workspace files",
                    path.display()
                ));
            }
            metadata if metadata.is_file() => {
                digest.update(b"file\0");
                let bytes = std::fs::read(&path).map_err(|error| {
                    format!(
                        "cannot read extension contribution '{}': {error}",
                        path.display()
                    )
                })?;
                digest.update(bytes);
            }
            metadata if metadata.is_dir() => digest.update(b"dir\0"),
            _ => {
                return Err(format!(
                    "project extension contribution '{}' is not a regular file or directory",
                    path.display()
                ));
            }
        }
        digest.update([0xff]);
    }
    Ok(Some(hex::encode(digest.finalize())))
}

fn collect_entries(
    root: &Path,
    path: &Path,
    entries: &mut Vec<PathBuf>,
    missing_ok: bool,
) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if missing_ok && error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "cannot enumerate extension contribution '{}': {error}",
                path.display()
            ));
        }
    };
    entries.push(path.to_path_buf());
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(());
    }
    let read_dir = std::fs::read_dir(path).map_err(|error| {
        format!(
            "cannot enumerate extension directory '{}': {error}",
            path.display()
        )
    })?;
    let mut children = read_dir
        .map(|entry| {
            entry.map(|entry| entry.path()).map_err(|error| {
                format!(
                    "cannot enumerate extension directory '{}': {error}",
                    path.display()
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    children.sort();
    for child in children {
        // Every top-level input is rooted below `root`; do not follow symlinks.
        if child.starts_with(root) {
            collect_entries(root, &child, entries, false)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let root = std::env::temp_dir().join(format!("muta-security-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn execution_and_extensions_are_independent_and_content_bound() {
        let root = scratch();
        let file = root.join("state/workspace_security.json");
        let store = WorkspaceSecurityStore::load_from(file);
        assert_eq!(
            store.snapshot(&root).execution,
            WorkspaceExecutionProfile::Unknown
        );
        assert_eq!(
            store.snapshot(&root).extensions,
            WorkspaceExtensionsState::Absent
        );

        std::fs::create_dir_all(root.join(".muta/skills/demo")).unwrap();
        std::fs::write(root.join(".muta/skills/demo/SKILL.md"), "one").unwrap();
        assert_eq!(
            store.snapshot(&root).extensions,
            WorkspaceExtensionsState::Quarantined
        );
        assert!(store.trust_extensions(&root).unwrap());
        assert_eq!(
            store.snapshot(&root).extensions,
            WorkspaceExtensionsState::Trusted
        );

        store
            .set_execution(&root, WorkspaceExecutionProfile::Development)
            .unwrap();
        std::fs::write(root.join(".muta/skills/demo/SKILL.md"), "two").unwrap();
        let snapshot = store.snapshot(&root);
        assert_eq!(snapshot.execution, WorkspaceExecutionProfile::Development);
        assert_eq!(snapshot.extensions, WorkspaceExtensionsState::Changed);
    }

    #[test]
    fn state_round_trips_without_legacy_path_only_trust() {
        let root = scratch();
        let file = root.join("state/workspace_security.json");
        WorkspaceSecurityStore::load_from(file.clone())
            .set_execution(&root, WorkspaceExecutionProfile::Restricted)
            .unwrap();
        let reloaded = WorkspaceSecurityStore::load_from(file);
        assert_eq!(
            reloaded.snapshot(&root).execution,
            WorkspaceExecutionProfile::Restricted
        );
    }

    #[test]
    fn independent_store_handles_merge_under_file_lock() {
        let state_root = scratch();
        let file = state_root.join("state/workspace_security.json");
        let first_workspace = scratch();
        let second_workspace = scratch();
        let first = WorkspaceSecurityStore::load_from(file.clone());
        let second = WorkspaceSecurityStore::load_from(file.clone());

        first
            .set_execution(&first_workspace, WorkspaceExecutionProfile::Restricted)
            .unwrap();
        second
            .set_execution(&second_workspace, WorkspaceExecutionProfile::Development)
            .unwrap();

        let reloaded = WorkspaceSecurityStore::load_from(file);
        assert_eq!(
            reloaded.snapshot(&first_workspace).execution,
            WorkspaceExecutionProfile::Restricted
        );
        assert_eq!(
            reloaded.snapshot(&second_workspace).execution,
            WorkspaceExecutionProfile::Development
        );
    }

    #[cfg(unix)]
    #[test]
    fn executable_mode_change_revokes_extension_trust() {
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
        assert!(store.trust_extensions(&root).unwrap());
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&hook, permissions).unwrap();
        assert_eq!(
            store.snapshot(&root).extensions,
            WorkspaceExtensionsState::Changed
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
        let error = store.trust_extensions(&root).unwrap_err();
        assert!(error.contains("symlink"));
        assert_eq!(
            store.snapshot(&root).extensions,
            WorkspaceExtensionsState::Quarantined
        );
    }
}
