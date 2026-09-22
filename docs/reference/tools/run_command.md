# `run_command`

`ExecuteCommandTool` (`crates/muta-agent/src/tools/execute_command/`) executes
a shell command in a non-interactive shell. It is the one built-in tool in the
`Execute` access tier — it runs commands but is not a file-mutation primitive,
so it sits between pure reads and file writes. The permission broker still
gates it (`Execute > Read`). It is excluded from every built-in subagent profile,
all of which carry a `Read` ceiling today, so `run_command` runs only in
the main agent. See [Tool access](access.md) and
[ADR-0012](../../adr/0012-toolaccess-tier-split.md).

The tool is a **single-shot, finite piped execution primitive**
([ADR-0263](../../adr/0263-axiom-of-linear-causality-and-pure-execution-primitives.md)):
a call runs to completion — or is cut off by the timeout / StreamGuard — and
returns its output. There is no background, service, or scheduling surface: the
former `background` / `service` parameters and the `process` companion tool were
removed ([ADR-0263](../../adr/0263-axiom-of-linear-causality-and-pure-execution-primitives.md),
superseding ADR-0190 / ADR-0215 / ADR-0234). Long-lived work belongs outside the
agent loop; an ephemeral test server runs as a self-terminating composite shell
(e.g. `(server & PID=$!; test; kill $PID)`).

The name is `run_command` (not `bash`) so the tool's contract is stated in
terms of what the agent wants done — run a command — rather than naming a
specific shell. The wire name never leaks an implementation detail, and the
host/sandbox split below is expressed as a **variant**, not a second tool.

## Variants

One implementation, two variants selected at registration time:

| Variant | `shell_isolation` | Behavior |
|---------|-------------------|----------|
| `default` | `Host` (or the session environment's setting) | Runs in the session's workspace root with the full parameter set below |
| `workspace` | `Workspace` | Runs inside the isolated workspace sandbox; host files outside the admitted workspace roots and network access are unavailable. Only offered when `muta_platform::workspace_sandbox::available()` |

Both answer to the same tool name, so a model that learned `run_command`
works unchanged whether the session runs on the host or in the sandbox.

## Parameters (default variant)

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `command` | string | yes | — | Shell command line; must be finite and self-terminating |
| `timeout` | integer | no | `1800` | Overall timeout in seconds (30 minutes); a command producing no output for `timeout/3` (min 5s, max 480s) is killed early as a blocked-command guard |
| `raw` | boolean | no | `false` | Bypass semantic folding and return the raw, unabridged output stream |

The `workspace` variant accepts only `command` and `timeout`.

### Finite execution is the contract

Foreground commands **must** be finite and self-terminating. Unbounded
monitoring or streaming tools (`top`, `tail -f`, `ping`, `watch`, …) must be
bounded (`timeout 2s <cmd>`, `| head`, or a one-shot flag), or the StreamGuard
silence watchdog terminates them early
([ADR-0257](../../adr/0257-unbounded-stream-guard-and-finite-execution-contract.md) /
[ADR-0263](../../adr/0263-axiom-of-linear-causality-and-pure-execution-primitives.md)).

`run_command` is broker-gated in the main agent: the user approves each call
(or caches an `Always` rule scoped to the command). See
[Subagent profiles](../../explanation/agent-design/subagents.md#profiles)
for why a command-execution role is not among the built-in profiles.

## Rendering

The TUI renders calls to this tool with the `⌘` command family (or `❯` when
the invocation came from the `!` shell prefix); long or multi-line output folds
behind a `+`/`-` disclosure. See
[tool steps](../tui/tool-step.md).
