# 0244. Immutable Session Role, Prefix-Cached Identity Preamble, and Truthful Scene Header Projection

- **Status:** Accepted
- **Date:** 2026-10-03
- **Scope:** `core/contracts`, `core/agent`, `runtime/session`, `runtime/slash`, `terminal/mutx`, `storage/persistence`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0056](0056-model-context-assembly-boundary.md) (model context assembly boundary), [ADR-0137](0137-server-side-kv-cache-alignment-and-zoning.md) (server-side KV cache alignment and zoning), [ADR-0161](0161-route-scoped-inference-protocols-and-prompt-cache-contracts.md) (prompt cache contracts), [ADR-0197](0197-one-frontend-truth-shell-as-client-of-engine-and-protocol.md) (one frontend truth), [ADR-0219](0219-session-scope-and-optional-workspace-binding.md) (workspace-free sessions), [ADR-0220](0220-personas-persisted-named-principals.md) (personas and roles), [ADR-0225](0225-persona-owns-identity-and-capability.md) (role owns identity and capability), [ADR-0242](0242-orthogonal-mcp-registry-and-role-capability-subscription.md) (orthogonal role capability), [ADR-0243](0243-unified-asset-attestation-and-zero-trust-hazard-mesh.md) (zero-trust hazard mesh)

---

## Context and Problem Statement

Muta separates top-level operational agents into distinct roles: primarily `developer` (workspace-bound, full native file/command tooling) and `philosophist` (workspace-free, dialectical inquiry, zero mutation tools), alongside user-defined roles loaded from `roles.toml`.

However, an examination of the runtime, model request assembler, slash command handler, and terminal client revealed three fundamental structural flaws:

1. **Identity Void in the Developer Base Tier**: While `philosophist` was given an explicit imperative role directive in `InstructionTier::Base`, the default `developer` role historically defaulted to an empty preamble (`AgentIdentity::default()`). The model was left to infer its identity and behavioral posture solely from available tool schemas and environment boilerplate. This led to cognitive drift in extended tasks and created a structural asymmetry across roles.
2. **Context Contamination and Cache Busting via In-Place Role Mutation**: The `/role <target>` slash command historically performed an in-place mutation on the live agent (`agent.apply_role(...)`), rewriting the active profile and workspace binding within the *current* session. This practice is architecturally unsound:
   - **Context Contamination**: Switching a developer session to a philosopher injected pure philosophical inquiry into a conversation transcript contaminated with file diffs, git hashes, and compiler errors.
   - **Prefix Cache Busting**: Altering the identity preamble and tool definitions in `InstructionTier::Base` completely invalidates all upstream KV cache blocks accumulated across prior rounds.
   - **Security Mesh Violation**: Blending workspace-bound high-privilege tool turns with workspace-free zero-privilege discourse in a single message ledger breaks provenance auditing ([ADR-0243](0243-unified-asset-attestation-and-zero-trust-hazard-mesh.md)).
   - **Corrupted Session Provenance**: A persisted session in SQLite has a single `role` column; mutating it mid-flight destroys session auditability and historical reconstruction.
3. **Untruthful TUI Scene Header Projection**:
   - In `apps/terminal/crates/mutx`, the scene header (`SessionHead`) hardcoded the terminal's working directory (`app.cwd`) into the header's primary path slot regardless of whether the session possessed a workspace binding. Consequently, a `philosophist` session launched with `mutx --role philosophist` misleadingly displayed the local repository directory as its active workspace.
   - The scene header failed to badge the active role, leaving users blind to whether they were driving a developer, a philosopher, or a custom role until an unpermitted tool call was rejected.

We require a rigorous, clean-break architecture that treats the session role as an immutable invariant, anchors deterministic identity preambles in the prefix-cache tier, redefines `/role` as a session-spawning operation, and ensures truthful visual projection in the client.

---

## Decision Drivers

- **Mathematical Purity of Session Identity**: A session is an immutable tuple: $\text{Session} = (\text{WorkspaceBinding}?, \text{Role}, \text{ImmutableEventHistory})$. A single session cannot house multiple personalities or contradictory tool access postures.
- **Uncompromised KV Cache Maximization**: `InstructionTier::Base` (`InstructionOrder::Head`) must remain 100% byte-invariant across all rounds of a session to guarantee maximum prompt cache hits on upstream providers.
- **Cognitive Hygiene**: Agents must operate with clean cognitive slates. A philosophical dialogue must never inherit contaminated tool state, and a developer must never inherit ungrounded conversational context.
- **Truthful Chrome Projection**: The TUI chrome must never project capabilities or workspace associations that the underlying session does not possess.

---

## Considered Options

