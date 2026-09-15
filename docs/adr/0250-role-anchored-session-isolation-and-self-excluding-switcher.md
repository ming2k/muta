# 0250. Role-Anchored Session Isolation, Symmetric Dual Partition, and Self-Excluding History Switcher

- **Status:** Accepted
- **Date:** 2026-10-04
- **Scope:** `core/contracts`, `storage/persistence`, `runtime/session`, `terminal/mutx`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0219](0219-session-scope-and-optional-workspace-binding.md) (workspace-free sessions), [ADR-0226](0226-session-grouping-is-derived.md) (derived grouping), [ADR-0244](0244-immutable-session-role-and-truthful-identity-projection.md) (immutable session role), [ADR-0245](0245-hermetic-session-role-manifest-snapshotting.md) (hermetic session role manifest)
- **Refines/Supersedes:** ADR-0226's "workspace is the sole partition" stance. Workspace and Role are established as symmetric, first-class dual anchors.

---

## Context and Problem Statement

Muta historically partitioned conversation history strictly by filesystem paths (`project_root`). ADR-0219 recognized that workspace-free roles (e.g., `philosophist`) required decoupled history lanes. However, ADR-0226 subsequently rolled back `SessionScope` in favor of a minimalist stance: *"The partition is the workspace, and nothing else; non-workspace sessions are simply unbound (`workspace_root IS NULL`)"*.

While ADR-0226 successfully prevented over-abstraction at the time, subsequent architectural developments—specifically the **Immutable Session Role Tuple** ([ADR-0244](0244-immutable-session-role-and-truthful-identity-projection.md)) $\text{Session} = (\text{WorkspaceBinding}?, \text{Role}, \text{ImmutableHistory})$ and **Hermetic Manifest Snapshotting** ([ADR-0245](0245-hermetic-session-role-manifest-snapshotting.md))—revealed two uncompromised structural defects in practice:

### 1. The "Unbound Vagrancy" Trap for Workspace-Free Roles
When an agent role declares no workspace binding (e.g., `philosophist`, pure-reasoning advisors, or research personas):
- Its sessions are stored with `workspace_root = NULL`.
- The database reader's `WorkspaceFilter::Unbound` blindly performs `WHERE workspace_root IS NULL AND fork_kind <> 'subagent'`.
- **Defect 1A (Namespace Collision)**: All workspace-free sessions across completely unrelated roles (`philosophist`, translators, autonomous personas) are dumped into a single chaotic pool.
- **Defect 1B (Inaccessibility from Project Contexts)**: When a developer operates in a workspace repository, `/sessions` queries are hard-filtered to `workspace_root = cwd`. Historical philosophical inquiry sessions become invisible and unreachable.
- **Defect 1C (Destructive In-Place Reset)**: Invoking `/role philosophist` from the slash handler executed `session.reset_with(...)`, unconditionally wiping in-memory state and spawning a fresh blank session, denying the user any path to resume their prior dialectical discourse.

### 2. Self-Referential Pollution in the Session Switcher
In `mutx`'s `/sessions` modal and `session_view::build_sessions_overview`:
- The active session is synthesized and injected into the overview list (`item.active = true`), occupying the top slot with an `●` active indicator badge.
- **Defect 2A (Broken MRU Toggle)**: The sole intent of invoking the session switcher is to **navigate to another session**. Injecting the current session at position `0` forces the user to move the cursor down before pressing `Enter`. This breaks the fundamental `[Open Sessions] -> [Enter]` instantaneous two-stroke toggle between concurrent conversation threads.
- **Defect 2B (Pointless Self-Switch)**: Selecting the current session performs a redundant, noisy reconnect to the exact same thread the user is already inhabiting.

We require a definitive, uncompromised architecture that elevates the autonomous cognitive Role into a first-class partition anchor alongside the Workspace, while purifying the session switching mechanic to be strictly self-excluding.

---

## Decision Drivers

- **Zero Legacy Burden**: Eliminate the asymmetric treatment of workspace-free sessions as "unbound / second-class citizens".
- **Symmetric Dual-Anchor Model**: A session's primary partition is determined truthfully: by its filesystem directory if bound, or by its cognitive Role if workspace-free.
- **Frictionless MRU Ergonomics**: Opening the session switcher must immediately target the most recently active *alternative* session. `[Open Sessions] + [Enter]` must toggle between the two most recent threads with zero cursor motion.
- **Strict Isolation**: `developer` sessions in a repository must never bleed into `philosophist` sessions, and `philosophist` sessions must never bleed into other custom workspace-free roles.
- **Continuity of Discourse**: Switching to a workspace-free role defaults to resuming its ongoing discourse thread, with explicit affordance for fresh branches.

---

## Decision Outcome

### 1. Symmetric Dual-Anchor Partition Model (`SessionPartition`)

We deprecate `WorkspaceFilter` and introduce `SessionPartition` as the canonical domain partition:

$$\text{Partition} = \begin{cases} 
\text{Workspace}(\text{PathBuf}) & \text{if } \text{session.workspace.is\_some()} \\
\text{Role}(\text{String}) & \text{if } \text{session.workspace.is\_none()}
\end{cases}$$

