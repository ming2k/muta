# Changelog

All notable changes to **neenee** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Dedicated todo bar.** The agent's live task list now has its own one-row
  bar directly below the activity bar (and above the queue bar), showing
  `todo · done/total · {current item}` with a `Ctrl+T expand` legend. It
  replaces the `todos d/t` badge that used to ride on the activity bar, so
  the activity bar is now purely transient (hidden while idle) and the task
  list stays glanceable whenever it is non-empty. Clicking the bar or
  pressing `Ctrl+T` opens the Activity modal on the Todos tab, as before.

- **Unified key-display vocabulary.** A physical key now has two canonical
  strings, both derived from one token table so they can never drift:
  `Key::chord()` — the compact lowercase form (`ctrl+t`, `enter`, `esc`, `↑`)
  used by Help prose rows — and `Key::display()` — the capitalized form
  (`Ctrl+T`, `Enter`, `Esc`, `↑`) used by footer hint strips, the activity-bar
  interrupt hint, and in-modal legends. A new `keyvocab` module owns the
  repeated affordance glyphs (`keyvocab::ARROWS_UD`, `keyvocab::SPACE`,
  `keyvocab::SHIFT_TAB`, …) plus the single-key display names
  (`keyvocab::ESC`, …) as `&'static str` constants, so every footer literal
  and every legend now references the vocabulary instead of typing the glyph
  inline. Side effect: the activity bar's idle hint and the queue legend now
  render in the same capitalized case as the footers (`ctrl+t` → `Ctrl+T`,
  `esc`/`tab`/`F2` → `Esc`/`Tab`/`F2`), so every surface finally agrees on how
  a key is spelled. The `FooterHint::key` field, the Help modal rows, and the
  activity-bar keycaps all read from the same place.

- **Standalone `neenee-server` binary and `neenee --attach` co-driving
  (ADR-0081).** A new headless session host (`neenee-server --project <path>
  [--session <id>] [--port <n>] [--public]`) hosts one session and serves it
  over the same WebSocket protocol `/serve` uses, writing a per-project
  discovery record (`$XDG_RUNTIME_DIR/neenee/serve/<project-bucket>.json`,
  mode 0600, removed on clean shutdown) so clients can find it.
  `neenee --attach [session-id]` runs the TUI as a WebSocket client of that
  live session — spawning the server on demand when none runs — so two
  frontends co-drive one session. The `History` frame gained a `session_id`
  field (the only protocol change). See
  [ADR-0081](docs/adr/0081-neenee-server-and-attach-model.md).

- **Session-harness assembly factory (ADR-0081).** The session assembly that
  lived in the CLI's `main.rs` moved behind
  `neenee_transport::bootstrap::assemble`, which both binaries now call with
  an injected identity, principal profile, and `UiBridge`. The transport
  layer stays application-neutral, and the CLI's direct dependencies on
  `neenee-tools` and `neenee-mcp` are gone. See
  [ADR-0081](docs/adr/0081-neenee-server-and-attach-model.md).

- **Modular prompt-cache control (ADR-0067).** A pure-domain `CachePolicy`
  classifier now resolves each model family's caching strategy
  (`Breakpoints`/`SessionKey`/`Automatic`). As a result the token-source report
  surfaces cache-hit counts for **every** provider — OpenAI's
  `prompt_tokens_details.cached_tokens`, Gemini's `cachedContentTokenCount`, and
  Moonshot's top-level `cached_tokens` — not just Anthropic. Moonshot/Kimi
  sessions now send the session id as `prompt_cache_key` so repeated prefixes
  hit a server-side cache at a discount. See
  [ADR-0067](docs/adr/0067-modular-prompt-cache-control.md).

- **System-reminder dynamic injection (ADR-0068).** A two-tier XML trust model
  gives event-driven, mid-turn instructions a canonical channel: authoritative
  `<system-reminder>` directives the model must follow vs `<untrusted_…>`
  escaped data it must treat as data. Two new `InjectionKind` variants
  (`SystemReminder`/`UntrustedDirective`) keep provenance traceable. See
  [ADR-0068](docs/adr/0068-system-reminder-dynamic-injection.md).

- **Pursuit budgets and runtime stats (ADR-0069).** `/pursue budget
  passes=N tokens=N time=Ms` sets opt-in hard budgets on an active pursuit;
  reaching any budget stops the loop with a named `terminal_reason` and a usage
  summary. `/pursue status` now shows live passes/tokens/elapsed, and a
  convergence reminder fires past 75% of a budget. The marker-based stop-gate
  is preserved (no LLM judge). See
  [ADR-0069](docs/adr/0069-pursuit-budgets-and-stats.md) and its accounting
  refinement,
  [ADR-0083](docs/adr/0083-crash-consistent-pursuit-attempt-accounting.md).

- **Configurable TUI color schemes.** The redesigned flat `/config` Settings
  overlay now includes live-previewed Zen, Midnight, Nord, Catppuccin, and
  Paper presets plus an editable eight-color custom palette. Appearance and
  layout choices apply immediately and persist under `[tui]` in `config.toml`.

- **Decision-intelligence workbench and expert council.** A new reusable
  `neenee-intelligence` crate collects ranked public-web topics, persists the
  last good result per source, and observes selected links with HTTP validators
  plus SHA-256 fallback fingerprints. The expert council runs five independent
  perspectives, a cross-examination round, and a separate meeting-manager
  synthesis while keeping all conclusions advisory and outside the order path.
  See [ADR-0063](docs/adr/0063-intelligence-workbench-and-expert-council.md).

- **Direct Longbridge/LongPort OpenAPI integration for `neenee-quant`.** The
  official Rust SDK now supplies real-time quotes, candlesticks, depth, live
  account balances and positions, order submission, and cancellation through
  one shared adapter. API-key and OAuth authentication are supported, secrets
  stay out of serialized/debug configuration, local risk and audit checks run
  before live submission, and client-side trade throttling follows LongPort's
  published limit. The quant GUI now distinguishes disarmed trading from paper
  brokerage and reports the configured live broker accurately. See
  [ADR-0062](docs/adr/0062-longport-openapi-quant-adapter.md).

- **Provider-scoped remote model metadata for GitHub Copilot (ADR-0070).**
  The Copilot provider now identifies itself with the public Copilot OAuth
  client id (unlocking real subscription entitlements), sends the integration
  headers the `/models` endpoint expects, and discovers per-model metadata —
  endpoint family, context window, reasoning, vision, effort tiers — per
  channel, persisted and overlaid onto the static baseline only for that
  provider route. Each Copilot model is routed to its declared endpoint
  (chat/responses/messages), and the picker hides entries whose model picker
  flag is off. Login and the add-provider flow now run live discovery
  automatically, with per-provider failures surfaced as a discovery warning
  instead of being swallowed. The OAuth modal is larger and scrolls with both
  keyboard and mouse. See
  [ADR-0070](docs/adr/0070-provider-scoped-remote-model-metadata.md),
  [How to avoid Copilot provider pitfalls](docs/how-to/copilot-provider-pitfalls.md),
  and the [model metadata reference](docs/reference/model-metadata.md).

- **Tool call arguments are schema-validated before dispatch.** A call whose
  arguments fail the tool's declared JSON Schema — wrong top-level type,
  missing required property, or a mistyped primitive — is rejected up front
  with an explicit error in the same shape as a tool failure, so the
  malformed call never reaches the tool implementation. Previously a type
  error landed in the tool's own parser, where some tools silently coerced
  it (for example a string `"3"` for an integer `offset`).

### Changed

- **Multi-question prompts now have a complete paged interaction.** `Enter`
  advances through up to five question pages and submits only from the final
  page; `Shift+Tab` goes back, with each page retaining its highlight,
  selections, and free-form **Other** text. `ask_user` now enforces its
  documented two-to-four-option contract.

- **Pursuit attempt state is crash-consistent and observable in matching
  units.** Checkpoints now store a typed status plus the actual one-based
  pursuit pass and 50-pass safety limit. Runtime persistence includes
  pass/token/time budget counters; resuming a restored in-flight pursuit
  preserves them. Every non-completion path records a terminal reason, while
  completion and re-arm clear stale reasons. See
  [ADR-0083](docs/adr/0083-crash-consistent-pursuit-attempt-accounting.md).

- **Pursuit contained behind the stop-gate; pursuit module slimmed to its
  domain values (ADR-0082).** Pursuit now has a written containment
  invariant: it may interact with the round loop only through the
  `stop_gate` composition point (the gate chain shared with `Stop` hooks),
  and any new touchpoint outside that chain needs its own ADR. The
  `neenee_core::pursuits` junk drawer was emptied — `TokenUsage` moved to
  `neenee_core::usage` (the `neenee_core::TokenUsage` re-export is
  unchanged), `RoundOutcome` moved into `neenee-agent` — leaving only
  `Pursuit` and `PursuitBudget`. No user-visible behavior change. See
  [ADR-0082](docs/adr/0082-contain-pursuit-behind-the-stop-gate.md).

- **Package renamed `neenee` → `neenee-cli`; the command stays `neenee`
  (ADR-0080).** With a second application binary (`neenee-server`), the
  single-binary premise of ADR-0075 no longer held. Only the Cargo package
  and directory changed: `[[bin]] name` is still `neenee`, so every
  invocation, alias, installer, and release artifact is untouched;
  `cargo -p neenee-cli` selects the package. See
  [ADR-0080](docs/adr/0080-rename-neenee-to-neenee-cli.md).

- **`neenee-tui-view` merged back into the binary (ADR-0079).** The view
  crate had a single consumer and changed in near-lockstep with the shell
  (92% of its commits also touched the binary), so the widgets, document
  model, and overlays moved back into `crates/neenee-cli/src/tui/` as
  ordinary modules and the crate was deleted. The engine/view/shell
  layering stays as a documented convention and the `TranscriptView` seam
  is unchanged. See
  [ADR-0079](docs/adr/0079-remerge-tui-view-into-binary.md).

- **Round lifecycle consolidated into `RoundLifecycle`; `loop_status` is now
  typed (ADR-0078).** The cancellation-token + generation protocol that
  guards "at most one active round per session" was copied inline across
  nine sites (interactive rounds, pursuits, `!` shell commands, `/btw` side
  sessions, session-switch slash commands, interrupt); it now lives in one
  `neenee_agent::RoundLifecycle` type whose API (`begin` / `supersede` /
  `cancel_current` / `finish`) makes the interrupt-vs-session-switch
  distinction explicit instead of comment-only. `HarnessSnapshot.loop_status`
  changed from `String` to the `LoopStatus` enum (`idle` / `running` /
  `pursue`) — the `/serve` wire format is unchanged, but mismatches are now
  compile errors. No user-visible behavior change. See
  [ADR-0078](docs/adr/0078-round-lifecycle-type.md).

- **Renamed `neenee-auth` → `neenee-oauth` (ADR-0077).** The crate does only
  OAuth2 credential acquisition (PKCE, device flow, proactive refresh, the
  `auth.toml` token store) — API-key auth lives in `neenee-persistence`. The
  old name overstated its scope and collided with the `ChannelAuth` concept in
  `neenee-core`; the new name matches the `OAuth` facade and ADR-0074's
  "name the job" test. Workspace-internal rename (not published): path
  dependencies and `use neenee_auth::` references update to `neenee-oauth` /
  `use neenee_oauth::`. See
  [ADR-0077](docs/adr/0077-rename-neenee-auth-to-neenee-oauth.md).

- **Breaking: renamed the application binary `neenee-code` → `neenee`
  (ADR-0075).** With the editor and quant products removed (ADR-0073), the
  `-code` suffix no longer disambiguates a sibling domain binary, so the sole
  product ships under its bare product name. The crate, the `[[bin]]` target,
  the `crates/neenee-code/` directory, the release artefact, the install
  script's `BIN_NAME`, and the default workspace member all become `neenee`.
  **Migration:** reinstall (`curl ... install.sh | bash`) so `~/.local/bin`
  holds `neenee`, or `ln -s neenee-code neenee` as a temporary bridge; update
  any shell aliases, completion scripts, and `RUST_LOG=neenee_code=…` targets
  (now `neenee=…`). See
  [ADR-0075](docs/adr/0075-rename-neenee-code-to-neenee.md).

