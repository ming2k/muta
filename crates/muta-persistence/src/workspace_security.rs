//! Durable workspace trust for project-supplied assets and configurations.
//!
//! Governs whether project-authored skills, MCP servers, hooks, and rules are
//! trusted to load for a given workspace.
//!
//! Trust is strictly content-bound via per-domain SHA-256 digests.
//! If project assets change (e.g. via git pull/checkout), trust drops back to
//! Quarantined until explicitly reviewed again.

use crate::{fsutil, paths};
use muta_contracts::{TrustDomain, WorkspaceSecuritySnapshot, WorkspaceTrustState};
use serde::{Deserialize, Serialize};

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 2;

const MCP_PATHS: &[&str] = &[".muta/mcp.json"];

const SKILLS_PATHS: &[&str] = &[".muta/skills", ".agents/skills", ".claude/skills", "skills"];

const HOOK_PATHS: &[&str] = &[".muta/hooks"];

const RULE_PATHS: &[&str] = &[
    ".muta/commands",
    "AGENTS.md",
    ".cursorrules",
    ".windsurfrules",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WorkspaceRecord {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    domain_digests: BTreeMap<String, String>,
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
        let state_for = |domain| match domain_digest(&root, domain) {
            Ok(current) => trust_state(
                current.as_deref(),
                record.domain_digests.get(domain.as_str()),
            ),
            Err(error) => {
                tracing::warn!(
                    %error,
                    workspace = %root.display(),
                    domain = domain.as_str(),
                    "cannot attest project asset domain; quarantining it"
                );
                if record.domain_digests.contains_key(domain.as_str()) {
                    WorkspaceTrustState::Changed
                } else {
                    WorkspaceTrustState::Quarantined
                }
            }
        };

        WorkspaceSecuritySnapshot {
            root: key,
            mcp: state_for(TrustDomain::Mcp),
            skills: state_for(TrustDomain::Skills),
            hooks: state_for(TrustDomain::Hooks),
            rules: state_for(TrustDomain::Rules),
            roots: state_for(TrustDomain::Roots),
        }
    }

    /// Trust one concrete project asset domain.
    pub fn trust_domain(&self, workspace: &Path, domain: TrustDomain) -> Result<bool, String> {
        Ok(!self.trust_domains(workspace, &[domain])?.is_empty())
    }

    /// Atomically trust every present domain in `domains`.
    ///
    /// Digests are computed before the state lock is taken. If any domain
    /// cannot be attested, no grant is persisted.
    pub fn trust_domains(
        &self,
        workspace: &Path,
        domains: &[TrustDomain],
    ) -> Result<Vec<TrustDomain>, String> {
        let root = workspace_identity(workspace);
        let key = canonical_string(&root);
        let mut digests = Vec::new();
        for &domain in domains {
            if let Some(digest) = domain_digest(&root, domain)? {
                digests.push((domain, digest));
            }
        }
        if digests.is_empty() {
            return Ok(Vec::new());
        }

        let _lock = fsutil::FileLock::acquire(&self.file).map_err(|error| {
            format!(
                "cannot lock workspace security state '{}': {error}",
                self.file.display()
            )
        })?;
        let mut state = self.read_state_for_update()?;
        let record = state.workspaces.entry(key).or_default();
        for (domain, digest) in &digests {
            record
                .domain_digests
                .insert(domain.as_str().to_string(), digest.clone());
        }
        self.persist(&state)?;
        Ok(digests.into_iter().map(|(domain, _)| domain).collect())
    }

    /// Revoke every project asset grant for a workspace.
    pub fn revoke_workspace(&self, workspace: &Path) -> Result<bool, String> {
        let key = canonical_string(&workspace_identity(workspace));
        let _lock = fsutil::FileLock::acquire(&self.file).map_err(|error| {
            format!(
                "cannot lock workspace security state '{}': {error}",
                self.file.display()
            )
        })?;
        let mut state = self.read_state_for_update()?;
        let changed = state.workspaces.remove(&key).is_some();
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

    /// Read state for an explicit new grant/revocation. Version 1 carried the
    /// retired aggregate `extensions_digest`; it cannot be translated into
    /// independent domain authority, so an explicit mutation securely replaces
    /// it with an empty version-2 store.
    fn read_state_for_update(&self) -> Result<PersistedWorkspaceSecurity, String> {
        match self.read_state() {
            Ok(state) => Ok(state),
            Err(error) => {
                let legacy_v1 = std::fs::read_to_string(&self.file)
                    .ok()
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                    .and_then(|value| value.get("version").and_then(|v| v.as_u64()))
                    == Some(1);
                if legacy_v1 {
                    tracing::info!(
                        path = %self.file.display(),
                        "discarding aggregate workspace trust while recording a new domain decision"
                    );
                    Ok(PersistedWorkspaceSecurity::default())
                } else {
                    Err(error)
                }
            }
        }
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

fn workspace_identity(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn canonical_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn trust_state(current: Option<&str>, trusted: Option<&String>) -> WorkspaceTrustState {
    match (current, trusted.map(String::as_str)) {
        (None, _) => WorkspaceTrustState::Absent,
        (Some(current), Some(saved)) if current == saved => WorkspaceTrustState::Trusted,
        (Some(_), Some(_)) => WorkspaceTrustState::Changed,
        (Some(_), None) => WorkspaceTrustState::Quarantined,
    }
}

fn domain_paths(domain: TrustDomain) -> &'static [&'static str] {
    match domain {
        TrustDomain::Mcp => MCP_PATHS,
        TrustDomain::Skills => SKILLS_PATHS,
        TrustDomain::Hooks => HOOK_PATHS,
        TrustDomain::Rules => RULE_PATHS,
        TrustDomain::Roots => &[],
    }
}

fn domain_digest(workspace: &Path, domain: TrustDomain) -> Result<Option<String>, String> {
    let mut files = Vec::new();
    for relative in domain_paths(domain) {
        let entry_path = workspace.join(relative);
        collect_asset_files(workspace, &entry_path, &mut files)?;
    }

    let config_projection = match domain {
        TrustDomain::Mcp => project_config_projection(workspace, "mcp")?,
        TrustDomain::Hooks => project_config_projection(workspace, "hooks")?,
        TrustDomain::Roots => project_config_projection(workspace, "workspace")?,
        TrustDomain::Skills | TrustDomain::Rules => None,
    };

    if files.is_empty() && config_projection.is_none() {
        return Ok(None);
    }
    compute_files_digest(files, config_projection)
}

fn compute_files_digest(
    mut files: Vec<(String, PathBuf)>,
    config_projection: Option<Vec<u8>>,
) -> Result<Option<String>, String> {
    files.sort();
    let mut hasher = Sha256::new();
    for (rel, abs) in files {
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        let meta = std::fs::symlink_metadata(&abs).map_err(|error| {
            format!(
                "cannot inspect project asset path '{}': {error}",
                abs.display()
            )
        })?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "workspace asset path '{}' is a symlink; symlinked asset content cannot be trusted",
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
                "cannot read project asset file content '{}': {error}",
                abs.display()
            )
        })?;
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update([0]);
        hasher.update(&bytes);
        hasher.update([0xff]);
    }
    if let Some(bytes) = config_projection {
        hasher.update(b".muta/config.toml#projection");
        hasher.update([0]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update([0]);
        hasher.update(bytes);
        hasher.update([0xff]);
    }
    Ok(Some(format!("{:x}", hasher.finalize())))
}

/// Serialize only one project-config table into the domain digest. This keeps
/// a hook-only edit from invalidating an MCP grant (and vice versa) even though
/// both declarations share `.muta/config.toml`.
fn project_config_projection(workspace: &Path, key: &str) -> Result<Option<Vec<u8>>, String> {
    let path = workspace.join(".muta/config.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot read project configuration '{}': {error}",
                path.display()
            ));
        }
    };
    let parsed = toml::from_str::<toml::Value>(&text).map_err(|error| {
        format!(
            "cannot attest project configuration '{}': {error}",
            path.display()
        )
    })?;
    let Some(value) = parsed.get(key) else {
        return Ok(None);
    };
    serde_json::to_vec(value)
        .map(Some)
        .map_err(|error| format!("cannot serialize project [{key}] contribution: {error}"))
}

