# 0253. Explicit Negative Attestation, XDG Standard Compliance, and Role-Scoped Assets

- **Status:** Proposed
- **Date:** 2026-10-18
- **Scope:** `security/attestation`, `core/contracts`, `core/runtime`, `muta-paths`, `muta-persistence`, `mutx`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0242](0242-orthogonal-mcp-registry-and-role-capability-subscription.md) (orthogonal role subscriptions), [ADR-0246](0246-symmetric-role-architecture-and-canonical-roles-schema.md) (symmetric role architecture), [ADR-0252](0252-unified-asset-identity-ttl-lifecycle-and-zero-bypass-attestation.md) (unified asset identity, TTL, and zero bypass)

---

## Context and Problem Statement

ADR-0252 unified asset identities, instituted 30-day bounded leases, and introduced multi-select partial authorization. However, two architectural inconsistencies and one major UX vulnerability remain:

1. **The Repeated Nagging Anti-Pattern (Missing Negative Attestation)**:
   When an operator unchecks an untrusted asset in the Pre-Attach Gate, their affirmative intent is: *"I do not trust this asset at this time; do not load it."*
   However, because the ledger previously only recognized `Quarantined` (un-evaluated) vs `Trusted` (approved), unselected assets remain `Quarantined`. On the very next session launch, the diff engine finds the asset still un-attested and aggressively opens the Pre-Attach Gate again. This creates **Security Fatigue**: users are badgered on every launch until they surrender and click "Trust All", neutralizing the security perimeter.

2. **Home Directory Pollution vs. XDG Compliance**:
   Arbitrary ad-hoc directories in the user's home directory (e.g. `~/.muta/`) violate the **XDG Base Directory Specification**. Muta's path resolution (`muta-paths`) is strictly built on XDG standards (`$XDG_CONFIG_HOME/muta`, `$XDG_DATA_HOME/muta`, `$XDG_STATE_HOME/muta`, `$XDG_CACHE_HOME/muta`). UI text and asset models must strictly honor standard XDG locations (`~/.config/muta/config.toml`, `~/.local/share/muta/skills/`) rather than polluting `$HOME` with non-standard hidden roots.