- **Breaking: renamed `neenee-session` → `neenee-transport` and `neenee-store`
  → `neenee-persistence` (ADR-0076).** Vocabulary cleanup companion to
  ADR-0075. `neenee-session` collided with the `neenee-store::session` storage
  module (two different things both called "session"); it owns the request loop,
  handlers, and `/serve` WebSocket bridge — i.e. the **transport** a frontend
  attaches to, which is what its own docs already called it. `neenee-store`
  held config, paths, advisory locks, and telemetry alongside storage, so
  "store" described only half the crate; **persistence** spans all of it. The
  word "session" now means one thing across the workspace: the persisted
  conversation in `neenee-persistence::session`. **Migration:** update path
  dependencies (`Cargo.toml`) and `use neenee_session::` / `use neenee_store::`
  references to `neenee_transport` / `neenee_persistence`. Internal `Session*`
  type names (`SessionDriver`, …) are unchanged. See
  [ADR-0076](docs/adr/0076-rename-session-and-store-crates.md).

- **Consolidated LLM client crate (ADR-0074).** The four `neenee-ai-sdk-*`
  crates (core + openai/anthropic/google) are merged into one
  `neenee-llm-client` crate: a pooled transport layer (`Client`, `Endpoint`,
  SSE, retry/error) plus one module per wire protocol
  (`protocol::{openai, anthropic, google}`). Providers now embed a single
  `Client` that reuses one `reqwest::Client` connection pool across every turn,
  replacing the previous per-request `reqwest::Client::new()` (which discarded
  keep-alive and TLS session reuse on every call). Runtime behaviour is
  unchanged apart from connection pooling. `neenee-providers` remains the
  channel registry/facade. See
  [ADR-0074](docs/adr/0074-consolidate-llm-client-crate.md).

- **Flat coding-focused workspace (ADR-0073).** All workspace members now live
  directly under `crates/`; the `apps/{code,editor,quant}/` and
  `crates/{platform,providers}/` grouping directories are gone. Package names
  and the dependency graph are unchanged, so `cargo -p <name>` is unaffected.
  `iris`/optics and `longport` workspace dependencies were dropped along with
  the products that used them. See
  [ADR-0073](docs/adr/0073-flat-coding-focused-workspace.md).

- **State bar relocated below the input and lowercased.** The persistent
  session-state row now sits directly under the input box (above the hint bar)
  rather than between the activity bar and the input, so `unattended` reads as
  an attribute of the composer area. The flag is rendered lowercase
  (`unattended`, warning tone + bold). The row still costs zero vertical space
  while no indicator is active and remains the designated home for future
  ambient state (workspace, etc.). See the
  [state bar reference](docs/reference/tui/state-bar.md).

- **`Ctrl+T` now opens the Todos modal.** It no longer bulk-toggles tool-step
  expansion; that affordance moved to per-step click / `Enter` / `Space`. The
  Todos modal surfaces the agent's live task list (read-only in the TUI) and is
  the same view reached by clicking the `todos d/t` badge on the activity bar.

- **Unified keybinding registry.** Global shortcuts now live in one place
  (`tui::keymap`) that both the input handler and the Help modal read from, so
  the keys shown in Help can never drift from the keys that actually fire.
  Adding a global shortcut is a single declarative entry that appears in Help
  automatically.

- **The `kimi-code` provider now serves Kimi K3 and tracks the platform's live
  model list.** Moonshot's coding platform released K3 — a 1,048,576-token
  context window, image/video inputs, and always-on thinking — so the preset's
  default model id moves from `kimi-k2.7-code` to `k3`. The template now
  discovers the platform's `GET /models` list at startup and **fits**
  capability metadata for platform ids the client registry does not know
  (context window, reasoning, vision, effort tiers; persisted per instance and
  overlaid onto model resolution behind the static registry), so future
  platform models become usable with zero client changes. Existing instances
  upgrade their model source from `Fixed` to `Api` automatically; their
  current default model is preserved while it remains advertised. See
  [ADR-0065](docs/adr/0065-runtime-fitted-model-capability-overlay.md).

- **Heavy native and broker integrations are now opt-in during development.**
  Root Cargo commands default to `neenee-code`; the editor GUI and LongPort
  SDK require explicit `gui` and `longport` features. Development and test
  profiles retain line information while omitting full dependency symbols,
  and test builds no longer retain incremental state indefinitely.

- **The quant GUI is now a modern decision workspace.** The optics/iris shell
  adds an overview cockpit, public-intelligence and expert-council workspaces,
  a persistent control plane, modern navigation, status treatments, and clear
  paper-versus-live execution boundaries. The optics Rust binding now exposes
  its existing headings, multiline text, icons, and theme controls so product
  layout remains in the application crate.

- **Transient provider failures now resume from completed tool checkpoints.**
  The retry path resends only the pending model request, preserving turn state
  and avoiding duplicate request hooks. An exact tool call repeated by the
  replacement completion is short-circuited instead of executing its side
  effects again.

- **A `/provider` switch now survives a restart.** The switch handler and the
  add-provider flow persist the provider/model selection as the global
  `config.toml` default (`default_provider`/`default_model`) in addition to
  pinning it to the session, so the next launch lands on the switched model
  instead of reverting to the startup default. The session pin still wins for
  resume: reopening a session restores its own model exactly, while a fresh
  session follows the new global default. Other live sessions keep their
  in-memory selection. Non-selection mutations (favorites, metadata edits,
  TUI layout and color scheme) keep preserving the on-disk selection. See
  [ADR-0066](docs/adr/0066-dual-write-provider-selection.md).

- **Context request usage now follows the conversation's round and turn
  structure.** The report lists one total per round and opens each round into
  its ReAct turns, instead of grouping requests by provider and model.
  Provider-reported values use bold styling, local estimates use underlining,
  and mixed totals use both, with a compact source legend before the list.

- **Provider requests are now atomic and request-scoped.** `ModelRequest` pairs
  provider-visible messages with admitted tool declarations; `Provider`
  implementations now consume it directly and the stateful `prepare_tools`
  API is removed. The agent separates durable `conversation_context` additions
  from ephemeral `model_request` assembly, and rebuilt system prompts are no
  longer written into the durable model window. The specialized
  `SystemPromptContext`, `SystemPromptSection`, and `SystemPromptRegistry` APIs
  remain agent-owned; embeddings must migrate provider implementations and
  test doubles to the new request signature.

- **Skills no longer expose an empty bundled-system tier.** The unused
  `skills.bundled` setting, `SkillScope::System`, embedded-skill loader, and
  `include_dir` dependency are removed. Skill priority now starts at remote
  sources and ends at project-local sources.

- **The agent now consumes its concrete tool bundle through a normal downward
  dependency.** `TodoWriteTool`, `TodoUpdateTool`, and `TodoToolContext` live
  in `neenee-tools`; every `Agent` automatically binds those tools to its own
  todo and turn state. Embeddings can add product-specific tools through
  `AgentBuilder::with_tool` / `with_tools` without repeating agent-owned
  wiring.

- **Skills and MCP now have explicit capability boundaries.** Skill discovery,
  registries, refresh, and tool adapters live in `neenee-skills` and attach to
  an agent through `AgentBuilder::with_skills`. MCP transport, adapters,
  connections, and refresh live in `neenee-mcp`; the session owns the runtime
  and publishes per-server snapshots through the connector-neutral
  `DynamicToolSink`. The former MCP-specific shared lock and source-name
  inference are removed.

- **Search results are now capped at the shared output budget.** The
  DuckDuckGo, Tavily, and SearXNG backends previously returned an unbounded
  formatted result list; `format_results` now passes through the same
  16,000-character cap the other search backends already applied.

- **`neenee-tools` crate merged away.** The built-in tools (`bash`,
  `read_text`, `grep`, `glob`, `webfetch`, todo, …) moved into `neenee-agent`'s
  new `tools` module, and slash-command discovery plus project scaffolding
  moved into `neenee-transport` as its `commands` and `project` modules. The
  standalone `neenee-tools` crate is deleted; tool behavior is unchanged.

- **OpenAI provider types renamed for clarity.** `OpenAiProvider` is now
  `OpenAiChatCompletionsProvider` (the Chat Completions implementation) and
  `ResponsesProvider` is now `OpenAiResponsesProvider` (the Responses API
  implementation), both in `neenee-llm-client`. Behavior is unchanged; the
  `OpenAiProviderSpec` registry concept keeps its name.

- **`neenee-oauth` crate merged into `neenee-providers`.** OAuth2 credential
  acquisition (PKCE browser loopback, the RFC 8628 and ChatGPT JSON device
  flows, single-flight refresh, the `auth.toml` token store) now lives in
  `neenee_providers::oauth`, alongside the registry whose `xai-oauth` /
  `chatgpt-oauth` / `copilot-oauth` templates it serves. The standalone
  `neenee-oauth` crate is deleted; login and refresh behavior is unchanged.

### Removed

- **Dead pursuit types and the expired legacy pursuit migrations
  (ADR-0082).** The unused `RoundTimer` and `ThreadPursuit` types are
  deleted, and the one-shot migrations that folded a pre-ADR-0032
  `pursuits.db` or pre-ADR-0010 `harness_goal*` config keys into
  `SessionData.pursuit` are gone — the migration window (~1 month, 10
  releases) has closed. The old file and config keys are left on disk but
  never read; upgrading across the window means re-setting the objective
  with `/pursue`. See
  [ADR-0082](docs/adr/0082-contain-pursuit-behind-the-stop-gate.md).

- **Editor and quant products.** `neenee-editor`, `neenee-quant`,
  `neenee-quant-gui`, and `neenee-intelligence` are deleted; the repository
  now ships the `neenee-code` coding agent only. Their how-to guides
  (`enable-live-quant-broker`, `use-intelligence-workbench`) and the
  "Quant runtime" / "Intelligence workbench" configuration sections are gone.
  ADR-0062 and ADR-0063 are superseded by ADR-0073.

- **Dormant ADR-0037 §6 server-move scaffolding.** `SessionRegistry`,
  `SessionHandle`, and `SharedState` — every method returned
  `Err("not yet populated")` — are gone from `neenee-session`; the crate docs
  now describe the single-driver model that actually runs. Reintroduce the
  factory when the multi-session server move resumes.

### Fixed

- **Cancelling an `ask_user` modal now settles the parked agent request.**
  `Esc` sends an explicit cancellation sentinel before closing the modal, so
  the tool returns a cancelled result instead of leaving its round blocked.
  The empty outer answer list is reserved for cancellation; valid
  multi-select replies remain distinguishable by carrying one inner list per
  question.

- **Round/turn semantics now follow ADR-0047 end to end.** A round is the
  complete user↔agent exchange and contains one or more ReAct turns; a turn is
  one model request plus its tool work. Runtime events and counters,
  transcript grouping, Activity and token reports, hooks, compaction, schemas,
  and current documentation now use that hierarchy. History and harness
  snapshots carry the persisted round counter, so compacted/resumed/attached
  sessions retain absolute numbering. New persistence/config output uses
  canonical round names; legacy `turn_counter`,
  `updated_at_turn`, `turn_counter_set`, `compaction_preserve_turns`,
  `RoundStart`, `round_end`, `turn_end`, pursuit `turns`, and `max_turns`
  values remain load-compatible.

- **Round supersession now settles every parked request and pursuit attempt.**
  Starting a successor, a direct shell command, interruption, or switching
  sessions rejects permission, question, and interactive-input waiters. The
  caller that supersedes a pursuit records its terminal reason and checkpoint
  before the stale task relinquishes ownership.

- **The opencode-go seed no longer includes models the relay does not serve.**
  Legacy-config migration seeds one channel per entry of
  `OPENCODE_GO_SERVED_MODELS` (mirroring models.dev) instead of every
  registry model in a served family, so newly registered models like Kimi
  `k3` — and the already-registered `glm-4.7` — no longer appear as go
  channels that would only answer "model not found".

- **The quant GUI now starts directly against a local optics build.** Its
  binary carries runtime search paths for the optics shared libraries instead
  of requiring a manual `LD_LIBRARY_PATH`. Explicit `--paper` and
  `--longport-live` launch profiles keep simulated and real-account entry
  points separate; live mode still starts disarmed.

