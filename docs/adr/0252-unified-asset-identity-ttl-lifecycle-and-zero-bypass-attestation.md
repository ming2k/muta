# 0252. Unified Asset Identity, TTL Lifecycle Lease, and Zero-Bypass Attestation

- **Status:** Proposed
- **Date:** 2026-10-18
- **Scope:** `security/attestation`, `core/contracts`, `core/runtime`, `cli/mcp`, `muta-persistence`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0147](0147-orthogonal-workspace-security-planes.md) (orthogonal security planes), [ADR-0175](0175-pre-attach-workspace-trust-interstitial.md) (pre-attach trust gate), [ADR-0185](0185-non-blocking-workspace-trust-pipeline.md) (non-blocking trust pipeline), [ADR-0243](0243-unified-asset-attestation-and-zero-trust-hazard-mesh.md) (unified asset attestation ledger)
- **Amends / Supersedes:** Amends ADR-0243 §1 (retiring "Implicit Intent Exemption"); permanently retires `muta mcp add/rm/set-enabled` CLI mutation paths

---

## Context and Problem Statement

ADR-0243 decoupled asset trust from directory-bound workspace roots by introducing `AssetAttestationLedger`. While this closed the workspace-free security blindspot, two deep design flaws and one conceptual compromise remain in the active architecture:

1. **The Content-Only Identity Anti-Pattern (Unbounded Accumulation)**:
   `AssetAttestationLedger` indexes records strictly by content digest (`attestation:{sha256}`). When an asset (e.g. an MCP server command line or a Python script) evolves at a known path, a new hash is appended while the old hash remains `Trusted` indefinitely. The ledger becomes a strictly monotonic, append-only graveyard of historical hashes. There is no concept of *superseding* an older revision at the same logical locator.

2. **The "Permanent Trust" Blindspot (Missing Lifecycle Lease)**:
   Once an asset is trusted, it remains valid forever until manually revoked. An experimental or third-party executable vetted six months prior retains full OS child-process spawn authority indefinitely, violating zero-trust principles.

3. **The "Implicit Intent" Privilege Bypass (`muta mcp add` & Global Config Auto-Trust)**:
   ADR-0243 permitted an "Implicit Intent Exemption" where `muta mcp add` silently stamped assets as `Trusted`, and `crates/muta-runtime/src/bootstrap.rs` automatically trusted all user-level configs with `sandbox_root.is_none()`. This creates a severe supply-chain vulnerability: any background script, installer, or injected tool executing `muta mcp add` or appending to `~/.config/muta/config.toml` achieves unmonitored arbitrary code execution (RCE) without human confirmation.

We require an uncompromising, long-term, zero-legacy architecture that:
- Establishes a composite identity `(Locator, ContentDigest)` with deterministic in-place replacement semantics;
- Enforces an immutable 30-day lease (Time-To-Live) on all grants;
- Eradicates all implicit intent bypasses and permanently retires imperative CLI configuration mutations (`muta mcp add`) in favor of declarative configuration and unified gate resolution.

---

## Decision Drivers

- **Zero Implicit Bypass**: Neither configuration file location (`~/.config/` vs `.muta/`) nor ingestion vehicle (`CLI command` vs `file edit` vs `git clone`) confers automatic execution authority.
- **No Permanent Trust**: All cryptographic approvals carry an explicit, bounded lease (default 30 days). Expired assets cleanly fall back to quarantine.
- **Replacement over Accumulation**: Modifying an asset at a known locator must supersede the prior digest rather than accumulating dangling historical approvals.
- **Single Gating Funnel**: All unverified, altered, or expired assets must converge into a single, unified entry gate before turn execution begins.
- **Declarative Configuration Uniformity**: Configuration is declared in TOML files. Imperative CLI mutation shims that smuggle privilege are eliminated.

---

## Considered Options

