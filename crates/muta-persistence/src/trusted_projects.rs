//! Project-scope trust store for external tools (ADR-0085 §5).
//!
//! A project's `.muta/config.toml` may declare `[mcp.*]` servers whose
//! `command` executes a process. Loading those automatically from a cloned or
//! vendored working tree is the same class of hazard as an npm `postinstall`
//! script or a git hook: a malicious repo should not get code execution merely
//! because the user opened it. This store records the project roots the user
//! has **explicitly trusted** via `/trust`; only those roots' project-scope
//! external tools auto-load.
//!
//! The store is a JSON set of absolute, canonical project-root paths under
//! `$XDG_STATE_HOME/muta/trusted_projects.json`. It is program-generated
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
use std::path::{Path, PathBuf};

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

/// Resolve the path that should be used for trust checks, mirroring the codex
/// `resolve_root_git_project_for_trust` idea: a trust decision should apply to
/// the whole repository, not just the current subdirectory or worktree, so a
/// single grant covers every checkout.
///
/// Walks **up** from `start` looking for the nearest ancestor containing a
/// `.git` entry (pure filesystem inspection — no `git` subprocess):
/// - `.git` is a directory → a normal repo; return that repo root.
/// - `.git` is a file (a linked worktree pointer) → read the `gitdir:` line
///   and, if it points under a `worktrees/` dir, return the **main** repo root
///   (the common dir's parent) so all worktrees share one trust entry. Other
///   `gitdir:` shapes are left unresolved (`None`).
/// - No `.git` ancestor → `None` (caller falls back to `start`).
///
/// `start` is treated as a directory; if a file path is passed its parent is
/// used. Symlinks are *not* canonicalized here — the caller canonicalizes the
/// final answer via `TrustedProjects::canon` before storage/lookup.
pub fn resolve_trust_root(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_dir() {
        Some(start)
    } else {
        start.parent()
    };

    while let Some(current) = dir {
        let git = current.join(".git");
        if git.is_dir() {
            // Normal repository: the trust root is this directory.
            return Some(current.to_path_buf());
        }
        if git.is_file() {
            // Linked worktree: `.git` is a `gitdir: <path>` pointer. Resolve to
            // the main repository root so a single trust entry covers every
            // worktree of this repo.
            if let Some(main_root) = resolve_worktree_main_root(&git) {
                return Some(main_root);
            }
            // Unrecognized gitdir pointer — do not trust a guess.
            return None;
        }
        dir = current.parent();
    }
    None
}