- **Expanded edit-diff scroll height no longer depends on the scroll offset.**
  The renderer accounted a logical row in `content_lines` only when it was
  painted on screen, so once the viewport clipped the body mid-hunk the
  measured step height shrank — and since the app loop derives `max_scroll`
  from it, the scroll position oscillated and the frame flickered during the
  animation heartbeat. Every logical row is now counted through one
  `RenderCtx::paint` call, decoupling height from scroll position.

- **No more silent fallback to a mock provider.** When no real provider
  channel resolves (unknown id, or no usable channel), the catalog returns
  `None` instead of a `MockProvider`: startup installs an explicit
  `NoProvider` sentinel, a `/provider` or default-model switch that resolves
  to nothing is refused with a notification, and chat, `/btw`, and
  queued-outbox sends fail fast with "No provider configured. Add one with
  /provider before sending a message." Previously a message could silently
  reach a non-functional mock and die there.

- **Streaming Anthropic turns no longer book zero prompt tokens.** The
  stream parser discarded `message_start` — the only event carrying
  `input_tokens` and the cache creation/read counts — and kept only the
  final `message_delta`, so every streamed turn recorded `prompt_tokens = 0`
  and lost all prompt-cache discounts. Streamed usage now merges
  `message_start` with subsequent deltas (input side replaced as a snapshot,
  output side cumulative), matching the non-streaming fold.

- **Truncated provider streams are now detected and retried instead of
  silently accepted.** A stream that ends mid-tool-call — a call slot with a
  partial id or arguments but no name, or arguments that are not valid
  JSON — now fails as a retryable "likely truncated" error instead of the
  call being silently dropped or executed with half its arguments. The SSE
  layer no longer swallows an incomplete trailing event at EOF, and invalid
  UTF-8 in a completed frame surfaces as an explicit retryable error instead
  of being masked by lossy replacement.

### Security

- **API keys and OAuth tokens are now redacted at the type level.** A new
  `SecretString` (`neenee-core`) masks `Debug`/`Display` output with `***`
  and keeps `expose_secret()` as the only plaintext path; it now backs
  provider `api_key` fields, built-in provider keys, stored credentials,
  OAuth token sets, PKCE verifiers, device codes, web-search keys, and the
  provider-management wire requests. On-disk files stay plaintext by design
  (mode 0600), so existing configs load unchanged. Transport errors
  additionally mask credential query parameters (`?key=` / `api_key` /
  `access_token`) in URLs, closing a path where a Gemini transport failure
  could print its key. See
  [ADR-0072](docs/adr/0072-type-level-secret-redaction.md).

## [0.20.3] - 2026-07-12

### Changed

- **Context accounting is now session-scoped and request-lifecycle aware.**
  Current context remains a replaceable projection of the next model input;
  provider usage is recorded separately for every principal or envoy
  round/turn/attempt. Completed, interrupted, failed, retried, and crash-
  abandoned attempts retain reported-versus-estimated provenance and survive
  session resume. Forks inherit context without duplicating the parent's
  historical request usage.

- **Renamed `neenee-server` to `neenee-session`.** The crate that owns one
  live agent session's runtime — the request loop, handlers, `/btw` side
  sessions, MCP runtime, slash-command dispatch, and `/serve` transport — is
  now named for what it actually is. The vocabulary it defines
  (`SessionDriver`, `SessionRegistry`, `SessionHandle`, `SharedState`) already
  centered on "session"; the crate name now matches. `agent_loop::Harness`
  and the free `agent_loop::run(req_rx, harness)` are gone: the module is
  `session_driver`, the type is `SessionDriver`, and it owns the request
  receiver itself (`tokio::spawn(driver.run())`). Embeddings that referenced
  `neenee_server::…` must update to `neenee_session::…`. This is a breaking
  public-API rename; ADRs (notably ADR-0037) retain the historical
  `neenee-server` / `agent_loop` names as decision records.

### Fixed

- **The context report now shows the initial pre-request estimate and refreshes
  immediately after interruption or unsend.** The modal separates current
  AI-visible context from request usage and expands provider/model
  totals into round, turn, and attempt lifecycle rows. Primary and `/btw` side
  sessions no longer overwrite each other's context meter or token report.

## [0.20.2] - 2026-07-11

### Fixed

- **Release tarball builds again with `--locked`.** The committed `Cargo.lock`
  locked the nine optics packages (`iris`, `iris-sys`, `lens`, `lens-sys`,
  `flux`, `flux-sys`, `flux-text`, `flux-text-sys`, `flux-text-layout`) as
  source-less path entries. This happens when the lock is regenerated while the
  local, gitignored `.cargo/config.toml` path override (pointing at a sibling
  `../optics` checkout) is active: cargo drops the `source = "git+..."` line.
  That lock is unresolvable anywhere the override is absent — CI, the release
  tarball, downstream packagers — and breaks every `--locked` step with
  "cannot update the lock file ... because --locked was passed". v0.19.0
  shipped this same bug; v0.20.1 regressed. Regenerated the lock with the
  override disabled so all nine entries carry their pinned git source
  (`rev = 0c9d4a2`), with no version or dependency changes.

## [0.20.1] - 2026-07-11

### Fixed

- **Transcript spacing now follows semantic segments within model-request
  rounds.** Round headers, thinking, tool batches, and assistant text use one
  separator row; parallel tool calls in the same round remain flush regardless
  of disclosure state. Expanded thinking begins directly below its header with
  no redundant first-line gap. Live and restored assistant components carry
  the same round stamp, including concurrent side sessions.

- **Context compaction again follows the active model's full window.** The
  undocumented 96k working-set ceiling and fixed 8k prompt reserve no longer
  make 1M-token models compact at roughly 7.5% utilization. Pruning and full
  compaction now compare the complete projected request — including the live
  system prompt, injected skills, and visible tool schemas — with the model's
  65%/85% thresholds. Legacy `max_active_tokens` and
  `prompt_reserve_tokens` config keys are ignored, and compaction notices label
  their UTF-8 size measurements as bytes rather than characters.

- **Completed edits no longer flash the transcript or show malformed context.**
  Bottom-follow layout now stages height-changing transcript frames and commits
  only the final scroll position, so an auto-expanded diff does not paint an
  intermediate viewport first. Edit patches preserve the file's original line
  boundaries and endings while selecting three real context lines on each side;
  failed edits render their error instead of a diff for a change that never
  reached disk. Completed Patch diffs are derived once in a bounded render-layer
  cache and reused across later animation frames.

## [0.20.0] - 2026-07-10

### Changed

- **Add provider now reads as a child page of Providers.** The template chooser
  keeps the provider list's panel footprint and shows `Providers / Add provider`
  in the header; `Esc` returns to the provider list as before.

### Fixed

- **Provider discovery no longer erases relay credentials or endpoints.** Live
  `/models` results are intersected with neenee's protocol-compatible model
  registry, so unknown or unsupported ids never enter the picker. Refreshing a
  model set preserves the provider's token, token environment variable, base
  URL, user agent, authentication mode, and surviving per-model settings; an
  empty intersection keeps the last valid list. API-discovered instances also
  retain their persisted subset across startup instead of being reset to the
  full template before the picker opens.

## [0.19.1] - 2026-07-10

### Fixed

- **Release binaries for v0.19.0 were never published.** The v0.19.0 release
  commit shipped a `Cargo.lock` in which the optics packages (`iris`, `flux`,
  `flux-text`, and friends) were recorded as source-less path entries — a side
  effect of regenerating the lock while the local, gitignored
  `.cargo/config.toml` path-override was active. With that override absent (as
  it is on CI runners and in release tarballs), every `cargo build --locked`
  failed with `cannot update the lock file ... because --locked was passed`,
  so all five release targets (linux x86_64/aarch64, linux musl, macOS
  x86_64/aarch64) exited 101 and no artifacts were uploaded. This release
  restores the git `source` lines and rebuilds the binaries. Added a CI guard
  (`lockfile resolves (--locked)`) to prevent recurrence.

## [0.19.0] - 2026-07-10

### Added

- **xAI OAuth (SuperGrok) provider.** "+ Add provider" offers **xAI OAuth**.
  Selecting it opens a pending modal, starts browser OAuth (loopback PKCE on
  `127.0.0.1:56121`), shows the authorize URL, and after success prompts for
  an instance name. Grok models (`grok-4.5` / `4.20` / `4.3` / `build-0.1`)
  over `https://api.x.ai/v1/chat/completions`. Tokens in `auth.toml` under
  `xai`. Backed by `neenee-auth` (mirrors opencode). See ADR-0052.

## [0.18.0] - 2026-07-09

### Added

- **Explicit-path `@mentions`.** The `@` mention trigger now accepts filesystem
  path prefixes (`@../`, `@./`, `@~/`, `@/`) and resolves them against the real
  directory, so files *outside* the project scan can be mentioned. Candidates
  expand to absolute paths, letting you reference any file on disk, not just
  descendants of the working directory.

- **Writing-a-skill guide.** Added `docs/how-to/write-a-skill.md` walking
  through authoring a skill: where to place it (project-local vs user-global),
  how the harness discovers it, and the minimal YAML frontmatter it needs.

### Changed

- **`@mention` accept drops the trigger.** The `@` is now treated purely as a
  completion *trigger*: once a concrete candidate is chosen, accepting a file
  (or an explicit-path) mention removes the leading `@`, splices the path in
  place (preserving surrounding prose), and appends a trailing space so you can
  keep typing. Directory accepts keep the `@` so the popup re-triggers to
  descend into the directory's contents. Absolute-path mention labels (which
  legitimately start with `/`) are no longer mistaken for slash commands.

### Fixed

- **Stray `/` no longer errors.** A message whose first token begins with `/`
  but is not a recognized command (built-in, discovered custom command, or the
  frontend-only `/serve`) is now sent as ordinary chat instead of being
  dispatched as a slash command the backend rejected with "Unknown command".
  The `/` is just a character you typed.

- **Question-modal caret tracking.** Pasting, typing, or backspacing in the
  "Other" free-text field now re-arms follow-scrolling, so the modal body
  scrolls to keep the caret on screen as the field wraps across lines.

## [0.17.0] - 2026-07-09

### Added

- **`neenee-editor` crate.** Added a headless UTF-8 text-editing core plus an
  optics-backed GUI shell for a small code editor. The workspace member includes
  buffer, selection, history, display-map, editor-controller, rendering, README,
  and integration-test coverage; the pure-Rust core can be built and tested with
  `--no-default-features`.

- **Transcript spacing reference.** Added `docs/reference/tui/transcript-spacing.md`
  to document the ownership contract for transcript gutters, inter-message gaps,
  component-local padding, metadata strips, and tool-step spacing.

- **`MetaStrip` render component.** The two-tone one-line metadata header —
  an info-tone bold anchor joined to muted ` · ` details — is now a single
  reusable component (`MetaStrip`/`MetaChip`/`MetaTone` in
  `render/components/meta_strip.rs`). The assistant round header
  (`◆ round N · model · HH:MM`) and the sent user-message header
  (`turn N · HH:MM` / `⏸ Queued`) both compose it instead of each
  hand-building a `Vec<Span>`. Future headers (token cost, tool-call count)
  are a `.detail()` call, not a new builder. No behaviour change; output is
  byte-identical apart from the new turn gutter below. (ADR-0049.)

- **Turn gutter rail.** Sent user-message headers now lead with a `▌`
  (left-half block) glyph in the accent tone, giving the user turn — the
  larger, user-perceived scope — a deliberate visual anchor the in-round
  band does not have. The glyph occupies the first column of the existing
  text gap, so `turn N` / `⏸ Queued` stay aligned with the message body. No
  column widths change. (ADR-0049.)

