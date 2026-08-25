# Glossary

Canonical terms used across the muta documentation. Each entry links to
its primary explanation or decision record. Where a term names a code
symbol, the symbol is backticked and never abbreviated.

## Execution model

muta names its two execution layers after
[ADR-0047](../adr/0047-round-contains-turn-vocabulary.md): a **round** is the
user-perceived exchange (one submitted message and one final reply), and a
**turn** is one iteration of the ReAct loop inside it. This is the inverse of
the pre-ADR-0047 convention, which older documents may still use. See
[Rounds and turns](../explanation/agent-design/rounds-and-turns.md).

| Term | Definition |
|------|------------|
| **round** | The unit the user perceives: one admitted message and one final reply. Opens after `UserPromptSubmit` admits the prompt, closes when the agent emits a final assistant message carrying no tool call. Driven by `execute_round`. [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) |
| **turn** | One pass through the ReAct loop inside a round: one model request plus the tool work that follows. The in-round iteration count resets every round. [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) |
| **provider attempt** | One concrete network attempt inside a turn. A safe retry increments the attempt while retaining the same round and turn. [State model](state-model.md#provider-request-accounting) |
| **round lifecycle** | The at-most-one-active-round protocol per session, owned by `RoundLifecycle`: a new round supersedes its predecessor (generation bump + fresh cancellation token); interrupt cancels without superseding, so the unwinding round still emits its own cleanup. [ADR-0078](../adr/0078-round-lifecycle-type.md) |
| **`round_counter`** | Monotonic counter bumped once per round and persisted across resume; stamps todo staleness. Legacy snapshots/config events using `turn_counter` remain readable. [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) |
| **ReAct loop** | The model-request → tool-call → result loop iterated once per turn inside a round. [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) |
| **harness** | The control plane around provider calls; keeps model output inside explicit state, execution, and safety boundaries. Owns steering, retry, and the round loop. [Harness architecture](../explanation/agent-design/harness.md) |
| **transcript** | The append-mostly message history resent in full on every request — the model's only memory between requests. Never edited to change meaning. [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) |
| **catalog** (tool catalog) | The list of tool schemas published to the provider on every request; ephemeral to the runtime, republished each turn. [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) |
| **gating stack** | The ordered checks every tool call crosses before running: lookup → write-scope gate → permission broker. [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) |
| **native tool-call path** | The runtime carries tool calls in its own structured field; nothing executes until the response terminates. [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) |
| **fallback tool-call path** | For providers without native function calling: the model emits a call as ordinary text, the agent extracts it and promotes it onto the assistant message. [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) |
| **repeated-call guard** | The only in-loop guardrail: three identical tool calls in a row are stuck, so the fourth is rejected as an error. [Harness architecture](../explanation/agent-design/harness.md) |
| **uncapped agentic loop** | Distinct tool calls and autonomous iterations are uncapped; context compaction is the backstop. [ADR-0009](../adr/0009-uncapped-agentic-loop.md) |
| **hidden user message** | A message that steers the model but is not rendered in the visible transcript (implicit skill body, hook-injected context). |

## Roles

The runtime has one execution engine (`Agent`) that runs in one of two roles.
`agent` is the umbrella term; `principal` and `envoy` name the concrete roles.

| Term | Definition |
|------|------------|
| **agent** | Umbrella term for the execution engine (`Agent`, crate `muta-agent`) and the engine-level protocol (`AgentRequest` / `AgentResponse` / `AgentEvent` / `AgentOp`). Every running role is an agent; use `principal` or `envoy` when the role matters. [Harness architecture](../explanation/agent-design/harness.md) |
| **principal** | The top-level, human-facing agent a frontend drives. Owns the visible conversation and the user-tunable `[principal]` config table (`hard_stop_turns`, `allow_model_stdin`, and the advanced `nudge` guard). [Configuration](configuration.md) |
| **envoy** | An isolated child agent the principal spawns via the `envoy` tool to serve a bounded sub-question; fresh history, profile-filtered tools, shares only the provider. See the [Envoys](#envoys) section. [Envoys](../explanation/agent-design/envoys.md) |

## Scheduling

| Term | Definition |
|------|------------|
| **`/schedule` scheduler** | Clock-driven scheduler: schedules a prompt on a cron expression (recurring) or a countdown / absolute-time (one-shot), stores jobs durably as session-scoped `ScheduledJob` state, fires a fresh round per tick, drops once-jobs after firing, and auto-expires recurring jobs after 30 days. |
| **`/repeat`** | Cron-only alias for `/schedule`, retained for the recurring-cron use case. |

## Task list

| Term | Definition |
|------|------------|
| **todo list** | The single source of truth for remaining work, shared with `todo`/`todo_update`, shown in the Activity modal, and persisted across restarts. The model populates it directly; there is no longer a plan tool that seeds it. [ADR-0020](../adr/0020-unified-task-list.md) |
| **stop-gate** | The round-exit forcing function: any `Stop` hooks. It is the only gate that can refuse a round ending and force one more turn. [Harness architecture](../explanation/agent-design/harness.md) |

## Envoys

| Term | Definition |
|------|------------|
| **envoy** | An isolated child agent spawned by the `envoy` tool to investigate a sub-question; shares only the provider with the parent, runs with a fresh history and profile-filtered tools. [Envoys](../explanation/agent-design/envoys.md) |
| **profile** | A declarative bundle (name, system-prompt fragment, and a `ToolPolicy`) that scopes an envoy's behavior; bound by reference by dispatch tools. [Envoys](../explanation/agent-design/envoys.md) |
| **`EXPLORE` profile** | Research role: `Read` ceiling, no write grant; pure read tools. Bound by the `envoy` tool. [Envoys](../explanation/agent-design/envoys.md) |
| **`CODE` profile** | Coding role: write-capable (admits `bash`/`edit_file`/`write_file`). Runs autopilot like every built-in envoy — the delegation via `envoy_code` is the authorization. Bound by the `envoy_code` tool. [ADR-0087](../adr/0087-code-envoy-runs-autopilot.md) |
| **`TITLE` profile** | Read-only role used to generate a session title in a single model call. [ADR-0022](../adr/0022-session-level-ai-title.md) |
| **full-duplex** | An envoy is not fire-and-forget: requests travel up to the parent, replies travel down to the exact child. [ADR-0029](../adr/0029-full-duplex-subagent-communication.md) |

## Tools and capabilities

| Term | Definition |
|------|------------|
| **`ToolAccess`** | An ordered enum (`Read < Execute < Write`); variant order is load-bearing. Each consumer expresses its rule as a threshold. [Tool access](tools/access.md) |
| **`Read` tier** | Inspects state, no side effects. Admitted by every envoy profile; bypasses the permission broker. [Tool access](tools/access.md) |
| **`Execute` tier** | Runs commands; may have external side effects but is not a file-mutation primitive. Broker-prompted. [Tool access](tools/access.md) |
| **`Write` tier** | The tool's purpose is to mutate the workspace. Broker-prompted unless covered by a `write_paths` grant. Default when a tool does not override `access()`. [Tool access](tools/access.md) |
| **capability axes** | Beyond `access()`, the `Tool` trait exposes `requires_user()` and `spawns_envoy()`, consulted for envoy admission. [Tool access](tools/access.md) |
| **`ToolPolicy`** | An envoy profile's policy: an `access` ceiling, an `allow_user_interaction` flag, and a `write_paths` grant. [Tool access](tools/access.md) |
| **ceiling** | The ordered `ToolAccess` threshold a profile admits tools at or below. [Envoys](../explanation/agent-design/envoys.md) |
| **`write_paths` grant** | A declarative relative-dir spec on `ToolPolicy`; admits a `Write` tool below the ceiling, then scoped at runtime. [ADR-0028](../adr/0028-capability-allocation-scoped-writes.md) |
| **`WriteScope`** | A runtime, per-agent filesystem-write boundary (`None` / `Scoped` / `Unrestricted`); enforced softly — out-of-scope calls go to the user, not a hard block. [ADR-0028](../adr/0028-capability-allocation-scoped-writes.md) |
| **write-scope gate** | The gating-stack step (after lookup, before the broker) that routes out-of-scope write tools to the broker for the user to decide; hard-blocks only under autopilot, where no human can answer. [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) |
| **permission broker** | The interactive authorization surface: Write/Execute tools pass through it before execution; offers once/always/reject. [Harness architecture](../explanation/agent-design/harness.md) |
| **autopilot** | When on, the agent runs without human intervention: tool permissions auto-approve, the question tool is reclaimed, and interactive stdin is closed — it decides and acts on its own authority. Session-persisted ([ADR-0132](../adr/0132-session-persisted-autopilot-posture.md)): a daemon restart restores the posture. [Slash commands](commands.md) |
| **`tool_call_id` pairing** | The wire requirement that every result message references a preceding call id; preserved across pruning and fallback. [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) |

## Skills

| Term | Definition |
|------|------------|
| **skill** | On-demand domain expertise: a Markdown document with a small YAML header whose body is injected into the conversation when needed. Not a tool — carries no executable code. [Skills](../explanation/agent-design/skills.md) |
| **`SKILL.md`** | The skill file inside its own directory (so it can carry auxiliary files); YAML frontmatter declares identity/behavior. [Skills](../explanation/agent-design/skills.md) |
| **skill discovery** | On-demand skill metadata returned by the `list_skills` tool; the system prompt carries no skills catalog. [Skills](../explanation/agent-design/skills.md) |
| **skill body** | The full Markdown expertise document, delivered on demand through `use_skill` or an explicit implicit-invocation marker. [Skills](../explanation/agent-design/skills.md) |
| **skill scope** | The ordered source priority cascade (lowest→highest): Remote, User, Extra, Repo. Higher scope overrides a same-named lower scope. [Skills](../explanation/agent-design/skills.md) |
| **implicit invocation** | Explicit mention detection: the harness recognizes `@skill-name`, the disambiguated `@skill:name` / `@skills:name`, or `skill://…` and loads allowed skills as a hidden user message. Plain name occurrences do not trigger loading. [Skills](../explanation/agent-design/skills.md) |

## Input mentions

The user input box recognizes `@`-prefixed mention syntax in the latest
visible user message. Each mention form injects context or switches state
before the round runs.

| Term | Definition |
|------|------------|
| **`@file:` mention** | Implicit file-content injection: `@file:src/main.rs` (or `@files:…`) reads that file and appends its contents as a hidden user message, so the model sees the source without an explicit `read_text` call. Sandboxed to the workspace root (symlink-hardened: absolute paths and `..` are rejected), capped at 50 KB per file and 10 files per round. Rejections surface as a hidden error note so the model learns why and can recover. |
| **`@skill:` mention** | Disambiguated skill mention: `@skill:name` / `@skills:name` (plural mirrors `@files:`) load the named skill as a hidden user message, alongside the bare `@name` and `skill://…` forms. See [Skills](#skills) |
| **`@principal:` mention** | Runtime role switch: `@principal:architect` (code / architect / reviewer / security) switches the active principal role for the round — same effect as `/principal <role>`. [Slash commands](commands.md#principal) |
| **`@path` mention** | TUI completion trigger only: typing `@` opens path completion; the `@` is dropped on accept. Not an injection form. [Input box](tui/input-box.md) |

## TUI surfaces

| Term | Definition |
|------|------------|
| **surface** | The TUI's exact foreground navigation unit: chat, a retained `View(ViewId)`, or a `Transient(Modal)`. `SurfaceRouter` is the sole owner of the active surface and transient return stack. [ADR-0139](../adr/0139-unified-tui-surface-router-and-view-lifecycle.md) |
| **view** | A stable, directly focusable TUI place with an exact `ViewId`, retained navigation state, MRU presence, and complete create/show/hide/switch/close semantics. A shared renderer does not merge identities: Activity and Todos are separate views. [TUI modals and lifecycle](tui/modals.md#surface-and-view-lifecycle) |
| **transient surface** | A request sheet, quick switcher, or transactional editor that temporarily pushes over a parent surface and pops back to that exact parent. It is not retained or listed as a view. [ADR-0139](../adr/0139-unified-tui-surface-router-and-view-lifecycle.md) |
| **modal** | A presentation/input discriminant for an overlay. `Modal` determines rendering, recess, and input dispatch, but is not navigation identity and cannot be inverted into a `ViewId`. [TUI architecture](tui/architecture.md#surface-routing-and-shared-presentation-discriminants) |

## Context projection

| Term | Definition |
|------|------------|
| **model context** | The provider-facing view for one request: rebuilt system prompt, current model window, and current tool catalog serialized for the selected provider. [Model context](../explanation/agent-design/model-context.md) |
| **model-context projection** | The durable archive-and-replace operation that records original context in the session store and produces the model-visible window sent on later provider requests. [Session persistence](../explanation/agent-design/session-persistence.md) |
| **model window** | The current model-visible message window restored on resume and sent to the provider after prompt assembly and provider-specific filtering. [Model context](../explanation/agent-design/model-context.md) |
| **archived transcript** | Original messages moved out of the model window by pruning or compaction but retained in the durable session for full recovery. [Session persistence](../explanation/agent-design/session-persistence.md) |
| **context pruning** | The cheap first projection layer: clears stale tool-result bodies while preserving the `tool_call_id` chain. [Context pruning](../explanation/agent-design/context-pruning.md) |
| **context compaction** | The heavier second projection layer: summarizes older complete rounds into a durable checkpoint with a visible `Compacted` notice. [Context compaction](../explanation/agent-design/context-compaction.md) |
| **overflow recovery** | The reactive backstop: if a provider reports context overflow before any tool event, the runner may compact and retry once. [Harness architecture](../explanation/agent-design/harness.md) |
| **pressure** | Context size estimated in tokens (~4 chars/token), compared against thresholds derived from the active model's context window. [Configuration](configuration.md) |
| **current context** | Replaceable token projection of the next provider input for one session. It is a state value, not an accumulated usage total. [Token accounting](../explanation/agent-design/token-accounting.md) |
| **request attempt** | One concrete provider request, identified within a session by actor, round, turn, and attempt number. Retries are separate attempts because each may be billable. [ADR-0055](../adr/0055-session-scoped-request-lifecycle-accounting.md) |
| **request usage** | Additive input/output/cache accounting for provider request attempts, recorded as reported, estimated, or pending. Distinct from current context. [Token accounting](../explanation/agent-design/token-accounting.md) |

## Providers

| Term | Definition |
|------|------------|
| **provider** | An LLM backend implementing the `Provider` trait; selected at startup and on `/models` switch. [Providers](providers.md) |
| **`ModelRequest`** | The immutable core contract carrying provider-visible messages and admitted tool declarations together for one call. [ADR-0061](../adr/0061-atomic-model-request-boundary.md) |
| **`Channel`** | The fully resolved materialization of a provider id: credentials, model id, transport, and optional provider-scoped remote metadata; one per `[[providers.channels]]` entry. [Model Metadata](model-metadata.md) |
| **effort** | Reasoning **depth** — the per-model "how hard should it think" knob (`none`…`max`), abstracted from every provider's depth field onto one ladder. Orthogonal to thinking on/off. [Reasoning effort](effort.md) |
| **thinking** | The reasoning on/off switch (an Anthropic/DeepSeek concept), distinct from effort (depth). [Model Metadata](model-metadata.md#thinking-support) |
| **transport** | The wire protocol a channel uses (`OpenAi`, `Anthropic`, `Google`). [Configuration](configuration.md) |
| **model catalog** | Centralized provider-construction factory; every provider id materializes into a `Channel`, so startup and runtime switching share one resolution source. [ADR-0005](../adr/0005-strict-layering-and-renames.md) |
| **`RetryableError`** | The marker type wrapping transient provider errors; prefixed `[MUTA_RETRYABLE]`. [Providers](providers.md) |
| **provider retry** | Round-level retry loop: transient HTTP 408/429/5xx failures retried with bounded exponential backoff; retryable errors become terminal once any tool has run. [Harness architecture](../explanation/agent-design/harness.md) |
| **fitted model** | A model id the static registry does not know, materialized from a trusted provider's live `/models` capability fields (context window, reasoning, vision, effort tiers); persisted per instance and overlaid onto `model::resolve` behind the static registry. [ADR-0065](../adr/0065-runtime-fitted-model-capability-overlay.md) |
| **model discovery** | Live `GET /models` fetch for template-sourced provider instances (`ModelSource::Api`); the result is intersected with the client registry, or fitted wholesale for trusted templates. [ADR-0065](../adr/0065-runtime-fitted-model-capability-overlay.md) |
| **remote model metadata** | A trusted provider's persisted capability and endpoint snapshot for one channel. Explicit remote fields override the static baseline only for that provider route. [Model Metadata](model-metadata.md) |

## Persistence

| Term | Definition |
|------|------------|
| **durable session** | The local recoverable scene for one coding session: durable transcript, model window, archived transcript, title, task list, and projection metadata. [Session persistence](../explanation/agent-design/session-persistence.md) |
| **admission** | Writes the visible or hidden user message before provider work; each round records its admission session id. [Harness architecture](../explanation/agent-design/harness.md) |
| **XDG layout** | Files classified by nature and routed to Config, Data, State, Cache, or Runtime categories with different operational lifetimes. [Persistence](../explanation/persistence.md) |
| **override precedence** | Who decides a path, highest→lowest: CLI flag → app env (`MUTA_*_DIR`) → standard XDG env → native per-OS default → `$HOME` fallback → current directory. [Persistence](../explanation/persistence.md) |
| **per-project bucket** | Under Data; keeps each working directory's history isolated. The hash is short (16 hex chars / 64 bits). [Persistence](../explanation/persistence.md) |
| **advisory lock** | Process-level single-instance-per-project lock; falls back to State when no runtime dir is available. [ADR-0018](../adr/0018-per-project-multi-instance-concurrency.md) |

## Hooks

| Term | Definition |
|------|------------|
| **lifecycle hook** | A user-configured shell command that runs automatically at a specific point in the agent's lifecycle. [Lifecycle hooks](../explanation/agent-design/hooks.md) |
| **lifecycle event** | The events hooks fire on: `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `Stop`, `Turn`, `TurnStart`, `PermissionRequest`, `UserQuestion`, `PreCompact`, `PostCompact`. [Lifecycle hooks](../explanation/agent-design/hooks.md) |
| **implicit capability** | What a hook may do is implied by its event, not a knob: `PreToolUse`/`Stop` may deny; `PostToolUse`/`UserPromptSubmit`/`PreCompact`/`Turn`/`TurnStart` may inject context; `PermissionRequest`/`UserQuestion` are observe-only (fire-and-forget notifications). [Lifecycle hooks](../explanation/agent-design/hooks.md) |
| **matcher** | A tool-name filter on the tool events: a `|`-separated exact-name list, or a regex; omitted/`*` matches all. [Lifecycle hooks](../explanation/agent-design/hooks.md) |

## Prompts

| Term | Definition |
|------|------------|
| **model-request assembly** | The pure pre-provider projection that clones the current window, removes non-driving command echoes and legacy system messages, composes one fresh system message, and snapshots admitted tools into `ModelRequest`. [ADR-0061](../adr/0061-atomic-model-request-boundary.md) |
| **`SystemPromptSection`** | An agent-owned declarative system-prompt fragment with a stable id, rank, activation predicate, and renderer. [ADR-0056](../adr/0056-model-context-assembly-boundary.md) |
| **system-prompt registry** | Agent policy that sorts active `SystemPromptSection`s by rank and folds them into the singleton head system message of an ephemeral request. It does not construct user-role context or mutate the durable model window. [ADR-0061](../adr/0061-atomic-model-request-boundary.md) |
| **`SystemPromptContext`** | The agent-owned, read-only snapshot of live identity, admitted tool names, model/provider guidance, and autopilot state used by system-prompt sections. [ADR-0056](../adr/0056-model-context-assembly-boundary.md) |
| **harness context message** | A model-visible user-role message inserted by the harness rather than authored by the user. Common constructors enforce role, visibility, and provenance; lifecycle owners decide payload and insertion time. [Prompt and message assembly](../explanation/agent-design/prompt-assembly.md) |

## Architecture

| Term | Definition |
|------|------------|
| **`muta-contracts`** | Zero-I/O contract crate: shared provider/tool traits, `ModelRequest`, messages and events, role profiles, scopes, serialized schemas, and value types. Pure agent policy is excluded unless another independent layer shares the contract. [ADR-0057](../adr/0057-contract-only-core-boundary.md) |
| **`muta-persistence`** | The local coding-agent persistence layer: event-sourced session, blob store, config, paths, embedding index, advisory locks, telemetry. [ADR-0005](../adr/0005-strict-layering-and-renames.md), [ADR-0076](../adr/0076-rename-session-and-store-crates.md) |
| **`muta-runtime`** | The session runtime layer between orchestration and frontends: `SessionDriver` request loop, chat/permission/provider/session/slash handlers, the `/serve` control-plane WebSocket bridge, the `client` control-plane client, `/btw` side sessions, MCP runtime ownership, hooks. Application-neutral. [ADR-0037](../adr/0037-server-layer.md), [ADR-0076](../adr/0076-rename-session-and-store-crates.md), [ADR-0098](../adr/0098-crate-renames-and-library-extractions.md) |
| **`muta-llm-client`** | The multi-protocol HTTP client: pooled transport (`Client`, `Endpoint`, SSE, retry/error) plus one module per wire protocol (OpenAI chat-completions + Responses, Anthropic Messages, Google native). [Crate layering](../explanation/crate-layering.md) |
| **`muta-providers`** | The channel registry and `build_provider_for_channel` factory, plus model-list discovery, the mock provider, and the `oauth` module (OAuth2 credential acquisition: PKCE S256, the RFC 8628 device-code grant, the ChatGPT JSON device variant, browser loopback OAuth, single-flight refresh, and the on-disk `auth.toml` token store); selects which backend, with `muta-llm-client` knowing how. API-key auth is not here — it is config resolution in `muta-persistence`. [Crate layering](../explanation/crate-layering.md), [ADR-0052](../adr/0052-xai-supergrok-provider.md) |
| **`muta-skills`** | Skill metadata, discovery, remote caching, registry, refresh, and skill tool adapters. Agent consumes it for optional model-context injection. [ADR-0060](../adr/0060-skills-and-mcp-extension-boundaries.md) |
| **`muta-agent`** | The orchestration layer; primary export is the `Agent` struct. Owns turn behavior and agent-specific policy, consumes built-in tools and optional skills through downward dependencies, and accepts connector tools through `DynamicToolSink`. Carries no MCP protocol dependency — the connector is `muta-mcp`. [ADR-0060](../adr/0060-skills-and-mcp-extension-boundaries.md) |
| **`muta-mcp`** | The MCP connector crate: stdio JSON-RPC transport, server processes, tool adapters, the live `McpRuntime`, and the refresh catalog. A session (in `muta-runtime`) owns each runtime; tools reach the agent through `DynamicToolSink`. [ADR-0060](../adr/0060-skills-and-mcp-extension-boundaries.md), [ADR-0098](../adr/0098-crate-renames-and-library-extractions.md) |
| **`SessionDriver`** | The server-side owner of one live session's request receiver, runtime state, and dispatch loop; external clients interact through a `SessionHandle`. [Crate layering](../explanation/crate-layering.md) |
| **`muta`** | The core package and binary: owns daemon lifecycle, product identity, and service-control commands. It has no TUI dependency. [ADR-0136](../adr/0136-muta-core-and-mutx-terminal-app.md) |
| **`mutx`** | The terminal app package and binary under `apps/tui`: owns interactive/headless prompt clients, attachment, dashboard, clipboard behavior, rendering, and `mutx-engine`. [ADR-0136](../adr/0136-muta-core-and-mutx-terminal-app.md) |
| **attach mode** | `mutx attach [session-id]`: the TUI driving a daemon-held session as a control-plane client; the default mode for every interactive session (ADR-0096). [ADR-0081](../adr/0081-neenee-server-and-attach-model.md), [ADR-0096](../adr/0096-unified-session-daemon.md) |
| **session daemon** | The single user-level daemon runtime (`muta-runtime`) that owns all sessions across all projects, started on demand by `mutx` or via `muta daemon start`. [ADR-0096](../adr/0096-unified-session-daemon.md), [ADR-0136](../adr/0136-muta-core-and-mutx-terminal-app.md) |
| **control plane** | The daemon's read/write session-management API: the `Monitor` observability stream plus the control verbs (`create_session` / `send_prompt` / `interrupt` / `resolve_permission` / `kill_session`), served over UDS by default and TCP + token when exposed. [ADR-0096](../adr/0096-unified-session-daemon.md), [Server WebSocket API](server-api.md) |
| **`/dashboard`** | The TUI session dashboard: a first-class, full-screen live view over every daemon session — a command console (dispatch receipts plus the selected session's live monitor read-out) over a sessions dock. The console speaks the ADR-0097 grammar (`@3 text` addresses a session, `@2 @3 text` fans out, `/kill` `/interrupt` `/suspend` `/new` `/help` manage, bare text prompts the selection) and logs every dispatch with the daemon's receipt. Enter previews, `a` attaches via detach + attach (never killing running work), and `i` / `s` / `k` / `p` / `n` interrupt / suspend / kill (confirm) / prompt / create. `/host` is a hidden alias. [ADR-0096](../adr/0096-unified-session-daemon.md), [ADR-0097](../adr/0097-session-addressing-and-orchestrator-console.md) |
| **`Agent`** | The central type in `muta-agent`; owns the round/turn loop, gates, permission broker, and operation scope. [ADR-0005](../adr/0005-strict-layering-and-renames.md) |
| **strict layering** | An acyclic dependency rule: shared contracts point toward core, concrete implementations point only downward, orchestration may consume implementations, and session/application layers never acquire reverse edges. [Crate layering](../explanation/crate-layering.md) |
| **MCP server** | A local stdio MCP server exposing dynamically discovered tools; surfaces as `mcp__<server>__<tool>`. [MCP servers](../explanation/agent-design/mcp.md) |

## Legacy terms

Terms superseded by the decisions above, retained for reading older
documentation and ADRs.

| Term | Superseded by | Reference |
|------|---------------|-----------|
| `neenee` (project and command) | Muta project; `muta` core plus `mutx` terminal app | [ADR-0136](../adr/0136-muta-core-and-mutx-terminal-app.md) |
| `neenee-host` | `neenee-runtime`, then `muta-runtime` | [Crate layering](../explanation/crate-layering.md) |
| `neenee-server` (binary) | merged into `neenee`, then split as the `muta` core | [ADR-0102](../adr/0102-unified-binary-and-runtime-rename.md), [ADR-0136](../adr/0136-muta-core-and-mutx-terminal-app.md) |
| `neenee-app` | `neenee-persistence`, then `muta-persistence` | [ADR-0005](../adr/0005-strict-layering-and-renames.md), [ADR-0136](../adr/0136-muta-core-and-mutx-terminal-app.md) |
| `neenee-cli` | the former unified package; split into `muta` and `mutx` | [ADR-0080](../adr/0080-rename-neenee-to-neenee-cli.md), [ADR-0136](../adr/0136-muta-core-and-mutx-terminal-app.md) |
| `neenee-code` | `neenee`, then the Muta project | [ADR-0075](../adr/0075-rename-neenee-code-to-neenee.md), [ADR-0136](../adr/0136-muta-core-and-mutx-terminal-app.md) |
| `neenee-server` (ADR-0037 server library) | `neenee-session`, `neenee-transport`, `neenee-host`, `neenee-runtime`, then `muta-runtime` | [ADR-0037](../adr/0037-server-layer.md), [ADR-0098](../adr/0098-crate-renames-and-library-extractions.md), [ADR-0136](../adr/0136-muta-core-and-mutx-terminal-app.md) |
| `neenee-core` | `neenee-contracts`, then `muta-contracts` | [ADR-0098](../adr/0098-crate-renames-and-library-extractions.md), [ADR-0136](../adr/0136-muta-core-and-mutx-terminal-app.md) |
| `neenee-auth` / `neenee-oauth` | merged into providers, now `muta-providers` | [ADR-0077](../adr/0077-rename-neenee-auth-to-neenee-oauth.md) |
| session mirroring (`Mirror` / `MirrorUpdate`, `SessionHosting::Mirrored`) | removed — unified daemon ownership (ADR-0096) makes standalone sessions obsolete | [ADR-0095](../adr/0095-standalone-session-mirroring.md), [ADR-0096](../adr/0096-unified-session-daemon.md) |
| `neenee-harness` | `neenee-agent`, then `muta-agent` | [ADR-0005](../adr/0005-strict-layering-and-renames.md) |
| `neenee-tui-view` / `neenee-tui` | the `mutx` terminal app | [ADR-0079](../adr/0079-remerge-tui-view-into-binary.md), [ADR-0136](../adr/0136-muta-core-and-mutx-terminal-app.md) |
| `muta-mcp` (ADR-0060 crate) | merged into `muta-agent` (`mcp` module), re-extracted as `muta-mcp` | [ADR-0060](../adr/0060-skills-and-mcp-extension-boundaries.md), [ADR-0098](../adr/0098-crate-renames-and-library-extractions.md) |
| `/goal` + `/loop` | removed (`/pursue` removed in ADR-0082; `/repeat` kept) | [ADR-0082](../adr/0082-remove-pursuit-stop-gate.md) |
| `[MUTA_GOAL_COMPLETE]` | removed (marker gone with the pursuit stop-gate) | [ADR-0082](../adr/0082-remove-pursuit-stop-gate.md) |
| Plan mode | plan-as-an-envoy | [ADR-0027](../adr/0027-plan-as-subagent.md) |
| per-plan progress panel | unified todo list | [ADR-0020](../adr/0020-unified-task-list.md) |
| `plan` / `verify_plan_execution` tools | removed (planning is prompt-level) | [ADR-0033](../adr/0033-remove-plan-and-verify-workflow.md) |
| `PLAN` / `VERIFY` profiles | removed | [ADR-0033](../adr/0033-remove-plan-and-verify-workflow.md) |
| verify-nudge / todo-continuation nudge | `Stop` hooks | [ADR-0033](../adr/0033-remove-plan-and-verify-workflow.md) |
| stall detector | session-review diagnostic | [ADR-0009](../adr/0009-uncapped-agentic-loop.md) |
| `PromptChannel` / `PromptSection` / `PromptRegistry` / `PromptContext` | specialized `SystemPrompt*` vocabulary plus model-context message constructors | [ADR-0056](../adr/0056-model-context-assembly-boundary.md) |

## See also

- [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) — the
  two-layer execution model
- [Harness architecture](../explanation/agent-design/harness.md) — the
  control plane
- [ADR-0005](../adr/0005-strict-layering-and-renames.md) — the crate
  topology and naming
