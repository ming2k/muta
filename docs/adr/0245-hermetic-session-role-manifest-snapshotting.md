# 0245. Hermetic Session Role Manifest Snapshotting and Deterministic Forensic Replay

- **Status:** Accepted
- **Date:** 2026-10-03
- **Scope:** `core/contracts`, `storage/persistence`, `runtime/assembly`, `security/forensics`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0220](0220-personas-persisted-named-principals.md) (personas and roles), [ADR-0226](0226-session-grouping-is-derived.md) (session partition by workspace), [ADR-0241](0241-session-ir-causal-graph-and-compiler-pipeline.md) (session IR and storage boundaries), [ADR-0243](0243-unified-asset-attestation-and-zero-trust-hazard-mesh.md) (zero-trust hazard mesh), [ADR-0244](0244-immutable-session-role-and-truthful-identity-projection.md) (immutable session role and truthful projection)

---

## Context and Problem Statement

[ADR-0244](0244-immutable-session-role-and-truthful-identity-projection.md) established the mathematical immutability of a session: $\text{Session} = (\text{WorkspaceBinding}?, \text{Role}, \text{ImmutableEventLedger})$. However, an examination of the persistence boundary revealed a critical structural vulnerability: **Sessions store only a superficial role identifier string (e.g. `"developer"`, `"philosophist"`, or `"finance-analyst"`) in SQLite (`sessions.persona`), referencing external, mutable configuration files (`roles.toml`)**.

This "dangling pointer" pattern introduces two severe failure modes into the session lifecycle:

1. **Orphaned Session Fatal Crash (Broken Reference)**: If a user configures a custom role in `.muta/roles.toml` or `~/.config/muta/roles.toml`, conducts a multi-turn conversation, and later removes or renames the role definition (or migrates `muta.db` to an environment lacking that local file), attempting to resume the session (`mutx attach <id>`) causes `registry.rs` to error with `AssembleErr::AssembleFailed("unknown role '...'")`. The session becomes un-openable and permanently bricked, despite its transcript being fully preserved in SQLite.
2. **Historical Context Contamination and Cache Invalidation (Temporal Drift)**: If a user modifies a role's directive, tone, or tool whitelist in `roles.toml`, resuming a past session re-evaluates the external file, assembling the agent with *today's* modified policy rather than the policy that actually generated the historical conversation. This distorts provenance, breaks conversational continuity, and causes an immediate prompt cache miss on LLM providers due to mutated Base-Tier byte streams.

A session cannot truly be immutable if its governing identity and capability definition are held as a fragile pointer to an external, mutable file.

---

## Decision Drivers

- **Hermetic Self-Containment**: A session must be a self-contained capsule. Any session stored in `muta.db` must remain fully reproducible, readable, and resumable even if all external configuration files are deleted.
- **Byte-Identical Prompt Cache Preservation**: Resuming a session months later must reproduce the exact same `InstructionTier::Base` byte stream, guaranteeing 100% KV cache hit rates.
- **Forensic Truth (Provenance Ground Truth)**: A session's recorded history must be accompanied by the exact immutable definition (the "birth certificate") that governed the model's behavior during execution.
- **Physical Safety Decoupled from Cognitive Identity**: Historical role definitions determine model cognition and prompt assembly. Physical tool execution remains subject to the live zero-trust hazard mesh ([ADR-0243](0243-unified-asset-attestation-and-zero-trust-hazard-mesh.md)).

---

## Considered Options

- **Option 1: Status Quo (External Reference via `role_id`)**: Retain pointer lookup against `roles.toml` on resume.
  - *Defects*: Permanently subject to orphaned session crashes and prompt cache invalidation upon configuration edits.
- **Option 2: Fallback to Default `developer` on Missing Role**: If `roles.toml` no longer contains the role ID, silently coerce the session into a standard developer session.
  - *Defects*: Gross context distortion. Rebuilding a specialized philosophical or legal research agent as a full-privilege coding agent violates security confinement and persona integrity.
- **Option 3 (Chosen): Hermetic Session Role Manifest Snapshotting**: Capture a lightweight, immutable `SessionRoleManifest` at session creation time, persist it alongside the session row in SQLite, and treat it as the authoritative sole source of truth during all future assemblies.

---

## Decision Outcome

