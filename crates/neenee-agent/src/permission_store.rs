//! Permission allowlist + pending-request registry, extracted from the
//! `Agent` god-object.
//!
//! Owns the "always allow" rule set (optionally persisted to disk per
//! project), the map of pending permission requests awaiting a user reply,
//! and the project root used for persistence. The [`crate::Agent`] owns a
//! single `PermissionStore` and delegates its permission-related public
//! methods here.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use neenee_core::PermissionDecision;
use tokio::sync::oneshot;

/// Internal lock-guard helper: poison-immune (recovers via `into_inner`).
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PermissionRule {
    pub tool: String,
    pub scope: String,
}

/// On-disk shape of the persisted "always allow" allowlist, versioned for
/// future schema evolution. Readers reject unknown future versions rather
/// than guessing, so a downgrade silently ignores the file.
///
/// Version 2 adds `revoked`: the rules a user has explicitly revoked, kept so a
/// declarative `[permissions]` seed cannot resurrect them (#3). A v1 file (no
/// `revoked`) loads with an empty revoked set via `#[serde(default)]`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PersistedPermissions {
    version: u32,
    rules: Vec<PermissionRule>,
    #[serde(default)]
    revoked: Vec<PermissionRule>,
}

impl PersistedPermissions {
    /// The on-disk version this build writes and accepts.
    const CURRENT_VERSION: u32 = 2;
    /// The version this build still reads for back-compat (older files lacking
    /// the `revoked` list). `CURRENT_VERSION` is preferred on write.
    const V1_VERSION: u32 = 1;
}

#[derive(Default)]
struct PermissionState {
    always: HashSet<PermissionRule>,
    /// Rules the user has **explicitly revoked** (via `revoke_allowed` /
    /// `clear_allowed`), remembered so a later `seed_from_config` cannot
    /// silently re-grant them. Without this, a rule sourced from `[permissions]`
    /// config would resurrect on every restart after the user revoked it — the
    /// declarative seed re-applying past an interactive decision. The set is
    /// persisted (see `PersistedPermissions`) and is the complement of `always`:
    /// `add_always` removes from it, `revoke_allowed` adds to it. See #3.
    revoked: HashSet<PermissionRule>,
    pending: HashMap<String, oneshot::Sender<PermissionDecision>>,
}

/// In-memory permission state: the "always allow" allowlist, the pending
/// request channels, and the optional project root for on-disk persistence.
pub struct PermissionStore {
    state: Mutex<PermissionState>,
    project_root: Mutex<Option<std::path::PathBuf>>,
    /// When true, the agent runs **unattended** — without human intervention:
    /// no permission confirmations, no questions. Operationally this skips the
    /// permission prompt entirely (and bypasses the allowlist wholesale), but
    /// the flag's meaning is "no human in the loop," not just "skip prompts."
    ///
    /// This is the **single source of truth** for the agent's attended state:
    /// `Agent::get_unattended`/`set_unattended` are thin forwards here, and the
    /// permission chain snapshots it once into `PolicyContext::unattended`
    /// (cloned before the async chain runs, so no lock is held across `.await`).
    unattended: Mutex<bool>,
}