- **Option 1: Retain CLI mutations with explicit trust prompts** (`muta mcp add` prompts for confirmation at invocation time).
- **Option 2: Content-Only Ledger with Global Garbage Collection** (Periodic cleanup of unreferenced SHA-256 keys).
- **Option 3: Composite Asset Identity, 30-Day TTL Lease, Complete Purge of Privileged CLI Bypasses, and Unified Gate Resolution** (Clean-Break Architecture).

---

## Decision Outcome

Chosen option: **Option 3**. We execute an uncompromised, clean-break overhaul of the asset attestation subsystem.

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ 1. Unified Asset Declaration (Declarative Sources Only)                     │
│    • User Config (~/.config/muta/config.toml)                               │
│    • Workspace Config (<root>/.muta/config.toml, .muta/skills/, hooks)     │
│    *(Imperative CLI mutations like `muta mcp add` permanently deleted)*     │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Raw Asset Specifications
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 2. Composite Asset Identity & Ledger (muta-persistence)                     │
│    • Primary Key: AssetLocator (e.g. `user:mcp:postgres`, `ws:<hash>:skill`)│
│    • Value: { fingerprint: SHA256, trusted_at_s, expires_at_s (TTL 30d) } │
│    • Mutation Semantics: Same Locator + New Hash ──► Supersede & Quarantine │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Candidate Asset Pool
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 3. Bootstrap Attestation Diff Engine & Unified Gate (muta-runtime / mutx)   │
│    • Computes Diff-Set: { a ∈ Candidates | status(a) ≠ Trusted ∨ Expired }  │
│    • Diff-Set == ∅  ──► Silent, instantaneous session entry (Zero Friction) │
│    • Diff-Set ≠ ∅  ──► Unified Pre-Attach Gate (Review New/Changed/Expired) │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

### 1. Composite Asset Identity (`AssetLocator`) & Replacement Semantics

Asset identity is elevated from a naked content hash into a composite pair:

$$\text{AssetKey} = (\text{Locator}, \text{Fingerprint})$$

#### Canonical Asset Locator (`AssetLocator`)
Every asset is assigned a deterministic, human-readable locator:
```rust
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AssetLocator {
    /// User-level MCP server declared in global config.toml.
    UserMcp { name: String },
    /// User-level custom skill in ~/.config/muta/skills or global paths.
    UserSkill { name: String },
    /// User-level lifecycle hook in global configuration.
    UserHook { event: String },
    /// Workspace-scoped MCP server declared in workspace configuration.
    WorkspaceMcp { workspace_root: PathBuf, name: String },
    /// Project-level custom skill.
    WorkspaceSkill { workspace_root: PathBuf, name: String },
    /// Project-level lifecycle hook.
    WorkspaceHook { workspace_root: PathBuf, event: String },
    /// Standalone external script or executable referenced by absolute path.
    ExternalPath { path: PathBuf },
}
```

#### Deterministic Replacement Semantics
When an asset at `locator` is evaluated against the ledger:
1. **Identical Hash & Unexpired**: Status is `Trusted`.
2. **Identical Hash & Expired**: Status is `Expired` (prompts for routine lease renewal).
3. **Different Hash**: The asset has changed on disk. The ledger immediately updates the active record for `locator` to `status = Changed`, binding the new candidate fingerprint. The prior hash is superseded and can no longer execute.
4. **Unknown Locator**: Status is `Quarantined`.

This guarantees that `assets.db` never leaks unbounded historical hashes, and rolling back or altering a file triggers explicit invalidation.

---

### 2. 30-Day Bounded Lease (`TTL = 2,592,000s`)

No asset grant is permanent.

- **Lease Duration**: Constant $\tau_{\text{lease}} = 30 \times 86400 = 2,592,000 \text{ seconds}$ (30 calendar days).
- **Lease Computation**:
  $$\text{is\_valid}(t) \iff \text{status} == \text{Trusted} \land t \le (\text{trusted\_at\_s} + \tau_{\text{lease}})$$
- **Lease Refresh**: Stamping `/trust` on an asset updates `trusted_at_s = now()` and `expires_at_s = now() + 2_592_000`, resetting the countdown.
- **Fail-Closed on Expiration**: When $t > \text{expires\_at\_s}$, the asset fails closed with `AttestationStatus::Expired`. It cannot be spawned or evaluated until the user reviews and renews the lease.

