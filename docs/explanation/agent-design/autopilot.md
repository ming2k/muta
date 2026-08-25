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

## Workspace preflight

Opening a directory creates a workspace identity but grants no execution
profile. A workspace starts as `unknown`; the operator chooses one of two
explicit profiles:

- `restricted` is read-oriented. Each side effect needs a narrow explicit
  grant.
- `development` pre-authorizes ordinary work inside the physical workspace
  sandbox.

No agent round or direct-shell command can start while the profile is `unknown`,
whether autopilot is on or off. It fails before the provider or process launch
and points to `/workspace restricted` and `/workspace development`. A persisted
`development` profile also fails preflight when the host cannot enforce the
required sandbox.

Project-authored MCP servers, hooks, skills, and commands use a separate
content-bound extension decision. `/extensions trust` does not select a
workspace execution profile, and `/workspace development` does not load
project extensions.

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

Package installation is an ordinary development action, not a proxy for
project-extension trust. In a `development` workspace, commands such as
`pnpm install` may run without a permission prompt, but they run inside the
workspace sandbox: only the workspace and a fresh temporary directory are
writable; the process otherwise sees a minimal read-only system runtime and
public DNS/TLS configuration. HOME is isolated, inherited credentials are
scrubbed, and user-home toolchain shims are not admitted.

Project MCP servers and hooks are narrower still: after content trust they run
with a read-only workspace, no network, and no ambient credentials. A project
cannot turn extension trust into workspace mutation or network authority.

High-risk actions remain distinct. Destructive commands, publishing,
infrastructure mutation, and remote-content pipe-to-shell patterns retain
hard-deny or explicit-confirm behavior. Under autopilot, an explicit-confirm
result is a missing grant and therefore fails immediately.

## Persistence and visibility

Autopilot remains session-persisted as defined by ADR-0132. Workspace security
is persisted separately in `workspace_security.json`, keyed by the canonical
exact workspace root. Every harness snapshot carries both fields, so a
frontend can show the interaction posture and authority state without deriving
either from transcript messages.

See [ADR-0140](../../adr/0140-workspace-authority-and-content-bound-extension-trust.md)
for the authority model and physical enforcement decision.