- **`RoundStart` lifecycle hook.** The hook event axis is now symmetric: in
  addition to the round-end `Turn` hook (ADR-0030), a `RoundStart` hook fires
  at the start of each tool round — after tools are prepared but before the
  next model completion. It honours `Inject` (folding context to the top of the
  model's attention for the upcoming round) and discards `Deny`, the same
  constraint as `Turn`. Configure it with `event = "RoundStart"` in a `[[hooks]]`
  table. Use it for periodic context re-injection, e.g. to re-anchor the
  principal's role after a run of read-only delegations.

- **Interrupt-event hooks (`PermissionRequest`, `UserQuestion`).** Hooks can
  now fire when the agent is about to **block** waiting for you — either on a
  permission approval prompt (`PermissionRequest`, honours a tool-name matcher)
  or on an `ask_user` question (`UserQuestion`). Both are **observe-only /
  fire-and-forget**: their outcomes are ignored, so a notification hook can
  never grant/deny or alter the transcript. The canonical use is a desktop /
  terminal-bell notification so a long-running task that goes unattended still
  grabs your attention. A drop-in `notify.sh` example is provided in
  `assets/hooks/`.

- **Scoped tool disabling (`ScopeTools` outcome).** A `PreToolUse`,
  `RoundStart`, or `Turn` hook may return `ScopeTools` to temporarily hide tools
  from the model and have them re-enabled automatically at a restore point
  (`round_end` or `turn_end`). This lets a policy hook scope the toolset to a
  scenario (e.g. drop `bash` for a read-only sub-task) without manual `/tools`
  toggling. Scoped disables are **never persisted**: they live in memory only
  and never collide with the session-level `/tools` mask. Nested disables
  compose by reference count.

### Fixed

- **Principal "role bleed" after read-only delegations.** An envoy's read-only /
  toolset-scoped persona framing flows back into the principal's transcript via
  the envoy summary; after a run of read-only delegations the recent context
  could co-activate "delegation ↔ read-only" strongly enough that the model
  over-generalized it into "I (principal) have no write tools" — even though the
  principal's toolset is unchanged (spawning an envoy never touches
  `resolved_tools`). A deterministic role-reanchoring note is now appended to
  every envoy tool-result text at the single choke point where the summary enters
  the transcript, reaffirming that the read-only scope applies to the envoy only
  and that the principal retains its full toolset (write/edit tools + shell). The
  note is structural and unconditional — it does not depend on a `[hooks]` entry
  being configured — and varies by the envoy's `failed` flag.

- **Inconsistent recording of interactive slash commands.** Commands that open
  a modal (`/provider`, `/permissions`, `/tools`, `/mcp`, `/skills`, `/config`)
  were intercepted locally in the input layer and never reached the `SendSlash`
  path, so — unlike notification-style slash commands (`/pursue`, …) — their
  invocation was dropped from both the transcript and the Ctrl+R input history.
  A text modal command now records its invocation identically to `SendSlash`:
  it pushes a `Role::User` message tagged `UserMessageOrigin::Slash` into the
  transcript and enters input history, so both command families behave the same
  regardless of whether their UI is modal or inline. A modal *outcome* (e.g. a
  provider switch) still lands as a follow-up notice, so a `/provider` switch
  reads as a natural pair (`> /provider` then `↳ Provider switched to …`).
  Keybinding-driven modals (Ctrl+R, F1, …) and `/exit` are deliberately
  excluded (no typed text / not replayable).

- **Slash/shell invocations now survive a restart.** Previously slash commands
  and `!cmd` shell passthroughs reached the live TUI transcript but were never
  written to the durable transcript, so they **vanished on resume** — a resumed
  session had no record you ever ran `/pursue`, opened the provider picker, or
  shell-passthrough'd `!ls`. They are now persisted as a new "non-driving"
  message category: a visible `Role::User` echo stamped with the `CommandEcho`
  injection provenance. Such echoes survive resume and `/export` (for audit
  faithfulness) but are **projected out before the provider wire** in
  `prepare_turn_messages`, so the model never sees them — they are not driving
  prompts. This adds a genuinely new "durable-but-not-model-visible" bucket
  (the `hidden` flag was the wrong axis: it hides from the UI but still sends
  to the model). Resume reconstructs the echoes with the correct
  `UserMessageOrigin` by consulting the stored origin first, and compaction no
  longer counts them as turn boundaries. (ADR-0050.)

## [0.16.0] - 2026-07-03

### Added

- **sub2api usage guide.** Added a how-to for configuring OpenAI, Anthropic,
  and Gemini-style sub2api relays, including the exact endpoint paths neenee
  expects.

### Changed

- **OpenAI sub2api provider template.** The template now seeds OpenAI text
  models directly instead of opening on a generic model-id field, and user-facing
  provider-editor copy uses "OpenAI" rather than "OpenAI-compatible".

- **OpenAI GPT effort controls.** GPT reasoning models now expose per-model
  effort controls in the TUI, and OpenAI chat-completions requests send
  `reasoning_effort` when a model-specific effort is configured.

## [0.15.0] - 2026-07-03

### Added

- **Gemini native tool calls.** The Google/Gemini provider now sends tool
  schemas as Gemini `functionDeclarations`, parses `functionCall` parts into
  neenee tool calls, streams function-call events, and replays tool results as
  `functionResponse` parts, while preserving Gemini thought signatures on
  function-call and text parts for stateless multi-turn replay. Gemini no
  longer relies solely on the JSON-in-text fallback for filesystem and shell
  tools.

- **Versioned Gemini relay base URLs.** The native Gemini transport now carries
  a configurable versioned base URL (default
  `https://generativelanguage.googleapis.com/v1beta`), so the built-in `google`
  provider and custom Gemini relays/中转站 share one code path — configured via
  `gemini_base_url`/`GEMINI_BASE_URL` or the custom-provider Base URL field.

- **GPT-5.5 / 5.4 / 5.4-mini registered** as frontier OpenAI models (1M and
  400K context windows); the GPT-4o family is annotated as legacy (kept
  registered so existing configs and older sessions still resolve metadata).

### Changed

- **Per-protocol SDK crates.** The monolithic `neenee-providers` vendor
  adapters are split into dedicated `neenee-ai-sdk-{core,openai,anthropic,
  google}` crates that own the transport/protocol layer (endpoint config, SSE
  reassembly, request/response shape), leaving `neenee-providers` as the thin
  registry. `/debug preview` now reflects the provider-wire body (via
  `Message::to_wire`) rather than the internal `Message` struct.

- **Anti-doomloop guard renamed** across the stack: `nudge` → `doom_guard`
  (`nudge.rs` → `doom_guard.rs`, `NudgeConfig` → `DoomGuardConfig`, with all
  call sites updated).

- **TUI transcript layouts renamed**: `compact` → `default`
  (`layout_default.rs`) and `turn_band` → `legacy` (`legacy.rs`); existing
  configs migrate automatically via `from_config()`.

- **Debug subcommands renamed**: `/debug context` → `/debug preview`,
  `/debug network` → `/debug trace`.

- **Shared modal UI primitives.** A single `modal_header`/
  `modal_header_parts` primitive now routes every centered overlay through one
  path, and a caret-following `field_viewport` lets long token/base-URL fields
  scroll within the modal. The Gemini add-model overlay now treats Gemini as a
  *closed* model set (no free-text fallback; an unmatched id is reported as a
  typo, and a transport 404 is clarified as "upstream does not serve this
  model").

### Removed

- **`GLM_GUIDANCE` constant dropped.** All known models now carry empty
  guidance; the read-loop nudge handles GLM deterministically instead of
  injecting per-model prompt text.

## [0.14.3] - 2026-07-02

### Fixed

- **cargo-deny license check passes.** The `v0.14.2` `deny` CI job failed on
  licenses: `BSL-1.0` (`clipboard-win`, `error-code`, via `arboard`) and
  `CDLA-Permissive-2.0` (`webpki-roots`/`webpki-root-certs`, from the rustls
  TLS switch) were not on the allow-list. Both are permissive and now allowed.

## [0.14.2] - 2026-07-02

### Fixed

- **Static and cross builds no longer fail on `openssl-sys`.** The release
  workflow's `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-gnu` targets
  failed with `Could not find directory of OpenSSL installation` because reqwest
  was configured with the `native-tls` feature, which links against system
  OpenSSL — unavailable under musl and absent in the aarch64 cross sysroot.
  Switched reqwest to `rustls` + `webpki-roots`: pure-Rust TLS with a
  compiled-in root certificate store, so the static and ARM64 binaries need no
  system OpenSSL.

### Changed

- **reqwest now uses rustls, matching the documented TLS policy.** `deny.toml`
  has always banned the `openssl` crate with the note "reqwest is configured
  with rustls in the workspace," but the workspace actually pulled
  `native-tls`/`openssl-sys` transitively — contradicting the ban. The
  dependency graph is now openssl-free (`cargo tree -i openssl-sys` returns no
  match), so the `deny.toml` ban is finally consistent with reality. This is
  purely a TLS-implementation swap for outbound HTTPS; no application behavior
  changes.

## [0.14.1] - 2026-07-02

### Fixed

- **CI no longer forces nightly + Cranelift on every job.** A committed
  `rust-toolchain.toml` (pinning `nightly` + `rustc-codegen-cranelift-preview`)
  and a `[profile.dev] codegen-backend = "cranelift"` in the root `Cargo.toml`
  were a local-dev speed-up that leaked into CI, breaking the entire pipeline:
  the `fmt` and `clippy` jobs installed `stable` + their components but the
  toolchain file overrode the active chain to a `nightly` that lacked
  `rustfmt`/`clippy`; the `coverage` job's `-Cinstrument-coverage` is LLVM-only
  and collided with Cranelift. Both files are removed — the Cranelift dev
  preference stays available locally via `~/.cargo/config.toml`
  (`[unstable] codegen-backend = true`). The codebase targets stable Rust (MSRV
  1.95) and contains no nightly-only features, so CI now builds on `stable` as
  every job already declared.

- **Broken rustdoc intra-doc link.** `[TranscriptMessage::round]` pointed at a
  field renamed to `turn` by the vocabulary swap; fixed to
  `[TranscriptMessage::turn]`.

### Changed

- Reformatted several files with the current stable `rustfmt` (import
  re-wrapping only; no logic change).

## [0.14.0] - 2026-07-02

### Added

- **Phase-1 interrupt now unsends the user message.** When the user interrupts
  a turn before any model output reaches the client (request in-flight, no
  response bytes, no tool execution), the turn is now reversible at the
  conversation layer: the user message is popped back out of the context and
  session store, the transcript entry is removed, and the prompt (with any
  pasted images) is restored into the input box for re-editing. Later interrupt
  phases (mid-stream drop, tool cancel) are unchanged.

- **`/debug preview` — dry-run the next *wire* request as a file dump.** A
  dev-only subcommand snapshots the provider-**wire** body the next turn would
  send (the minimal shape the provider serializes — `role`/`content`/`tool_calls`/
  `tool_call_id`/`images`/`reasoning_content` only), with a simulated `This is a
  test.` probe user message appended so the snapshot reflects "what if the user
  sent this now". Out-of-band fields (nested envoy `children`, `envoy_meta`,
  attribution, injection `origin`, `hidden`) are stripped via `Message::to_wire`,
  so the dump shows what the model actually sees — not the internal `Message`
  struct that also carries durable-session sidecars. The head system message is
  rebuilt, skills auto-load, and token/byte pressure is estimated on the wire
  set. The full JSON (messages + tool schemas + provider/model identity +
  context window + active pursuit) is persisted to one owner-only file under the
  per-project `debug/` dir for offline inspection. **No** provider call is made
  and nothing is mutated. Pairs with `/debug trace` (which captures real
  round-trips).

### Changed

- **Vocabulary swap: a *round* now contains *turns*.** The two execution
  layers are relabelled so that a **round** is the unit the user perceives
  (one submitted message → one final reply) and a **turn** is one iteration
  of the ReAct loop inside it — the inverse of the prior convention. This is
  a pure rename; no behavior, persistence format, or wire protocol changes.
  See [ADR-0047](docs/adr/0047-round-contains-turn-vocabulary.md).
  - **Breaking config rename.** `hard_stop_rounds` → `hard_stop_turns`,
    `review_start_round` → `review_start_turn`,
    `review_interval_rounds` → `review_interval_turns`, and the hook event
    value `"Round"` → `"Turn"`. Old keys are silently ignored by serde
    (falling back to defaults); rename them in your config to keep your
    values.
  - **Breaking API rename.** `TurnEvent` → `RoundEvent`,
    `AgentResponse::Turn` → `AgentResponse::Round`, `execute_turn` →
    `execute_round`, `TurnInput`/`TurnContext` → `RoundInput`/`RoundContext`,
    `append_round` → `append_turn`, `RoundStarted` → `TurnStarted`,
    `RoundBand`/`"round_band"` → `TurnBand`/`"turn_band"`.
  - The Activity-modal detail line flips from `turn N · round M · <model>`
    to `round N · turn M · <model>`.

