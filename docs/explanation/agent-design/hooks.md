# Lifecycle hooks

A **lifecycle hook** is a user-configured action that runs automatically at a
specific point in the agent's lifecycle — before a tool call, after the model
tries to end a round, when a session starts, before context is compacted. Hooks
let external practices (format-on-edit, a CI gate before completion, a
session-start notification, a "keep the test files" instruction folded into
compaction) attach to the agent as configuration, without touching the core
loop.

This page is the design deep dive. For where hooks sit in the control plane
see [Harness architecture](harness.md); for the events they share with other
mechanisms see [Rounds and turns](rounds-and-turns.md) and
[Context compaction](context-compaction.md). For the configuration fields see
[Configuration Reference](../../reference/configuration.md#hooks); for the
decision history see [ADR-0025](../../adr/0025-lifecycle-event-hooks.md).

## Why hooks exist

The agent's lifecycle has a small set of natural interception points: a tool
is about to run, a tool just finished, the model is about to stop, a session
is opening or closing, the context is about to be summarized. Each is a place
where an external script could do useful work — enforce a rule, run a check,
inject a reminder, record an event.

Without hooks, every such practice earns its own code path through the core
loop. The result is exactly what muta had before this design: a handful of
one-shot abstractions, each invented for a single job, each with one
implementation. Hooks replace that with one configurable surface: "when X
happens, run Y."

The distinguishing constraint is that hooks are **user-programmable
lifecycle points**, not a second copy of the engines that already drive the
agent. Context pressure, turn counting, and the clock are each already owned
by a purpose-built mechanism — `CompactionPolicy` decides when to relieve
pressure, `/repeat` drives clock-based work. Hooks do not re-expose those as
configurable axes; they expose the lifecycle events those engines fire on.

## One event axis, implicit capability

Hooks fire on **lifecycle events**, grouped by cadence into four families:

- **per session** — `SessionStart`, `SessionEnd`;
- **per round** — `UserPromptSubmit`;
- **per attempted round stop** — `Stop`;
- **per ReAct turn** — `TurnStart`, `Turn`;
- **per tool call** — `PreToolUse`, `PostToolUse`, `PostToolUseFailure`;
- plus the compaction pair `PreCompact` / `PostCompact`.

What a hook is *allowed to do* is not a knob the user picks — it is implied by
the event it fires on. A `PreToolUse` hook may block the call; a `Stop` hook
may force another turn; a `PostToolUse` or `UserPromptSubmit` hook may
inject context the model then sees; the rest only observe. The user writes
"on `Stop`, run this script"; the system already knows a `Stop` hook's result
is a continue-or-stop decision.

This is deliberate. A design that exposed the context threshold, the turn
count, and the clock as further hook axes would duplicate the engines that
already govern them and muddy two clean concerns — "what the harness does
under pressure or on a schedule" versus "what the user adds on a lifecycle
event." muta keeps the first internal and exposes only the second.

## The event set

| Event | Fires | Capability |
|-------|-------|------------|
| `SessionStart` | A session begins or resumes | Observe; injected context becomes hidden setup messages |
| `SessionEnd` | A session ends on clean exit | Observe |
| `UserPromptSubmit` | The user submits a prompt, before it enters the transcript | Deny (drop the prompt) or inject (prepend context) |
| `PreToolUse` | Before a tool call runs | Deny (block the call) |
| `PostToolUse` | After a tool call succeeds | Inject context |
| `PostToolUseFailure` | After a tool call fails | Inject context |
| `Stop` | The model tries to end the round | Deny (force another turn, feeding the reason back) or inject |
| `PreCompact` | Before a summarizing compaction | Inject (folded into the summary prompt) |
| `PostCompact` | After a compaction completes | Observe |
| `Turn` | After each non-terminal ReAct turn, before the next model request (ADR-0030) | Inject only — **`Deny` is ignored**, so a turn-count hook cannot become a de-facto turn cap. Carries the read-only-turn streak so a hook can target exploration-without-progress. The harness declares no built-in threshold here; users opt in. |
| `TurnStart` | Once per ReAct turn, at turn **start** — after tools are prepared, before the next model completion | Inject only — **`Deny` is ignored** (same constraint as `Turn`). It is the symmetric partner of `Turn`; use it to (re)inject context for the upcoming turn, for example to re-anchor the principal's role after read-only delegations. The former event name `RoundStart` remains a read-only compatibility alias. |
| `PermissionRequest` | The agent is about to **block** waiting for your approval (a tool with a side effect needs permission) | Observe-only — **`Pass` only**, fire-and-forget. The canonical use is a desktop/bell notification so you notice a long-running task is parked on you. Honours a tool-name matcher (e.g. only `execute_command`). Cannot grant or deny. |
| `UserQuestion` | The agent is about to **block** on an `ask_user` question | Observe-only — same fire-and-forget contract as `PermissionRequest`. No matcher (`ask_user` is a single tool). |

A hook returning a capability the event does not honour is ignored, so a
script that unconditionally reports a deny only bites on events that act on a
deny.

## Scoped tool disabling (`ScopeTools`)

A `PreToolUse`, `TurnStart`, or `Turn` hook may return `ScopeTools` to
**temporarily** hide tools from the model — their schemas are dropped and
dispatch rejects them — and have them come back automatically at a restore
point. This lets a policy hook scope the toolset to a scenario (e.g. drop `execute_command`
for a read-only sub-task) without you toggling `/tools` by hand.

| Restore point | When the disable is undone |
|---------------|---------------------------|
| `react_turn_end` | At the end of the current ReAct turn (next `Turn` hook boundary) |
| `user_round_end` | When the whole user round ends (the model replies with no tool calls, or the round terminates) |

New output always uses the explicit canonical strings above. The pre-ADR-0047
values remain load-compatible only: legacy `round_end` is interpreted as
`react_turn_end`, and legacy `turn_end` as `user_round_end`.

Scoped disables are **never persisted**: they live in memory only, never reach
the session store, and never collide with your manual `/tools` toggles (which
use a separate, persisted mask). Nested disables compose by reference count, so
two hooks disabling the same tool at different restore points don't fight — the
tool stays hidden until its latest restore point fires.

## Matchers

The three tool events (`PreToolUse`, `PostToolUse`, `PostToolUseFailure`)
filter on the tool name. A matcher is a plain string evaluated by its shape:

| Matcher shape | Evaluation | Example |
|---------------|------------|---------|
| Only letters, digits, `_`, and `|` | A pipe-separated list of exact names | `Write|Edit` matches either tool exactly |
| Any other character | A regular expression | `^Bash.*`, `mcp__.*` |
| Omitted or `*` | Matches every tool | — |

MCP tools surface as `mcp__<server>__<tool>` and match identically, so a
single `mcp__memory__.*` matcher covers every tool on the `memory` server.
The non-tool events ignore the matcher and fire on every occurrence.

## The command contract

A hook runs a shell command. The command receives a JSON snapshot of the
event on stdin and replies through its exit code and stdout:

Hook definitions and Hook execution are separate security decisions. A
project Hook must first belong to a trusted Hooks asset domain. Every command
Hook must also have an already-present exact runtime permission rule using
`tool = "hook"` and the command string as its scope. Hooks run from inside the
agent's lifecycle, so a missing rule skips the Hook and reports a fail-closed
diagnostic instead of recursively opening an approval prompt. For example:

```toml
[permissions]
allow = [
  { tool = "hook", scope = ".muta/hooks/lint.sh" },
]
```

```text
event fires, matcher matches
  └─ spawn  sh -c <command>,   cwd = project root
        stdin  ←  { "event", "session_id", "tool_name", ... }  (JSON)
  └─ within 60 s:
        exit 2 + stderr          → deny;  stderr is the reason fed back
        stdout is a JSON object  → { "decision": "deny"|"approve",
                                     "reason": "...", "context": "..." }
        anything else            → pass (a non-blocking error never aborts)
```

The three reply shapes map to the three capabilities: `decision: "deny"` (or
exit 2) blocks or continues depending on the event; `context` injects text;
anything else passes. A hook that times out, fails to spawn, or exits
non-zero with no decision JSON is treated as pass — a flaky script cannot
wedge the agent loop. Hard rules belong to the
[permission system](harness.md), not a hook.

The JSON object is flat and `jq`-friendly: one level with `event`,
`session_id`, `cwd`, and the event-specific fields (`tool_name`,
`tool_input`, `tool_output`, `prompt`, `last_message`, …).
`Turn` and `TurnStart` also carry the one-based enclosing `round`, the
zero-based ReAct `turn`, and `consecutive_readonly`; retrying a provider
request does not change either position.

## Composition with the loop

Hooks do not replace the agent's existing gates; they sit alongside them.

A tool call flows through several gates in order. A `PreToolUse` hook runs
first — before the permission broker is even asked — so a hook can spare the
user a permission prompt for a call it intends to block:

```text
tool call declared
  ├─ [Hooks]        PreToolUse (matcher?)  ── deny? → blocked, reason to model
  ├─ [WriteScope]   per-agent write boundary (envoys only)
  ├─ [Harness]      permission broker (Write / Execute tools)
  ├─              tool executes
  └─ [Hooks]      PostToolUse (success) | PostToolUseFailure (error)
                        └─ inject context? → hidden message on the next turn
```

At round end, a `Stop` hook is the only thing that can refuse a round ending
and force one more turn. A `Stop` hook that denies forces one more turn with
its reason fed back as a hidden user message. If no `Stop` hook is configured
(or none denies), the round ends on the model's natural stop.

Around compaction, a `PreCompact` hook's injected context is folded into the
summary prompt (so a hook can say "prefer keeping the test files" and have it
influence what the model summarizes), and `PostCompact` observes the result.

## What hooks are not

- **Not a threshold or time axis, and only a constrained turn axis.** Context
  pressure and the clock stay internal (`CompactionPolicy`, `/repeat`). Turn
  counting is exposed as the `Turn` event (ADR-0030) but **`Deny`-forbidden**
  — it lets a hook inject context at a turn boundary (e.g. to react to a
  read-only streak) without being able to abort the round, which would recreate
  the blanket turn cap ADR-0009 removed. The harness sets no built-in
  threshold on it; only the user does, at their own risk.
- **Not a substitute for permissions.** A hook deny is best-effort and
  non-fatal on failure; the permission broker is the hard enforcement
  surface. Enforce mandatory policy with permissions, use hooks for
  project-specific practice.
- **Not synchronous with the model.** A hook runs between turns or before a
  call; it does not pause generation. Long work should be offloaded (a hook
  can itself spawn detached processes); the 60-second bound keeps the loop
  responsive.

## See also

- [Harness architecture](harness.md) — the control plane the hooks attach to,
  and the permission broker a `PreToolUse` hook precedes
- [Rounds and turns](rounds-and-turns.md) — the model/tool turn the per-tool
  events bracket
- [Context compaction](context-compaction.md) — the summarization the
  `PreCompact` / `PostCompact` events surround
- [Configuration Reference](../../reference/configuration.md#hooks) — the
  `[[hooks]]` table fields
- [ADR-0025](../../adr/0025-lifecycle-event-hooks.md) — the decision to
  adopt a single event axis with implicit capability, and the multi-axis
  design rejected along the way
- [ADR-0030](../../adr/0030-early-loop-intervention-and-round-hook.md) — the
  `Deny`-forbidden event now exposed as `Turn`; the ADR retains its historical
  `Round` vocabulary. It partially supersedes ADR-0025's exclusion of
  loop-count (the in-loop review nudge it also added was later reworked into
  the deterministic guard of
  [ADR-0034](../../adr/0034-range-aware-pruning-and-deterministic-read-loop-guard.md))