```rust
// crates/muta-contracts/src/session_partition.rs

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionPartition {
    /// Filesystem-anchored project history (e.g., developer role in a repo)
    Workspace(PathBuf),
    /// Role-anchored autonomous cognitive thread (e.g., philosophist)
    Role(String),
}

impl SessionPartition {
    pub fn from_parts(workspace: Option<&Path>, role: Option<&str>) -> Self {
        match (workspace, role) {
            (Some(ws), _) => Self::Workspace(ws.to_path_buf()),
            (None, Some(r)) => Self::Role(r.to_string()),
            (None, None) => Self::Role("developer".to_string()),
        }
    }
}
```

### 2. Strictly Self-Excluding Switch Candidate Query

The persistence reader contract is rewritten from "list all sessions" to "list switch candidates":

```rust
// crates/muta-persistence/src/db/reader.rs

impl DatabaseReader {
    /// Retrieve all alternative sessions within the partition, strictly excluding `active_session_id`.
    pub fn list_switch_candidates(
        &self,
        partition: &SessionPartition,
        active_session_id: &str,
    ) -> Result<Vec<SessionSummary>> {
        const COLS: &str = "id, parent_id, fork_kind, title, created_at_s, updated_at_s, \
                            msg_count, last_user_prompt, digest";
        match partition {
            SessionPartition::Workspace(path) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions \
                     WHERE workspace_root = ?1 AND id != ?2 AND fork_kind <> 'subagent' \
                     ORDER BY updated_at_s DESC;"
                );
                self.query_summaries(&sql, params![path.to_string_lossy(), active_session_id])
            }
            SessionPartition::Role(role_id) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions \
                     WHERE workspace_root IS NULL AND persona = ?1 AND id != ?2 AND fork_kind <> 'subagent' \
                     ORDER BY updated_at_s DESC;"
                );
                self.query_summaries(&sql, params![role_id, active_session_id])
            }
        }
    }
}
```

A dedicated SQLite partial covering index ensures microsecond response times:
```sql
CREATE INDEX IF NOT EXISTS idx_sessions_role_partition 
    ON sessions(persona, updated_at_s DESC) 
    WHERE workspace_root IS NULL AND fork_kind <> 'subagent';
```

### 3. Pure Context-Aware TUI Projection (`mutx`)

1. **Self-Exclusion by Design**:
   `session_view::build_sessions_overview` completely deletes the synthetic insertion of the active session. The resulting list sent to the TUI contains *only alternative candidates*.
2. **Instant Toggle Mechanics**:
   Because the active session is absent, index `0` is mathematically guaranteed to be the *Most Recently Used alternative session*. 
   Pressing `[Session Palette] -> [Enter]` immediately switches between concurrent streams without navigation overhead.
3. **Contextual Chrome & Zero Noise**:
   The `● active` indicator is purged from list rows. The modal header reflects the truthful scope:
   - For workspace sessions: `Switch Session [project-name]`
   - For workspace-free sessions: `Switch Session [@philosophist]`
4. **Clean Empty State**:
   If no alternative sessions exist for the current role/workspace, the modal renders:
   ```text
   No other sessions for [@philosophist].
   Press [Enter]/[Esc] to dismiss, [n] to fork a fresh branch.
   ```

### 4. Smart `/role` Routing (Resume vs Fresh)

The slash handler `/role <id>` is upgraded from a destructive reset to an intentional routing primitive:
- `/role <id>`: If prior sessions exist for `<id>`, automatically resumes the latest active thread (preserving continuity with the cognitive partner).
- `/role <id> --new` (or `-n`): Spawns an explicit pristine session under `<id>`.
- If no prior session exists, `/role <id>` transparently initializes a new session.

---

## Invariants & Behavioral Boundaries

1. **[INV-PARTITION-01] Universal Symmetry**: Every session belongs to either `SessionPartition::Workspace` or `SessionPartition::Role`. The concept of an "unbound/orphaned" session is eliminated.
2. **[INV-SWITCH-01] Strict Self-Exclusion**: A session switch list MUST NEVER contain the requesting session's own ID.
3. **[INV-ISOLATION-01] Role Cleanliness**: A workspace-free session query for Role $A$ MUST NOT return sessions staffed by Role $B$.
4. **[INV-HERMETIC-01] Birth-Manifest Fidelity**: Resuming an existing role-anchored session MUST restore from its immutable persisted `role_manifest` ([ADR-0245](0245-hermetic-session-role-manifest-snapshotting.md)), preserving upstream prompt cache invariants.

---

## Consequences

### Positive
- **Instantaneous Task Switching**: Users can alternate between two active tracks with instant `[Key] + [Enter]`.
- **First-Class Dialectical Experience**: Philosophist and reasoning roles acquire true persistence, isolation, and discoverability without hacks.
- **Architectural Harmony**: Fully aligns `storage/persistence`, `runtime`, and `mutx` with the immutable role guarantees established in ADR-0244.

### Negative & Mitigations
- *User expects to see the current session in the list to verify active session details*:
  - *Mitigation*: The current session's ID, token count, and active role are already prominently projected in the permanent scene header (`SessionHead`) and footer bar. The switcher modal is exclusively for *switching*.
