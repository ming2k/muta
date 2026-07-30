# 0053. Declarative Principal profiles

- **Status:** Accepted
- **Date:** 2026-07-02

## Context

ADR-0042 fixed the vocabulary: `agent` (engine) · `Principal` (top-level role)
· `Envoy` (spawned child role). ADR-0041 gave both roles the same two-selector
model — `ToolSelection` (scope + variants) — so the engine is genuinely
role-agnostic.

But the *declaration* was asymmetric:

- **Envoy side** is declarative. A role is a `const EnvoyProfile` (name, system
  prompt, `ToolPolicy`, `variant_pins`, `autopilot`, `allow_model_stdin`); the
  `EnvoyTool` binds it and the role's `resolve_operation_scope` becomes a hard
  write/command boundary. Adding an envoy role = adding a const.
- **Principal side** was imperative. `neenee-code`'s `main.rs` hand-assembled
  the identity (`neenee_identity()`), left the capability scope at the
  constructor default, left the write/command boundary unrestricted, and seeded
  the runtime knobs (`hard_stop_turns`, `nudge`, `allow_model_stdin`) from a
  single `[principal]` table. There was no first-class object that *named* a
  principal *role*.

Consequences:

1. Adding a principal instance (a new binary, a new persona) duplicated assembly
   logic, not binding a profile.
2. `QUANT` exists as an `EnvoyProfile`, but the quant *product*
   (`neenee-quant-gui`) is a principal — so the same domain's capability
   description was written on the wrong side and never reused.
3. The asymmetry blocked the multi-principal group chat ADR-0042 named: there
   was no first-class object to register, compare, or restrict a principal by.

The `Agent` engine already accepted every knob the profile would set
(`set_agent_selection`, `set_operation_scope`, `set_autopilot`,
`set_hard_stop_turns`, `set_doom_guard_config`, `set_allow_model_stdin`); only
the declarative bundling was missing.

## Decision

Introduce `PrincipalProfile`, mirroring `EnvoyProfile`, defined in
`neenee-core` (domain vocabulary, like envoy profiles). A principal role is a
value the embedding binds after constructing the agent:

```rust
pub struct PrincipalProfile {
    pub name: &'static str,
    pub identity: AgentIdentity,
    pub agent_selection: ToolSelection,   // capability scope (default unrestricted)
    pub operation_scope: OperationScope,  // write/command boundary (default unrestricted)
    pub config: PrincipalRuntimeConfig,   // hard_stop / nudge / allow_model_stdin
    pub autopilot: bool,
}
```

`Agent::apply_principal_profile(&profile)` sets every mutable knob (scope,
operation boundary, runtime config, attended flag) in one call. The profile's
`AgentIdentity` is **not** re-applied there — identity feeds the system-prompt
preamble and is immutable past the constructor, so the embedding supplies it to
`Agent::new` / `from_toolset` (as envoys do via `from_persona`). A role whose
identity should differ per instance (side conversations, group chat) composes
the profile with `PrincipalProfile::with_identity` before construction.

### Relocate `AgentIdentity` to core

To keep `PrincipalProfile` (and future role vocabulary) in `neenee-core`,
`AgentIdentity` moves from `neenee-agent` to `neenee-core/src/identity.rs`. It
is pure domain data (three strings + a `preamble()` formatter) with no
agent-layer dependencies — exactly the kind of vocabulary ADR-0042 wants
centralized. The agent crate re-exports it via `pub use neenee_core::*`, so every
existing `neenee_agent::AgentIdentity` / `crate::AgentIdentity` reference keeps
resolving unchanged.

### Built-in coding principal

`PRINCIPAL_CODE` is provided by `neenee-server` as `principal_code()` — the
declarative form of today's hand-assembled identity. `neenee-code`'s `main.rs`
is refactored from inline assembly to binding it:

```rust
agent.apply_principal_profile(&principal_code());
```

placed *before* the existing `[principal]` config overlay, so per-installation
config still wins. A future quant/research/ops principal is another value.

## Alternatives considered

- **Do nothing; keep imperative assembly.** Rejected: blocks multi-principal,
  leaves `QUANT` semantically homeless, and means each new frontend duplicates
  wiring.
- **Make `PrincipalProfile` a server-layer type.** Rejected: envoy profiles live
  in core as vocabulary; the principal/role vocabulary belongs beside them so
  ADR-0042's role taxonomy is declared in one place.
- **Unify `PrincipalProfile` and `EnvoyProfile` into one `Role`.** Deferred: a
  shared role abstraction is a plausible end state, but the fields differ enough
  (identity struct vs. system-prompt string; runtime-config bundle; `autopilot`
  default) that a premature merge would blur the principal/envoy distinction
  ADR-0042 deliberately kept. The two types intentionally share `ToolPolicy` /
  `OperationScope` / `ToolSelection` as their common vocabulary instead.

## Consequences

- **Symmetry restored.** Adding a principal instance = binding a
  `PrincipalProfile` value, exactly as adding an envoy role = binding a const.
- **`QUANT`-class reuse path.** Next step: extract a shared capability bundle so
  both a `PRINCIPAL_QUANT` and the `QUANT` envoy profile share one declaration
  (future ADR).
- **Migration.** `neenee-code` loses inline assembly and gains one
  `apply_principal_profile` call. Behaviour is identical by construction: the
  profile's defaults equal the constructor's defaults, and the `[principal]`
  config overlay still runs last and wins.
- **Config stays single-valued for now.** `[principal]` continues to mean "the
  principal this binary runs". Multi-principal-in-one-process (group chat) is a
  follow-up that will add named sub-tables; this ADR does not require it.

## References

- [0042](0042-principal-envoy-role-vocabulary.md) — the role vocabulary.
- [0041](0041-tool-capabilities-scope-and-override.md) — the two-selector model
  both roles share.
- [0028](0028-capability-allocation-scoped-writes.md) — `OperationScope`.
