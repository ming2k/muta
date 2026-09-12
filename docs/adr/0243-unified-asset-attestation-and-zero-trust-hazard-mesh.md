# 0243. Unified Asset Attestation, Provenance Gate, and Zero-Trust Hazard Mesh

- **Status:** Accepted
- **Date:** 2026-10-02
- **Scope:** `security/attestation`, `core/contracts`, `core/agent`, `muta-mcp`, `muta-persistence`, `muta-runtime`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0146](0146-tool-hazard-model-and-permission-submissions.md) (tool hazard model), [ADR-0147](0147-orthogonal-workspace-security-planes.md) (orthogonal workspace security planes), [ADR-0204](0204-egress-confinement-ssrf-defense-and-http2-boundary.md) (egress confinement), [ADR-0219](0219-session-scope-and-optional-workspace-binding.md) (workspace-free sessions), [ADR-0240](0240-main-agent-mcp-unification-and-lifecycle-activity-architecture.md) (main agent direct MCP), [ADR-0242](0242-orthogonal-mcp-registry-and-role-capability-subscription.md) (orthogonal MCP architecture)

---

## Context and Problem Statement

Security in AI agent harnesses historically mirrored traditional IDE conventions: security gates were anchored exclusively to a physical repository directory (the "Workspace"). ADR-0147 established domain-specific content digests, and ADR-0240 instituted a two-tier model distinguishing global user configs from quarantined workspace configs.

However, the expansion into workspace-free conversational and research roles ([ADR-0219](0219-session-scope-and-optional-workspace-binding.md), [ADR-0242](0242-orthogonal-mcp-registry-and-role-capability-subscription.md)) and the realities of modern AI agent security revealed critical structural flaws in this heritage:

1. **The Workspace-Free Security Blindspot**: Because `WorkspaceSecurityStore` was gated strictly on `workspace_root.is_some()`, any session without a workspace (e.g. `mutx --role philosophist`) completely bypassed asset trust. External stdio subprocesses (`command = ["python", "..."]`) declared in global or user-imported role files were spawned silently in the background without user knowledge or consent.
2. **The Fragile "User Config is Always Trusted" Dogma**: In traditional Unix systems, `~/.config/` was assumed to be manually authored by the machine owner. In the agentic era, users download third-party role bundles, copy-paste community configurations, or run `muta mcp import`. Treating all user-level configs as implicitly trusted creates an unmonitored supply-chain vector for arbitrary code execution (RCE).
3. **The Confused Deputy Problem (Indirect Prompt Injection)**: Even when a tool is legitimately configured by the user (e.g., a local Obsidian or Notion MCP), an untrusted external web page or document read by the agent can inject malicious instructions coercing the model into exfiltrating private data via that tool. Static trust at launch does not equal safe execution at runtime.
4. **Workspace Capability Specialization**: While workspace-level MCP definitions exist, projects also require the ability to tighten or customize role capability slices (e.g., restricting `developer` to internal databases and forbidding public web search tools within a proprietary codebase).

We require an uncompromising, legacy-free architecture that decouples asset attestation from workspace directories, establishes cryptographic fingerprinting for all external executable units, enforces a four-tier runtime hazard mesh against prompt injection, and provides clean cascading workspace role overrides.

---

## Decision Drivers

- **Zero Implicit Trust**: Configuration placement (`~/.config/` vs `.muta/`) determines discovery scope, never security immunity. No external OS child process may execute without cryptographic attestation.
- **Universal Security Parity**: Workspace-free sessions (`philosophist`) and workspace-bound sessions (`developer`) must share identical, uncompromised asset attestation standards.
- **Asymmetric Friction UX**: The happy path (99.9% of everyday usage and explicit CLI operations) must remain 100% zero-friction and instantaneous. Friction must appear exclusively on first-use unverified assets or high-hazard data exfiltration.
- **Defense in Depth Against Cognitive Hijacking**: Distinguish physical process launch authorization from model-directed runtime tool invocation. High-hazard actions (network egress, destructive mutations) must pass through an active runtime hazard mesh.
- **Clean Cascading Specialization**: Workspace configurations must be able to override global role subscriptions without configuration multiplication.

---

## Decision Outcome

We establish a clean-break security and capability architecture across three unified pillars:

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ Pillar 1: Universal Asset Attestation Ledger (Physical Spawn Plane)         │
│    • Replaces WorkspaceSecurityStore with universal AssetAttestationLedger. │
│    • Covers ALL external stdio commands and remote endpoints across all     │
│      scopes (User Global, Roles, and Workspace).                            │
│    • Cryptographic SHA-256 fingerprinting: untrusted assets fail closed     │
│      (Quarantined) regardless of whether a workspace is bound.             │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Verified Process Pool
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ Pillar 2: Cascading Role Capability Slicing (Logical Governance Plane)       │
│    • Effective Role Slice = Workspace Override ?? Global Role Subscription  │
│    • Enables workspaces to enforce strict tool confinement for compliance.  │
│    • Zero-latency in-memory set evaluation.                                 │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Candidate Admitted Tools
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ Pillar 3: Four-Tier Runtime Hazard Mesh (Execution & Egress Plane)          │
│    • Model-directed invocations pass through runtime Hazard Engine:         │
│      - Tier 0: Pure Local Query / Read-Only → Silent execution.             │
│      - Tier 1: Workspace Mutation → Sandboxed with undo/commit journal.     │
│      - Tier 2: Network Egress & Exfiltration → Interactive consent sheet.   │
│      - Tier 3: Shell & Arbitrary Code Exec → Strict gate / approval.       │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 1. Pillar 1: Universal Asset Attestation Ledger

