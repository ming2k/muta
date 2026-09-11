# `execute_command`

`ExecuteCommandTool` (`crates/muta-agent/src/tools/execute_command.rs`) executes
a shell command in a non-interactive shell. It is the one built-in tool in the
`Execute` access tier — it runs commands but is not a file-mutation primitive,
so it sits between pure reads and file writes. The permission broker still
gates it (`Execute > Read`). It is excluded from every built-in subagent profile,
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
| `timeout` | integer | no | `1800` | Overall timeout in seconds (30 minutes); a command producing no output for `timeout/3` (min 5s, max 480s) is killed early as a blocked-command guard |
| `background` | boolean | no | `false` | Bounded work dispatched to the background job fabric (ADR-0190 `JobKind::Interactive`). The call returns a `job_id` immediately; collect the outcome with the `process` tool |
| `service` | boolean | no | `false` | Long-lived work (dev server, watcher, daemon) as a `JobKind::Service` job: readiness is reported and an unsolicited exit is recorded as a task event. Takes precedence over `background` when both are set (ADR-0234) |
| `label` | string | no | — | Human-readable job label (`cargo-test`, `dev-server`) shown in the session's task list |

The `workspace` variant accepts only `command` and `timeout`.

### What a background call does and does not promise

Dispatching to the fabric is *not* a promise that the model is woken when the
job ends. The completion is published as a task event (task bar, `/jobs`,
ledger) and the model collects it with the `process` tool — `action: 'wait'` to
block until it settles, `'status'`/`'logs'` to inspect it meanwhile. An
autonomous wake turn is specified in [ADR-0234](../../adr/0234-authorized-background-job-continuations.md)
but is not implemented; until it is, a foreground call whose sync budget
expires with the child still alive hands the child to the same fabric
(detached, same `process` collection path).

### Scheduling is not offered

`execute_command` has no `schedule_in_secs`/`repeat` parameters. They were
removed in [ADR-0234](../../adr/0234-authorized-background-job-continuations.md):
the tool described "the command runs at fire time", but the runtime's Timer arm
only publishes a digest and never executes a shell command — and with the wake
turn disabled ([ADR-0212](../../adr/0212-decouple-followup-queue-and-authoritative-task-bar.md))
that digest had no consumer either. A model-facing timer returns only with a
real execution contract (`JobSpec::Timer` remains in the fabric, unused by any
tool).

## The `process` tool

`ProcessTool` (`crates/muta-agent/src/tools/process_jobs.rs`) is the controller
for jobs the fabric already knows about — it never starts one
([ADR-0215](../../adr/0215-tool-surface-consolidation.md)). One tool, one
`action`, addressed by the `job_id` returned when the job was dispatched.

| Action | Parameters | Returns |
|--------|------------|---------|
| `status` | — | The job snapshot — spec, state, timestamps, latest output line — plus the settled `summary`/`log_path` once the job has finished |
| `logs` | `tail_lines` (default 50, max 200) | Recent stdout/stderr lines |
| `wait` | `timeout_seconds` (default 60, max 600) | Blocks until the job settles, then returns state, `summary`, `log_path`, `tail_logs`, and `deliveries_claimed` |
| `kill` | — | Terminates a job whose cancel handle is still live |

### `wait` is the collection point

A settle is retained (bounded, oldest evicted loudly) rather than only
broadcast, so a result survives a missed notification
([ADR-0234](../../adr/0234-authorized-background-job-continuations.md)). `wait`
returns that settled summary and **claims** the delivery: the same settlement is
never delivered twice, and a later automatic continuation — once SystemWake
exists — will not redo work the model has already seen. `deliveries_claimed: 0`
means the terminal result was already collected.

Claiming governs automatic delivery, not readability: the job keeps its own
settled result, so `status` and a repeated `wait` still report the `summary` and
`log_path` after the delivery has been claimed.

`status` and `logs` deliberately do not claim: inspecting a job's progress is
not accepting its outcome, so the result stays collectable.

`kill` refuses a job that has already settled or is already terminating — its
pid may belong to an unrelated process by then — and reports the job's state
instead.

### `wait` on a service

A running service has no terminal state by contract — running *is* its success
state — so `wait` returns immediately with `service_still_running` rather than
blocking out the whole budget and ending in a timeout error. Use `status` or
`logs` to inspect it, or `kill` to stop it. A service that has actually exited
*has* settled and is waitable like any other job.

### Autonomous continuation after a job finishes

A settled job normally does **not** start a model round: the agent collects the
result with `wait` when it is ready. Autonomous continuation exists for the case
where nobody is present to ask again, and it is deliberately narrow
([ADR-0234](../../adr/0234-authorized-background-job-continuations.md)):

| Condition | Behavior |
|-----------|----------|
| Interactive session (the default) | **Never wakes.** A human is present and will ask; the result stays collectable through `process` |
| Unattended session | One wake round per originating request |
| A wake round finishes | It does not re-arm the budget — an autonomous chain cannot extend itself |
| Human follow-up queued | The wake is refused; the human's turn goes first |
| A round is already running | The wake is refused; the result reaches the model through the ordinary tool path |
| Job readiness / progress | No wake — a service reporting "ready" is a task-bar fact, not a result |

A wake round is driven by a harness-authored digest naming each finished job, its
state, and its bounded summary, explicitly labelled as *not* a user message. The
full output stays behind `process`.

`execute_command` is broker-gated in the main agent: the user approves each
call (or caches an `Always` rule scoped to the command). See
[Subagent profiles](../../explanation/agent-design/subagents.md#profiles)
for why a command-execution role is not among the built-in profiles.

## Rendering

The TUI renders calls to this tool with the `⌘` command family (or `❯` when
the invocation came from the `!` shell prefix); long or multi-line output folds
behind a `+`/`-` disclosure. See
[tool steps](../tui/tool-step.md).