- **Option 1: In-Place Mutation with Context Partitioning**: Retain in-place `/role` switching on the live session, but insert a synthetic boundary marker and prune historical tool calls upon role change.
  - *Defects*: Busted upstream prompt cache; still corrupts database provenance; complex rollback logic if the new role fails to initialize.
- **Option 2: Session Branching / Forking**: Automatically fork the current session into a child session with the new role, copying prior message history.
  - *Defects*: Perpetuates context contamination into the child session; fails the cognitive hygiene requirement.
- **Option 3 (Chosen): Immutable Session Role Tuple + Discrete Session Instantiation/Switching + Mandatory Base Tier Identity Preamble + Truthful Scene Head Projection**.

---

## Decision Outcome

We adopt Option 3 across four core pillars:

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ Pillar 1: Immutable Session Invariant                                       │
│   • Session = (Workspace?, Role, EventLedger)                               │
│   • `role` is fixed at session creation (CLI `--role` or daemon spawn).     │
│   • In-place role mutation on a live session is rejected outright.          │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ Pillar 2: Deterministic Base-Tier Identity Preamble                         │
│   • Developer: Explicit engineering identity & system architecture posture. │
│   • Philosophist: Explicit dialectical inquiry & zero-mutation directive.   │
│   • Placed at `InstructionTier::Base` (Head) -> 100% Prefix-Cache Stable.   │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ Pillar 3: Clean-Break `/role` Command Semantics                             │
│   • `/role <id>` creates/resumes a discrete session bound to `<id>`.        │
│   • Fires `switch_to` navigation signal to attach the client to the target. │
│   • Zero mutation of the origin session; pristine cognitive isolation.      │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ Pillar 4: Truthful Scene Header Projection                                  │
│   • Scene head badge prominently displays `[ROLE]` (e.g. `[PHILOSOPHIST]`). │
│   • Workspace path rendered ONLY when `session.workspace.is_some()`.        │
│   • Workspace-free sessions suppress path chrome entirely.                  │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

### Pillar 1: Immutable Session Invariant

1. **Constructor Parameter Only**: A session's `role` is supplied exclusively via `SessionInitOptions` at creation time (or inferred as `developer` if omitted).
2. **Persistence Guarantee**: The `role` column in the SQLite `sessions` table is written at session initialization and is immutable thereafter. The method `SessionStore::set_role` is removed from the mutable session lifecycle.
3. **Audit Integrity**: Security and hazard tracking ([ADR-0243](0243-unified-asset-attestation-and-zero-trust-hazard-mesh.md)) can trust that a session's security envelope never fluctuates across turns.

---

### Pillar 2: Deterministic Base-Tier Identity Preamble

Every Main Agent Role MUST supply a non-empty, canonical identity preamble to `AgentIdentity`. This preamble is placed in `InstructionTier::Base` at `InstructionOrder::Head`:

- **Developer (`developer`)**:
  ```text
  Role: developer. You are an expert AI software engineer with native tool access and deep system architecture capability. Execute commands and edit files with surgical precision, maintain strict testing discipline, and prioritize root-cause solutions over superficial patches.
  ```
- **Philosophist (`philosophist`)**:
  ```text
  Role: philosophist. Engage in deep philosophical inquiry, examine principles, question assumptions, and explore ideas with clarity and nuance. Do not modify files or run commands.
  ```
- **User Roles (`roles.toml`)**:
  Supplied via the role's configured `directive` or composed from `name` and `mission`.

By fixing these strings at `InstructionTier::Base`, prompt prefix caching operates with 100% determinism from turn 1 through turn $N$.

---

### Pillar 3: Clean-Break `/role` Command Semantics and Session Inheritance for `/new`

The `/role` (and legacy `/persona`) slash command ceases to be an in-place session modifier, and `/new` strictly adheres to session inheritance:

1. Bare `/role` continues to list the active role and all available built-in and configured user roles.
2. `/role <target_role> [workspace]` evaluates the target role:
   - Verifies role existence (built-in or `roles.toml`).
   - If switching to `developer` and no workspace is passed, uses the current workspace if present, or errors if unresolvable.
   - Spawns a **new session** with that role (and resolved workspace), leaving the origin session intact, pristine, and unmutated in SQLite.
   - Clears the active conversation and reinitializes the agent with the target role and workspace boundaries.
3. **Session Inheritance for `/new`**:
   - `/new` starts a fresh session that strictly inherits the active session's configuration tuple `(WorkspaceBinding?, Role)`.
   - Running `/new` from a `developer` session bound to `~/projects/muta` spawns a fresh `developer` session bound to the same workspace.
   - Running `/new` from a `philosophist` session spawns a fresh `philosophist` session without workspace binding (`workspace = None`).
   - Running `/new` from a custom user role (`researcher`) spawns a fresh `researcher` session. Zero unexpected defaulting or persona drift.