/// Parse a worktree `.git` pointer file (`gitdir: <path>`) and resolve the
/// main repository root. Returns `None` unless the pointer resolves into a
/// `…/.git/worktrees/<name>/<files>` layout, in which case the main root is the
/// `…/.git` directory's parent.
fn resolve_worktree_main_root(git_pointer: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(git_pointer).ok()?;
    let line = content.lines().next()?;
    let target = line.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let resolved = if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        git_pointer.parent()?.join(target)
    };
    // Linked-worktree layout: <main>/.git/worktrees/<name>/<files>. Walk up to
    // the `worktrees` segment, then one more to `.git`, then one more to the
    // main repo root.
    let mut cursor = resolved.parent()?;
    for _ in 0..3 {
        if cursor.file_name().and_then(|n| n.to_str()) == Some("worktrees") {
            // parent = .git, parent again = main repo root.
            return cursor
                .parent()
                .and_then(|p| p.parent())
                .map(Path::to_path_buf);
        }
        cursor = cursor.parent()?;
    }
    None
}
/// Owned handle pairing a loaded trust store with thread-safe mutation,
/// exposing the gated decision the bootstrap/reload path needs: "should this
/// project's external tools auto-load?"
///
/// Held as a single value for the session; share it by wrapping in `Arc` when
/// multiple owners need to mutate it (`/trust` / `/untrust`).
///
/// All membership checks are **git-aware**: the `project_root` passed in is
/// first resolved to its repository trust root via [`resolve_trust_root`], so a
/// trust grant made at the repo root covers subdirectories and linked
/// worktrees. When `start` is outside any git repo, the path is used as-is.
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

    /// Resolve a raw project path to its trust root (the git repository root
    /// when `start` is inside one, else `start` unchanged).
    fn trust_root(start: &Path) -> PathBuf {
        resolve_trust_root(start).unwrap_or_else(|| start.to_path_buf())
    }

    /// Whether `project_root` is trusted (project-scope external tools may
    /// auto-load). Git-aware: resolves to the repo root first.
    pub fn is_trusted(&self, project_root: &Path) -> bool {
        let root = Self::trust_root(project_root);
        self.inner
            .lock()
            .map(|s| s.contains(&root))
            .unwrap_or(false)
    }

    /// Mark `project_root` trusted and persist. Returns whether it was newly
    /// added. Git-aware: the grant is recorded against the repo root.
    pub fn trust(&self, project_root: &Path) -> bool {
        let root = Self::trust_root(project_root);
        self.inner.lock().map(|mut s| s.add(&root)).unwrap_or(false)
    }

    /// Revoke trust for `project_root` and persist. Returns whether it was
    /// previously trusted. Git-aware: revokes the repo-root entry.
    pub fn untrust(&self, project_root: &Path) -> bool {
        let root = Self::trust_root(project_root);
        self.inner
            .lock()
            .map(|mut s| s.remove(&root))
            .unwrap_or(false)
    }
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::path::PathBuf;

    /// Sandbox the process-wide paths so `add`/`remove`/`trust`/`untrust` —
    /// which persist through `paths::get()` — write into a throwaway tempdir
    /// instead of the developer's real `$XDG_STATE_HOME` (regression: these
    /// tests once replaced the real `trusted_projects.json` with an empty
    /// set, silently revoking every project trust grant). Uses the
    /// crate-sanctioned `set_test_default` + `TEST_OVERRIDE_GUARD` pattern
    /// (see `config::tests::sandbox_config_dir`) so concurrent override
    /// users stay serialised.
    fn sandbox_paths() -> (std::path::PathBuf, std::sync::MutexGuard<'static, ()>) {
        let override_guard = paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("muta-trust-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        paths::set_test_default(Some(paths::Dirs {
            config_dir: tmp.clone(),
            data_dir: tmp.join("data"),
            state_dir: tmp.join("state"),
            cache_dir: tmp.join("cache"),
            runtime_dir: None,
        }));
        (tmp, override_guard)
    }

    fn scratch_file() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("muta-trust-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("trusted_projects.json")
    }

    #[test]
    fn add_and_remove_track_membership_in_memory() {
        let _sandbox = sandbox_paths();
        // add/remove mutate the in-memory set; their persistence goes to the
        // (sandboxed) well-known path. Here we verify the membership
        // semantics + idempotency.
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
        let _sandbox = sandbox_paths();
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
        assert!(
            TrustedProjects::load_from(std::path::Path::new("/no/such/file"))
                .roots_sorted()
                .is_empty()
        );
        // Corrupt JSON.
        let path = scratch_file();
        std::fs::write(&path, "not json {").unwrap();
        assert!(TrustedProjects::load_from(&path).roots_sorted().is_empty());
    }

    // --- git-aware trust-root resolution ---

    /// A scratch repo root with a `.git` directory marker.
    fn git_repo(subdirs: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("muta-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        for sub in subdirs {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        root
    }

    #[test]
    fn resolve_trust_root_finds_repo_from_subdirectory() {
        let root = git_repo(&["src/nested"]);
        let deep = root.join("src/nested");
        assert_eq!(resolve_trust_root(&deep), Some(root.clone()));
        // The repo root itself resolves to itself.
        assert_eq!(resolve_trust_root(&root), Some(root));
    }

    #[test]
    fn resolve_trust_root_none_when_outside_any_repo() {
        let dir = std::env::temp_dir().join(format!("muta-nogit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(resolve_trust_root(&dir), None);
    }

    #[test]
    fn resolve_trust_root_follows_worktree_pointer_to_main_root() {
        // Build <main>/.git/worktrees/<name>/ and a worktree dir whose `.git`
        // file points at it.
        let main = std::env::temp_dir().join(format!("muta-main-{}", uuid::Uuid::new_v4()));
        let wt_private = main.join(".git").join("worktrees").join("feature");
        std::fs::create_dir_all(&wt_private).unwrap();
        let worktree = std::env::temp_dir().join(format!("muta-wt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", wt_private.display()),
        )
        .unwrap();

        // Resolving from the worktree should land on the MAIN repo root, so a
        // single trust grant covers both the main checkout and the worktree.
        assert_eq!(resolve_trust_root(&worktree), Some(main));
    }

    #[test]
    fn trust_gate_grant_at_repo_root_covers_subdirectory() {
        let _sandbox = sandbox_paths();
        // Git-awareness lives in TrustGate: trusting the repo root means an
        // is_trusted query from a subdirectory returns true.
        let root = git_repo(&["pkg/inner"]);
        let deep = root.join("pkg/inner");

        let gate = TrustGate::from_store(TrustedProjects::default());
        assert!(!gate.is_trusted(&deep), "nothing trusted yet");
        gate.trust(&root);
        assert!(gate.is_trusted(&deep), "subdir covered by repo-root grant");
        gate.untrust(&deep);
        assert!(
            !gate.is_trusted(&root),
            "untrust from a subdir revokes the grant"
        );
    }
}
