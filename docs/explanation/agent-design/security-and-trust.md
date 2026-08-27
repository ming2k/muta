# Security and trust architecture

Muta treats workspace security as three independent decisions. They are
orthogonal: satisfying one never satisfies either of the others.

| Plane | Question | State owner |
|------|----------|-------------|
| Project asset trust | May this repository's instructions and extension definitions be loaded? | Content-bound domain grants |
| Spatial workspace boundary | Which physical directories may native file tools address? | Primary and linked roots |
| Runtime permission gate | May this concrete hazardous operation execute now? | Hazard policy and permission rules |

This separation prevents a common category error: trusting a repository's
Skill or MCP definition is not permission to edit files or run arbitrary
commands, and allowing an edit does not make repository-authored prompt
content trustworthy.

## Project asset trust

Project asset trust protects input that travels with a repository. It is split
into five concrete domains:

- **MCP** — project MCP server definitions;
- **Skills** — project-local Skills;
- **Hooks** — project lifecycle hook definitions and scripts;
- **Rules** — project instructions and slash-command templates;
- **Roots** — project-declared linked workspace roots (`[workspace].additional_roots`).

Each domain has its own SHA-256 attestation. The digest covers names, bytes,
and relevant permission modes. A change to Hooks therefore quarantines Hooks
without disturbing an unchanged MCP grant. Symlinks and content that cannot be
enumerated or read fail closed because their effective bytes cannot be
attested reliably.

The states are `absent`, `quarantined`, `trusted`, and `changed`. Grants are
durable for the canonical workspace root, so reopening the same unchanged
repository restores only the domains that were explicitly trusted.

`/trust` and `/trust all` trust every present domain. Narrow commands such as
`/trust mcp`, `/trust skills`, and `/trust roots` approve single domains.
`/trust status` reports every domain; `/trust revoke` and `/untrust` revoke all of them.
There is no aggregate grant and no `/trust workspace` or `/extensions` compatibility surface.

A mutation reloads all asset consumers from one new snapshot. Consumers that
can act later, such as project Skills, MCP tools, and Hooks, re-attest before
use so a changed working tree cannot keep using a stale catalog grant.

## Spatial workspace boundary

The spatial boundary limits native file reads, edits, writes, directory
operations, and metadata queries. The primary workspace is admitted by
default. A user may add linked roots for cross-repository work in global
configuration.

Paths are resolved from the primary workspace, canonicalized through their
existing ancestor, and checked against the admitted root set before I/O. This
blocks `..` traversal and symlink escapes, including dangling-link
destinations. Invalid linked-root configuration fails closed to the primary
workspace.

The linked-root list is user-owned. Repository configuration cannot expand it;
otherwise an unreviewed checkout could name a sensitive host directory and
widen its own containment. Linked roots affect file placement only. They do
not trust the linked repository's assets and do not authorize shell commands.

## Runtime permission gate

Runtime policy evaluates what an operation will do now. Safe inspection can
proceed without a grant. File modification, command execution, process
lifecycle changes, and network or external calls submit a structured hazard
description with an exact scope.

The broker can satisfy that scope in three ways:

- **Once** approves only the pending invocation;
- **Session** keeps an in-memory rule until the session ends;
- **Always** persists the exact rule for the workspace.

Both native and sandboxed shell commands pass through the same command policy.
MCP tool calls are external operations and also require runtime authority.
Lifecycle Hook commands require an existing exact Hook rule; because Hooks run
inside agent control flow, they fail closed rather than recursively opening a
permission prompt.

Autopilot is an interaction posture, not a fourth grant. An already-authorized
operation behaves the same in attended and autopilot sessions. A missing grant
opens an approval prompt when a human is reachable and returns a clear
`[permission required]` failure when no approver is available. The remedy is a
runtime permission rule or interactive approval, never an asset-trust command.

## How the planes compose

For a project MCP call, the decisions are evaluated in order:

1. the MCP domain must still match its trusted content digest before the
   project definition is available;
2. any native file path used by Muta's file tools must remain within an
   admitted root;
3. the concrete MCP call must have runtime authority for its submitted scope.

No step inherits a decision from another. The same composition applies to
project Hooks and Skills: content admission decides whether Muta may load the
asset, the spatial boundary controls native file placement, and runtime policy
controls hazardous effects.

See [ADR-0147](../../adr/0147-orthogonal-workspace-security-planes.md) for the
decision record and [Slash commands](../../reference/commands.md#trust-and-untrust)
for the command grammar.
