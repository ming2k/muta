# 0220. Personas: persisted named principals as first-class conversation lanes

- **Status:** Proposed
- **Date:** 2026-09-10
- **Builds on:** [ADR-0053](0053-declarative-principal-profile.md) (declarative
  principal profile), [ADR-0183](0183-homogeneous-agent-kernel-and-spatiotemporal-aspect-engine.md)
  (homogeneous agent kernel and role switching),
  [ADR-0211](0211-agent-role-harness-facets-and-ephemeral-ast-code-intelligence.md)
  (agent roles, tools, and harness facets),
  [ADR-0219](0219-session-scope-and-optional-workspace-binding.md) (session
  scope and optional workspace binding).
- **Depends on:** ADR-0219's `SessionScope::Persona` and `WorkspaceRequirement`.

## Context

ADR-0219 gives a workspace-free session a place to live —
`SessionScope::Persona(PersonaKey)` — but leaves the persona itself undefined.
Today there is no first-class, persistent, user-authorable principal:

- `AgentIdentity` (`crates/muta-contracts/src/identity.rs`) is the right shape
  (name, mission, optional full-text persona) but it is a value, not a
  persisted entity; nothing reads it from config.
- The startup principal is hard-coded: `muta/src/identity.rs::agent_code()`
  returns an identity-less `AgentRole`, bound at startup
  (`crates/muta-runtime/src/bootstrap.rs`).
- The only named principals are the `AgentRoleId` enum variants (`code`,
  `code_analyst`, `architect`, `reviewer`, `security`) in
  (`crates/muta-contracts/src/agent_preset.rs`), switched at runtime with
  `/role`. A role is a **capability preset** — tool selection, operation
  boundary, runtime knobs — not a persistent identity and not a conversation
  lane.

So a user cannot express "an English-practice agent identity": a persistent
name, a mission, a fixed capability surface, and its own history lane that
survives restarts and is addressable from the CLI.

## Decision

### 1. A `Persona` is a persisted named principal

```rust
pub struct Persona {
    pub id: PersonaId,               // stable, immutable address (the scope key)
    pub identity: AgentIdentity,     // name + mission + optional persona override
    pub role: AgentRoleId,           // capability surface (ADR-0211)
    pub workspace: PersonaWorkspace, // binding policy (ADR-0219)
    pub unattended: bool,
}

pub enum PersonaWorkspace {
    /// Never bind a workspace; the session is trust-free (ADR-0219 §5).
    None,
    /// Bind the caller's working directory at launch.
    Inherit,
    /// Bind a declared absolute root.
    Fixed(PathBuf),
}
```

A persona is the declarative source of exactly one conversation lane:
launching it materializes `SessionScope::Persona(id)`. History, resume, search,
and daemon auto-bind all key on `persona:<id>` exactly as they key on a
workspace path.

### 2. Personas are config, stored in `personas.toml`

Personas are user-authored, hand-editable, and portable, so they live under the
config category (`$XDG_CONFIG_HOME/muta/personas.toml`) beside
`config.toml` and `model_providers.toml`, keyed by id:

```toml
[personas.english-practice]
name = "English Practice"
mission = "a patient English conversation partner who corrects gently in context"
persona = """Optional verbatim preamble override."""
role = "conversational"
workspace = "none"      # none | inherit | "/absolute/path"
unattended = false
```

The file is additive and optional: absent, the shipped behaviour is unchanged.

### 3. The first workspace-free role: `conversational`

ADRs 0219 and 0211 give roles a `WorkspaceRequirement`. This ADR adds one
built-in role, `AgentRoleId::Conversational`, declared
`WorkspaceRequirement::Forbidden`, whose tool surface is read/search/web plus
`ask_user` — no `write_file`, `edit_text`, or `execute_command`. It is the
capability surface a practice/tutor/companion persona binds. Coding roles keep
`Required`; a persona may bind any declared role.

### 4. Resolution and surface

- Launch: `mutx --persona <id>` opens a new session in that lane, or resumes
  the lane's most recent session.
- Runtime: `/persona` lists lanes; `/persona <id>` opens that lane.
- The resolved persona supplies the agent's `AgentIdentity` at construction
  (identity is immutable past `Agent::new`; ADR-0053) and binds its
  `AgentRoleId` before `apply_preset`.
- `PersonaWorkspace` resolves to a `WorkspaceBinding` or to `None`, and is
  validated against the bound role's `WorkspaceRequirement`: a `Required` role
  with `workspace = "none"` is a configuration error surfaced at launch, not a
  silent fallback.

### 5. Persona versus role

A persona is a persistent identity and a lane; a role is an in-place capability
preset. Selecting a persona selects a lane (a different session scope); `/role`
within a session still narrows or restores capabilities without changing the
lane. This preserves ADR-0211's runtime role model and ADR-0219's invariant
that scope is never a capability.