impl PermissionStore {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(PermissionState::default()),
            project_root: Mutex::new(None),
            unattended: Mutex::new(false),
        }
    }

    // ── unattended ────────────────────────────────────────────────────

    pub fn unattended(&self) -> bool {
        *lock(&self.unattended)
    }

    pub fn set_unattended(&self, value: bool) {
        *lock(&self.unattended) = value;
    }

    // ── pending requests ────────────────────────────────────────────────

    /// Register a pending permission request and return the receiver the
    /// caller should `await` for the user's decision, alongside the instant the
    /// request was parked. The caller measures the elapsed time across the
    /// `await` so it can be subtracted from the round's wall-clock to derive an
    /// honest active-generation time (tokens/sec that excludes the human pause).
    pub fn park_request(
        &self,
        request_id: String,
    ) -> (oneshot::Receiver<PermissionDecision>, std::time::Instant) {
        let (sender, receiver) = oneshot::channel();
        lock(&self.state).pending.insert(request_id, sender);
        (receiver, std::time::Instant::now())
    }

    /// Resolve a pending permission request. Rejecting one settles the whole
    /// concurrent permission batch, so every other pending request is also
    /// resolved with `Reject` to avoid deadlocking the `join_all`. Returns
    /// whether a sender was found.
    pub fn reply(&self, request_id: &str, decision: PermissionDecision) -> bool {
        let mut perms = lock(&self.state);
        let sender = perms.pending.remove(request_id);
        let sent = sender.is_some_and(|sender| sender.send(decision).is_ok());
        if sent && decision == PermissionDecision::Reject {
            for (_, pending_sender) in perms.pending.drain() {
                let _ = pending_sender.send(PermissionDecision::Reject);
            }
        }
        sent
    }

    /// Reject every pending permission request (e.g. on turn abort).
    pub fn reject_pending(&self) {
        let pending = std::mem::take(&mut lock(&self.state).pending);
        for (_, sender) in pending {
            let _ = sender.send(PermissionDecision::Reject);
        }
    }

    // ── allowlist ───────────────────────────────────────────────────────

    /// Check whether a rule is in the "always allow" set. A stored scope of
    /// `"*"` is a wildcard for that tool (matches any scope). Any other stored
    /// scope must match `rule.scope` **exactly** — there is no prefix or
    /// substring matching, so `bash git` does not allow `bash git status`.
    /// (`PermissionRuleConfig.scope` documents this contract for config authors.)
    pub fn is_always_allowed(&self, rule: &PermissionRule) -> bool {
        let state = lock(&self.state);
        state.always.contains(rule)
            || state.always.contains(&PermissionRule {
                tool: rule.tool.clone(),
                scope: "*".to_string(),
            })
    }

    /// Add a rule to the "always allow" set and persist. Re-approving a rule
    /// also clears it from the `revoked` set (the user has reversed their
    /// earlier revocation), so a subsequent `seed_from_config` will re-grant
    /// it as expected rather than treating it as still-revoked.
    pub fn add_always(&self, rule: PermissionRule) {
        {
            let mut state = lock(&self.state);
            state.always.insert(rule.clone());
            state.revoked.remove(&rule);
        }
        self.persist();
    }

    pub fn allowed_tools(&self) -> Vec<String> {
        let mut tools = lock(&self.state)
            .always
            .iter()
            .map(|rule| format!("{} {}", rule.tool, rule.scope))
            .collect::<Vec<_>>();
        tools.sort();
        tools
    }

    pub fn allowed_tools_structured(&self) -> Vec<neenee_core::PermissionRuleInfo> {
        let mut rules: Vec<neenee_core::PermissionRuleInfo> = lock(&self.state)
            .always
            .iter()
            .map(|rule| neenee_core::PermissionRuleInfo {
                tool: rule.tool.clone(),
                scope: rule.scope.clone(),
            })
            .collect();
        rules.sort_by(|a, b| a.tool.cmp(&b.tool).then_with(|| a.scope.cmp(&b.scope)));
        rules
    }

    /// Clear the entire "always allow" set. Every cleared rule is added to the
    /// `revoked` set so a `[permissions]` config seed cannot silently resurrect
    /// it — the user's blanket revocation must survive a restart. (#3)
    pub fn clear_allowed(&self) {
        {
            let mut state = lock(&self.state);
            // Record each revoked rule so the declarative seed cannot re-grant
            // it on the next start. Collect before inserting into `revoked`:
            // `drain` holds a mutable borrow of `state.always` for its whole
            // duration, so mutating `state.revoked` inside the loop would
            // borrow `state` twice at once.
            let drained: Vec<PermissionRule> = state.always.drain().collect();
            for rule in drained {
                state.revoked.insert(rule);
            }
        }
        self.persist();
    }

    /// Revoke one rule. Returns whether it was present. The rule is added to the
    /// `revoked` set so a `[permissions]` config seed cannot resurrect it on the
    /// next start — an interactive revocation must survive a restart, even for
    /// rules originally sourced from config. (#3)
    pub fn revoke_allowed(&self, tool: &str, scope: &str) -> bool {
        let rule = PermissionRule {
            tool: tool.to_string(),
            scope: scope.to_string(),
        };
        let removed = {
            let mut state = lock(&self.state);
            let removed = state.always.remove(&rule);
            if removed {
                // Remember the revocation so the declarative seed does not
                // re-grant it.
                state.revoked.insert(rule);
            }
            removed
        };
        if removed {
            self.persist();
        }
        removed
    }

    // ── persistence ─────────────────────────────────────────────────────

    /// Seed the allowlist from declarative `[permissions]` config rules. Called
    /// at startup after `set_project_root` (so persistent rules are already
    /// loaded). Config rules are **not** persisted to `permissions.json` — they
    /// are re-applied on every start from `config.toml`, keeping them
    /// declarative and version-controllable. Rules already present (from disk)
    /// are not duplicated.
    ///
    /// #3 — a rule present in the `revoked` set is **skipped**: the user has
    /// already revoked it interactively (or via `clear_allowed`), and the
    /// declarative seed must not silently reverse that decision. The revocation
    /// is itself persisted, so this holds across restarts. Re-approving the
    /// rule later (`add_always`) clears the revocation and lets the seed apply
    /// again.
    pub fn seed_from_config(&self, rules: &[neenee_persistence::config::PermissionRuleConfig]) {
        let mut state = lock(&self.state);
        let mut added = 0;
        let mut skipped_revoked = 0;
        for rule in rules {
            let permission_rule = PermissionRule {
                tool: rule.tool.clone(),
                scope: rule.scope.clone(),
            };
            if state.revoked.contains(&permission_rule) {
                skipped_revoked += 1;
                continue;
            }
            if state.always.insert(permission_rule) {
                added += 1;
            }
        }
        drop(state);
        if added > 0 {
            tracing::info!(added, "seeded {} permission rules from config", added);
        }
        if skipped_revoked > 0 {
            tracing::info!(
                skipped_revoked,
                "skipped {} config permission rules previously revoked by the user",
                skipped_revoked,
            );
        }
    }

    /// The persisted project root, if any.
    pub fn project_root(&self) -> Option<std::path::PathBuf> {
        lock(&self.project_root).clone()
    }

    /// Designate the project whose bucket backs the persistent "always"
    /// allowlist, and load any rules already on disk into the in-memory set.
    /// Pass `None` to disable persistence (envoys and most tests do this).
    pub fn set_project_root(&self, root: Option<std::path::PathBuf>) {
        {
            *lock(&self.project_root) = root.clone();
        }
        if let Some(root) = root {
            self.load_persistent(&root);
        }
    }

    fn load_persistent(&self, root: &std::path::Path) {
        let path = neenee_persistence::paths::get().project_permissions(root);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        match serde_json::from_str::<PersistedPermissions>(&text) {
            Ok(persisted)
                if persisted.version == PersistedPermissions::CURRENT_VERSION
                    || persisted.version == PersistedPermissions::V1_VERSION =>
            {
                // v1 files predate the `revoked` list; the `#[serde(default)]`
                // on that field yields an empty vec, so they load cleanly with
                // no revocations remembered (the pre-#3 behaviour).
                let mut perms = lock(&self.state);
                let count = persisted.rules.len();
                for rule in persisted.rules {
                    perms.always.insert(rule);
                }
                let revoked_count = persisted.revoked.len();
                for rule in persisted.revoked {
                    perms.revoked.insert(rule);
                }
                tracing::info!(
                    count,
                    revoked_count,
                    path = %path.display(),
                    "loaded persistent permission rules",
                );
            }
            Ok(other) => {
                tracing::warn!(
                    version = other.version,
                    path = %path.display(),
                    "unsupported persisted permissions version; ignoring file",
                );
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not parse persistent permissions file; ignoring",
                );
            }
        }
    }

    /// Atomically mirror the current `always` allowlist **and** `revoked` set
    /// into the project bucket. Best-effort: logs on failure and never
    /// propagates the error.
    fn persist(&self) {
        let root = lock(&self.project_root).clone();
        let Some(root) = root else {
            return;
        };
        let path = neenee_persistence::paths::get().project_permissions(&root);
        let snapshot = {
            let perms = lock(&self.state);
            let mut rules: Vec<PermissionRule> = perms.always.iter().cloned().collect();
            rules.sort_by(|a, b| a.tool.cmp(&b.tool).then_with(|| a.scope.cmp(&b.scope)));
            let mut revoked: Vec<PermissionRule> = perms.revoked.iter().cloned().collect();
            revoked.sort_by(|a, b| a.tool.cmp(&b.tool).then_with(|| a.scope.cmp(&b.scope)));
            PersistedPermissions {
                version: PersistedPermissions::CURRENT_VERSION,
                rules,
                revoked,
            }
        };
        if let Err(e) = neenee_persistence::fsutil::atomic_write_json(&path, &snapshot) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "could not persist permission rules",
            );
        }
    }
}

