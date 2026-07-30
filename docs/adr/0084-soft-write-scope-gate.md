# 0084. Soft write/operation-scope gate

- **Status:** Accepted
- **Date:** 2026-07-27
- **Supersedes:** ADR-0028 (the hardening of the write-scope gate into an
  outright block)

## Context

ADR-0028 introduced the per-agent `WriteScope` (now generalized to
`OperationScope`: paths + commands) and a **hard** gate —
`ScopeGatePolicy`, gate 4 in the permission chain — that *blocked* any tool
call whose `ScopeTarget` fell outside the agent's granted scope, returning a
`ToolOutput::Text` denial. The stated rationale for making it hard (ADR-0028,
Alternatives) was: *"a sub-agent has no user reachable to answer a prompt, and
the boundary is a capability limit, not a confirmation."*

Two things have changed since:

1. **A user is often reachable.** ADR-0029 (full-duplex subagent
   communication) wired a reply path so a sub-agent's permission prompt
   surfaces to the operator, and the `INTERACTIVE` envoy profile sets
   `autopilot: false` precisely so its prompts can be answered. The "no user
   reachable" premise now holds only for genuinely autopilot runs
   (`--autopilot`, read-only envoy profiles), not for attended ones.
2. **Operators asked for the right to decide.** A hard builtin block is too
   strict for the attended case: it removes the operator's authority to grant
   an elevation, which is exactly what the permission broker exists to offer.

The `ToolAccess` admission ceiling (ADR-0012) is unchanged — that remains a
hard capability limit. This ADR concerns only the *scope* gate (gate 4).

## Decision

The scope gate becomes **soft when attended, hard when autopilot**:

- **Attended** (`ctx.autopilot == false`, a human is reachable): a call whose
  `ScopeTarget` is outside the granted `OperationScope` is *not* blocked. The
  gate returns `Pass` and lets the call fall through to the next gate — the
  permission broker (`BrokerPolicy`) — which emits the standard
  approve / always-allow / reject prompt. The **user**, not a builtin limit,
  decides whether the elevation is granted. "Always allow" then caches the
  rule as for any other broker ask.
- **Autopilot** (`ctx.autopilot == true`, no human reachable): the call is
  still **hard-denied** with a `ToolOutput::Text` message. With no one to
  answer a prompt, blocking is the only safe resolution; auto-allowing would
  remove the safety floor for autonomous runs entirely.

Calls that are *in* scope, and tools with `ScopeTarget::Unspecified` (no
locatable target, e.g. `read_text`, `grep`), always `Pass` exactly as before —
the broker then applies as usual.

The denial under autopilot stays a `ToolOutput::Text` (not
`PermissionDenied`) so the model can retry with a different path/command,
preserving ADR-0028's retry semantics for the one path that is still blocked.

`ScopeGatePolicy` now reads `ctx.autopilot`, but it still opens **no
interactive modal of its own** — it never parks. The broker remains the only
interactive surface; the gate only decides whether the broker gets the chance.

## Alternatives considered

- **Remove the gate entirely.** Rejected: it would leave autopilot envoys
  fully unrestricted (the broker auto-approves under `autopilot`), removing
  the safety floor for autonomous runs. The attended/autopilot split keeps
  that floor while restoring operator authority where a human exists.
- **Auto-allow under autopilot too** (treat autopilot as fully trusted).
  Rejected: it directly contradicts the autopilot posture's safety contract
  and ADR-0028's core (still-valid for the autopilot case) rationale.
- **Add an explicit "elevation" flag to `PermissionRequest`.** Rejected as
  unnecessary: an out-of-scope ask renders acceptably as a normal broker
  prompt (the target path/command is shown), and `Tool::permission_label` /
  `permission_description` are free to set if a future tool wants to signal it.

## Consequences

- **Positive.** Operators regain the right to grant a one-off or permanent
  elevation when they are present; the tool no longer refuses work the user
  was willing to authorize.
- **Positive.** The autopilot safety floor is preserved verbatim — autonomous
  sub-agents and `--autopilot` sessions are still bounded by their scope.
- **Negative.** Under autopilot, a rejection that previously let the model
  retry still does (it is `Text`, not `PermissionDenied`). Under *attended*, a
  user **Reject** on an out-of-scope ask returns `PermissionDenied`, which
  stops the round and rejects the whole concurrent batch — consistent with any
  other broker rejection.
- **Neutral.** `Always` rules cached from an elevation use the broker's
  existing, exact-string scope matching (no path normalization at store time),
  so they are per-target, not per-prefix — same as all broker `Always` rules.

Migration: none required. The change is local to `ScopeGatePolicy`
(`crates/neenee-agent/src/permission_policy.rs`). ADR-0028's body is left
intact per the immutability rule; this ADR supersedes its "hard boundary, not a
prompt" decision only.

## References

- ADR-0028 (the hard gate this softens for the attended case)
- ADR-0029 (full-duplex subagent communication — makes the user reachable)
- `docs/explanation/agent-design/autopilot.md` — attended vs autopilot
- `crates/neenee-agent/src/permission_policy.rs` — `ScopeGatePolicy`,
  `BrokerPolicy`, `default_chain`
