# `execute_command`

`ExecuteCommandTool` (`crates/muta-agent/src/tools/execute_command.rs`) executes
a shell command in a non-interactive shell. It is the one built-in tool in the
`Execute` access tier — it runs commands but is not a file-mutation primitive,
so it sits between pure reads and file writes. The permission broker still
gates it (`Execute > Read`). It is excluded from every built-in envoy profile,
all of which carry a `Read` ceiling today, so `execute_command` runs only in
the main agent. See [Tool access](access.md) and
[ADR-0012](../../adr/0012-toolaccess-tier-split.md).

The name is `execute_command` (not `bash`) so the tool's contract is stated in
terms of what the agent wants done — run a command — rather than naming a
specific shell. The wire name never leaks an implementation detail, and the
host/sandbox split below is expressed as a **variant**, not a second tool.

## Variants

One implementation, two variants selected at registration time:

| Variant | `shell_isolation` | Behavior |
|---------|-------------------|----------|
| `default` | `Host` (or the session environment's setting) | Runs in the session's workspace root with the full parameter set below |
| `workspace` | `Workspace` | Runs inside the isolated workspace sandbox; host files outside the admitted workspace roots and network access are unavailable. Only offered when `muta_platform::workspace_sandbox::available()` |

Both answer to the same tool name, so a model that learned `execute_command`
works unchanged whether the session runs on the host or in the sandbox.

## Parameters (default variant)

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `command` | string | yes | — | Shell command line |
| `timeout` | integer | no | `300` | Overall timeout in seconds; a command producing no output for `timeout/3` (min 5s, max 60s) is still killed early as a blocked-command guard |
| `terminal_id` | string | no | — | Persistent terminal session identifier to reuse environment variables, cwd, and shell state across commands |
| `run_persistent` | boolean | no | `false` | Run in a persistent terminal session |

The `workspace` variant accepts only `command` and `timeout`.

`execute_command` is broker-gated in the main agent: the user approves each
call (or caches an `Always` rule scoped to the command). See
[Envoy profiles](../../explanation/agent-design/envoys.md#profiles)
for why a command-execution role is not among the built-in profiles.

## Rendering

The TUI renders calls to this tool with the `⌘` command family (or `❯` when
the invocation came from the `!` shell prefix); long or multi-line output folds
behind a `+`/`-` disclosure. See
[tool steps](../tui/tool-step.md).
