# 0225. Persona owns identity and capability (AgentRole → AgentPersona)

- **Status:** Accepted
- **Date:** 2026-09-10
- **Supersedes:** [ADR-0053](0053-declarative-principal-profile.md) and the `AgentRole` / `AgentRoleId` / `AgentPreset` vocabulary introduced by ADR-0211 (terminology and capability ownership only).
- **Builds on:** [ADR-0223](0223-capability-is-the-extension-set.md), [ADR-0224](0224-unified-extension-primitive.md), [ADR-0220](0220-personas-persisted-named-principals.md).

## Context

Shipped principals (`developer`, `architect`, …) and user declarations
(`personas.toml`) are the same thing: an identity plus a set of capabilities.
The vocabulary keeps them apart — `AgentRole`/`AgentRoleId` for the compiled-in
ones, `Persona` for the user ones — and "role" suggests a switchable position
rather than an identity.

Once capability is the extension set (ADR-0223/0224), the only remaining
payload of a principal is `identity + extensions + runtime knobs`, which is
exactly a persona. Two names for one concept is the confusion.

## Decision

### 1. One type: `AgentPersona`

`AgentRole`, `AgentRoleId`, and `AgentPreset` are renamed to `AgentPersona`,
`AgentPersonaId`, and (dropping the alias) no preset type:

```rust
pub struct AgentPersona {
    pub id: AgentPersonaId,
    pub identity: AgentIdentity,
    pub extensions: Vec<ExtensionRef>,   // capability (ADR-0223/0224)
    pub runtime: AgentRuntimeConfig,
}
```

### 2. Shipped and user personas are the same concept

- Shipped personas (`PERSONA_DEVELOPER`, `PERSONA_CONVERSATIONAL`, …) are
  built-in `AgentPersona` values.
- User personas are produced from `personas.toml` by the same shape.
- `/persona` lists both.

### 3. `personas.toml` declares extensions directly

```toml
[personas.philosopher]
name = "Philosopher"
mission = "a Socratic philosopher …"
extensions = ["read_url", "search_web", "ask_user"]
```

A named preset is a data convenience — a shipped persona's extension list — not
a mandatory indirection. `preset = "conversational"` may be offered as an
alias for that list.

### 4. `/role` becomes `/persona`

Switching replaces the live agent's extension set (and, since identity is
live-mutable, its identity). It never changes the conversation grouping
(ADR-0226).

### 5. Clean break

No `AgentRole` alias survives. Every call site migrates in one tranche.

## Invariants & Behavioral Boundaries

1. **One concept.** There is exactly one type for "identity + capability",
   built-in and user alike.
2. **Capability rides on the persona.** No separate capability/profile type.
3. **`/persona` switches capability, not grouping.** The conversation lane is
   chosen independently (ADR-0226).
4. **Clean break.** `AgentRole*` names are deleted; no deprecation alias.
5. **Shipped personas are ordinary values.** No special-cased code path for
   built-ins.

## Alternatives considered

- **Keep `AgentRole` for shipped and `Persona` for user.** Rejected. Two names,
  two code paths, one concept.
- **Alias `AgentRole = AgentPersona` for migration.** Rejected (clean break);
  aliases are baggage and hide stale call sites.
- **Split identity (persona) from capability (role) permanently.** Rejected.
  ADR-0223 deletes the capability type, so the split has nothing to hold.

## Consequences

- Broad mechanical rename across `muta-contracts`, `muta-agent`,
  `muta-runtime`, `muta-persistence`, and the frontends.
- `Agent::apply_preset` / `apply_profile` / `apply_role` collapse into
  "resolve a persona's extension set onto the agent".
- `personas.toml` stops carrying a `role` string and carries `extensions`
  (or a preset alias).
- ADR-0211's facade terms (`AgentRole`, `HarnessFacet`) are retired.

## References

- ADR-0053 — declarative principal profile (superseded).
- ADR-0211 — agent roles and harness facets (superseded terminology).
- ADR-0220 — persistent personas (`personas.toml`, lanes).
- ADR-0223 / ADR-0224 — capability = extension set.
- ADR-0226 — session grouping is derived.