---

### 3. Eradication of Implicit Intent Bypasses & Retirement of `muta mcp add`

#### Deletion of Imperative CLI Mutations
The following subcommands in `crates/muta/src/commands/mcp.rs` are completely deleted:
- `muta mcp add`
- `muta mcp rm` / `muta mcp remove`
- `muta mcp enable` / `muta mcp disable`

#### Rationale for Complete Deletion:
1. **Zero Legacy Burden**: Muta is a declarative-first system. Configuration belongs in clean, versionable, user-managed TOML files (`~/.config/muta/config.toml` or `<repo>/.muta/config.toml`). Imperative CLI mutators encourage ad-hoc, untracked modifications.
2. **Elimination of the Privilege Trojan**: Any executable invoked in a shell script could run `muta mcp add` to unilaterally grant itself persistent background child-process spawn authority. Deleting the command completely removes this attack surface.
3. **Discovery & Inspection Only**: The `muta mcp` CLI is retained strictly for read-only inspection (`muta mcp list`, `muta mcp get <name>`).

#### Deletion of Global Config Auto-Trust
The auto-trust loop in `crates/muta-runtime/src/bootstrap.rs` (`if server_cfg.sandbox_root.is_none() { attestation_ledger.trust_asset(&spec); }`) is completely removed. User-level MCP servers declared in global `config.toml` must pass the exact same attestation ledger as workspace assets.

---

### 4. Unified Session-Entry Attestation Diff Engine & First-Session Review

Rather than scattering trust checks across bootstrap, background connection spawns, and TUI sheets, all attestation requirements converge into a single entry check:

1. **Pre-Session Attestation Diff**:
   At session launch (`mutx` or daemon attach), the runtime collects every candidate capability required by the session (all enabled MCP servers, skills, hooks, and instructions across **both** user-level and workspace-level scopes).
2. **Diff Calculation**:
   The engine computes:
   $$\mathcal{D} = \{ a \in \text{Candidates} \mid \text{status}(a) \in \{\text{Quarantined}, \text{Changed}, \text{Expired}\} \}$$
3. **Interactive Multi-Select & Partial Asset Authorization**:
   - **If $\mathcal{D} = \emptyset$**: Proceed directly to session execution. Zero friction, zero latency.
   - **If $\mathcal{D} \neq \emptyset$**: Render the unified Pre-Attach Interstitial with interactive multi-select checkboxes (`multi_select: true`).
   - **Default Full Selection with Surgical Un-toggling**: All unverified assets start pre-selected by default. The operator may navigate with `↑`/`↓`, toggle individual assets with `Space`, and confirm with `Enter`. Pressing `Esc` cleanly aborts the session.
   - **Zero Forced Quits for Partial Trust**: The operator is **never forced to quit** to withhold trust from a specific tool. If an asset is left unselected (`[ ]`), the session enters normally, but the unselected asset remains strictly quarantined:
     - Unselected MCP servers are **not spawned** and their tools are completely invisible to the agent;
     - Unselected skills and hooks are **not loaded**;
     - Unselected instructions are **omitted from prompt context**.
   - **Transparent File Asset Representation**: Assets are presented directly by their physical file paths:
     ```text
     [file asset trust]

     Select unverified file assets to authorize:
     (Unselected assets will remain quarantined and invisible during this session)

       [x] ~/.muta/config.toml
           User global configuration
       [x] .muta/config.toml
           Workspace configuration & additional roots
       [x] .muta/skills/
           Project-local custom skills
       [x] AGENTS.md
           Project rules and instructions
       [x] .muta/mcp.json
           Model Context Protocol servers

     ↑/↓ navigate   Space toggle   Enter confirm   Esc quit
     ```
   - Confirming with `Enter` updates the ledger exclusively for the selected assets (writing SHA-256 fingerprints and 30-day lease timestamps), then proceeds into the session immediately.

---

