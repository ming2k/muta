# 0086. The `CODE` envoy profile and the `envoy_code` dispatch tool

- **Status:** Superseded by ADR-0087 (the `unattended: false` decision only;
  the profile, toolset, and dispatch tool below remain in force)
- **Date:** 2026-08-14

## Context

Every built-in envoy profile carried a `Read` ceiling (ADR-0011 / ADR-0012):
`EXPLORE`, `REVIEW`, `TITLE`, and the reserved `INTERACTIVE` all admitted only
pure inspection tools. The single dispatch tool reachable from a model call,
`envoy`, was read-only by construction. This is the right contract for
*research* — an explorer should find and report, not mutate — but it leaves no
delegation path for *implementation* work. Today the principal must do every
edit itself, or hand a sub-task to an `envoy` only to be told what to change
and then apply the edits manually. There is no way to isolate a substantial,
self-contained coding task (a feature, a refactor, a bug fix with a verification
loop) in its own context window the way a research sub-question is isolated.

The capability machinery to do better already exists: the `ToolPolicy` name
scope admits any tool by name, the full-duplex channel (ADR-0029) already
forwards a child's `PermissionRequest` up and routes the user's reply back down,
and `unattended: false` already keeps the permission broker on for a spawned
agent. What was missing was (a) a profile that frames a write-capable coding
role and (b) a dispatch tool bound to it.

The reference product here is kimi-code's `coder` subagent
(`packages/agent-core/src/profile/default/coder.yaml`): the only subagent type
with file-editing tools, used for "non-trivial software engineering work that
may require reading files, editing code, running commands, and returning a
compact but technically complete summary."

## Decision

Add a fifth built-in profile, `CODE` (`crates/neenee-core/src/envoy.rs`),
and a second dispatch tool, `envoy_code` (`crates/neenee-transport/src/bootstrap.rs`),
that binds it.

**The `CODE` profile:**

- Admits a coding tool surface by name — the read-only inspection tools shared
  with `EXPLORE`, plus `bash`, `edit_file`, `write_file`, and the `todo` /
  `todo_update` pair — via a new `CODING_TOOLS` constant. Name-scoped (like
  every other profile) so adding a future side-effecting tool to the parent
  never silently widens it.
- Sets `allow_user_interaction: true` (admits `ask_user`) and
  `unattended: false`. The latter is load-bearing: it leaves the permission
  broker on, so every `bash`/`edit_file`/`write_file` the envoy emits surfaces
  as a `EnvoyEvent::PermissionRequest` that round-trips through the parent
  harness ↔ TUI ↔ registry handle. The user approves each one exactly as they
  would a top-level write.
- Carries a framing system prompt: read the relevant code first, then edit and
  verify; treat the parent as the caller; make the final message a technically
  complete handoff. (Modeled on kimi-code's `coder` `roleAdditional`.)

**The `envoy_code` dispatch tool:**

- A second `EnvoyTool` instance, constructed alongside `envoy` in the bootstrap.
  `EnvoyTool` gains a `tool_name` / `tool_description` field and `named` /
  `named_with_registry` constructors so two instances coexist as distinct
  capabilities (`envoy` and `envoy_code`) in the parent toolset instead of
  colliding on the name. The default `new`/`with_registry` constructors are
  unchanged.
- **Shares** the read-only `envoy` tool's `EnvoyRegistry`. Tool-call ids are
  globally unique, so a user's permission/`ask_user` reply routes to the
  correct live child regardless of which dispatch tool spawned it. The driver
  hands one `Arc<EnvoyRegistry>` to the harness; both tools lodge their
  children into it.
- Inherits the parent's variant selection and accounting exactly as `envoy`
  does (`bind_variant_selection` + `bind_accounting`).

## Alternatives considered

- **Add a `role`/`profile` selector parameter to the existing `envoy` tool**
  (kimi-code's `subagent_type` model). Rejected: the profile is bound at
  construction today, and surfacing it as a model-facing parameter would force
  `run_envoy_outcome` to switch profiles per call, thread the selection through
  identity/scope/unattended resolution, and rework the event-forwarding that
  assumes a fixed profile. Two distinct tools are a smaller, more local change
  that keeps each profile's framing in its own dispatch description.

- **Make `CODE` autonomous (`unattended: true`), like `EXPLORE`.** Rejected:
  a write-capable envoy that silently edits files and runs commands with no
  human gate is a meaningfully different trust posture. Routing every side
  effect through the broker — the same gate a top-level write hits — is the
  conservative default; a future unattended-coding profile (e.g. for CI/batch)
  can opt back in explicitly.

- **Grant `CODE` a scoped `write_paths` (ADR-0028) instead of leaving it
  unconstrained + brokered.** Rejected for now: the broker is the stronger and
  already-wired gate, and the user picks the scope per-call at approval time.
  A scoped-coding profile remains available for a future role that should be
  confined to e.g. `./src` without prompting.

## Consequences

- **Positive.** Substantial, self-contained implementation work can now be
  delegated into an isolated context window — the same context-isolation
  benefit `EXPLORE` gives research, extended to coding. The principal's
  transcript stays clean of intermediate file dumps.
- **Positive.** The read-only research contract of `EXPLORE` is untouched.
  `envoy` is byte-for-byte the same tool; `CODE` is strictly additive.
- **Neutral.** Two dispatch tools now share one registry; this is correct
  (global call-id namespace) but means the registry is no longer 1:1 with a
  single tool. Documented on `EnvoyTool::with_registry`.
- **Negative.** A `CODE` envoy can spend real tokens before its first write is
  approved. This is inherent to any write-capable delegation and is bounded by
  the existing turn cap and the user's ability to deny at the broker.

No migration: the change is additive. The profile is exported from
`neenee-core` (`CODE`); the tool is wired in `bootstrap.rs`.

## References

- [ADR-0011](0011-subagent-profiles.md) — the capability-axis profile primitive.
- [ADR-0012](0012-toolaccess-tier-split.md) — the `Read < Execute < Write` tier
  split.
- [ADR-0028](0028-capability-allocation-scoped-writes.md) — the `WriteScope`
  grant (available, unused by `CODE` for now).
- [ADR-0029](0029-full-duplex-subagent-communication.md) — the up/down channel
  that carries `CODE`'s permission requests.
- [Envoys](../explanation/agent-design/envoys.md) — the profiles table and
  tool-admission reference.
- kimi-code `coder` subagent — `packages/agent-core/src/profile/default/coder.yaml`.