fn collect_asset_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    let meta = match std::fs::symlink_metadata(current) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "cannot inspect project asset path '{}': {error}",
                current.display()
            ));
        }
    };
    if meta.file_type().is_symlink() {
        return Err(format!(
            "workspace asset path '{}' is a symlink; symlinked asset content cannot be trusted",
            current.display()
        ));
    }
    if meta.is_file() {
        let rel = current
            .strip_prefix(root)
            .map_err(|error| {
                format!(
                    "project asset path '{}' escaped root '{}': {error}",
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
                "cannot read project asset directory '{}': {error}",
                current.display()
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "cannot read project asset directory entry in '{}': {error}",
                    current.display()
                )
            })?;
            collect_asset_files(root, &entry.path(), out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_content_is_quarantined_then_invalidated_by_change() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
        assert_eq!(store.snapshot(root).skills, WorkspaceTrustState::Absent);

        std::fs::create_dir_all(root.join(".muta/skills/demo")).unwrap();
        std::fs::write(root.join(".muta/skills/demo/SKILL.md"), "one").unwrap();
        assert_eq!(
            store.snapshot(root).skills,
            WorkspaceTrustState::Quarantined
        );
        assert!(store.trust_domain(root, TrustDomain::Skills).unwrap());
        assert_eq!(store.snapshot(root).skills, WorkspaceTrustState::Trusted);

        std::fs::write(root.join(".muta/skills/demo/SKILL.md"), "two").unwrap();
        assert_eq!(store.snapshot(root).skills, WorkspaceTrustState::Changed);
    }

    #[cfg(unix)]
    #[test]
    fn executable_mode_change_revokes_trust() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let hook = root.join(".muta/hooks/check.sh");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&hook, permissions).unwrap();

        let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
        assert!(store.trust_domain(root, TrustDomain::Hooks).unwrap());
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&hook, permissions).unwrap();
        assert_eq!(store.snapshot(root).hooks, WorkspaceTrustState::Changed);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_domain_content_cannot_be_trusted() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let outside = root.join("outside-skill.md");
        std::fs::write(&outside, "mutable target").unwrap();
        std::fs::create_dir_all(root.join(".muta/skills/demo")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join(".muta/skills/demo/SKILL.md")).unwrap();

        let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
        let error = store.trust_domain(root, TrustDomain::Skills).unwrap_err();
        assert!(error.contains("symlink"));
        assert_eq!(
            store.snapshot(root).skills,
            WorkspaceTrustState::Quarantined
        );
    }

    #[test]
    fn domains_are_granted_persisted_and_revoked_independently() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let file = root.join("state/workspace_security.json");
        let store = WorkspaceSecurityStore::load_from(file.clone());

        std::fs::create_dir_all(root.join(".muta/skills/demo")).unwrap();
        std::fs::write(root.join(".muta/skills/demo/SKILL.md"), "skill body").unwrap();
        std::fs::write(root.join(".muta/mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
        std::fs::write(
            root.join(".muta/config.toml"),
            "[[hooks]]\nevent = \"SessionStart\"\ncommand = \"echo ready\"\n",
        )
        .unwrap();

        let snap = store.snapshot(root);
        assert_eq!(snap.mcp, WorkspaceTrustState::Quarantined);
        assert_eq!(snap.skills, WorkspaceTrustState::Quarantined);
        assert_eq!(snap.hooks, WorkspaceTrustState::Quarantined);
        assert_eq!(snap.rules, WorkspaceTrustState::Absent);

        assert!(store.trust_domain(root, TrustDomain::Mcp).unwrap());
        let snap = store.snapshot(root);
        assert_eq!(snap.mcp, WorkspaceTrustState::Trusted);
        assert_eq!(snap.skills, WorkspaceTrustState::Quarantined);

        let granted = store.trust_domains(root, &TrustDomain::ALL).unwrap();
        assert_eq!(
            granted,
            vec![TrustDomain::Mcp, TrustDomain::Skills, TrustDomain::Hooks]
        );
        let reloaded = WorkspaceSecurityStore::load_from(file);
        let snap = reloaded.snapshot(root);
        assert_eq!(snap.aggregate(), WorkspaceTrustState::Trusted);
        assert_eq!(snap.mcp, WorkspaceTrustState::Trusted);
        assert_eq!(snap.skills, WorkspaceTrustState::Trusted);
        assert_eq!(snap.hooks, WorkspaceTrustState::Trusted);

        assert!(reloaded.revoke_workspace(root).unwrap());
        let snap = reloaded.snapshot(root);
        assert_eq!(snap.mcp, WorkspaceTrustState::Quarantined);
        assert_eq!(snap.skills, WorkspaceTrustState::Quarantined);
        assert_eq!(snap.hooks, WorkspaceTrustState::Quarantined);
    }

    #[test]
    fn config_projections_do_not_cross_invalidate_domains() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join(".muta")).unwrap();
        let config = root.join(".muta/config.toml");
        std::fs::write(
            &config,
            "[mcp.demo]\ncommand = [\"demo\"]\n\n[[hooks]]\nevent = \"SessionStart\"\ncommand = \"echo one\"\n",
        )
        .unwrap();
        let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
        store
            .trust_domains(root, &[TrustDomain::Mcp, TrustDomain::Hooks])
            .unwrap();

        std::fs::write(
            &config,
            "[mcp.demo]\ncommand = [\"demo\"]\n\n[[hooks]]\nevent = \"SessionStart\"\ncommand = \"echo two\"\n",
        )
        .unwrap();
        let snap = store.snapshot(root);
        assert_eq!(snap.mcp, WorkspaceTrustState::Trusted);
        assert_eq!(snap.hooks, WorkspaceTrustState::Changed);
    }

    #[test]
    fn roots_domain_projection_tracks_workspace_table() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join(".muta")).unwrap();
        let config = root.join(".muta/config.toml");
        std::fs::write(&config, "[workspace]\nadditional_roots = [\"../optics\"]\n").unwrap();
        let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
        let snap = store.snapshot(root);
        assert_eq!(snap.roots, WorkspaceTrustState::Quarantined);

        store.trust_domain(root, TrustDomain::Roots).unwrap();
        let snap = store.snapshot(root);
        assert_eq!(snap.roots, WorkspaceTrustState::Trusted);

        std::fs::write(
            &config,
            "[workspace]\nadditional_roots = [\"../optics\", \"../backend\"]\n",
        )
        .unwrap();
        let snap = store.snapshot(root);
        assert_eq!(snap.roots, WorkspaceTrustState::Changed);
    }
}