We implement Option 3. Every session captures a self-contained **Role Manifest Snapshot** at birth, decoupling historical execution from external file lifecycles:

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ Birth: Role Manifest Materialization (Session Creation Time)                │
│   • Evaluates target role from built-in roles or `roles.toml`.              │
│   • Materializes complete `SessionRoleManifest` (ADR-0246):                 │
│     - `role_id`: String identifier (kebab-case, e.g. "finance-analyst")     │
│     - `name`: Display name                                                  │
│     - `description`: Short description for listing                          │
│     - `instructions`: System prompt instructions in Base Tier               │
│     - `identity`: Canonical AgentIdentity                                   │
│     - `tools`: Permitted native tool allowlist patterns                     │
│     - `admit_mcp`: Permitted MCP server admission patterns                  │
│     - `created_at_s`: Epoch timestamp                                       │
│   • Atomically persisted into `sessions.role_manifest` in SQLite.          │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Immutable Provenance Ledger
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ Resume / Replay: Manifest as Sole Source of Truth                           │
│   • Reopening an existing session directly reads `role_manifest`.           │
│   • Zero dependencies on `roles.toml` remaining present or unmodified.      │
│   • 100% byte-identical `InstructionTier::Base` prefix cache restoration.  │
│   • External `roles.toml` serves strictly as a factory template for NEW     │
│     sessions, never retroactively altering existing ones.                   │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

### Data Model & Persistence Contract

In `muta-contracts`:
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRoleManifest {
    pub role_id: String,
    pub name: String,
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub identity: AgentIdentity,
    pub tools: Vec<String>,
    pub admit_mcp: Vec<String>,
    pub created_at_s: u64,
}
```

In `muta-persistence`:
- Migration introduces a `role_manifest TEXT` JSON column to the `sessions` table.
- `SessionData` incorporates `pub role_manifest: Option<SessionRoleManifest>`.
- `pin_fresh` and `reset_with` serialize the manifest upon session initialization.
- Database readers populate `SessionData.role_manifest` losslessly.

---

### Assembly & Replay Contract

When `muta-runtime` assembles an existing session (`assemble_hosted` under `SessionStart::Resume` or picker attachment):

1. **Manifest Priority**: The runtime inspects the loaded session's `role_manifest`.
2. **Self-Contained Restoration**:
   - If present, the agent is assembled directly from the manifest's `identity`, `tools`, and `admit_mcp`.
   - The assembly does not query `roles.toml`. Even if `roles.toml` has been deleted or changed, the session resumes with 100% fidelity.
3. **Graceful Backward Compatibility**:
   - For legacy sessions created prior to this ADR that lack a `role_manifest`, the runtime falls back to querying `roles.toml` / built-in roles, and on first load backfills the synthesized manifest into SQLite.

---

## Invariants & Behavioral Boundaries

- **`[INV-ROLE-05]` Manifest Ground Truth**: A persisted session's `SessionRoleManifest` is the sole authoritative specification of its identity, cognitive posture, and capability envelope. The runtime must never overwrite an existing session's manifest with external configuration file changes.
- **`[INV-ROLE-06]` Zero Dangling Dependencies**: No session in `muta.db` may fail to resume due to missing external role configuration files. Every session must be completely hermetic and portable across machines.
- **`[INV-ROLE-07]` Cognitive Invariance vs. Physical Egress**: The manifest freezes the model's cognitive identity and system prompt. Physical tool invocations remain governed by the host's active `AssetAttestationLedger` ([ADR-0243](0243-unified-asset-attestation-and-zero-trust-hazard-mesh.md)); if an external tool has been uninstalled or revoked on the host, invocation fails gracefully at dispatch without altering model prompt history.

---

## Positive Consequences

- **Total Immunity to Configuration Deletion**: Users can freely delete, refactor, or experiment with `.muta/roles.toml` without risking the destruction of historical sessions.
- **Perfect Prompt Cache Retention**: Every turn resumed in an existing session retains 100% byte-matching with prior rounds in `InstructionTier::Base`.
- **Database Portability**: Moving `muta.db` to a fresh environment (e.g. CI/CD or another workstation) allows instant, error-free resumption of any session without copying `~/.config/muta/`.
- **Forensic Auditability**: An auditor or operator can inspect the exact role specification that produced a turn at any point in history.

---

## Negative Consequences & Trade-offs

- **Storage Footprint**: Adds approximately 300–500 bytes of JSON per session record in SQLite.
  - *Mitigation*: Negligible compared to the multi-kilobyte transcript payloads; written exactly once per session lifetime.
- **Explicit Evolution Required**: Modifying a role configuration will not retroactively update past conversations.
  - *Mitigation*: This is the exact design intent. To use an updated role configuration on an existing task, the user forks the session (`/fork`) or creates a new one (`/new`), preserving historical integrity.

---

## Links

- Related ADRs: [ADR-0220](0220-personas-persisted-named-principals.md), [ADR-0226](0226-session-grouping-is-derived.md), [ADR-0241](0241-session-ir-causal-graph-and-compiler-pipeline.md), [ADR-0243](0243-unified-asset-attestation-and-zero-trust-hazard-mesh.md), [ADR-0244](0244-immutable-session-role-and-truthful-identity-projection.md)
