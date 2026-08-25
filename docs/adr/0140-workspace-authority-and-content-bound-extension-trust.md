# ADR-0140: Workspace authority and content-bound extension trust

- **Status:** Accepted
- **Date:** 2026-08-25
- **Supersedes:** the path-only trust and autopilot-as-authority portions of
  ADR-0085, ADR-0107, and ADR-0132

## Context

Muta had one path-keyed `trusted_projects.json` boolean with two unrelated
jobs. It decided whether project-authored MCP servers, hooks, skills, and slash
commands could load, and bootstrap also used its inverse to inject a bash rule
for package installation and pipe-to-shell commands.

That coupling created an unreachable state. A repository without project
extensions produced no startup trust notice, and `/trust` answered “nothing to
trust,” but `pnpm install` was still classified as an untrusted-project action.
In autopilot, the confirmation could not be answered and the command was
refused. The operator had no valid transition that made the requested action
possible.

The deeper problem was semantic. Autopilot participated in authorization:
the broker approved every surviving tool call, the scope gate changed an
elevation from interactive to denied, and bash confirmation could become
allow or deny from `autopilot_confirm`. The same action and stored grants
therefore had different security decisions depending on whether a human was
reachable.

The final enforcement boundary was also too weak. Bash invoked the native host
shell directly. Workspace path checks and command regular expressions were
useful policy signals, but they were not a physical capability boundary.

## Decision

### 1. Separate the security axes

Represent workspace execution authority and project-extension trust as
independent domain values.

Workspace execution has three profiles:

- `unknown`: no decision exists;
- `restricted`: read-oriented, with side effects requiring explicit rules;
- `development`: ordinary development is pre-authorized inside the enforced
  workspace sandbox.

Project extensions have four states:

- `absent`;
- `quarantined`;
- `trusted`;
- `changed`.

Neither axis implies the other. Selecting `development` does not load project
instructions or processes. Trusting extensions does not authorize workspace
writes or shell commands.

### 2. Make autopilot interaction-only

Permission policies return `Approve`, `Deny`, or `MissingAuthority` without
consulting autopilot. The caller resolves `MissingAuthority` after the
security decision:

- attended sessions may park and offer once/always/reject;
- autopilot sessions fail immediately and name the missing tool scope.

Only interaction-specific facilities inspect autopilot: `ask_user`,
interactive stdin, and whether a missing grant can be requested. Remove the
broker bypass and remove `bash_policy.autopilot_confirm`.

The invariant is testable: the same action, scope, workspace profile, and
explicit rules produce the same policy result in both postures.

### 3. Require an explicit workspace preflight

Opening a directory grants nothing. Publish `WorkspaceSecuritySnapshot` on
every harness snapshot and emit a retained startup banner when execution is
`unknown` or project extensions are quarantined.

Any model round or direct-shell command in `unknown` fails before a provider or
process launch, regardless of interaction posture. A persisted `development`
profile also fails preflight when its physical sandbox cannot be enforced.
`/workspace status|restricted|development|reset` is the canonical
execution-authority surface.

### 4. Bind extension trust to content

Replace `trusted_projects.json` with versioned `workspace_security.json`, keyed
by the canonical exact workspace root. Do not widen a grant to a parent Git
repository or share it across worktrees. Store a SHA-256 digest over the
project contribution paths:

- the entire `.muta` control-plane tree (configuration, hook/MCP executables,
  skills, and commands);
- `.agents/skills`;
- `.claude/skills`.

Reject symlinks in contribution trees: hashing only a link target string would
not attest the mutable target content, while following it could escape the
workspace. A digest mismatch is `changed` and loads nothing until
`/extensions trust` records the new exact content. The canonical surface is
`/extensions status|trust|untrust`; remove `/trust` and `/untrust`.

The digest includes file type and permission mode as well as paths and bytes.
Any symlink, special file, enumeration failure, or read failure is an
attestation failure, not an omitted entry: the contribution stays quarantined.
Executable extension paths re-attest immediately before every hook fire and MCP
connection/tool call; a mismatch refuses execution and drops a cached MCP
connection, so a startup-time check cannot become a stale grant.

Do not migrate the old path-only set. A path grant that cannot prove content
identity is not equivalent to the new decision and must return to quarantine.

### 5. Enforce a physical workspace sandbox

The product runtime uses `WorkspaceExecutionEnvironment`, not the direct host
environment.

- Filesystem operations resolve existing ancestors, follow symlinks for the
  containment check, and deny any target outside the canonical workspace.
- Direct generic process execution fails closed.
- On Linux, bash executes through bubblewrap from an empty root containing only
  a read-only system runtime and public DNS/TLS configuration, a writable exact
  workspace bind, and fresh `/tmp`. User homes, runtime sockets, unrelated host
  files, and inherited environment variables are absent. User/process/IPC/UTS/
  cgroup namespaces and dropped capabilities provide the process boundary.
  Network remains available for dependency resolution.
- Persistent terminals are disabled in the workspace sandbox until they have
  an equally strong containment implementation.
- Project-defined MCP servers and hooks use the same physical primitive with a
  read-only workspace and a private network namespace. They receive no ambient
  environment or credentials. Networked or workspace-mutating integrations
  must be installed deliberately in user-global configuration; extension
  trust alone cannot create those authorities.
- Missing bubblewrap, disabled user namespaces, or an unsupported platform
  never falls back to the host shell.

Package installation is an ordinary `development` action inside this boundary.
Its executable must exist in the admitted system runtime; user-home shims and
language-manager state are intentionally absent. This makes toolchain admission
explicit instead of mounting an operator's home directory into untrusted code.
The bash policy still supplies unconditional destructive denies and explicit
confirmation for publishing, infrastructure mutation, destructive repository
operations, and remote-content pipe-to-shell patterns.

## Alternatives considered

### Keep one trusted boolean and improve the prompt

Rejected. Better wording cannot repair the invalid state machine or separate
project prompt trust from execution authority.

### Make autopilot imply full workspace trust

Rejected. An interaction preference is not an authority source. This would
turn unattended operation into a privilege escalation mechanism.

### Treat package installation as extension trust

Rejected. Installing dependencies is an execution action. Project MCP, hooks,
skills, and commands are project-authored control-plane contributions. They
have different principals, revocation triggers, and audit questions.

### Keep regex policy as the security boundary

Rejected. Shell syntax is open-ended and aliases, interpreters, generated
scripts, and subprocesses make lexical classification incomplete. Regex rules
remain useful action policy but cannot confine filesystem or credential access.

### Migrate old trusted roots as trusted extension digests

Rejected. The old record proves only that a path was once accepted, not which
content was inspected. Silent migration would preserve the exact weakness the
new model removes.

## Consequences

- Autopilot is genuinely walk-away-able without being more privileged.
- New workspaces disclose their authority state before work begins.
- A project extension edit automatically revokes its effective trust.
- `pnpm install` no longer depends on whether `.muta` contributions exist; it
  depends on the workspace execution profile and runs inside containment.
- Existing path-only trust decisions are intentionally discarded.
- Linux development shell execution requires a working bubblewrap installation.
  Other platform adapters must provide equivalent containment before enabling
  the `development` profile.
- The wire contract grows additively with workspace security state; older
  snapshots deserialize to the safe unknown/unavailable defaults.

## References

- ADR-0028: scoped operation delegation
- ADR-0085: project external-tool trust
- ADR-0107: trust gate for project skills and commands
- ADR-0130: native platform capability boundary
- ADR-0132: session-persisted autopilot posture
