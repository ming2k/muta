# 0087. The `CODE` envoy runs unattended

- **Status:** Accepted
- **Date:** 2026-07-30
- **Supersedes:** [ADR-0086](0086-coding-envoy-profile.md) (the `unattended:
  false` decision only; the profile and the `envoy_code` dispatch tool from
  0086 stay)

## Context

ADR-0086 introduced the `CODE` envoy profile and the `envoy_code` dispatch
tool — a write-capable coding sub-agent, the first built-in envoy with side
effects. Its load-bearing choice was `unattended: false`: every
`bash` / `edit_file` / `write_file` the child emits surfaces up as an
`EnvoyEvent::PermissionRequest` and round-trips through the parent harness ↔
TUI ↔ registry handle, so the user approves each one exactly as they would a
top-level write. ADR-0086 listed `unattended: true` under "Alternatives
considered" and rejected it as "a meaningfully different trust posture."

Two things surfaced once the profile was actually exercised end-to-end:

1. **The delegation already *is* the authorization.** The principal emits the
   `envoy_code` call itself; that tool call is the moment the user (via the
   principal) consents to a bounded implementation task running. Routing
   every nested write/command back through the same broker gate the
   *principal's* writes go through double-charges the user: they authorize
   the delegation, then authorize every step inside it. For a non-trivial
   coding task this is dozens of modal interruptions for a single user
   intent, with no new information at any step the principal did not already
   sign off on.

2. **The broker is the principal's gate, not the envoy's.** The permission
   sheet, `/permissions`, and the `Always` allowlist all reason about the
   top-level actor. An envoy is an isolated child with a fresh history and a
   profile-bounded toolset; its identity is "the thing the principal
   delegated to." Making the envoy's calls hit the principal's broker
   conflates two roles in one gate and produces allowlist entries and
   approval prompts that are awkward to attribute ("whose `bash` is this?").

Meanwhile every *other* built-in envoy profile (`EXPLORE`, `REVIEW`, `TITLE`,
`QUANT`) runs `unattended: true`. CODE was the lone exception, which made
the role vocabulary inconsistent: an envoy either runs on its own authority
or it does not, and the line should not be drawn by *whether the profile
happens to admit write tools* — admission and supervision are orthogonal
axes (ADR-0011). The principal stays accountable either way: it sees the
envoy's final handoff (the list of files touched, commands run, verification
results) and can deny or undo from there.

A second, independent defect reinforced the timing: the TUI rendered
`envoy_code` steps through the generic `draw_tool_step` path (the expandable
"disclosure" step) instead of `draw_envoy_inline_step`, because
`is_envoy_task()` matched only `name == "envoy"`. The coding envoy looked
like an ordinary tool call rather than the navigable envoy step `EXPLORE`
produces. This is a presentation bug, not a policy question, but it is fixed
in the same change so the `CODE` role looks and behaves as one envoy
vocabulary.

## Decision

1. **`CODE` runs unattended.** Set `unattended: true` on the `CODE` profile
   in `crates/neenee-core/src/envoy.rs`, matching every other built-in
   envoy. The child's writes and commands execute on the envoy's own
   authority; no `EnvoyEvent::PermissionRequest` is surfaced for them. The
   principal's act of calling `envoy_code` is the authorization for the
   delegated task.

2. **`allow_user_interaction: true` stays.** `ask_user` still routes through
   the full-duplex channel (ADR-0029) so a genuinely ambiguous requirement
   can be surfaced rather than guessed. Supervision of *decisions* is
   preserved; only supervision of *individual side effects* is dropped, and
   that supervision moves to the result (the handoff the principal reviews).

3. **The TUI renders `envoy_code` as an envoy step, not a tool step.**
   `is_envoy_task()` (`crates/neenee-cli/src/tui/model/document.rs`) and
   `presenter_for` (`crates/neenee-cli/src/tui/tools/mod.rs`) now match
   `envoy_code` alongside `envoy`, so a coding delegation renders through
   `draw_envoy_inline_step` with the `EnvoyPresenter` summary — one
   navigable line plus a live status line, Enter to drill in — identical in
   shape to an `EXPLORE` run.

The profile, its name-scoped toolset (`CODING_TOOLS`), the `envoy_code`
dispatch tool, and the shared `EnvoyRegistry` from ADR-0086 are unchanged.
Only the supervision posture and the TUI routing change.

## Alternatives considered

- **Keep ADR-0086 as-is (`unattended: false`).** Rejected for the reasons
  above: it double-charges the user, conflates the envoy and principal roles
  in one broker, and is inconsistent with every other built-in profile.

- **A third, "supervised-coding" profile alongside an unattended `CODE`.**
  Rejected: the capability is already expressible as a future
  `write_paths`-scoped profile (ADR-0028 / ADR-0084) or via the reserved
  `INTERACTIVE` shape. Adding a second coding profile now, before anyone has
  asked for it, speculatively widens the vocabulary. The clean baseline is
  "envoys run unattended"; a future role that needs per-call approval opts
  back in explicitly, the way `INTERACTIVE` already does.

- **Drop `allow_user_interaction` too, for full autonomy.** Rejected:
  `ask_user` is a *decision* the model defers to the user, not a side
  effect to be approved. Keeping it preserves the one supervision path that
  carries information the envoy genuinely does not have.

## Consequences

- **Positive.** A coding delegation is no longer a waterfall of approval
  modals; the principal delegates, the envoy implements, the principal
  reviews the handoff. This matches how `EXPLORE` already works and makes
  the envoy vocabulary uniform.
- **Positive.** The TUI now shows `envoy_code` as a navigable envoy step,
  so a coding task is inspectable (drill into the child transcript) instead
  of an opaque expandable blob.
- **Negative.** A `CODE` envoy can now mutate the workspace with no per-call
  human gate. This is inherent to the delegation-as-authorization model and
  is bounded by the existing turn cap, the principal's review of the
  handoff, and the user's ability to interrupt the round.
- **Neutral.** The permission broker's `Always` allowlist and `/permissions`
  no longer see envoy-internal calls; they continue to govern the
  principal's own calls exactly as before. Full-duplex (ADR-0029) still
  carries `ask_user` for `CODE`.

No migration: the change is a constant flip plus two TUI name-match
additions. ADR-0086's text is left intact and marked Superseded by this ADR
on its supervision decision only.

## References

- [ADR-0086](0086-coding-envoy-profile.md) — the `CODE` profile and
  `envoy_code` tool this revises (superseded on the `unattended` decision).
- [ADR-0011](0011-subagent-profiles.md) — admission and supervision are
  orthogonal axes.
- [ADR-0028](0028-capability-allocation-scoped-writes.md) /
  [ADR-0084](0084-soft-write-scope-gate.md) — the scoped-write path a future
  supervised-coding role could use.
- [ADR-0029](0029-full-duplex-subagent-communication.md) — the up/down
  channel that still carries `CODE`'s `ask_user`.
- [Envoys](../explanation/agent-design/envoys.md) — the profiles table.