## [0.13.2] - 2026-07-01

### Fixed

- **Wide characters no longer leave jagged edges on modal/panel borders.** When a
  rectangle fill, row clear, or string overwrite had to repaint a cell occupied
  by a wide glyph (emoji, CJK — display width 2), only one half of the cell was
  reconciled, leaving the other half as a stray width-0 continuation carrying a
  stale background — visible as ghosting along the edges of modals, bands, and
  panels. Every mutating path in `Grid` (`set`, `put`, `fill_rect`, `clear_row`)
  now detects and repairs the neighboring half-wide cell before writing, so
  borders stay clean regardless of the wide-character content underneath.

### Changed

- **The token bill modal is now a two-level report with a per-model drill-down.**
  What was a flat provider/model bill gained an interactive detail view: pressing
  `Enter` on a selected row opens that provider/model's detail, showing the
  reported-upstream vs. local-estimate source split, input/output totals,
  Anthropic prompt-cache read/write counts with a computed cache hit-rate, and a
  per-round line-item table (round number, source, input, output, total, cache
  r/w). `↑`/`↓` scroll the detail body; the first `Esc` steps back to the bill
  list, the second closes the modal. The token ledger now records a `TokenRound`
  per turn so the report can break usage down by round; the title switches from
  "Token Bill" to "Token Detail" and the footer hints update accordingly.

## [0.13.1] - 2026-06-30

### Fixed

- **Split SGR mouse reports no longer leak as stray keypresses.** crossterm
  occasionally returns a single mouse report split across two `read()` calls as
  a run of spurious `Char`/`Esc` events — worst on resize, fast trackpad
  scrolling, and inside multiplexers — which reached the composer as phantom
  keypresses. The reader thread now runs an `SgrLeakGuard` state machine behind
  a reassembly sink: when a freshly read event matches the `ESC [ < …` prefix of
  an SGR sequence, it keeps draining within a short deadline and drops the
  reassembled sequence at the source. A symbol-layer guard in the event loop
  remains as a backstop for any fragment that still escapes the window.

## [0.13.0] - 2026-06-30

### Fixed

- **Resume no longer shows a one-frame scroll jump.** Opening a session replaced
  the whole transcript while `scroll`/`max_scroll` still held the previous
  (short) transcript's measurement, so the first frame painted the tall new
  transcript pinned too high and the *next* frame snapped to the true bottom —
  a visible reflow rather than a single rendered final frame. When the
  transcript changes and bottom-follow is on, the loop now pins to the
  freshly-measured bottom and forces a back-to-back redraw within the same
  iteration (no input-poll wait), so the corrected frame replaces the stale one
  imperceptibly.

- **Sessions picker rows no longer spill out the left edge of the modal.** A
  session `overview` built from a multi-line first user message could carry
  embedded `\n`/`\r`, which the terminal paints as a carriage return — dumping
  the rest of the row at column 0 of the *screen*. Control characters are now
  collapsed to spaces both in the view layer (`overlays::common::one_line`)
  before truncation and at the source (`store::truncate_preview`), so the row
  stays inside its column budget.

### Changed

- **Session resume is now O(snapshot + tail), not O(whole-history).** The
  session snapshot (`<id>.json`) gained an `applied_seq` watermark recording the
  highest event-seq it has already folded. On load, a checksum-valid snapshot
  with a watermark is read as a fast path and only log events *after* the
  watermark are replayed — a clean close leaves an empty tail, so resume is a
  single JSON read; a crash mid-turn (`append_round`'s
  `MessagesAppended` not yet folded by the next `replace_messages`) leaves a
  tail of at most a few events. A corrupt/legacy/no-watermark snapshot, a
  checksum mismatch, or a divergent `Started` id all fall back to the
  authoritative full-replay path (and rewrite the snapshot so the next load is
  fast). Schema bumped to 5; old sessions load with `applied_seq = None` and
  backfill the watermark on first persist. `EventLog::append` now returns the
  reserved seq; `load_since(watermark)` and a metadata-stat `is_empty`/
  `high_seq` replace the O(n) per-mutation full-log re-reads.

- **Event logs self-compact.** Once an append-only log exceeds 1024 events at a
  full-snapshot persist point (every turn boundary), it is rewritten to a single
  seed derived from the current snapshot, so the replay tail stays bounded over
  a long-lived session. The only non-full persist (`append_round`'s mid-turn
  `Persist::None` arm) never reaches the compaction path, so no unabsorbed event
  is ever dropped.

- **Restoring a tool-heavy session is O(n), not O(n²).** `transcript_messages_from_core`
  paired each tool result with its originating step by rescanning the whole
  restored transcript; it now indexes still-open steps per tool name in a FIFO
  queue, pairing in O(1) while preserving the earliest-open-step semantics.

## [0.12.0] - 2026-06-30

### Fixed

- **IME composition window anchor drift.** The terminal cursor — which the host
  terminal's IME anchors its composition window to — was repositioned only as a
  per-frame side effect of drawing, so a keystroke's logical caret position and
  its physical terminal position disagreed for one frame, and the IME (which
  samples the cursor the instant the keystroke arrives) anchored to the stale
  coordinate. The caret position is now derived from a single pure function
  (`composer::cursor_screen_pos`) shared by both the draw path and a new
  input-driven *immediate flush* that syncs the backend cursor in the same
  iteration a keystroke is handled, before the next frame is rendered. All
  caret moves now route through `App::set_cursor` (the single sanctioned write
  site), which arms the flush; cursor show/hide is a real state transition
  rather than a per-frame guess, so the IME never samples a hide↔show edge at a
  coordinate that no longer matches the visible text. See
  [ADR-0038](docs/adr/0038-in-house-grid-diff-rendering-engine.md).

### Added

- **`neenee-quant` live HTTP broker mode.** Quant now defaults to paper trading
  but can be explicitly switched to `NEENEE_QUANT_BROKER=live-http`, sending
  account-mutating orders through an HTTPS broker gateway after fetching the
  live portfolio and applying local risk checks. Missing live broker URL/token
  fails startup instead of silently falling back, risk rejections do not call
  the gateway, and the GUI labels live accounts as pending refresh instead of
  paper. The `../optics` Rust bindings now auto-discover the checkout's
  `build/meson-uninstalled` pkg-config files so the quant GUI `gui` feature
  builds from the local optics tree without manual `PKG_CONFIG_PATH`.

### Changed

- **Extended thinking is opt-in and per-model (ADR-0046).** A model no longer
  reasons on its own: the default for every model is thinking **off**, and
  extended thinking is opted in per model from the stage-2 model `e` editor (the
  `[model_reasoning."<model-id>"]` table for built-in models, the channel's
  `effort`/`thinking` for a custom Anthropic relay). An entry's presence opts a
  model in — thinking defaults on at the chosen effort unless explicitly set
  off. The effort/thinking controls have been removed from the provider level:
  the stage-1 provider key editor and the custom-provider create/edit form no
  longer show them, and `SwitchProvider`/`AddProvider`/`EditProvider` no longer
  carry them. The model list now shows a model's effort only when it is actually
  opted in (`◆ think on · <effort>`); unconfigured models show nothing. The
  legacy flat `anthropic_effort`/`anthropic_thinking` config keys are deprecated
  (still load, no longer read) — migrate them into a `[model_reasoning]` entry.

### Removed

- **Phase 1 session-layout migration code.** The one-shot layout migrations
  (`migrate_legacy_active_to_sessions`, `migrate_flat_sessions_to_project_buckets`)
  that moved pre-ADR-0018 flat `data_dir/sessions/*.json` archives and per-project
  root `session.json`/`events.jsonl` files into the per-session
  `sessions/<id>.{json,jsonl}` layout have been removed, along with the now-unused
  `Dirs::legacy_sessions_dir` accessor. Every session has been written to the
  ADR-0018 layout since the transition completed, so the migrations are no-ops on
  any current install; any user still holding the obsolete Phase 1 layout should
  upgrade through an earlier release first. Field-level schema migration
  (`migrate_session_data`) is unchanged and remains the load path for older
  snapshots.

## [0.11.0] - 2026-06-28

### Added

- **Bash stdin execution contract: non-interactive by construction (ADR-0043).**
  The `bash` tool now provisions a child's stdin explicitly via a first-class
  `StdinPolicy` parameter on `Tool::call_structured_with_events`, decided before
  spawn. The default (`Closed` → `/dev/null`) is a **hard floor**: an
  interactive command (`gpg`/`sudo`/`passwd`/editors/pagers) gets instant EOF
  and fails fast with a real exit code instead of hanging silently for 30s. An
  **idle watchdog** (no output for ~10s → `IdleBlocked`) and a wall-clock
  ceiling (`Timeout`) replace the single coarse timeout, and an advisory
  `is_interactive_command()` classifier speeds failure and sharpens the error.
  A themed termination footer (`ShellTermination`: Exited/IdleBlocked/
  InteractiveBlocked/Timeout/Cancelled) explains *why* a command ended. The one
  legitimate interactive case (sudo/gpg passwords) has a human-input escape
  hatch: the classifier pauses the turn, surfaces an inline `Modal::
  InputInjection` panel (masked for secrets), and pipes the operator's reply in
  — mirroring `ask_user`'s oneshot round-trip, with no PTY. An opt-in
  `[principal] allow_model_stdin` (default `false`) lets an unattended flow let
  the model supply `stdin` directly; otherwise stdin is structurally unreachable
  from the model's arguments. `ToolOutput::Shell` gains a `termination` field
  (`#[serde(default)]`, back-compatible). See ADR-0043.

- **Block-level surfaces unified on one design contract.** The `edit`/`write`
  diff's four banding colors are now first-class theme tokens
  (`diff_add_bg`/`del_bg`/`add_hl`/`del_hl`) instead of inline `Color::Rgb`
  literals, and every tool-step code/text block (read/listing/grep/bash/diff)
  resolves its surface through `code_surface()` and shares `CODE_BAND_*`
  geometry tokens with the markdown code block. The tool-step code block now
  renders a language tag (matching markdown), so a code block reads identically
  in prose and inside a tool step.

- **Tools are a pool; the agent and the model each select from it.** The toolset
  is now resolved through a single entry point, `ToolSet::resolve_for(model,
  agent_selection, model_selection)`, that composes two independent selectors
  over two orthogonal axes: **scope** (which capabilities) by *intersection* —
  a capability survives only if both the agent's identity (Principal scope or
  Envoy role) and the model admit it — and **override** (which variant) by
  *precedence*, where an agent-side override beats a model-side override. A
  model's hard capability limits live on the scope/pool axis, not the override
  axis: a variant a model cannot execute (e.g. a `requires_vision` tool like
  `read_image` on a text-only model) is simply absent from the resolved pool, so
  no agent override can reinstate it. New `Tool::requires_vision()` capability
  axis (mirrors `requires_user`); new `ToolScope` / `ToolSelection` core types;
  the Principal now carries a first-class `agent_selection`, re-composed with the
  live model on every model switch.

### Changed

- **MSRV is now Rust 1.95.** The locked dependency graph now includes crates
  that require newer compiler features, so crate manifests and CI's MSRV job
  were raised in lockstep.

- **BREAKING — agent vocabulary is now Principal / Envoy.** "Agent" is kept only
  as the umbrella term for the execution engine (the `Agent` struct, the
  `neenee-agent` crate, and the `AgentRequest`/`AgentResponse`/`AgentEvent`/
  `AgentOp` protocol). The two concrete roles are renamed: the top-level,
  human-facing agent is the **Principal**, and the isolated child it spawns to
  research a sub-question is an **Envoy**. Two breaking surfaces: the
  `[agent]` config table is now `[principal]` (move `hard_stop_rounds` /
  `loop_review_enabled` under it — an `[agent]` table is silently ignored), and
  the `subagent` tool is renamed to `envoy`. No compatibility aliases. See
  [ADR-0042](docs/adr/0042-principal-envoy-role-vocabulary.md).