## Invariants & Behavioral Boundaries

- **`[INV-ASSET-01]` Zero Implicit Bypass**: Neither file placement nor CLI invocation method may grant attestation status. Every external process or endpoint requires an explicit human attestation record in `AssetAttestationLedger`.
- **`[INV-ASSET-02]` Bounded Lease Lifespan**: No attestation record may have an indefinite lease. All grants expire after 30 days ($\le 2,592,000$ seconds) without exception.
- **`[INV-ASSET-03]` Single-Writer Composite Keying**: Ledger entries are keyed by `(AssetLocator, Fingerprint)`. A change in content at an existing locator replaces the prior active record and reverts status to `Changed`.
- **`[INV-ASSET-04]` Pure Read-Only MCP CLI**: The CLI command `muta mcp` must not expose mutation verbs (`add`, `rm`, `enable`, `disable`). Configuration is purely file-authored.
- **`[INV-ASSET-05]` Fail-Closed Execution Spawning**: Any attempt by `McpClient` or lifecycle hooks to spawn a child process or connect to a remote endpoint whose active attestation record is not `Trusted` (or whose lease is expired) must abort immediately with a typed attestation error.
- **`[INV-ASSET-06]` Atomicity of Trust Grants**: Approving an asset grant updates `fingerprint`, `trusted_at_s`, and `expires_at_s` atomically via the authoritative `PersistenceHandle`.

---

## Positive Consequences

- **Total Supply-Chain Immunity**: Malicious scripts, dependencies, or compromised user config files can never achieve silent RCE. Even if placed in `~/.config/`, they are caught at entry by the diff engine.
- **Zero Fingerprint Leaks**: Old, vulnerable binary hashes are automatically overwritten when an asset is updated.
- **Hygiene via Expiration**: Dormant or forgotten external tools automatically fall out of trust after 30 days, keeping the attack surface tightly bound to actively used tools.
- **Radical Architectural Simplification**: Eradicates hundreds of lines of imperative CLI mutation code, ad-hoc bypass loops in bootstrap, and divergent trust paths between user and workspace assets.

---

## Negative Consequences & Trade-offs

- **Periodic User Friction (Once Every 30 Days)**: Developers will see the trust review gate once a month for long-lived configurations.
  - *Mitigation*: The diff card is presented once at session start and resolves with a single keypress (`Enter`), requiring less than one second.
- **Manual TOML Editing**: Removing `muta mcp add` means users must edit `config.toml` directly using a text editor.
  - *Mitigation*: Modern editor tooling (e.g. TOML schema validation) provides superior autocompletion and comment support compared to brittle CLI parameter flags.

---

## Rejected Alternatives & Negative Knowledge

### Retaining `muta mcp add` with a Prompt
- **Why considered**: Preserving CLI workflow familiarity.
- **Why rejected**: A CLI command executed in a terminal can be spoofed by shell aliases, malicious Makefile targets, or piped install scripts. Maintaining imperative mutations creates two competing sources of truth for configuration (TOML files vs CLI verbs) and invites accidental bypasses.

### Indefinite Trust Leases
- **Why considered**: Zero recurring friction for developers who never want to re-approve tools.
- **Why rejected**: Violated core zero-trust principles. Forgotten scripts in developer machines represent a primary vector for privilege escalation in long-lived environments.

### Pure Content Hash Ledger (Status Quo)
- **Why rejected**: Ledgers indexed solely by hash cannot distinguish between an intentional software upgrade and an unexpected content substitution. Tracking locators is required for meaningful diff reporting (`Changed` vs `New`).

---

## Links

- Extends: [ADR-0147: Orthogonal Workspace Security Planes](0147-orthogonal-workspace-security-planes.md)
- Extends: [ADR-0175: Pre-Attach Workspace Trust Interstitial](0175-pre-attach-workspace-trust-interstitial.md)
- Amends: [ADR-0243: Unified Asset Attestation and Zero-Trust Hazard Mesh](0243-unified-asset-attestation-and-zero-trust-hazard-mesh.md)
