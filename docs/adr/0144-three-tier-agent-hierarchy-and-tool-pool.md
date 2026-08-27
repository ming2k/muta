# 0144. Three-tier Agent Hierarchy and Unified Tool Pool

- **Status:** Accepted
- **Date:** 2026-08-26
- **Supersedes:** ADR-0042, ADR-0138

## Context

The agent model grew two vocabularies over time that were never unified:

1. **`Principal` / `Envoy`** (ADR-0042, ADR-0011, ADR-0053): the *live* design.
   A session hosts one principal agent (the user-facing conversation) that may
   spawn read-only envoys through the `task` tool. Roles are declarative
   profiles (`PrincipalProfile::for_role`, `EnvoyProfile`) that scope tools.
2. **`Actor`** (ADR-0138): a *scaffolding* design — an actor-model subsystem
   (`muta-agent/src/actor/`, `muta-contracts/src/actor.rs`) with mailboxes, a
   supervisor registry, hierarchical cancellation, worktree isolation. It was
   never wired into the live path.

Two problems follow from that split:

- **The tier structure is implicit.** Nothing in the vocabulary says "the
  thing that hosts sessions" differs in kind from "the thing the user talks
  to" and "the thing spawned to keep the conversation clean." The runtime's
  `SessionRegistry` does supervisor work (hosting, reaping, monitoring
  sessions) but is not an agent, has no tools, and cannot be steered.
- **The sandbox is baked into the trust model.** `WorkspaceSandboxState`
  lives in `security.rs` and decides containment policy for the *whole
  workspace*. But the architectural distinction users actually care about —
  "developer" vs. "code analyst" — is exactly *whether native command-line
  execution is available*. Code analysis needs to run `cargo test`-class
  probes in containment; the developer persona runs on the host. That is a
  **capability of a role**, not a posture of a workspace.

At the same time, the pieces already exist for the right architecture:

- `ToolSet` + `register_tool!` + `ToolContext` is already a distributed
  capability registry — a de-facto **tool pool** that agents *declare*
  requirements against (profiles already filter it by `ToolScope`).
- `EnvoyTool` already proves the spawn/reap pattern: child agents are created
  from a profile, tracked in a registry keyed by parent call id, and steered
  through an inbox (`AgentOp`).
- `Agent::apply_principal_role` already proves atomic role replacement at the
  top tier.

## Decision

### 1. One word: agent. Three tiers: Supervisor / Master / Runner

All three tiers are agents. They differ **only** in tier, never in kind.
Formally, `crates/muta-contracts/src/tier.rs` defines:

```rust
pub enum AgentTier { Supervisor, Master, Runner }
```

with a total ordering (`Supervisor < Master < Runner` by depth), an `is_parent_of`
predicate, and a `hops_to_root`. Every agent identity in the system carries its
tier. The old mapping is:

| Old | New |
| --- | --- |
| `Principal` (top-level role/profile) | `Master` (tier-1 agent), presets replace roles |
| `Envoy` (task tool child) | `Runner` (tier-2 agent), presets replace profiles |
| `SessionRegistry` hosting duties | `Supervisor` (tier-0 agent), one per daemon |
| `actor::ActorRole` scaffolding | subsumed: tier + preset id |

`PrincipalProfile` → `MasterPreset`, `PrincipalRole` → `MasterPresetId`,
`EnvoyProfile` → `RunnerPreset`, `EXPLORE`/`CODE`/`TITLE`/`MCP_SPECIALIST` →
`RUNNER_EXPLORE`/`RUNNER_CODE`/`RUNNER_TITLE`/`RUNNER_MCP_SPECIALIST`
(with legacy `const` aliases so in-tree call sites compile unchanged through
the transition). The `actor/` scaffolding crate-module is retired: its
surviving ideas live on in the **mesh** (§4).

### 2. Agents declare tools; tools live in one pool

The existing `ToolSet` *is* the pool; what was missing is the declaration
seam. `ToolPool` (in `tool_registry.rs`) formalizes it:

- `ToolPool::new(toolset)` freezes a capability set as the session-wide pool.
- `ToolPool::declare(name, ToolScope)` records an agent's requirement.
- `ToolPool::resolve(declaration)` yields the concrete variant set for that
  agent, layering the pool's runtime hard rules (non-interactive agents never
  see `ask_user`) on top of the declared scope.
- `ToolPool::snapshot()` reports `(capability, requested, admitted, denied)`
  for UI/audit.

Crucially **the sandbox becomes a tool, not a trust posture**: a
`sandbox_bash` capability exists in the pool alongside `bash`. A preset that
declares `sandbox_bash` gets contained execution; one that declares `bash`
gets host execution. `WorkspaceSandboxState` remains as the *physical
availability* fact the platform reports (you cannot enforce containment on a
host without the runtime), but it no longer decides role capability — a
preset demanding `sandbox_bash` on a host where enforcement is `Unavailable`
fails closed at **resolve** time with an explicit error, instead of silently
degrading the whole workspace into the sandbox posture. (Landed shape note:
containment is expressed through the preset's delegation — `write_paths` /
`command_allowlist` in `MasterPresetDelegation` — rather than a literal
`sandbox_bash` tool variant; the capability split is the decision, the
spelling above was the working sketch.)

### 3. Master presets: developer and code-analyst

Two tier-1 presets ship (in `crates/muta-contracts/src/master.rs`):