- **Question modal single-select is now live — no marker, no Space step.**
  Single-select questions drop the `●`/`○` radio dots entirely: the highlighted
  row *is* the selection, so moving with `↑`/`↓` (or jumping with a digit key)
  immediately commits the choice and `Enter` submits exactly the highlighted
  option — there is no longer a separate "navigate, then Space to confirm, then
  Enter" sequence. Multi-select is unchanged (still `[x]`/`[ ]` checkboxes with
  a `Space` toggle); the footer's `Space select` hint is now shown only for
  multi-select, since it is a harmless no-op for single-select.

## [0.10.1] - 2026-06-28

### Changed

- **All crates:** applied `cargo clippy --fix` across the workspace — 74 files
  cleaned up with no functional changes.

## [0.10.0] - 2026-06-28

### Changed

- **Session context vocabulary tightened around model-context projection.** Session
  snapshots and event logs now write `model_window`, `archived_transcript`,
  `last_projection`, and `context_projection_committed` instead of the older
  `messages` / `archived_messages` / `last_relief` / `context_relief_committed`
  vocabulary. Prune and compact commits now record an explicit projection
  operation (`prune`, `compact`, or legacy `unknown`), and `/session status`
  reports the last context projection. Older session files still load through
  serde aliases.

- **Shell output interleaves stdout/stderr by arrival order.** `bash` no longer
  renders all of stdout followed by all of stderr — both pipes now merge into a
  single arrival-ordered line stream, so the expanded view and detail overlay show
  warnings and progress (which hit stderr) interleaved with results (stdout) exactly
  as the process wrote them. This fixes the reorder symptom for tools like
  `cargo`/`git`/`npm`, whose diagnostics were pushed below their results. Each line
  keeps its source tag so stderr still colours distinctly in `error_fg`. (Legacy /
  restored sessions and the pre-final streaming seed fall back to the old
  all-stdout-then-all-stderr bands.) `stderr` is now streamed live alongside stdout
  rather than accumulated silently.

### Fixed

- **Carriage-return / control-byte corruption of shell output.** A `\r`-refreshed
  progress bar or spinner (which lands as one line with embedded `\r`s under
  line-buffered capture) is now normalized to its final frame: `normalize_
  carriage_returns` resolves `\r` as caret-return-overwrite and `\b` as a
  column-step-back (CI-log-viewer semantics), drops stray control bytes
  (BEL/FF/VT), and preserves `\t`. The renderer no longer collapses multi-`\r`
  lines to only their last segment. Applied once at capture, shared with the
  renderer.

- **Streaming shell view lost stderr colour and interleaving.** The live
  streaming seed now builds real `ShellLine` records (each tagged with its
  source stream), so the streaming view matches the final result: stderr stays
  red-tinted and stdout/stderr keep their true arrival interleaving, instead of
  the all-stdout-then-all-stderr degraded band.

- **ANSI escape sequences in shell output.** Colour codes (`\x1b[...]m`, cursor
  moves, OSC sequences) emitted even under a non-tty (`--color=always`,
  `CLICOLOR_FORCE`) are stripped at capture time, so they no longer corrupt the TUI's
  width math or render as literal `[0;32m` glyphs.

- **Carriage returns (`\r`) in shell lines.** Progress bars and spinners that refresh
  a line with `\r` now show the surviving text (after the last `\r`) instead of
  drawing the raw return and overlapping the two halves.

## [0.9.1] - 2026-06-27

### Added

- **Structured `AgentNotice` turn events.** A typed notice (kind/severity/surface/source)
  is now emitted as `TurnEvent::Notice` and `SubagentEvent::Notice`, replacing ad-hoc
  banners for provider retries and session-review alerts.

- **One-line installer (`install.sh`).** A `curl | bash` installer detects the host
  platform, resolves the latest GitHub Release, and drops the prebuilt `neenee-code`
  binary into `~/.local/bin` (or `$INSTALL_DIR`).

### Changed

- **Provider→model picker flattened.** The two-stage provider→model picker is now a
  single flat list of every `(provider, model)` pair; multi-model providers fan out into
  one ranked entry each. The picker mirrors the input-history modal's two-mode design —
  browse mode (favorites→last-used→name) and a `/` fuzzy-search sub-layer borrowing the
  composer line.

- **Disclosure/permission rendering reworked.** The render `step` module is renamed to
  `disclosure`, with summary colors modeled as three orthogonal axes (lifecycle,
  disclosure, interaction) under a disclosure-first monotonic weight model that fixes
  focused/expanded steps reading as too dim. The permission overlay becomes a modal sheet
  with Allow/Always/Reject/Details actions, queued-request handoff, and turn-aborting
  rejection of the remaining batch.

- **Spinner timed on wall-clock.** The per-frame `spinner_tick` counter is replaced with a
  wall-clock `spinner_epoch`, so the breathing-indicator cadence stays constant regardless
  of redraw frequency.

### Fixed

- **Tagged release builds failed.** The release workflow still built `--bin neenee` after
  the binary was renamed to `neenee-code`, so every tagged build since v0.6.1 failed with
  `no bin target named 'neenee'`. It now builds and packages `neenee-code`.

## [0.9.0] - 2026-06-27

### Added

- **Tools manager overlay (`/tools`).** A new slash command opens a focused modal listing
  every session tool — builtins, `mcp:<server>`, `pursuit`, and `plan` — each with a
  `Space` toggle to enable/disable it. The tool list is pulled out of the session dashboard
  so that overview stays a glanceable, read-only summary (a one-line `enabled/total` count
  plus a `t → /tools` hint) while per-tool control lives in its own surface.

### Changed

- **`auto_approve` renamed to `unattended`.** The no-prompt permission flag is renamed
  across the agent, permission store, events, server, and docs. The hint bar no longer
  renders a separate `AUTO-APPROVE` pill — the shell-mode pill now conveys the active state
  on its own.

- **History modal (Ctrl+R) redesigned.** The modal now opens in a reverse-chronological
  browse mode by default and drops into a fuzzy-search sub-layer on `/` (borrowing the
  composer line as a live query). Rows are recomputed each frame from a single source of
  truth (`App::history_rows`), and the input draft is stashed and restored on open/close.
  Read-only overlays like this are now click-outside-to-dismissable, while entry modals
  holding in-progress input stay put.

- **Step summary colour reworked to a three-tone, hover-priority model.** `summary_weight`
  now maps hover/focus to the hover tone, an expanded (open) body to the primary foreground,
  and a collapsed idle step to muted. Expanded and collapsed are mutually exclusive peers
  decided only when idle, so closing a step darkens it immediately instead of staying bright
  under a stale focus override. Accent blend factors were updated to mirror the new weight
  ladder.

### Fixed

- **Long footer hints spilled past modal panels.** Non-wrapped `Paragraph` spans are now
  clipped to the panel rect (`clip_to_cols`), so long footer hints can no longer overflow
  past modal panels into the backdrop. Clipping is grapheme-aware.

- **Mouse wheel leaked through the question modal.** When a question modal is open, the
  wheel now drives option selection (`QuestionUp`/`QuestionDown`) instead of scrolling the
  transcript behind the modal.

### Removed

- **Removed the `progress_update` tool and the `/config` modal.** The model-facing
  `progress_update` tool (and its `[agent.progress_updates]` config table), the
  `AgentEvent`/`TurnEvent::ProgressUpdate` events, the `ConfigSnapshot` request/response
  pair, and the now-empty configuration modal (`/config`, `Modal::Config`) are gone. The
  activity bar now shows only the harness liveness status; the glanceable model-authored
  status line is no longer surfaced. (These were added in the same cycle, so nothing is
  dropped from a prior release.)

## [0.8.0] - 2026-06-27

### Fixed

- **`/review` reviewer prompt reached the model clobbered.** The session-review
  diagnostic subagent pre-seeded its system message (role persona + the
  dimensions to evaluate + the JSON verdict contract) and then ran the streaming
  turn loop, whose per-round `ensure_system_prompt` replaces any leading system
  message — so on round 1 the review prompt was overwritten by the default
  neutral set and never reached the model. The feature limped along only because
  verdict parsing degrades gracefully. The reviewer now carries a dedicated
  prompt registry (`review.persona` + `review.dimensions` + `review.json_contract`)
  installed via `Agent::set_prompt_registry`, and its transcript opens at the
  user message so the composed review prompt is rebuilt correctly every round.
  See [ADR-0039](docs/adr/0039-unified-prompt-registry.md).

## [0.7.1] - 2026-06-27

### Fixed

- **Multi-segment table cell drag selection.** `TableCell` click targets now carry
  `cell_segments` (per-line render/source mappings) instead of a single `(lo, hi)`
  byte range, enabling substring selection across wrapped/padded table cell display
  lines. Previously, any overflow or padding in a cell broke drag-to-select by
  referencing the wrong byte offsets outside the grid line.

## [0.7.0] - 2026-06-26

### Added

- **Session/server layer: `neenee-server`.** A new crate peering
  `neenee-code` at the application layer enables a long-running daemon holding
  multiple concurrent agent sessions that several clients can subscribe to.
  `SharedState` / `SessionRegistry` / `SessionHandle` replace the single-process
  `mpsc` pair with a broadcast fan-out, so a browser frontend can hot-attach to
  a running session over WebSocket (`serve` mode). This unblocks a future web
  frontend while the TUI keeps working unchanged. See
  [ADR-0037](docs/adr/0037-server-layer.md).

- **In-house TUI grid + diff rendering engine (`neenee-tui`).** ratatui is
  removed from the workspace and replaced with a vim-style retained cell grid
  with write-marks-dirty tracking. The diff now walks only cells that changed
  (idle frames emit nothing), wide-glyph trailing columns are owned at write
  time, and `bce` (back-color-erase) support makes clearing a line tail a single
  `\x1b[K`. The widget layer is fully migrated off ratatui. See
  [ADR-0038](docs/adr/0038-in-house-grid-diff-rendering-engine.md).

- **`neenee-quant` application crate** — the quantitative-trading application, a
  peer of `neenee-code` at the application layer. Depends on `neenee-agent` and
  layers on quant domain tools: `market_data`, `backtest`, `place_order`, and
  `list_positions`. The quant tools deliberately do not self-register, so a
  coding agent can never link `place_order` and a quant agent can never link
  `write_file` — domain isolation is enforced at assembly time. See
  [ADR-0035](docs/adr/0035-application-layer-split.md).

- **`QUANT` subagent profile** — a bounded subagent profile in `neenee-core`
  admitting read-only quant tools plus shared read-only inspection, while
  excluding live trading and all coding write/edit/exec tools.

- **Inline code rendering.** Inline-code spans (`` `read_file` ``) in assistant
  prose, headings, list items, blockquotes, and reasoning traces are now styled
  on the same surface as fenced code blocks (`` `code_fg` `` on `` `code_bg` ``)
  instead of being flattened into the surrounding text with bare backticks. The
  markdown parser records each span's byte range on the prose block and the
  renderer paints the run (delimiters included) as a distinct chip, so the span
  reads as inline code at a glance. Copy and semantic selection are unchanged:
  the flattened text still carries the original backticks and the byte-addressable
  model is untouched, so selecting and copying an inline-code span yields the
  exact source.

### Changed

- **Renamed the coding application: crate `neenee-cli` → `neenee-code`, binary
  `neenee` → `neenee-code`.** The workspace now has two domain applications
  (coding and quant), so neither carries the bare name. Every existing `neenee`
  invocation is now `neenee-code`. See [ADR-0035](docs/adr/0035-application-layer-split.md).

### Fixed

- **CJK wide-character "ghost" cells.** Scrolling a transcript containing CJK
  (double-width) text under foot + tmux no longer leaves stray gray blocks next
  to the glyphs at the wrap column. The in-house grid engine owns each wide
  glyph's trailing column at write time, so the background stays fresh through
  scroll and partial redraws. (Originally patched by the third-buffer
  `WideHealBackend` wrapper of ADR-0036, now superseded by ADR-0038.)

## [0.6.1] - 2026-06-26

### Fixed

- **Non-streaming chat lost assistant content.** The OpenAI-compatible
  provider's `chat()` path fed the response through the tool-call echo filter
  but discarded `feed`'s return value, keeping only `finish()`'s output. Since
  `feed` emits ordinary prose as it classifies, every plain assistant response
  came back empty in the non-streaming path — silently breaking title
  generation, session summarization, and the non-streaming agent fallback. The
  streaming path was already correct. It now accumulates `feed`'s emission
  before resolving the held remainder with `finish`.