We retire the directory-bound `WorkspaceSecurityStore` and introduce the global `AssetAttestationLedger` (persisted in SQLite at `$XDG_DATA_HOME/muta/assets.db`).

#### Canonical Asset Specification (`AssetSpec`)
Every external capability is represented as an atomic `AssetSpec`:
```rust
pub enum AssetSpec {
    /// External OS child process (Stdio MCP server, lifecycle hooks).
    Process {
        command: Vec<String>,
        env: BTreeMap<String, String>,
    },
    /// External network service (Streamable HTTP / SSE MCP server).
    RemoteEndpoint {
        url: String,
        headers: BTreeMap<String, String>,
    },
}
```

#### Cryptographic Fingerprint
The ledger computes a normalized SHA-256 digest over the canonical representation of the asset (command tokens, sensitive environment keys, endpoint URL).

#### Attestation Lifecycles
1. **Implicit Intent Exemption**: When a user explicitly registers a capability through interactive CLI verbs (e.g. `muta mcp add <name> -- <cmd>` or `/role add`), the human interaction itself constitutes cryptographic consent. The CLI automatically registers the calculated digest as `Trusted`.
2. **External / Imported Provenance**: When an asset definition is loaded from a cloned repository (`.muta/mcp.json`), a downloaded configuration file (`roles.toml`), or a piped import (`muta mcp import`), its fingerprint is checked against `assets.db`:
   - **Matched Fingerprint**: Trusted, spawns immediately without prompts.
   - **Unknown / Altered Fingerprint**: Quarantined (`[INV-TRUST-02]`). Background daemon spawn is blocked. The TUI/CLI displays a non-blocking attestation card offering `[A] Trust Always`, `[S] Allow for Session Only`, or `[D] Deny`.

### 2. Pillar 2: Cascading Role Capability Slicing

Role capability subscriptions ([ADR-0242](0242-orthogonal-mcp-registry-and-role-capability-subscription.md)) gain clean cascading override semantics:

```text
Global Role Definition (~/.config/muta/roles.toml)
  [roles.developer] admit_mcp = ["*"]
        │
        ▼  (Overridden when present in workspace)
Workspace Override (<workspace_root>/.muta/config.toml)
  [roles.developer] admit_mcp = ["local_pg", "corp_audit"]
        │
        ▼
Effective Role Slice = Workspace Override ?? Global Definition
```

- When a workspace defines `[roles.<name>]`, its `admit_mcp` rules completely override the global role's subscription for sessions bound to that workspace.
- Unbound sessions (workspace-free) fall back directly to global definitions.

### 3. Pillar 3: Four-Tier Runtime Hazard Mesh

Physical process trust does not guarantee runtime intent safety. To prevent confused deputy attacks via indirect prompt injection, all tool calls pass through a unified runtime hazard classification:

- **Tier 0: Pure Local Query / Read-Only** (`read_text`, `search_web`, read-only MCP queries). Executed silently with audit logging. Zero user interruption.
- **Tier 1: Local Workspace Mutation** (`edit_text`, `write_file`, sandboxed database modifications). Permitted within workspace boundaries; recorded in the turn commit journal for instant undo.
- **Tier 2: Outbound Egress & Exfiltration** (External HTTP calls, MCP tools transmitting payloads to external hosts). The harness pauses the turn and renders an interactive disclosure card detailing the target host, tool name, and payload summary. The user must approve or reject the egress.
- **Tier 3: Arbitrary Command Execution** (`execute_command`, shell tools). Governed by strict bash policy and approval gates.

---

## Invariants & Behavioral Boundaries

- **`[INV-TRUST-01]` Zero Implicit Trust**: Configuration file placement never grants automatic code execution. Any external OS process or network endpoint must possess an attestation record in `AssetAttestationLedger` before physical execution.
- **`[INV-TRUST-02]` Universal Asset Attestation**: The absence of a filesystem workspace binding (`workspace_root = None`) must never bypass asset attestation. Workspace-free roles and workspace-bound projects are subject to identical cryptographic verification.
- **`[INV-TRUST-03]` Fail-Closed Process Spawning**: `McpRuntime` must reject spawning any child process whose `AssetSpec` digest is unverified or quarantined.
- **`[INV-TRUST-04]` Cryptographic Provenance Invalidation**: Any mutation to a tool's command line, environment flags, or endpoint URL alters its SHA-256 fingerprint, immediately invalidating prior trust and reverting the asset to `Quarantined`.
- **`[INV-TRUST-05]` Runtime Egress Quarantine**: No tool may transmit arbitrary prompt-derived payloads to an external network destination under automated execution without passing Tier 2 hazard verification.
- **`[INV-TRUST-06]` Cascading Slice Override**: Workspace role definitions take absolute precedence over global role definitions for sessions bound to that workspace, without mutating global configuration files.

---

## Positive Consequences

- **Total Security Coherence**: Completely eliminates the blindspot where non-workspace roles could be exploited for unverified code execution.
- **Robust Defense Against Indirect Prompt Injection**: Egress hazards are checked at invocation time, neutralizing malicious web-page instructions attempting data exfiltration.
- **Zero Friction on Developer Workflows**: Interactive CLI commands remain instant and prompt-free; trusted assets stay silent forever until their bytes change.
- **Enterprise & Monorepo Readiness**: Projects can enforce tight compliance boundaries on agent capabilities via workspace role overrides.
