# Harness Architecture

The harness is the control plane around provider calls. It keeps model output
inside explicit state, execution, and safety boundaries.

## Round execution

Every CLI round runs the streaming agent loop:

1. Refresh the system context with pursuit, tools, and skill metadata.
2. Stream provider text and reconstruct native tool-call deltas by index.
3. Execute native or JSON fallback tool calls through the same registry.
4. Emit tool call/result events for the TUI.
5. Stop on a final assistant message or a harness safety bound.

Streaming remains inside the harness. Text fallback JSON is withdrawn from the
visible transcript before its tool step is emitted.

Provider adapters must preserve the harness system context. OpenAI-compatible
providers use system messages; Google maps them to `systemInstruction` and
returns fallback tool results as user-context text.

The TUI merges each tool call and result into a semantic step. Steps are
collapsed to a one-line status by default and expand inline on click or
`Enter` (when focused) to show complete JSON arguments and output. Session
replay rebuilds the same steps in FIFO order,
including parallel calls with identical tool names.

## Provider capabilities

The harness distinguishes two model capability surfaces. Tools are declared
to the provider on every request; reasoning is observed from the provider
when the model emits it. For the capability model and wire-level protocol,
see [Provider capabilities](../provider-capabilities.md) and
[Rounds and turns](rounds-and-turns.md).

### Declared: tools

Tool schemas live in the ephemeral model request, not the conversation. Each
round snapshots the admitted tools together with the provider-visible
messages before any network work. The adapter translates that same snapshot
into its protocol's declaration field.

Tool schemas are request-scoped. Every ReAct turn, including the turn that
carries tool results back upstream, sends the same complete schema set
alongside the full message history. The provider is stateless across requests.

The OpenAI-compatible providers declare schemas natively: the registry
presets (`kimi-code`, `zai-code`) and the catalog-built `openai`/`deepseek`
multi-model entries all share one adapter, so they inherit native tool
declaration. The Anthropic adapter declares Anthropic-format `tools`; the
Google adapter converts the same schema set into Google
`functionDeclarations` and replays results as `functionResponse` parts.
A provider that does not serialize the supplied declarations never sends a
native tools field; tool calls on it travel only through the universal
fallback below.

### Observed: reasoning

The harness never declares reasoning support and never sends a flag that
requests it. Providers passively read `reasoning_content` from stream deltas
and complete messages, forwarding it as a reasoning event. Only models that
emit the field (`deepseek-v4-flash` thinking mode, reasoning-tuned GLM and
Qwen variants) surface reasoning; other models produce none.

Reasoning is rendering metadata. It is not summarized, not re-injected as
follow-up context, and not used for control flow.

### Tool call transport

Both execution paths feed one shared registry:

| Path | Transport | Tool calls |
|------|-----------|-----------|
| Non-streaming | Single HTTP request/response cycle | `choices[0].message.tool_calls` complete |
| Streaming | SSE stream | `delta.tool_calls` fragments accumulated by `index` |

The streaming path accumulates `id`, `name`, and `arguments` per index while
text and reasoning deltas render live. After the stream reaches `[DONE]`,
calls with an empty `id` are assigned `call_<uuid>`, calls with an empty
`name` are dropped, and the survivors are executed. Side effects never fire
mid-stream.

### Universal fallback

For providers without native function calling, the harness extracts
`{"tool": "<name>", "arguments": {…}}` from assistant text and promotes the
parsed call onto the preceding assistant message as a native `tool_calls`
entry so OpenAI-compatible `tool_call_id` pairing stays valid on the next
turn.

Fallback text is withdrawn from the visible transcript before the tool step
is emitted, matching the native streaming path. The same registry, permission
broker, and result-message format apply to native and fallback calls.

## Pursuit state

`/pursue <condition>` creates a durable, per-session objective persisted as a
field on `SessionData` (`Option<Pursuit>` via `SessionStore`, ADR-0032), so it
survives restarts and `/resume`. The durable objective holds its objective,
completion bit, optional user budget, and the latest terminal reason; an
orthogonal runtime record holds the armed flag, continuation count, and budget
counters. This is intentionally not one flattened status machine and has no
checklist. See
[ADR-0010](../../adr/0010-slim-goal-primitive.md) and
[ADR-0015](../../adr/0015-pursue-stop-gate-and-repeat-cron.md). There are no
model-facing pursuit tools: the user sets the condition via `/pursue`, the
harness drives continuation via the stop-gate, and the model signals completion
with `[NEENEE_PURSUIT_COMPLETE]` (ADR-0031). See
[Pursuits](pursuits.md) for the primitive and the persistence model.