impl Default for PermissionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neenee_persistence::config::PermissionRuleConfig;

    /// A unique temp directory for a test, with no external `tempfile` dep.
    /// Cleaned up (best-effort) on drop.
    struct ScratchDir(std::path::PathBuf);
    impl ScratchDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "neenee-permstore-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            ScratchDir(dir)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn seed_from_config_adds_rules_to_allowlist() {
        let store = PermissionStore::new();
        // No project root → no persistence, so seeding is purely in-memory.
        let rules = vec![
            PermissionRuleConfig {
                tool: "bash".to_string(),
                scope: "*".to_string(),
            },
            PermissionRuleConfig {
                tool: "read_text".to_string(),
                scope: "*".to_string(),
            },
        ];
        store.seed_from_config(&rules);
        assert!(store.is_always_allowed(&PermissionRule {
            tool: "bash".to_string(),
            scope: "*".to_string(),
        }));
        assert!(store.is_always_allowed(&PermissionRule {
            tool: "read_text".to_string(),
            scope: "*".to_string(),
        }));
    }

    #[test]
    fn wildcard_scope_allows_any_scope_for_same_tool() {
        let store = PermissionStore::new();
        store.seed_from_config(&[PermissionRuleConfig {
            tool: "bash".to_string(),
            scope: "*".to_string(),
        }]);
        assert!(store.is_always_allowed(&PermissionRule {
            tool: "bash".to_string(),
            scope: "git status".to_string(),
        }));
        assert!(!store.is_always_allowed(&PermissionRule {
            tool: "edit_file".to_string(),
            scope: "*".to_string(),
        }));
    }

    #[test]
    fn seed_from_config_does_not_duplicate_existing_rules() {
        let store = PermissionStore::new();
        let rule = PermissionRuleConfig {
            tool: "bash".to_string(),
            scope: "*".to_string(),
        };
        store.seed_from_config(std::slice::from_ref(&rule));
        store.seed_from_config(std::slice::from_ref(&rule)); // idempotent
        assert_eq!(store.allowed_tools().len(), 1);
    }

    #[test]
    fn park_request_returns_a_receiver_and_timestamp() {
        // The receiver is awaited by the round task; the timestamp lets the
        // caller measure how long the human took to decide, so that pause can
        // be subtracted from the round's wall-clock for an honest tokens/sec.
        let store = PermissionStore::new();
        let (mut receiver, parked_at) = store.park_request("req-1".to_string());
        let elapsed_before_reply = parked_at.elapsed();
        // Replying resolves the receiver; the elapsed measured at the call site
        // is non-negative by construction.
        assert!(store.reply("req-1", PermissionDecision::Once));
        let decision = receiver
            .try_recv()
            .expect("reply resolves the parked receiver");
        assert_eq!(decision, PermissionDecision::Once);
        assert!(elapsed_before_reply.as_nanos() < u128::MAX);
    }

    // ── #3: revoked config rules must not resurrect on re-seed ──────────

    #[test]
    fn revoke_then_reseed_does_not_resurrect_config_rule() {
        // The core #3 fix: a config-sourced rule the user revoked must NOT come
        // back when seed_from_config runs again (e.g. on restart).
        let store = PermissionStore::new();
        let rule = PermissionRuleConfig {
            tool: "bash".to_string(),
            scope: "git status".to_string(),
        };
        store.seed_from_config(std::slice::from_ref(&rule));
        assert!(store.is_always_allowed(&PermissionRule {
            tool: "bash".to_string(),
            scope: "git status".to_string(),
        }));
        // User revokes it.
        assert!(store.revoke_allowed("bash", "git status"));
        assert!(!store.is_always_allowed(&PermissionRule {
            tool: "bash".to_string(),
            scope: "git status".to_string(),
        }));
        // Re-seed (simulating a restart): the revoked rule stays revoked.
        store.seed_from_config(std::slice::from_ref(&rule));
        assert!(
            !store.is_always_allowed(&PermissionRule {
                tool: "bash".to_string(),
                scope: "git status".to_string(),
            }),
            "a revoked config rule must not resurrect on re-seed"
        );
    }

    #[test]
    fn re_approving_a_revoked_rule_lets_seed_apply_it_again() {
        // add_always clears the revocation, so a later seed re-grants the rule.
        // The user reversed their revocation; the declarative seed wins again.
        let store = PermissionStore::new();
        let rule = PermissionRuleConfig {
            tool: "bash".to_string(),
            scope: "git status".to_string(),
        };
        store.seed_from_config(std::slice::from_ref(&rule));
        store.revoke_allowed("bash", "git status");
        // Re-approve interactively.
        store.add_always(PermissionRule {
            tool: "bash".to_string(),
            scope: "git status".to_string(),
        });
        // A subsequent seed is idempotent (rule present) but no longer blocked.
        store.seed_from_config(std::slice::from_ref(&rule));
        assert!(store.is_always_allowed(&PermissionRule {
            tool: "bash".to_string(),
            scope: "git status".to_string(),
        }));
    }

    #[test]
    fn clear_allowed_revokes_every_rule_against_future_seed() {
        // clear_allowed is a blanket revocation: every rule is added to the
        // revoked set, so a config seed cannot resurrect any of them.
        let store = PermissionStore::new();
        store.seed_from_config(&[
            PermissionRuleConfig {
                tool: "bash".to_string(),
                scope: "*".to_string(),
            },
            PermissionRuleConfig {
                tool: "edit_file".to_string(),
                scope: "/src".to_string(),
            },
        ]);
        store.clear_allowed();
        // Re-seed: neither rule comes back.
        store.seed_from_config(&[
            PermissionRuleConfig {
                tool: "bash".to_string(),
                scope: "*".to_string(),
            },
            PermissionRuleConfig {
                tool: "edit_file".to_string(),
                scope: "/src".to_string(),
            },
        ]);
        assert_eq!(store.allowed_tools().len(), 0);
    }

    #[test]
    fn revoked_set_persists_across_load() {
        // The revoked set round-trips through the on-disk file: revoke, persist,
        // reload into a fresh store, and the seed still skips the rule.
        //
        // Hermetic: this test touches `paths::get()`, which reads
        // `NEENEE_DATA_DIR`. Other permission tests mutate that env var under
        // `ENV_GUARD`; without acquiring the same guard this test would race
        // them and read a polluted env. Pin a private data dir and serialize.
        let _guard = crate::tests::ENV_GUARD.blocking_lock();
        let tmp = ScratchDir::new();
        // SAFETY: `ENV_GUARD` is held, so no other test observes this var and
        // it is removed before the guard is released.
        unsafe {
            std::env::set_var("NEENEE_DATA_DIR", tmp.path());
        }
        let project_root = tmp.path().to_path_buf();

        let store = PermissionStore::new();
        store.set_project_root(Some(project_root.clone()));
        let rule = PermissionRuleConfig {
            tool: "bash".to_string(),
            scope: "git push".to_string(),
        };
        store.seed_from_config(std::slice::from_ref(&rule));
        store.revoke_allowed("bash", "git push");

        // A fresh store pointed at the same bucket loads the revoked set.
        let reloaded = PermissionStore::new();
        reloaded.set_project_root(Some(project_root));
        reloaded.seed_from_config(std::slice::from_ref(&rule));
        assert!(
            !reloaded.is_always_allowed(&PermissionRule {
                tool: "bash".to_string(),
                scope: "git push".to_string(),
            }),
            "revocation must survive a restart (persisted revoked set)"
        );

        unsafe {
            std::env::remove_var("NEENEE_DATA_DIR");
        }
    }
}