---

### Pillar 4: Truthful Scene Header Projection

Frontends (`mutx`) must project session reality without fallback assumptions:

1. **Role Badge**:
   The primary `SessionHead` in `view_header.rs` receives the active role string and renders it as an uppercase, brand-toned badge immediately adjacent to `SESSION <id_tail>`:
   ```text
   SESSION 3f2a [PHILOSOPHIST]
   SESSION e891 [DEVELOPER]  ~/projects/muta
   ```
2. **Conditional Workspace Path**:
   `mutx` must discontinue synthesizing `current_workspace` from the client's local `app.cwd`. Instead, `SessionHead.workspace` reflects the session's actual `WorkspaceBinding`:
   - `Some(path)`: Formatted as tilde-abbreviated path (`~/projects/muta`).
   - `None`: Rendered as empty string; the header layout suppresses the path spacer entirely, leaving a clean, unencumbered view for reasoning and dialogue.

---

## Invariants & Behavioral Boundaries

- **`[INV-ROLE-01]` Role Immutability**: A session's role is immutable. No handler, RPC, or internal agent method may change `session.role` after creation. Any requirement to switch roles must be satisfied by creating or attaching to another session.
- **`[INV-ROLE-02]` Deterministic Identity in Base Tier**: Every main agent role must register a canonical, non-empty directive in `InstructionTier::Base` (`InstructionOrder::Head`). No main agent session may open with an empty identity preamble.
- **`[INV-ROLE-03]` Non-Mutating `/role` Dispatch**: The `/role` slash command must never mutate the executing session's state, tools, or workspace. It is strictly a session instantiation and switching gateway.
- **`[INV-ROLE-04]` Faithful Workspace Display**: Client chrome must never display a local directory path as a session workspace unless that directory is formally bound in the session's `WorkspaceBinding`. Workspace-free sessions must project zero workspace indicators.

---

## Positive Consequences

- **Prefix Cache Stability**: 100% prefix cache retention across every single turn within a session. Zero cache-busting from role alteration.
- **Cognitive Hygiene**: Each agent role operates in a dedicated, uncontaminated conversation context.
- **Auditing & Provenance**: Sessions in the persistence store are pure, monolithic entities whose logs accurately reflect the declared role's permissions and behavior throughout history.
- **Honest User Interface**: The terminal header truthfully conveys the agent's persona and authority boundaries at a glance.

---

## Negative Consequences & Trade-offs

- **Session Multiplexing**: Users switching from coding to philosophical reflection will create a new session rather than continuing in the same buffer.
  - *Mitigation*: The TUI's instant session switching (`Ctrl+S` / `attach` / `/role`) makes hopping between sessions instantaneous (sub-millisecond locally), preserving seamless workflow.
- **Shared Working Memory**: Users cannot immediately ask the philosopher about code generated in the developer session without pasting or quoting it.
  - *Mitigation*: This is the exact design intent of cognitive isolation. Cross-session context can be intentionally bridged via explicit prompt injection, `@path` mentions, or system reminders, rather than accidental context leak.

---

## Rejected Alternatives & Negative Knowledge

### In-Place Profile Swapping (`agent.apply_role`)
- *Why considered*: Minimal implementation effort; preserves the active terminal buffer without triggering client re-attach.
- *Why rejected*: Catastrophically busts LLM prompt cache; creates contradictory tool history; introduces security ambiguity where tools admitted in round $N$ are invalid in round $N+1$ but remain in the model's chat history.

### Client-Side Working Directory Defaulting
- *Why considered*: Ensured the header always had a path to display so the layout looked uniform.
- *Why rejected*: Blatantly dishonest UI. Indicating that a `philosophist` or workspace-free research session is bound to `~/projects/muta` implies that the agent has access to repository files and git commands when it intentionally does not.

---

## Links

- Related ADRs: [ADR-0056](0056-model-context-assembly-boundary.md), [ADR-0137](0137-server-side-kv-cache-alignment-and-zoning.md), [ADR-0161](0161-route-scoped-inference-protocols-and-prompt-cache-contracts.md), [ADR-0197](0197-one-frontend-truth-shell-as-client-of-engine-and-protocol.md), [ADR-0219](0219-session-scope-and-optional-workspace-binding.md), [ADR-0242](0242-orthogonal-mcp-registry-and-role-capability-subscription.md), [ADR-0243](0243-unified-asset-attestation-and-zero-trust-hazard-mesh.md)