## Pursue stop-gate

`/pursue` arms a **stop-gate** on the agent and drives one round. Each time the
model would end the round, the gate re-injects the condition as a hidden user
message and forces another turn instead of returning. The round therefore runs
across many turns.

| Form | Effect |
|------|--------|
| `/pursue <condition>` | Set the condition, arm the gate, and drive the round until met |
| `/pursue` | Re-arm and drive on the existing active pursuit |

The pursuit stops when:

- the model emits `[NEENEE_PURSUIT_COMPLETE]`;
- the 50-pass safety cap is hit (the gate disarms);
- the user presses `Esc` or runs `/pursue stop`;
- a newer request supersedes it;
- the provider or tool pipeline returns an error.

This replaces the old outer multi-round `/loop` with
within-round continuation — one driver, no outer loop. The clock-driven
counterpart is `/repeat`, a cron scheduler; see
[Pursuits](pursuits.md) for the comparison.

Task generation ids prevent an older task from clearing the cancellation state
of a newer task.

## Provider retry

Transient HTTP 408, 429, 5xx, connection, and timeout failures are retried up
to `provider_retry_max_attempts` (default 6, hard maximum 10). Provider
`Retry-After` or `retry-after-ms` headers take priority; otherwise the delay is
bounded exponential backoff using `provider_retry_base_ms` and
`provider_retry_max_ms`.

The TUI shows the next attempt and countdown without adding transcript noise.
`Esc`, `/pursue stop`, session switching, or a newer request cancels the wait.
Partial streamed assistant text is withdrawn before retry. A completed
tool-bearing turn is a checkpoint: its results stay in history while only the
pending provider request is retried. Request preparation, turn-start hooks, and tools
that produced the checkpoint are not replayed. If a replacement completion
nevertheless repeats an exact pre-retry tool call, the checkpoint result remains
authoritative and the duplicate call is not executed.

## Safety bounds

- Exact tool-call replays after a provider retry are never re-executed.
- An optional deterministic doom-loop guard can block repeated watched tool
  signatures before execution.
- 8 seconds to initialize an MCP server.

