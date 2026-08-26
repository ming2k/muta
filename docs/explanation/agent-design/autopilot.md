# Autopilot operation

Autopilot is an interaction posture: the agent runs without waiting for a
human. It is not an authority level and does not expand what the agent may do.

This distinction is the core safety invariant:

> The same action with the same grants produces the same authority decision in
> attended and autopilot sessions.

The posture is session-scoped and persisted. A daemon restart restores it, but
restoring the posture does not restore, infer, or widen workspace authority.
For command and flag syntax, see [Slash commands](../../reference/commands.md).

## Authority and interaction are separate

Every side-effecting tool passes through the authority chain first. The chain
can approve the call, deny it unconditionally, or report a missing grant. Only
after that result exists does the interaction posture matter:

| Authority result | Attended | Autopilot |
|------------------|----------|-----------|
| Approved | Execute | Execute |
| Hard denied | Refuse | Refuse |
| Missing grant | Offer once/always/reject | Refuse immediately with the missing scope |

Autopilot never auto-approves the broker, converts a confirmation into an
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

Autopilot consults the same three planes as an attended session. It does not
provide a workspace execution profile, and there is no preflight mode that
converts ordinary development work into implicit authority. See
[Security and trust architecture](security-and-trust.md).

## Non-interactive surfaces

Autopilot reclaims the surfaces that would otherwise park a round:

- `ask_user` is unavailable; the model must choose a reasonable default.
- interactive command stdin is closed instead of opening an input panel.
- a missing authority grant returns immediately instead of emitting a
  permission modal.

These rules guarantee that an unattended round does not deadlock. They do not
guarantee that every attempted action succeeds: missing grants, sandbox
failures, hard bash denies, provider errors, and ordinary tool failures remain
terminal or model-visible results.

## Bash and dependency installation

Package installation is a command execution action, not a proxy for project
asset trust. A command such as `pnpm install` runs only when its exact runtime
scope is authorized and still passes the bash safety policy. Autopilot returns
`[permission required]` immediately when that authority is absent.

Project MCP servers and hooks are narrower still: after asset trust they run
with a read-only workspace, no network, and no ambient credentials. A project
cannot turn asset trust into workspace mutation or network authority.

High-risk actions remain distinct. Destructive commands, publishing,
infrastructure mutation, and remote-content pipe-to-shell patterns retain
hard-deny or explicit-confirm behavior. Under autopilot, an explicit-confirm
result is a missing grant and therefore fails immediately.

## Persistence and visibility

Autopilot remains session-persisted as defined by ADR-0132. Project asset
trust is persisted separately in `workspace_security.json`, keyed by the
canonical exact workspace root and concrete domain. Runtime `Always` rules use
the per-project permission store, while Session rules remain in memory.

See [ADR-0147](../../adr/0147-orthogonal-workspace-security-planes.md) for the
three-plane security model.