### Added

- **Provider wire-level integration tests.** A new `tests/wire.rs` in
  `neenee-providers` stands up a mock HTTP server (mockito) and drives the full
  request → HTTP → SSE-byte-reassembly → event-parse path for both the
  OpenAI-compatible and Anthropic `/messages` providers — covering header
  attachment, 5xx retry classification, keyless auth, text/reasoning/tool-call
  stream parsing, echo suppression, tool_use argument fragmenting, and in-band
  stream errors. providers previously had zero async tests; the first one
  caught the regression above.

- **Coverage reporting CI.** A `coverage` job runs `cargo-llvm-cov` to produce
  an lcov report (uploaded as a workflow artifact) plus a textual summary. It
  builds without `-D warnings` and never gates merges.

- **Tag-driven release workflow.** `release.yml` builds release binaries for
  x86_64/aarch64 (linux gnu + musl) and macOS (both arches) on `v*` tags and
  publishes a GitHub release with auto-generated notes.

### Changed

- The `flock`-exclusion test in `neenee-store` is now `#[cfg(unix)]`-gated so
  the suite is Windows-ready, and the `path_scan` cache access in the TUI uses
  `get_or_insert_with` instead of a check-then-`unwrap`.

## [0.6.0] - 2026-06-26

### Added

- **Deterministic read-loop guard + range-aware prune staleness (ADR-0034).**
  A model that re-issues the same `read_file` (one page, or thrashing between
  two pages) without progress no longer spins unchecked. A per-turn guard keeps
  a sliding window of recent read-round signatures and, when one recurs past a
  threshold, injects a hidden anti-anchoring nudge naming the repeated read and
  demanding a different action. Detection is pure signature bookkeeping (no
  inference, no false positives on genuine paging) and the nudge is
  **non-terminating** — `Esc`, `hard_stop_rounds`, and `abort` stay the hard
  backstops. Gated by `[agent] loop_review_enabled` (default on; off for
  sub-agents and `/review`). Accompanying it, prune staleness is now
  range-aware: an earlier read is stale only when a *later* same-file result
  supersedes it — a mutation, or a read that *fully covers* its line range — so
  paging through different regions of one large file no longer self-evicts.

- **Anthropic-compatible `/messages` provider + OpenCode Go relay.** A new
  `Transport::Anthropic` (the `anthropic_compat` provider) speaks the Anthropic
  Messages surface, used by opencode-go's MiniMax/Qwen models and any
  Anthropic-format relay. Per-model `max_tokens` is capped at the model's
  registered output limit (MiniMax M3: 131072) so long agent turns from
  high-output models run untruncated. One `OPENCODE_API_KEY` authenticates a
  single provider hosting many models; `default_model` selects the active model
  id within a multi-model provider.

- **MCP auto-reconnect.** MCP server connections are now wrapped in a
  reconnect-capable `McpServer` handle: a crashed server (stdout closed
  mid-session) is transparently restarted, tool calls retry once on connection
  failure, and a background refresh loop re-discovers tools — no more manual
  restart after a transient MCP server crash.

- **models.dev catalog cache + dynamic model registry.** The model catalog is
  now backed by a cached mirror of models.dev
  (`$XDG_CACHE_HOME/neenee/models-dev.json`), refreshed at startup and every 60
  minutes, with the compiled-in registry as fallback so a missing cache never
  blocks startup.

- **Declarative `[permissions]` rules.** Default "always allow" policies can now
  be pre-declared in `config.toml` (`[[permissions.allow]]` with `tool` +
  `scope`), seeding the allowlist at startup so common tools (e.g. `bash`,
  `read_file`) don't prompt on every fresh install. Runtime "Always" decisions
  still persist to `permissions.json`; config rules are re-applied on every
  start.

- TUI component showcase for rendering/testing individual modals in isolation;
  `question_model` picker; `/mcp-catalog` command.

### Changed

- **`[agent] loop_review_enabled` repurposed.** Previously a deprecated no-op
  (the ADR-0030 semantic review it gated was removed); it now toggles the new
  deterministic read-loop guard's anti-anchoring nudge.

### Removed

- **`LlamaServerProvider`.** The dedicated local provider module is gone:
  `llama-server --jinja` speaks the full OpenAI chat-completions surface
  (native tool calls + streaming tool-call deltas), so local servers are now
  reached through the same `OpenAiCompatProvider` as any cloud endpoint.
  Keyless `Transport::Llama` channels suppress the `Authorization` header
  entirely (an empty bearer token could be rejected by some servers).

## [0.5.0] - 2026-06-25

### Changed

- **Migrated to Rust 2024 edition.** MSRV lowered from 1.88 to 1.85. The 2024
  edition makes `std::env::set_var`/`remove_var` `unsafe`; all test call sites
  are now wrapped in `unsafe` blocks. `resolver = "3"` (MSRV-aware dependency
  resolution) is now implied by the edition.
- **Major dependency upgrades** to the latest ecosystem:
  - `ratatui` 0.26 → **0.30** and `crossterm` 0.27 → **0.29** (API migration:
    `Frame::size()` → `area()`, `set_cursor` → `set_cursor_position`,
    `Buffer::get` → index syntax, `Rect::inner(&Margin)` → `Rect::inner(Margin)`,
    `Backend::Error` is now generic).
  - `reqwest` 0.12 → **0.13** (`query`/`form` are now opt-in features; default
    TLS backend switched to rustls).
  - `rusqlite` 0.32 → **0.40**, `toml` 0.8 → **1**, `pulldown-cmark` 0.10 →
    **0.13** (`Tag::BlockQuote` now carries `Option<BlockQuoteKind>`).
  - `arboard` 3.4 → **3.6**, `dirs`/`directories` 5 → **6**, `insta` → **1.48**.

### Security