3. **Workspace-Free Role Asset Scoping**:
   Workspace-free roles (e.g. `philosophist`) require role-specific skills (e.g. memory recall, dialethic analysis) and external tools (e.g. specialized knowledge-base MCP servers). Currently, non-workspace assets must either be global (polluting every role's context) or declared in ad-hoc locations. Roles must possess first-class asset scoping within the XDG hierarchy.

---

## Decision Drivers

- **Zero Security Fatigue (Durable Negative Decisions)**: Rejecting or untoggling an asset must be a durable, recorded decision (`Denied`), not a transient state that invites repeated harassment.
- **Strict XDG Standard Compliance**: No non-standard directories in `$HOME`. Configuration belongs in `$XDG_CONFIG_HOME/muta/`, data in `$XDG_DATA_HOME/muta/`.
- **First-Class Role Asset Scoping**: Enable workspace-free roles to declare and consume dedicated skills and MCP definitions without context pollution.
- **Content Invalidation Invariant**: A negative attestation record binds strictly to the exact SHA-256 fingerprint of the rejected content. Any modification to the file immediately invalidates the denial and re-prompts for review.

---

## Decision Outcome

We adopt a clean-break architecture formalizing **Explicit Negative Attestation**, **Strict XDG Directory Compliance**, and **Role-Scoped Asset Scoping**:

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ 1. Strict XDG Asset Hierarchy                                               │
│    • User Config: $XDG_CONFIG_HOME/muta/config.toml (~/.config/muta/)       │
│    • Role Bundles: $XDG_CONFIG_HOME/muta/roles/<name>/ (role.toml, mcp.json)│
│    • Role Skills:  $XDG_DATA_HOME/muta/roles/<name>/skills/                 │
│    • Workspace:    <workspace_root>/.muta/                                  │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Candidate Asset Pool
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 2. Tri-State Attestation Lifecycle (muta-persistence)                       │
│    • Trusted: Human affirmatively approved (30-day lease)                   │
│    • Denied:  Human affirmatively rejected (silent quarantine, no nagging)  │
│    • Quarantined: Unknown / never-reviewed asset                            │
│    • Changed: Content hash mutated ──► Invalidates both Trusted and Denied  │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Pre-Session Diff
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 3. Zero-Nagging Diff Engine                                                 │
│    • Diff-Set D = { a ∈ Candidates | status(a) ∈ {Quarantined, Changed, Expired} }
│    • Assets with status(a) == Denied (matching hash) are EXCLUDED from D!   │
│    • Denied assets stay 100% silent, un-spawned, and invisible to the model.│
└─────────────────────────────────────────────────────────────────────────────┘
```

---

### 1. Explicit Negative Attestation (`AttestationStatus::Denied`)

#### The `Denied` State
We expand `AttestationStatus` to include `Denied`:
```rust
pub enum AttestationStatus {
    Quarantined, // Never evaluated by human -> Triggers gate review
    Trusted,     // Approved by human        -> Admitted with 30-day lease
    Denied,      // Rejected by human        -> Silently isolated, zero nagging
    Changed,     // Hash mutated on disk     -> Re-triggers gate review
    Expired,     // Lease exceeded 30 days   -> Re-triggers routine review
}
```

#### The Untoggle Workflow
When the Pre-Attach Gate opens and the user unchecks an asset `A` (leaving it `[ ]`) and presses `Enter`:
1. The gate records `A` with:
   $$\text{Record} = \{ \text{locator}: A, \text{fingerprint}: \text{SHA256}(A), \text{status}: \text{Denied}, \text{reviewed\_at\_s}: \text{now}() \}$$
2. In subsequent launches, the diff calculation evaluates `A`:
   - If $\text{disk\_hash}(A) == \text{recorded\_hash}(A)$: status evaluates to `Denied`.
   - `Denied` is **not** an actionable difference. The gate **does not pop up**.
   - `McpClient`, skills registry, and hook loaders treat `Denied` assets as strictly unauthorized: they are **not spawned, not loaded, and completely invisible** to the agent.
3. If $A$ is edited on disk:
   - $\text{disk\_hash}(A) \neq \text{recorded\_hash}(A)$.
   - Status immediately transitions to `Changed`.
   - The Pre-Attach Gate opens on the next launch with an explicit notice:
     `• .muta/mcp.json (Previously denied, content changed)`

---

### 2. Strict XDG Compliance

We explicitly reject the creation of `~/.muta/` in the user's home directory.

- **Configuration Root**:
  $$\text{UserConfigPath} = \$XDG\_CONFIG\_HOME/\text{muta}/\text{config.toml} \quad (\text{default: } \sim/.config/\text{muta}/\text{config.toml})$$
- **Data Root**:
  $$\text{UserDataPath} = \$XDG\_DATA\_HOME/\text{muta}/ \quad (\text{default: } \sim/.local/share/\text{muta}/)$$
- **UI Presentation**:
  Display labels in the TUI Pre-Attach Gate must strictly reflect standard XDG paths:
  - `~/.config/muta/config.toml` (User global configuration)
  - `.muta/config.toml` (Workspace configuration)
  - `.muta/skills/` (Project skills)
  - `.muta/mcp.json` (Project MCP definitions)
  - `.muta/hooks/` (Project hooks)
  - `AGENTS.md` (Project instructions)

---

### 3. Role-Scoped Asset Taxonomy

To support workspace-free cognitive agents (`philosophist`, specialized research roles) without global context pollution, assets may be scoped to specific roles:

#### Directory Convention
- Role definitions: `$XDG_CONFIG_HOME/muta/roles/<name>/role.toml`
- Role MCP servers: `$XDG_CONFIG_HOME/muta/roles/<name>/mcp.json`
- Role skills: `$XDG_DATA_HOME/muta/roles/<name>/skills/`

#### Locator Variants
```rust
pub enum AssetLocator {
    // User Global
    UserMcp { name: String },
    UserSkill { name: String },
    UserHook { event: String },

    // Role-Scoped (ADR-0253)
    RoleMcp { role: String, name: String },
    RoleSkill { role: String, name: String },

    // Workspace-Scoped
    WorkspaceMcp { workspace_root: String, name: String },
    WorkspaceSkill { workspace_root: String, name: String },
    WorkspaceHook { workspace_root: String, event: String },
    WorkspaceConfig { workspace_root: String },
    WorkspaceInstructions { workspace_root: String },
}
```

When running `mutx --role philosophist`, only candidate assets belonging to `User` and `Role(philosophist)` are evaluated.

---

## Invariants & Behavioral Boundaries

- **`[INV-ASSET-07]` Durable Negative Decisions**: An explicit rejection of an asset during the trust gate must persist as `AttestationStatus::Denied`. It must not prompt again unless its content hash changes.
- **`[INV-ASSET-08]` Content Invalidation of Denials**: Modifying any byte of a `Denied` asset invalidates the negative attestation, returning it to `Changed` for fresh human review.
- **`[INV-ASSET-09]` XDG Purity**: Under no circumstances may Muta create or expect an ad-hoc `~/.muta/` directory in the user's home directory. All paths must adhere to `$XDG_CONFIG_HOME` and `$XDG_DATA_HOME`.
- **`[INV-ASSET-10]` Invisible Denied Capabilities**: Any capability whose status is `Denied` must fail closed at physical spawn and must be omitted from model tool projections and prompt contexts.

---

## Positive Consequences

- **Elimination of Security Fatigue**: Users can permanently reject unwanted project tools without being harassed on every single attach.
- **Preserved Zero-Trust Security**: Rejected assets remain 100% dead and invisible. If an attacker modifies a rejected script, the gate re-arms immediately.
- **Standards Compliant**: Clean alignment with standard Linux and macOS filesystem conventions.
- **Seamless Workspace-Free Role Extension**: Roles can carry dedicated tools without contaminating global namespaces.

---

## Links

- Extends: [ADR-0242: Orthogonal MCP Registry and Role Capability Subscription](0242-orthogonal-mcp-registry-and-role-capability-subscription.md)
- Extends: [ADR-0252: Unified Asset Identity, TTL Lifecycle Lease, and Zero-Bypass Attestation](0252-unified-asset-identity-ttl-lifecycle-and-zero-bypass-attestation.md)