- **`MASTER_DEVELOPER`** — native toolchain. Declares `bash` (host exec), the
  file tools, web, and the full runner catalog. This is the default.
- **`MASTER_CODE_ANALYST`** — no native command line. Declares `sandbox_bash`
  instead of `bash`: contained execution good for `cargo metadata`-class
  probes and basic functional tests, never host writes. Runner presets
  available to it are restricted to read-only ones (`RUNNER_EXPLORE`,
  `RUNNER_TITLE`).

A session still has exactly one master at a time. Replacement is **atomic and
reclaims runners first**: `MasterSlot::replace` drains the old master's live
runners (cancel + join + registry sweep) *before* the new master takes the
slot, so no runner can hold a reference (or a conversation reference) across
the swap — the "word-source leak" the old design allowed when a principal
role change left an envoy's transcript parented to a dead role.

### 4. The mesh: bottom-up, top-down, and peer communication

Networking is a first-class seam, designed like a BitTorrent tracker: one
**tracker** per tier per daemon knows every live address; agents talk by
address. `crates/muta-contracts/src/mesh.rs` defines:

- `MeshAddress { tier, session, agent }` — globally unique, sortable,
  serializable.
- `MeshEnvelope { id, sender, recipient, message }` — one envelope shape for
  all directions (up, down, peer).
- `MeshMessage` — the payloads: `Ping`/`Pong` (liveness), `Status` (state
  digest), `Instruction`/`InstructionAck`, `Report`/`ReportAck`,
  `ProgressNote` (fire-and-forget up-channel), `PeerNote` (fire-and-forget
  peer channel), `RunnerEol` (graceful runner end-of-life).
Elders sign their directions (`Instruction`/`Report`), so a runner cannot be
commanded by a sibling and a master cannot be reported to by a runner it does
not own.
- `MeshTracker` — the in-process tracker: address → live mailbox sender,
  hierarchical cancellation per address, and lease-based reaping.

Delivery semantics are deliberately minimal and honest: control payloads are
acknowledged (`Instruction`/`Report`); progress is fire-and-forget
(`ProgressNote`); peer traffic is fire-and-forget (`PeerNote`) with the
tracker mediating address discovery. The in-process transport is an unbounded
mpsc per agent; the contract types are transport-neutral so a future socket
transport can carry the same envelopes without touching the agent loop.

### 5. Supervisor

One `Supervisor` per daemon (tier-0). It is the agent-ification of what
`SessionRegistry` already does: session orchestration, tracking, and joint
debugging (联调). It gets a narrow tool surface (`supervisor_*` capabilities:
session listing/overview, attach-for-read, WIP consultation) and is the root
of the mesh: masters register with it, runners register with their master.
Its monitoring bus (`MonitorEvent`) is unchanged — the supervisor *publishes*
to it rather than owning it.

### 6. Compatibility

- **Wire/persistence:** every renamed serialized shape keeps its old tag:
  `EnvoyMeta`/`EnvoyEvent` serialize as `envoy`-spelled JSON (serde alias on
  the struct name is not possible, so the *variant/field* names carry the
  compat via `#[serde(alias)]` where needed and the `ts_rs` export keeps the
  generated TS type names stable). Session transcripts recorded before this
  ADR load unchanged.
- **Config:** `[principal]` config tables keep parsing; new `[master]` /
  `[runner]` spellings are accepted as aliases; configuration standardizes
  on `[master]` (e.g. `[master.doom_guard]`).
- **Naming:** in-tree source identifiers move to the new vocabulary
  (`envoy_tool.rs` → `runner_tool.rs`, `EnvoyTool` → `RunnerTool`,
  `EnvoyRegistry` → `RunnerRegistry`, `EnvoyHandle` → `RunnerHandle`);
  low-level task panic supervision moved under the orchestration/runner-tool
  modules rather than a dedicated `task_fault_tolerance.rs` file, so it
  cannot collide with the top-level `Supervisor`.
  Frontend rendering vocabulary ("envoy" strings in the TUI) is updated in the
  same change; snapshot tests are regenerated.
- **ADR-0042 / ADR-0011 / ADR-0053 / ADR-0138:** superseded where they speak
  to tier vocabulary and the actor scaffolding; their *profile/scoping*
  mechanics survive intact inside the new preset types.

## Consequences

- Clean conceptual hierarchy: every entity is an `Agent` distinguished only
  by tier, mission, and tool declaration.
- Deterministic KV-cache stability across turns through centralized tool
  resolution (one pool, one facade, no per-call-site schema drift).
- Seamless multi-agent collaboration with unified distributed trace metadata
  (`MeshEnvelope`).
- The tier lattice is now explicit, typed, and total: any code can ask "what
  tier am I talking to" and enforce invariants (e.g. only elders command).
- The sandbox question stops being a workspace posture and becomes a declared
  capability, which is what the developer/analyst distinction actually needs.
- Master replacement is safe: no orphaned runners, no leaked conversation
  references — reclamation is a precondition of the swap, not a cleanup after
  it.
- Peer communication has a place to grow (`PeerNote` + tracker-mediated
  discovery) without inventing a second messaging substrate later.
- Cost: a wide but mechanical rename across contracts/agent/runtime/frontends,
  one new crate-module (`mesh`), and regenerated snapshots. The `actor/`
  scaffolding is deleted rather than maintained in parallel.