## Invariants & Behavioral Boundaries

1. **Persona id is an immutable address.** It is the persisted scope key. A
   rename changes display metadata, never the lane.
2. **Config and history are independent lifecycles.** Losing `personas.toml`
   never deletes a lane's sessions; recreating a persona with the same id
   re-attaches to the existing lane. Resetting the database never loses persona
   definitions.
3. **A role is not an identity.** No session is keyed by `AgentRoleId`; two
   personas may share a role without sharing history.
4. **Workspace policy is validated, never silently coerced.** A persona bound
   to a `Required` role must resolve a workspace; a `Forbidden` role must not.
5. **The shipped CLI stays persona-less.** With no `personas.toml`, startup
   matches today's `agent_code()` baseline exactly.
6. **Personas are declarative config.** They are never written by the daemon as
   runtime state and never stored in SQLite.
7. **Personas are user-global only.** Project-scope `.muta/config.toml` cannot
   define or override a persona: a lane is cross-workspace, so repo content must
   not own the user's identity namespace.

## Migration

No database change: ADR-0219's `scope_key` column already holds
`persona:<id>`. Implementation is additive — read `personas.toml` at startup,
resolve `--persona`, add the `conversational` role, and pass the resolved
identity into assembly. `docs/reference/paths.md` gains the `personas.toml`
row when the code lands (per `INV-TEMP-01`).

## Alternatives considered

- **Declare personas inside `config.toml` (e.g. `[master]` / `[principal]`).**
  Rejected. `config.toml` is daemon and core behaviour only; a persona is a
  per-lane principal with its own identity, capability binding, and lane. It
  also could not express more than one persona.
- **Add personas as new `AgentRoleId` variants.** Rejected. Roles are
  compile-time capability presets; personas are runtime, user-authored
  identities. Encoding one as the other forces a rebuild to add a persona and
  blurs capability with identity.
- **Persist personas in SQLite.** Rejected. User-authored declarative
  definitions are config (ADR-0014 / ADR-0115): they must be hand-editable,
  portable, and survive a database wipe. Storing them in the DB also makes a
  definition edit a schema-adjacent operation.
- **Reuse the role id as the persona lane (`role:english`).** Rejected
  in ADR-0219 and here: a role is a switch, not a persistent identity; multiple
  personas sharing a role would collide.
- **Let a persona id be renamed or reassigned.** Rejected. An id is a persisted
  address; renaming it silently splits a lane, and reassigning it silently
  merges two.
- **Allow project-local `.muta/config.toml` to define personas.** Rejected. A
  lane is cross-workspace, so a repository defining user identities creates
  ambiguous ownership and lets repo content widen the user's persona namespace,
  the same class of hazard as a project `[workspace]` table widening filesystem
  admission.
- **Make persona selection a runtime in-place identity swap (like `/role`).**
  Rejected. Identity is immutable past construction (ADR-0053), and switching
  identity mid-session would either rewrite the lane's history under a new name
  or leak one identity's context into another's lane.

## Consequences

- "Create an English-practice agent identity" becomes declarative: one
  `personas.toml` entry, then `mutx --persona english-practice`.
- Persona lanes are workspace-free by default and never traverse workspace
  trust, permissions, or sandbox machinery.
- The shipped coding CLI remains unchanged and persona-less.
- A follow-up may add persona-aware tool restriction beyond the bound role
  (per-persona tool allowlists); this ADR deliberately keeps capability in the
  role and identity in the persona.

### Negative & mitigations

- **A new config surface and a new built-in role.** Mitigation: both are
  additive; the absence of `personas.toml` leaves behaviour identical, and the
  `conversational` role is one more value in an existing enum.
- **Orphaned lanes.** Deleting a persona leaves its sessions in the database.
  Mitigation: this is intentional — the lane is immutable history. Tooling
  should label an unbound lane rather than delete it; id reuse re-attaches.

## References

- [ADR-0014](0014-xdg-persistence-architecture.md) — unified XDG persistence architecture (config vs. data).
- [ADR-0053](0053-declarative-principal-profile.md) — declarative principal profile.
- [ADR-0115](0115-credential-placement-config-vs-state.md) — credential placement and the config/state test.
- [ADR-0183](0183-homogeneous-agent-kernel-and-spatiotemporal-aspect-engine.md) — homogeneous agent kernel and role switching.
- [ADR-0211](0211-agent-role-harness-facets-and-ephemeral-ast-code-intelligence.md) — agent roles, tools, and harness facets.
- [ADR-0219](0219-session-scope-and-optional-workspace-binding.md) — session scope and optional workspace binding.
