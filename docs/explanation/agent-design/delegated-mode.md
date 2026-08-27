# Delegated autonomous execution mode

Delegated autonomous execution is an interaction posture: the agent runs
without waiting for a human. It is not an authority level and does not expand
what the agent may do.

This distinction is the core safety invariant:

> The same action with the same grants produces the same authority decision in
> attended and delegated sessions.

The posture is session-scoped and persisted. A daemon restart restores it, but
restoring the posture does not restore, infer, or widen workspace authority.
For command and flag syntax, see [Slash commands](../../reference/commands.md).

## Naming history

The posture was previously called **autopilot**, and before that carried an
even earlier internal spelling. As of v0.36 the user-facing name is
**delegated autonomous execution** (`/delegate`, `--delegate`, the
`DELEGATED` head-row badge). Legacy spellings remain accepted as input
aliases — `/yolo`, `/auto`, `/autopilot`, `-y`, `--autopilot` all map to the
same command — so old scripts, old sessions, and muscle memory keep working.
Persisted session snapshots and wire events deserialize the legacy
`yolo` / `autopilot` field names transparently; new writes use `delegated`.

## Authority and interaction are separate

Every side-effecting tool passes through the authority chain first. The chain
can approve the call, deny it unconditionally, or report a missing grant. Only
after that result exists does the interaction posture matter:

| Authority result | Attended | Delegated |
|------------------|----------|-----------|
| Approved | Execute | Execute |
| Hard denied | Refuse | Refuse |
| Missing grant | Offer once/always/reject | Refuse immediately with the missing scope |

Delegated mode never auto-approves the broker, converts a confirmation into an
allow, or treats an unrestricted operation scope as authority. This keeps
unattended execution deterministic and prevents a UI convenience flag from
becoming a privilege escalation mechanism.

## Workspace security planes

Opening a directory establishes a primary filesystem root but grants no
runtime authority and loads no quarantined project assets. Those decisions are
independent:

- `/trust` controls content-attested project asset domains only;
- `[workspace].additional_roots` controls the user-owned native-file boundary;
- runtime permission rules control concrete hazardous calls.

Delegated mode consults the same three planes as an attended session. It does
not provide a workspace execution profile, and there is no preflight mode that
converts ordinary development work into implicit authority. See
[Security and trust architecture](security-and-trust.md).

## Non-interactive surfaces

Delegated mode reclaims the surfaces that would otherwise park a round:

- `ask_user` is unavailable; the model must choose a reasonable default.
- interactive command stdin is closed instead of opening an input panel.
- a missing authority grant returns immediately instead of emitting a
  permission modal.

These rules guarantee that an unattended round does not deadlock. They do not
guarantee that every attempted action succeeds: missing grants, sandbox
failures, hard command denies, provider errors, and ordinary tool failures
remain terminal or model-visible results.