Distinct ordinary tool turns are **uncapped by default**, matching the codex /
claude-code agentic-loop model. Context compaction (thresholds derived from
the active model's context window, plus mid-round pruning) is the backstop that
keeps them within the model window; the user can interrupt at any time. An
explicit `hard_stop_turns` remains available as an opt-in bound.

The pursuit stop-gate is deliberately different: an explicitly armed
autonomous attempt has a 50-pursuit-pass safety cap, in addition to any
user-set pass/token/time budget. A pursuit pass is one natural-stop decision,
not every tool-calling model turn. This does not reintroduce a default cap on
ordinary rounds; it bounds only the opt-in mechanism that keeps overriding the
model's natural stop.

### Advanced doom-loop guard

The optional doom-loop guard detects repeated normalized signatures for common
read, search, command, fetch, and file-mutation tools. When a watched signature
would recur within the configured window, the guard blocks it before execution
and injects a hidden explanation that directs the model to change approach.
The block lasts only for the current round and does not terminate other work.

The guard is deterministic bookkeeping with no model call, but signature
normalization is intentionally conservative: operations on the same target may
collide even when secondary arguments differ. It is therefore an advanced,
default-off policy configured through `[principal.nudge]`, not a routine TUI
preference. Envoys and `/review` force it off. See the
[Configuration Reference](../../reference/configuration.md#agent-behavior).

### Session review (ADR-0016)

Because an uncapped loop can still *appear* stuck, `/review` runs an
**on-demand session-review** diagnostic over the current round. It spawns a
bounded read-only `REVIEW` envoy, returns one verdict per registered dimension,
and never aborts or automatically steers the live round. There is no periodic
review cadence and legacy `[agent.review]` settings are ignored.

The only execution cap is an explicit, opt-in `hard_stop_turns` (default **0**
= off); a finite value is a user-declared budget and the sole thing that
hard-stops a round. Envoys do not expose their own `/review` path.

Invoke it with the no-argument `/review` slash command.

These are execution bounds, not a security sandbox. Tool permission policy is
a separate future layer.

Write capability is enforced per-agent through a `WriteScope` boundary
(ADR-0028): the main agent is unrestricted (the permission broker is still the
interactive layer inside it); an envoy carries a scope resolved from its
profile, and a write tool whose target is outside that scope is blocked. All
built-in envoy profiles carry a `Read` ceiling today, so this gate is
inactive in practice but available to future scoped-write roles. MCP servers
with `read_only = false` declare `Write` and are subject to the same gate when
run inside a scoped envoy.

## Permission broker

Write-capable tools pass through a core permission broker before execution:

1. Core stores a one-shot waiter and emits `PermissionRequest`.
2. The CLI projects the request to the TUI.
3. The permission modal offers once, always, or reject.
4. Always requires a separate confirmation and is cached by tool plus resource
   scope for the current process. File writes scope by path and bash scopes by
   its complete command.
5. The reply resolves the waiter and tool execution resumes or returns a
   denied result.

Interrupting or superseding a task rejects all pending waiters and clears the
TUI blocker. `/permissions` makes cached rules observable and
`/permissions clear` revokes them.

The headless entry point automatically rejects write permissions.
Interactive clients use the event-driven entry point and reply to emitted
requests.

The `unattended` toggle suppresses this broker entirely: when on, a
side-effecting tool never parks a request and the once/always/reject modal
never appears. It is the live, blanket form of the relaxation the `always`
allowlist grants per rule. For the design intent behind running without human
intervention — and where the flag is forced on — see
[Unattended operation](unattended.md).

## Durable session

The CLI persists one active session as an atomic JSON snapshot and keeps
branch snapshots under `sessions/<id>.json`:

- Admission writes the visible or hidden user message before provider work.
- Agent execution uses a local message snapshot and does not hold the shared
  history mutex while waiting for providers, tools, or permissions.
- Commit replaces shared history and writes the full tool/assistant result.
- Startup restores visible messages, reconstructing native tool-call entries
  while filtering system and hidden harness prompts.
- `/session fork` creates a child with the same transcript and clears its loop
  checkpoint; `/session list` and `/session open <id-prefix>` allow branch
  navigation.
- Each round records its admission session id and refuses a late commit after a
  session switch.

Pursuit checkpoints project the one-based pursuit pass, the 50-pass maximum, and
a typed attempt status (`running`, `completed`, `interrupted`, or `error`).
`/session status` exposes that projection. The objective record and pursuit
runtime — not the display checkpoint — restore an unfinished attempt's armed
flag, continuation count, and budget counters; running `/pursue` on that
restored attempt preserves those counters. `/session new` cancels old work and
creates a fresh session id.

## Context projection

The runner projects the durable session into a model-visible window in three
pressure-driven layers, cheapest first. Every threshold is derived from the
**active model's context window** — measured in
tokens and re-seeded whenever the provider switches — so a 1M-token model is
no longer over-compacted at ~3% of its window and a 128k model is no longer
under-protected (ADR-0019). Pruning and compaction commit through one durable
model-context projection mechanism (`ContextProjection*`), so the complete
recoverable scene survives while only the model window changes (ADR-0040).

| Layer | Trigger | Surfaced? |
|-------|---------|-----------|
| [Tool-result pruning](context-pruning.md) | ~65% of the window (`prune_utilization`) | Implicit — `debug` trace only |
| [Summarizing compaction](context-compaction.md) | ~85% of the window (`utilization`) | Visible — `Compacted` notice; `/compact` runs it manually |
| Overflow recovery | a provider reports context overflow | Reactive (see below) |

The first two layers each have a dedicated deep-dive — [Context
pruning](context-pruning.md) and [Context compaction](context-compaction.md);
exact keys and defaults live in the
[Configuration Reference](../../reference/configuration.md#compaction).

**Overflow recovery** is the harness's own reactive backstop and has no separate
page. If a provider reports context overflow *before* any `ToolCall` event, the
runner may compact and retry the same logical round once. Overflow *after* tool
activity is terminal, so tool side effects are never replayed.

## Extension surfaces

- Skills add on-demand model instructions.
- MCP servers add dynamically discovered tools.
- Built-in tools and MCP tools share the `Tool` trait and event pipeline.
- Future permissions should wrap tool execution in the shared execution path.
- Future durable sessions should persist messages and loop checkpoints without
  changing the provider abstraction.
