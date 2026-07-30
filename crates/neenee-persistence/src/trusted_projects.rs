//! Project-scope trust store for external tools (ADR-0085 §5).
//!
//! A project's `.neenee/config.toml` may declare `[mcp.*]` servers whose
//! `command` executes a process. Loading those automatically from a cloned or
//! vendored working tree is the same class of hazard as an npm `postinstall`
//! script or a git hook: a malicious repo should not get code execution merely
//! because the user opened it. This store records the project roots the user
//! has **explicitly trusted** via `/trust`; only those roots' project-scope
//! external tools auto-load.
//!
//! The store is a JSON set of absolute, canonical project-root paths under
//! `$XDG_STATE_HOME/neenee/trusted_projects.json`. It is program-generated
//! trust state (not user preference), so it sits in state alongside
//! `history.json` / `provider_usage.json`. Loss is safe: the store reverts to
//! empty and projects re-prompt for trust.
//!
//! Only *project-scope* config is gated. The global `config.toml` is
//! user-authored on the user's own machine and is trusted unconditionally.

use crate::fsutil;
use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// The on-disk trust set. Serialized as a JSON array of canonical path strings
/// for human readability and audit (`cat` shows real paths, not opaque hashes).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustedProjects {
    /// Canonicalized absolute project-root paths the user has trusted.
    #[serde(default)]
    roots: HashSet<String>,
}

impl TrustedProjects {
    /// Load from the well-known state file. Returns an empty store when the
    /// file is missing or unreadable (safe default: re-prompt for trust).
    pub fn load() -> Self {
        Self::load_from(&paths::get().trusted_projects_file())
    }

    /// Load from an explicit path (testable form; `load()` is the well-known-
    /// path wrapper). Missing/unreadable → empty store.
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save back to the well-known state file. Best-effort: a write failure is
    /// logged but does not propagate, since the in-memory set is the source of
    /// truth for the current session and the next launch re-derives from disk.
    fn save(&self) {
        self.save_to(&paths::get().trusted_projects_file());
    }

    /// Save to an explicit path (testable form).
    fn save_to(&self, path: &Path) {
        if let Err(err) = fsutil::atomic_write_json(path, self) {
            tracing::warn!(error = %err, "failed to persist trusted_projects.json");
        }
    }

    /// Canonicalize a project root for stable comparison across invocations
    /// (resolves symlinks / `..`). Falls back to the raw path if
    /// canonicalization fails (e.g. the path is gone), so a trust grant made
    /// while the dir exists still matches after a checkout moves things.
    fn canon(root: &Path) -> String {
        std::fs::canonicalize(root)
            .ok()
            .and_then(|p| p.to_str().map(str::to_string))
            .unwrap_or_else(|| root.to_string_lossy().into_owned())
    }

    /// Whether `project_root` is in the trusted set.
    pub fn contains(&self, project_root: &Path) -> bool {
        self.roots.contains(&Self::canon(project_root))
    }

    /// Add `project_root` to the trusted set and persist. Returns `true` if it
    /// was newly added (false if already trusted — idempotent).
    pub fn add(&mut self, project_root: &Path) -> bool {
        let key = Self::canon(project_root);
        let inserted = self.roots.insert(key);
        if inserted {
            self.save();
        }
        inserted
    }

    /// Remove `project_root` from the trusted set and persist. Returns `true`
    /// if it was present (false if not trusted — idempotent).
    pub fn remove(&mut self, project_root: &Path) -> bool {
        let key = Self::canon(project_root);
        let removed = self.roots.remove(&key);
        if removed {
            self.save();
        }
        removed
    }

    /// Sorted snapshot of the trusted roots (canonical path strings).
    #[cfg(test)]
    fn roots_sorted(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.roots.iter().map(String::as_str).collect();
        v.sort_unstable();
        v
    }
}

/// Owned handle pairing a loaded trust store with thread-safe mutation,
/// exposing the gated decision the bootstrap/reload path needs: "should this
/// project's external tools auto-load?"
///
/// Held as a single value for the session; share it by wrapping in `Arc` when
/// multiple owners need to mutate it (`/trust` / `/untrust`).
#[derive(Debug, Default)]
pub struct TrustGate {
    inner: std::sync::Mutex<TrustedProjects>,
}

impl TrustGate {
    /// Load the trust store from disk.
    pub fn load() -> Self {
        Self {
            inner: std::sync::Mutex::new(TrustedProjects::load()),
        }
    }

    /// Build a gate from an already-loaded store (test affordance).
    #[cfg(test)]
    fn from_store(store: TrustedProjects) -> Self {
        Self {
            inner: std::sync::Mutex::new(store),
        }
    }

    /// Whether `project_root` is trusted (project-scope external tools may
    /// auto-load).
    pub fn is_trusted(&self, project_root: &Path) -> bool {
        self.inner
            .lock()
            .map(|s| s.contains(project_root))
            .unwrap_or(false)
    }

    /// Mark `project_root` trusted and persist. Returns whether it was newly
    /// added.
    pub fn trust(&self, project_root: &Path) -> bool {
        self.inner
            .lock()
            .map(|mut s| s.add(project_root))
            .unwrap_or(false)
    }

    /// Revoke trust for `project_root` and persist. Returns whether it was
    /// previously trusted.
    pub fn untrust(&self, project_root: &Path) -> bool {
        self.inner
            .lock()
            .map(|mut s| s.remove(project_root))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::path::PathBuf;

    fn scratch_file() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("neenee-trust-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("trusted_projects.json")
    }

    #[test]
    fn add_and_remove_track_membership_in_memory() {
        // add/remove mutate the in-memory set; their persistence goes to the
        // well-known path (exercised via TrustGate in integration). Here we
        // verify the membership semantics + idempotency.
        let project = std::env::temp_dir().join(format!("proj-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&project).unwrap();

        let mut store = TrustedProjects::default();
        assert!(!store.contains(&project), "empty store trusts nothing");
        assert!(store.add(&project), "newly added");
        assert!(store.contains(&project));
        assert!(!store.add(&project), "idempotent on re-add");
        assert!(store.remove(&project), "was present");
        assert!(!store.contains(&project));
        assert!(!store.remove(&project), "idempotent on re-remove");
    }

    #[test]
    fn save_to_and_load_from_round_trip() {
        let path = scratch_file();
        let project = std::env::temp_dir().join(format!("proj-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&project).unwrap();

        let mut store = TrustedProjects::default();
        store.add(&project);
        store.save_to(&path);

        let reloaded = TrustedProjects::load_from(&path);
        assert!(reloaded.contains(&project), "persisted then reloaded");
    }

    #[test]
    fn trust_gate_threadsafe_handle() {
        let project = std::env::temp_dir().join(format!("proj-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&project).unwrap();

        let gate = TrustGate::from_store(TrustedProjects::default());
        assert!(!gate.is_trusted(&project));
        gate.trust(&project);
        assert!(gate.is_trusted(&project));
        gate.untrust(&project);
        assert!(!gate.is_trusted(&project));
    }

    #[test]
    fn missing_or_corrupt_file_is_empty() {
        // Non-existent path.
        assert!(TrustedProjects::load_from(std::path::Path::new("/no/such/file"))
            .roots_sorted()
            .is_empty());
        // Corrupt JSON.
        let path = scratch_file();
        std::fs::write(&path, "not json {").unwrap();
        assert!(TrustedProjects::load_from(&path).roots_sorted().is_empty());
    }
}