- **Replaced the archived `serde_yaml` 0.9 with `yaml_serde` 0.10** (the
  YAML organization's maintained fork), resolving the `RUSTSEC-2024-0320`
  unmaintained-advisory that failed the `security audit` CI job. Applied via
  Cargo package rename so all `use serde_yaml::` imports are unchanged.

### Fixed

- Fixed two CI compile failures under `-D warnings`: an unused `lines` binding
  in `neenee-tools` tests and an un-gated `read_command_output` in
  `neenee-cli` that became dead code on macOS (the function's only callers are
  `#[cfg(target_os = "linux")]`).
- Updated the `create_project` rust scaffold template to emit `edition = "2024"`.

## [0.4.0] - 2026-06-25

### Added

- **`abort` tool + `Tool::affects_control_flow` axis — the model's
  self-initiated emergency escape hatch.** A new `abort` tool lets the model
  stop the program when it detects a stuck state it cannot recover from: a
  tool loop (repeating the same call with identical arguments), a dangerous or
  irreversible operation, or a dead end. Calling it cancels the in-flight turn
  (the same path as `Esc` / `Ctrl+C`) and then triggers a **graceful exit** —
  the session is saved and `SessionEnd` hooks fire before the process and its
  background tasks end. No hard `process::exit`, so nothing is lost.

  This fills the gap left by the removed loop guards (the ADR-0009 equality
  guard and the ADR-0030 loop-review nudge were both deleted), giving the model
  an *active* way out instead of spinning until the user notices. It is gated
  by a new **orthogonal capability axis**, `Tool::affects_control_flow`, not by
  the filesystem-damage ladder (`ToolAccess`): process control is a separate
  concern from filesystem mutation, so the permission broker is bypassed (an
  escape hatch that waits for approval is useless) and **sub-agents are
  excluded from it unconditionally** — a spawned agent must never be able to
  tear down the whole program. `affects_control_flow` joins `requires_user`
  and `spawns_subagent` as the third non-filesystem capability axis; the
  `abort` tool is its first consumer.

- **`read_image` tool + `ToolOutput::Image` — the model can now see images.**
  A new `read_image` tool reads an image file (PNG, JPEG, GIF, WebP), resizes
  it to a sensible resolution, and returns it as a structured
  `ToolOutput::Image`. Because OpenAI Chat Completions tool messages only
  accept string content, the harness peels the image out of the tool result
  and injects it into a follow-up user-role message (the same channel paste-up
  uses) — mirroring how opencode lowers images for OpenAI-Chat providers. This
  works across kimi / GLM / OpenAI / Gemini; the design was cross-checked
  against codex's `view_image` and opencode's `read`. `read_file`'s
  description was also tightened to make its text-only scope unambiguous.

- **In-loop loop detection steers a stuck turn before the hard abort
  (ADR-0030).** A model that repeats the same or near-identical read-only
  actions (micro-adjusted `read_file` ranges, `grep` argument tweaks) no longer
  runs unchecked until the equality guard's hard abort — its arguments never
  compare equal, so it bypassed the guard entirely. The harness now fires the
  semantic review (`/review`'s `LoopingReview`) once per turn on a read-only
  round streak or a repeated-call count, and on a `Stuck` verdict injects an
  **anti-anchoring nudge** that names the loop, forbids re-reading, and demands
  a forward action — non-terminating, so the user keeps `Esc` and the opt-in
  `hard_stop_rounds` as the backstop. The new `steering` module is the one home
  for built-in nudges.
- **A constrained `Round` lifecycle hook (ADR-0030, partially supersedes
  ADR-0025).** A new `event = "Round"` hook fires once per tool round, carrying
  the read-only-round streak. Unlike other events it is **`Deny`-forbidden** —
  a round-count hook may inject context or observe, but cannot become a de-facto
  round cap (the ADR-0009 concern). The harness declares no built-in threshold
  on this axis; it only provides the trigger point users opt into.
- New `[agent] loop_review_enabled` config key (default `true`) toggles the
  in-loop review. Always off on sub-agents (no `/review` path, no recursion).

### Changed

- **Modals no longer erase the background.** Opening a centered modal used to
  fully occlude the transcript, input, hint bar, and activity bar. Every modal
  except the sessions picker now **dims** the live surface in place instead —
  the background stays visible for context while the modal reads as the focal
  layer. The dim brightness is tunable via the new `modal_dim_factor` theme
  field (default 0.5). The sessions picker keeps its full-takeover behavior
  (footer collapse + solid occlusion) since switching sessions is a context
  switch. This is driven by a single new `Modal::recess` policy
  (`None` / `Dim` / `Takeover`) that the footer-collapse flag and the
  per-frame paint both consult, replacing the old opaque `draw_dim_backdrop`
  fill.

### Removed

- **The in-loop loop guards (ADR-0009 equality guard + ADR-0030 loop-review
  nudge) were removed.** Both could reinforce the very read-loops they
  targeted, and the equality guard was trivially bypassed by micro-adjusted
  arguments. This leaves the harness with no automatic intervention against a
  model that repeats identical tool calls — the new `abort` tool (see Added)
  restores an escape hatch, but as a **model-initiated** action rather than a
  harness-enforced hard stop. `Agent::set_loop_review_enabled` is now a no-op
  stub, and `[agent] loop_review_enabled` is accepted but ignored. The opt-in
  `hard_stop_rounds` total-round cap and user `Esc` remain as backstops. (The
  ADR-0030 entries above are retained for history but describe features that no
  longer ship.)

## [0.3.0] - 2026-06-25

> Note: the v0.3.0 tag was cut but its crate-version bump and CHANGELOG entry
> were never landed — crates stayed at `0.2.0` at that tag. This section is
> backfilled at `0.4.0` release time so the history is honest; the crates jump
> straight `0.2.0 → 0.4.0`.

### Added

- **Lifecycle event hooks** — `pre_tool` / `post_tool` / `turn` / `session`
  hooks fire at well-defined points in the agent loop, letting user scripts
  observe or veto behavior. See ADR-0025.
- **SQLite-backed session migrations** — pragmatic, append-only schema
  migrations replace ad-hoc storage evolution. See ADR-0024.
- **Session-tagged turn events (ADR-0017).** Every turn event now flows under
  `AgentResponse::Turn { session_id, event }`, letting a `/btw` side
  conversation stream alongside the primary transcript over one channel.
- **AI session titles (ADR-0022).** A `TITLE` subagent profile generates a
  title on first turn; `/title` regenerates on demand and titles are
  lockable. Empty transcripts fall back to the first message.
- **Relevance-aware, tiered context pruning (ADR-0021 / ADR-0023).** Pre-turn
  pruning is now gated (not every turn), implicit (no `Compacted` notice), and
  selects by relevance (staleness / dedup / forward keep-alive) with tiered
  degradation (truncate → clear) and informative placeholders.
- **Pursuits store, repeat scheduler, `tool_output` and catalog refinements.**

### Changed

- **Agent-design docs restructured:** consolidated subagents documentation, new
  hooks page, context-pruning / context-compaction explanation pages.
- **Model channel abstraction documented (ADR-0002)** and picker recency
  ordering.
- **TUI:** `read_file` offset rendering, snapshot test, theme/layout updates.

## [0.2.0] - 2026-06-24

### Removed

- **The per-plan progress tracker is consolidated into the unified task list
  (ADR-0020, supersedes ADR-0007).** `update_plan_progress`, the
  `PlanProgress` / `PlanSection` / `PlanSectionStatus` types, the
  `PlanProgressUpdated` events, and the persisted `plan_progress` session field
  are removed — they duplicated the `todo` / `todo_update` task list, which is
  now the single source of truth. `plan_exit` now seeds one `TodoList` from the
  approved plan's `##` headings; `plan_enter` clears it; the
  model tracks steps with `todo` / `todo_update`. One list, one panel, one
  persisted field. Old sessions load with graceful degradation: the dropped
  field triggers at most a checksum warning, and stale `plan_progress_set`
  event-log lines are skipped, so previously persisted progress is simply not
  restored.

### Changed

- **Context compaction is now relative to the active model's context window
  (ADR-0019).** Compaction previously triggered on a single fixed character
  budget (`compaction_max_chars`, default ~30k tokens) regardless of model —
  so a 1M-token model was over-compacted at ~3% of its window and a 128k model
  was merely coincidental. Thresholds are now derived from the live model's
  context window (token-denominated): cheap tool-result pruning at 65%, a full
  summarizing compaction at 85%, compressed toward a 25% target, with a 32k
  fallback window for unknown/local models. The mid-turn prune threshold is
  re-seeded on every `/provider` switch so relief tracks the current model
  instead of the one active at startup. Pressure is estimated in tokens to
  match the window's unit; provider-reported `prompt_tokens` is a future
  enhancement that slots in without changing the threshold shape. See the
  [Configuration Reference](docs/reference/configuration.md#compaction).
  - Config: `compaction_max_chars` and `compaction_prune_protect_chars` are
    removed; a `[compaction]` table (`utilization`, `target_utilization`,
    `prune_utilization`, `fallback_window_tokens`) and
    `compaction_prune_protect_tokens` (default 6_000) replace them. Existing
    `config.toml` files keep parsing (the removed keys are ignored).

- **The base system prompt now directs the agent to be concise and direct.**
  `build_system_prompt` previously stated only the agent's identity and current
  mode; it now also sets explicit output norms — answer with the minimum
  needed, skip preamble and recaps, no unsolicited explanations or code
  comments, never commit unless asked, take the reasonable action with ordinary
  tools instead of asking permission, prefer dedicated file tools over shell
  pipelines, and verify with the project's build/tests/linter when correctness
  is implied. This brings neenee's default conversational behavior in line with
  the conciseness baseline that other coding agents (codex, opencode,
  claude-code) ship in their base prompts. No mechanism change; only the
  assembled system message wording.

- **Session review replaces the round-counting stall detector (ADR-0016).**
  The read-only "stall detector" (a reflection nudge at 8 read-only rounds and
  a hard abort at 14) is removed — it was an arbitrary cap ADR-0009 had
  rejected, and "no write fired" is a poor proxy for "stuck" (it mis-flagged
  legitimate exploration, especially read-only research sub-agents). In its
  place, after `review_start_round` (default 64) tool rounds and every
  `review_interval_rounds` (default 16) thereafter, the harness spawns a
  bounded read-only diagnostic sub-agent that reads the live transcript and
  returns a verdict per pluggable review dimension (`LoopingReview` first).
  Review surfaces an alert (and a one-shot reflection nudge on a `Stuck`
  verdict) but **never aborts the turn**; the only execution cap is an opt-in
  `hard_stop_rounds` (default 0 = off). Sub-agents run with review disabled.
  - Config: `[agent] stall_threshold` → `[agent.review]` (`review_start_round`,
    `review_interval_rounds`, `hard_stop_rounds`).
  - Slash command: `/stall-threshold` → `/review` (`/review off`,
    `/review N [M]`, `/review default`).
  - Events: `StallWarning` → `SessionReview { alert }`.

## [0.1.0] - 2026-06-24

### Added

- **`/pursue <condition>`** — a Claude-Code-style **stop-gate**. Setting a
  pursuit persists the condition and drives a single agent turn that refuses
  to end until the model signals completion (`[NEENEE_PURSUIT_COMPLETE]`), a
  50-round safety cap is hit, or you interrupt (`/pursue stop` / `Esc`). The
  gate re-injects the condition on each stop attempt, so the pursuit is
  within-turn continuation. Subcommands: `/pursue` (re-arm), `status`, `edit`,
  `done`, `stop`, `clear`.
- **`/repeat <cron> <prompt>`** — a durable **cron scheduler**. A real
  five-field cron expression engine fires the prompt as a normal turn on a
  clock. Jobs persist in `repeat.db` (survive restarts), auto-expire after 30
  days, and the first run fires immediately. `/repeat list`, `/repeat cancel
  <id>`, `/repeat help`.

### Removed

- **`/goal` and `/loop`.** Replaced by `/pursue` (condition-driven stop-gate)
  and `/repeat` (clock-driven cron). `/loop resume` has no equivalent — a
  pursuit is a single turn. Migrate: `/goal <x>` + `/loop` → `/pursue <x>`.
- **The goal checklist primitive** (`goal_checklist` tool, checklist gating,
  completion-defer). Completion is now a single boolean driven by the
  completion marker.
- **Legacy pre-XDG skill and command paths.** neenee no longer scans
  `~/.neenee/skills/` or `~/.neenee/commands/`. Move their contents to the
  XDG locations to keep them loaded:
  ```bash
  mv ~/.neenee/skills/*   $XDG_DATA_HOME/neenee/skills/   2>/dev/null || true
  mv ~/.neenee/commands/* $XDG_DATA_HOME/neenee/commands/ 2>/dev/null || true
  rmdir ~/.neenee/skills ~/.neenee/commands ~/.neenee     2>/dev/null || true
  ```
- **`~/.kimi-code/skills/` external skill directory.** Only `~/.agents/skills/`
  and `~/.claude/skills/` are read as external application conventions now
  (both user-global and project-local). Move any kimi-code skills into one of
  the remaining external directories or the neenee XDG skill directory.

### Fixed

- **Skill discovery priority now overrides as documented.** A higher-priority
  source (project-local, then configured paths, then user-global, then remote,
  then bundled) now correctly overrides a lower-priority source that defines a
  skill with the same name. Previously the first source scanned won, which
  inverted the intended priority.

## [0.0.1] - 2026-06-24

First usable release. neenee is now a working AI coding agent with a semantic
TUI, tool use, on-demand skills, plan mode, and durable sessions.

### Added

- **Semantic TUI** built on Ratatui: live status, expandable tool steps,
  streaming bash output, structured diffs, per-step detail overlays, sticky
  headers, and a persistent right-side sidebar for plans and goal state.
- **Tool use** via a full ReAct loop with native and fallback tool-calling;
  bundled tools include bash, file read/write/edit, grep, glob, web search,
  and MCP server integration.
- **Autonomous goals**: set an objective with `/goal`, run `/loop`, and let
  the agent work iteratively with a checklist and bounded autonomy.
- **Plan mode**: read-only analysis and planning that does not touch the
  codebase, plus `/plan` and `/verify` slash commands and a plan preview
  modal with a sticky progress panel and stale-plan detection.
- **Durable sessions**: atomic on-disk persistence with context compaction,
  session resume and fork, a sessions picker, and `/export` to Markdown.
- **Skills system**: domain-specific instructions loaded on demand or
  automatically by mention, stored under XDG paths with compile-time-embedded
  bundled skills.
- **Model and provider management**: provider/model picker (`Ctrl+M`),
  split provider and model registries, provider timeouts, and persistent
  per-session permissions with labeled permission prompts.
- **Sub-agents** with tool-admission profiles driven by a `ToolAccess` tier
  split, and an inline sub-agent view.
- **Reliability aids**: stalled-agent detection with a configurable verify
  hard nudge (`/stall-threshold`, `/verify-nudge`), plus an uncapped agentic
  loop anchored to a single breathing indicator.
- **Observability**: opt-in file-based tracing across the harness.
- **Slash commands**: `/goal`, `/loop`, `/compact`, `/plan`, `/verify`,
  `/session list`, `/export`, `/mcp`, `/stall-threshold`, and `/verify-nudge`.

### Changed

- Adopted a strict six-crate workspace topology
  (`neenee-core` ← `{neenee-providers, neenee-tools, neenee-store}` ←
  `neenee-agent` ← `neenee-cli`) with typed errors and a unified agent loop.
- Standardized on MIT-only licensing.

[Unreleased]: https://github.com/ming2k/neenee/compare/v0.20.3...HEAD
[0.20.3]: https://github.com/ming2k/neenee/releases/tag/v0.20.3
[0.20.2]: https://github.com/ming2k/neenee/releases/tag/v0.20.2
[0.20.1]: https://github.com/ming2k/neenee/releases/tag/v0.20.1
[0.20.0]: https://github.com/ming2k/neenee/releases/tag/v0.20.0
[0.19.1]: https://github.com/ming2k/neenee/releases/tag/v0.19.1
[0.19.0]: https://github.com/ming2k/neenee/releases/tag/v0.19.0
[0.18.0]: https://github.com/ming2k/neenee/releases/tag/v0.18.0
[0.17.0]: https://github.com/ming2k/neenee/releases/tag/v0.17.0
[0.16.0]: https://github.com/ming2k/neenee/releases/tag/v0.16.0
[0.15.0]: https://github.com/ming2k/neenee/releases/tag/v0.15.0
[0.14.3]: https://github.com/ming2k/neenee/releases/tag/v0.14.3
[0.14.2]: https://github.com/ming2k/neenee/releases/tag/v0.14.2
[0.14.1]: https://github.com/ming2k/neenee/releases/tag/v0.14.1
[0.14.0]: https://github.com/ming2k/neenee/releases/tag/v0.14.0
[0.13.2]: https://github.com/ming2k/neenee/releases/tag/v0.13.2
[0.13.1]: https://github.com/ming2k/neenee/releases/tag/v0.13.1
[0.11.0]: https://github.com/ming2k/neenee/releases/tag/v0.11.0
[0.10.1]: https://github.com/ming2k/neenee/releases/tag/v0.10.1
[0.10.0]: https://github.com/ming2k/neenee/releases/tag/v0.10.0
[0.9.1]: https://github.com/ming2k/neenee/releases/tag/v0.9.1
[0.9.0]: https://github.com/ming2k/neenee/releases/tag/v0.9.0
[0.8.0]: https://github.com/ming2k/neenee/releases/tag/v0.8.0
[0.7.1]: https://github.com/ming2k/neenee/releases/tag/v0.7.1
[0.7.0]: https://github.com/ming2k/neenee/releases/tag/v0.7.0
[0.6.1]: https://github.com/ming2k/neenee/releases/tag/v0.6.1
[0.6.0]: https://github.com/ming2k/neenee/releases/tag/v0.6.0
[0.5.0]: https://github.com/ming2k/neenee/releases/tag/v0.5.0
[0.4.0]: https://github.com/ming2k/neenee/releases/tag/v0.4.0
[0.3.0]: https://github.com/ming2k/neenee/releases/tag/v0.3.0
[0.2.0]: https://github.com/ming2k/neenee/releases/tag/v0.2.0
[0.1.0]: https://github.com/ming2k/neenee/releases/tag/v0.1.0
[0.0.1]: https://github.com/ming2k/neenee/releases/tag/v0.0.1
