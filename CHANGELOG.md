# Changelog

All notable changes to **Muta** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.36.2] - 2026-08-27

### Changed
- Replaced long positional argument lists with self-describing parameter
  structs across the TUI (`ComposerDrawOptions`, `BodyRenderOptions`,
  `CodeGutterParams`, `RichLineParams`), agent (`NativeSearchParams`,
  `AddProviderParams`, test `PolicyContext` builders), contracts
  (`BeginRequestParams`), and runtime crates. Internal API hygiene
  refactor with no intended behavioral changes.

## [0.36.1] - 2026-08-27

### Added

- **Project Workspace Roots Trust Gate:** support linked workspace roots trust
  domain (`TrustDomain::Roots`) in `WorkspaceSecuritySnapshot`, quarantine prompt,
  and project-local `.muta/config.toml` `[workspace].additional_roots` merging.
- **TUI Checklist Result Rendering:** support checklist item rendering
  (`ResultKind::Checklist`) with completion glyphs (`☒`, `☐`, `✕`) and strikethrough
  styling in tool outputs.

### Changed

- **Runner Terminology & Protocol Polish:** unified inline runner step display
  and envoy-to-runner tool naming transitions across agent, contracts, and
  documentation.
- **Command Extraction & Normalization:** hardened executable extraction and
  argument normalization for cross-platform scripts and command execution.

## [0.36.0] - 2026-08-27

### Added

- **Harness Steward Plane & Typed Cognitive Contracts (ADR-0150):** partitioned
  the agent architecture into Operational Core (Master/Runner), Fleet Control
  Plane (Supervisor), and Harness Infrastructure Plane (Steward). Introduced
  typed `StewardTask` contracts for semantic loop detection (`SemanticLoopSentinelTask`),
  sanity checks (`SanityVerifierTask`), and session titling (`SessionTitlerTask`),
  executing out-of-band, stateless, and fail-open.
- **In-flight Stream Loop Detector:** stream loop detector (`StreamLoopDetector`,
  `DegeneratePattern`) and self-healing circuit breaker that inspects token and
  chunk repetition cycles during streaming responses to abort runaway token loops.
- **TUI: paste copied files as attachments.** Copying an image file in a file
  manager (Nautilus, Dolphin, Finder, Explorer) and pressing Ctrl+V in the
  composer now stages it as an `[Image #N]` attachment instead of inserting
  the file's path as text. Multi-file pastes attach every supported image and
  skip non-images with a toast; plain text copies still paste as text.
  Read per-platform: `text/uri-list` / `x-special/gnome-copied-files`
  (Linux), Finder file URLs (`osascript`, macOS), `CF_HDROP`
  (PowerShell `Get-Clipboard -Format FileDropList`, Windows).

### Changed

- **Autonomous execution mode naming:** refactored and standardized YOLO mode
  to delegated autonomous execution mode across CLI, runtime, and documentation.
- **Workspace path resolution & admission:** unified path resolution and admission
  for additional roots across contracts and agent runtime.
- **Host environment & shell dialect:** enhanced platform guidance and cross-platform
  shell dialect detection.

## [0.35.7] - 2026-08-27

### Changed

- **Provider preset vocabulary, repo-wide clean break:** the "provider
  template" concept is renamed to **preset** everywhere — code, wire
  contracts, persistence, and docs — with no compatibility shims:
  - Wire: `AgentRequest::AddProvider.template_id` is now `preset_id` and the
    `template_id` serde aliases on `AddProvider`/`ProviderPickerRow` are gone;
    the AsyncAPI schema is updated to match.
  - Registry: `ProviderTemplateSpec`/`PROVIDER_TEMPLATE_SPECS`/
    `provider_template_spec` are now `ProviderPresetSpec`/
    `PROVIDER_PRESET_SPECS`/`provider_preset_spec` (the transitional type
    aliases are deleted); per-preset constants renamed `TEMPLATE_SPEC` →
    `PRESET_SPEC`.
  - TUI: `ProviderPreset` (was `ProviderTemplate`), `PROVIDER_PRESETS`,
    `preset_label_for`, `Modal::ProviderPreset`, `custom_preset_id`,
    `preset_choice`/`preset_scroll`.
  - Reference docs (`add-a-provider`, `providers`, `paths`, `glossary`,
    `new-model-onboarding`) now use preset/connection vocabulary throughout.
- **Legacy state migration removed:** the one-shot migrator from the
  pre-ADR-0123 layout (`[[providers]]` in `config.toml` +
  `[builtins]/[user]` keys) is deleted (`catalog/legacy.rs` and its call
  sites in daemon bootstrap and the `config`/`auth` CLI paths). Installs
  still on the old layout must re-add their connections.
- **OpenAI preset relabeled:** the `openai` connection preset is now labeled
  **OpenAI Platform** (chooser row, editor header, Connections provider-type
  column) to disambiguate it from the ChatGPT subscription preset. The preset
  id and stored `preset_id` values are unchanged, so existing connections and
  configs are unaffected.
- **Connections › Add Provider chooser restyle:** the trailing `⚿ oauth`/`⚿
  token` badge is gone along with the dead right column it forced on every
  row; titles now own the full row width. Auth scheme lives in the prose:
  each preset's description was rewritten from the stiff
  `Service — blurb (Auth)` formula into one sentence covering what the
  service is, what it serves, and how to sign in.

### Fixed

- **Create-mode editor title:** the provider editor header resolved its title
  by wire protocol, where several presets (`chatgpt-oauth`, `openai`,
  `custom-openai`, `deepseek`, …) share `openai`. Creating a connection from
  any of them showed "ChatGPT" as the title. The title now resolves from the
  seeded preset id (`preset_label_for`).

## [0.35.6] - 2026-08-27

### Added

- **Structured Command Acks & Notices:** `CommandResult::Ack` now carries
  optional structured detail lines, rendered cleanly across TUI and Web
  interfaces, with inline transcript notices recorded for YOLO posture
  changes.

- **MCP Streamable HTTP transport:** `[mcp.<name>]` now accepts a `url` key
  (e.g. `url = "https://example.com/mcp"`) alongside the existing `command`
  stdio form; `url` wins when both are present. Responses are parsed in both
  legal shapes (plain JSON and SSE `data:` frames), a server-issued
  `Mcp-Session-Id` is captured at initialize and echoed on every request,
  and HTTP-level connection failures classify as retry-safe transport errors
  while delivered 4xx/5xx bodies are never retried. `muta mcp ls` displays
  the endpoint for `url` servers.
- **MCP config-time tool scoping (ADR-0085 follow-up):** `[mcp.<name>]` and
  project `.muta/mcp.json` entries accept `allow_tools` / `deny_tools`,
  matched against the server's original (unsanitized) tool names. Deny wins
  over allow; an empty allow-list admits everything. Filtered tools are never
  advertised to the model.
- **mcp_specialist runner delegation wired (ADR-0138):** the `mcp_specialist`
  runner preset now receives the session's live MCP tools at spawn — runner
  dispatch reads the master's dynamic-tool registry through the new
  `DynamicToolSource` port, so the 10-minute re-discovery loop and `/mcp`
  reconnects reach later children without re-binding. Only this profile
  consults the source; `explore` / `code` / `title` children still see no MCP
  tools, profile runtime hard rules still apply to injected tools, and static
  tools win name collisions.

### Changed

- **MCP protocol negotiation:** the client now offers `2025-06-18` and
  accepts any server answer in the supported set (`2025-06-18`,
  `2025-03-26`, `2024-11-05`); a server replying with a revision outside
  that set fails the connection instead of continuing in an undefined
  dialect.
- Shell execution: the idle watchdog's no-output budget now scales with the
  caller's `timeout` (one third, clamped to [5s, 60s]) instead of a fixed 10s,
  so an explicitly larger `timeout` grants proportionally more idle tolerance
  for legitimately quiet commands (long sleeps, network waits, `--quiet`
  builds). The default (30s → 10s) is unchanged.
- Shell termination footers: the `IdleBlocked` and `Timeout` messages now lead
  with the fact (killed at the no-output limit / timed out after the configured
  timeout) and keep a single retry hint, instead of a long multi-clause
  explanation of password prompts and TUI tools. The decorative leading
  emoji (⏸/⛔/⏱/✗) are dropped — state is already carried by the warn/err
  colour styles.

## [0.35.5] - 2026-08-27

### Changed

- **Doom guard relaxed (ADR-0148):** `[master.doom_guard] threshold` is a
  live key again with a new default of `3` — one same-signature re-run per
  window is tolerated (a transient retry, a re-run of the same test command
  after an edit) and the second repeat is blocked pre-dispatch. The strict
  ADR-0113 first-repeat block is still available with `threshold = 2`.
  `edit_file`/`write_file` signatures are now content-addressed (path +
  stable payload hash) instead of path-only, so sequential distinct edits
  to one file no longer collide; an exact payload repeat (true A→B→A
  thrash) still does. Wired through `muta config get/set
  master.doom_guard.threshold`.
- Command steps (`execute_command`, legacy `bash`) now default to collapsed
  in the TUI transcript, matching every other tool kind. The summary line
  ("Run cargo test · 1.2s") covers the common case; failures still
  force-expand, and `[tui.default_expanded] execute_command = true` restores
  the old open-by-default behavior.
- Expanded command steps now always paint an `exit N` footer when the exit
  code is known, including a dimmed `exit 0` on success, so a completed run
  ends with a diagnostic fact instead of silence. (Previously only non-zero
  codes rendered.)

### Fixed

- `/yolo` ack toast: collapsed the multi-line `•`-bulleted confirmation into
  a single-line `Status: explanation` string. The toast bubble renders one
  line capped at 58 columns, so the old three-line title was flattened and
  mid-word truncated. The slash-handler ack now matches the `--yolo` startup
  toast phrasing.
- Legacy-session posture restore no longer re-escalates a de-escalated
  session: the ledger heuristic classified ack titles with an "on"-first
  substring test, but every OFF phrasing shipped before 0.35.4 also contains
  "on" ("permission", "questions", "interaction"), so a resumed legacy
  session whose last ack was OFF came back with auto-approval silently
  re-enabled. Classification now tests "off" first via
  `classify_yolo_ack_title`, with unit and ADR-0132 integration regressions
  covering all historical ack phrasings.
- Reworded the shell idle-blocked footer ("⏸ no output for a while…") to
  state what actually happened: the command was killed after ~10s of silence
  because it was almost certainly waiting for stdin input the agent can't
  provide, with the same non-interactive retry hints.

## [0.35.4] - 2026-08-26

### Changed

- Replaced autopilot posture with YOLO mode (`/yolo` / `--yolo` / `-y`), automatically approving tool permission requests while retaining destructive command guards.
- Updated session persistence, contracts, runtime permission policies, and frontend indicators to reflect YOLO mode semantics.

## [0.35.3] - 2026-08-26

### Changed

- Unified steering (`Steer`, `CancelSteer`) and follow-up (`FollowUp`) queue semantics across agent runtime, contracts, and frontend.
- Standardized queue events (`SteerAdmitted`, `SteerUnavailable`, `SteerCancelled`, `SteerCancelFailed`, `FollowUpStarted`) and introduced configurable `QueueMode`.

## [0.35.2] - 2026-08-26

### Changed

- Replaced aggregate workspace trust with independent content-bound `mcp`,
  `skills`, `hooks`, and `rules` grants. `/trust` now has the closed grammar
  `/trust [all|mcp|skills|status|revoke]`; `/untrust` revokes all domains, and
  the ambiguous `/extensions` and `/trust workspace` surfaces are removed.
- Separated linked filesystem roots from project asset trust. Additional roots
  now come only from the user-owned global `[workspace].additional_roots`
  setting; repository configuration cannot widen its own file boundary.
- Routed sandbox shell, MCP calls, and lifecycle Hook commands through the
  runtime hazard/permission model. Missing unattended authority now reports
  `[permission required]` with runtime-only guidance.

### Added

- Project MCP definitions in `.muta/mcp.json`, top-level `skills/` discovery,
  trusted project rules in model context, and live reload/unload of every asset
  domain (ADR-0147).
- Per-model capability overrides in the settings () editor: two tri-state
  rows (Vision, Tool call — inherit / force on / force off, Space to cycle,
  Tab to reach them) persisted per (provider-instance, model) in the
  route-settings **state** store, never in . Submitting the
  editor sends the full record, so clearing both rows to "inherit" removes
  the stored overrides.
- Three-layer model capability resolution, formalized as ADR-0149: user
  `RouteSettings::capability_overrides` > remote `RemoteModelMetadata` >
  the static baseline registry. `CapabilityOverrides` (family, context
  window, max output tokens, thinking, tool call, vision — all optional,
  `Some(false)` meaningful) is carried on `Channel::user_overrides` and
  applied by the single `ModelCapabilities::apply_overrides` site; empty
  records are filtered before persistence. A cross-provider test now locks
  shared baseline ids (e.g. `glm-5.2`, `kimi-k2.7-code`) byte-identical.
- `glm-5.3-flash` on the `zai-code` provider. Zhipu's GLM Coding Plan now
  serves it on every tier (native multimodal vision, 1M context, ~1/3 of
  GLM-5.3's credit burn); the model picker offers it between the flagship and
  `glm-5.2`, with the same always-on thinking ladder (`low`/`high`/`xhigh`/
  `max`).

## [0.35.1] - 2026-08-26

### Added

- Dedicated XDG configuration and state resolution for `mutx` terminal frontend (`$XDG_CONFIG_HOME/mutx/config.toml`, `$XDG_CONFIG_HOME/mutx/themes/`, `$XDG_CONFIG_HOME/mutx/logo.txt`, `$XDG_STATE_HOME/mutx/history.json`).

### Changed

- Decoupled terminal presentation settings (`tui`, `input_history`) from core daemon configuration schema (`muta-persistence`).
- Refined tool permission policies, hazard level evaluations, and local execution boundaries.

## [0.35.0] - 2026-08-26

### Added

- Three-tier agent hierarchy (Supervisor, Master, Runner) with runtime mesh coordination (ADR-0144).
- Decoupled workspace asset trust and tool hazard model (ADR-0145).
- Structured tool permission submissions (`HazardLevel`, `ToolPermissionSubmission`, `ToolPermissionPayload`), session-scoped permission grants, and broker submissions (ADR-0146).

### Changed

- Transitioned agent role vocabulary from principal/envoy to master/runner across crates, contracts, and TUI overlays.
- Replaced multi-binary test layout with single-binary integration targets (`tests/integration.rs`) and integrated `cargo nextest` watchdog profile.
- Composer `@` path completion now scans project files in-process with
  ripgrep's `ignore` walker instead of shelling out to a system-installed
  `rg --files`. Machines without `rg` installed get identical gitignore and
  hidden-file semantics (previously the fallback walked the tree without
  ignore rules, surfacing `target/` and `node_modules/`); directories are
  included alongside files so the trailing-`/` completion UX is preserved.
  The walk is bounded at 2,000 entries and sorted deterministically.

## [0.34.5] - 2026-08-26

### Changed

- Retired the `INTERACTIVE` and `QUANT` envoy profiles: the
  reserved `INTERACTIVE` role never gained a dispatch tool, and `QUANT`'s
  domain-specific tools are not wired into any dispatch surface. The
  remaining built-ins (`EXPLORE`, `CODE`, `TITLE`, `MCP_SPECIALIST`) are
  unchanged; `CODE` retains `allow_user_interaction` so its ask_user
  request/reply path still works.
- Consolidated filesystem search into three single-purpose tools (ADR 0143):
  `find_files` takes an explicit `patterns` array (OR, ripgrep-style gitignore
  globs) for recursive discovery, `search_text` runs content search in-process
  (Rust `regex` + ripgrep's `ignore` walker — no external `rg` dependency), and
  `list_dir` is shallow-only. Retired `glob`, `find`, and `grep` with no
  aliases; tool policies naming them must use `find_files` or `search_text`.
- Widened workspace admission (ADR 0142): a project may declare
  `[workspace].additional_roots` in `.muta/config.toml`; roots load only when
  the project's extensions are content-trusted, widen filesystem confinement
  and sandbox bind mounts, and surface in the system prompt.
- Simplified TUI context accounting: the hint bar now shows only committed
  context, while Context Usage folds a draft into its projected total and
  breaks the draft down into composer text and approximate message framing.

## [0.34.4] - 2026-08-25

### Added

- Added dynamic shell isolation transition based on workspace security handle: elevating to host native execution when explicitly trusted for development while strictly confining untrusted/restricted workspaces.
- Added slash command syntax and option expansion for interactive auto-completion of subcommands and parameters (e.g. `/trust`, `/debug trace`, `/principal`).

### Changed

- Consolidated `/workspace` command into `/trust` (`/trust workspace`, `/trust extensions`, `/trust all`, `/trust readonly`, `/trust status`, `/trust revoke`) with enhanced runtime mode status.
- Refined initial workspace trust interactive prompt options with clear descriptions for full development, workspace only, and restricted read-only mode.

## [0.34.3] - 2026-08-25

### Added

- Added interactive workspace trust question prompt during session bootstrap when a workspace has no persisted trust decision.
- Added session reply handler for workspace trust decisions to automatically persist development or restricted profile selections.

## [0.34.2] - 2026-08-25

### Added

- Added `/trust` command (`/trust workspace`, `/trust extensions`, `/trust all`, `/trust readonly`, `/trust status`, `/trust revoke`) to manage workspace execution authority and project extensions.
- Added comprehensive Security and Trust Architecture documentation detailing the three orthogonal domains (Authority, Posture, and Sandbox) and two-axis authority model.

### Changed

- Updated preflight check to require explicit workspace trust while gracefully falling back to host execution authority when physical bubblewrap sandbox containment is unavailable.

## [0.34.1] - 2026-08-25

### Added

- Added first-class workspace security (ADR-0140): independent execution
  profiles and content-bound extension admission, visible preflight state,
  interaction-only autopilot semantics, and a fail-closed physical workspace
  sandbox for filesystem and shell operations.

### Fixed

- Rebuilt TUI cursor submission as one terminal commit transaction. The
  backend now installs the final coordinate while the cursor is hidden and
  reveals it only afterward, keeps cursor-only frames free of visibility
  toggles, parks hidden frames at the last input anchor, clamps coordinates,
  and forces a full recovery repaint after any failed terminal write.
- Made scroll translation proof-based. Every overlapping row must match its
  shifted source and the operation must eliminate real changed rows, so local
  edits and repeated blank lines can no longer produce false terminal scrolls.
  Diff planning no longer mutates committed terminal state before a successful
  flush, and identical retained-grid writes stay clean.
- Unified text-input geometry around grapheme clusters. Long picker and editor
  fields now use bounded horizontal viewports, caret ownership follows the
  active edit mode, and cursor motion or deletion cannot split combining
  sequences, wide glyphs, or zero-width-joiner emoji.

## [0.34.0] - 2026-08-25

### Added

- **Unified TUI Surface Router and Retained-View Lifecycle (ADR 0139)**:
  - Replaced the TUI's split modal/navigation state with one exact surface router and a complete retained-view lifecycle across shortcuts, mouse entry, the quick switcher, request sheets, and workflow editors.
  - Views refresh backend-owned data on every show, preserve MRU state when hidden, and can be explicitly closed with `Del` in the switcher without deleting backend data.
  - Added explicit session-list and session-tree snapshot requests plus separate TUI presentation signals, making both views refreshable without navigation side effects.
- **Subagent Actor Runtime & Execution Isolation**:
  - Actor subsystem with supervisor, mailbox, handles, and lifecycle events in `muta-contracts` & `muta-agent`.
  - Isolated worktree manager for subagent execution isolation.
- **Syntax Defense Guard & Observation Folding**:
  - Pre-execution syntax defense guard for edit and write tools.
  - Heuristic compaction evaluation and observation folding with budget token tracking utilities.
- **Session DAG Tree Rollback & Slash Commands**:
  - Implemented `/diff` and `/undo` slash commands with session DAG tree rollback.
- **Server-Side KV Cache Alignment (ADR 0137)**:
  - Dynamic prompt zoning and KV cache alignment for Anthropic and Google protocols.
- **Session Tree Branching & Split Compaction (ADR 0138)**:
  - Session tree branching, subagent isolation, split compaction with file tracking, and session tree overlay in Mutx TUI.
- **Agent Tools Enhancement**:
  - Added `find` tool and enhanced agent tools (`bash`, `grep`, `read`, `edit`, `envoy`, `catalog_picker`).

### Fixed

- Preserved the distinct Activity and Todos identities, exact parent return after transient sheets, Queue exit hooks on every switch path, and separate picker filters from parked chat drafts.

## [0.33.1] - 2026-08-25

### Added

- **Antigravity Dynamic Model Discovery**:
  - Implemented dynamic model discovery endpoint for Antigravity OAuth via `/v1internal:fetchAvailableModels`.
  - Automatic filtering of deprecated, internal helper (`chat_*`, `tab_*`), and legacy 3.6 flash models.
  - Wire name normalization routing `gemini-3.7-flash` to canonical wire identifier `gemini-3.7-flash-tiered`.
- **Configurable `hidden_models`**:
  - Added `hidden_models` configuration option to `Config` supporting exact and glob pattern filtering across model pickers.

### Changed

- **Provider and Model Picker UI Polish**:
  - Refined models modal and provider overlay styling with clean row layout, aligned labels, and circle node effort sliders.

## [0.33.0] - 2026-08-25

### Changed

- **TUI engine rendering pipeline overhaul**:
  - **Cursor Absolute Shielding**: Physical cursor is strictly hidden (`\x1b[?25l`) throughout cell diff and hardware scroll emission, and positioned/unhidden (`\x1b[?25h`) only at the end of the frame in a single atomic step inside the synchronized update envelope.
  - **Single Pipeline Submission**: Eliminated out-of-band cursor sync and split-brain guessing, preventing caret bouncing and IME candidate jumping.
  - **Typing / Composition Quiescence**: Background micro-animations (e.g. 100ms spinner/breathing ticks) are suspended during active typing/composition (150ms window), preventing 10Hz IME candidate box vibration.
  - **Zero-Allocation `CompactSymbol`**: Replaced heap-allocated `String` in `Cell` and `Draw::Cells` with 22-byte stack-inlined `CompactSymbol`, eliminating memory allocation overhead during high-frequency diffing and screen rendering.
  - **Batch `ClearEol`**: Optimized non-BCE ClearEol to emit batched spaces instead of queuing individual commands per cell.

## [0.32.2] - 2026-08-25

### Fixed

- **Every terminal write now sits inside a DEC synchronized-update
  envelope — the out-of-band cursor paths included.** The engine gained
  `Backend::begin_sync_update`/`end_sync_update` (queue-only begin, single
  flush at end — presentation is order-based, so an intermediate flush could
  only add a syscall and present an empty envelope). The input-driven
  immediate caret flush and the caret show/hide visibility edge are now
  bracketed by it, closing the last paths whose bytes could interleave with
  a concurrently-committing frame on the same stdout — the residual seam
  that read as caret jitter on mode-2026 terminals (no-op markers
  elsewhere). Two engine tests lock the envelope contract: markers bracket
  the out-of-band write, and envelopes never nest or dangle.
- **Masked-input caret pairing is now structural, not incidental.**
  Rendering the ModelEditor's API-key field as `•`s while handing the layout
  the *unmasked* buffer's caret byte offset worked only by an arithmetic
  accident (`•` is 3 bytes/char vs the raw text's 1–4). `App::
  displayed_input_with_cursor` is now the single source of truth for the
  (displayed text, caret offset) pair — masking the offset through the same
  mask — and both the renderer and the between-frame geometry probe resolve
  through it, so the two can never measure different strings. Four tests
  lock the pairing (boundary safety, end-mapping, clamping included).
- **The geometry probe measures the renderer's own numbers.** `TranscriptRender`
  now reports `input_rows` — the exact wrapped-row count that sized the input
  box this frame — and `App::observe_input_rect` records it instead of
  re-deriving from the raw buffer. The probe's width is resolved through the
  new `view::composer_layout_text_width` (the height reservation's own
  formula), removing the derived-invariant coupling on the placed rect.
  Together with the flush's single-compute restructure (the scroll probe's
  result *is* the flushed coordinate — no second evaluation, no window for
  the two to disagree), the flush's decision and its action are now one
  computation.
- **Caret anti-drift: the input-driven immediate cursor flush now defers to
  the frame whenever the composer's geometry is provably in flux.** The
  immediate flush (the 0.12.0 IME-anchor fix) placed the terminal cursor
  against the *previous* frame's composer rect and wrote it outside the DEC
  synchronized-update envelope. Whenever that rect was about to change — a
  wrap boundary crossed by the keystroke, a paste, a history recall, a
  resize, or a streaming round toggling the activity/todo/queue footer bars
  underneath the box — the flush wrote one coordinate and the next
  `commit_frame` corrected it, producing the two-step visible caret jump
  reported as 反复漂移/闪烁 while typing. The flush is now gated by
  `App::input_geometry_is_clean` (live terminal size unchanged + re-measured
  wrapped-row count equal to what the last frame reserved), a scroll probe
  (the flush must not move `input_scroll` ahead of the repaint, which parked
  the caret on a still-unscrolled row), and the transcript-update gate. When
  any gate trips, the committed frame — inside one synchronized update — is
  the sole writer of the caret coordinate.
- **The immediate flush is armed only by caret-moving input.**
  `note_cursor_moved` fired after every `process_event`, including mouse
  reports (one armed flush per motion report under mode-1002 drags) and
  resizes, emitting out-of-envelope cursor writes on essentially every loop
  iteration during pointer activity. Only `Key` and `Paste` events arm it
  now; mouse-driven caret moves still arm it via `App::set_cursor` in the
  selection handlers.
- **Staged measurement frames no longer poison the flush's geometry cache.**
  A staged (byte-less) layout pass ran the full `observe_input_rect`
  bookkeeping; when the settle logic then discarded the staged grid and
  redrew at a corrected scroll offset, the next iteration's flush used a rect
  no committed frame had ever published. The pre-stage snapshot is now
  restored on the discard path and kept on the commit path.
- **Resize no longer flushes mid-synchronized-update envelope.**
  `Backend::invalidate` queued an SGR reset and flushed it immediately;
  inside `Terminal::commit`'s DEC-2026 envelope that split the frame in two
  and let the terminal paint a half-reset screen. The reset is queued only —
  the envelope's single closing flush delivers it atomically with the
  `Clear(All)` that follows.
- **`dim_surface` and the modal scrollbar keep the grid's dirty tracking
  honest.** Both mutated cells in place through `buffer_mut`, bypassing
  `Grid::mark`; correctness depended on the full-screen background fill
  having dirtied every row earlier in the frame. They now mark the rows they
  touch explicitly (defensive; no behavior change today).
- Engine: added `Terminal::size()` — the live terminal size with a
  retained-grid fallback — for callers deciding between frames whether
  cached geometry is still valid.

## [0.32.1] - 2026-08-24

### Changed

- **CLI top-level verb ergonomics and bare invocation default.** `start`, `stop`,
  `status`, and `token` are canonical top-level commands on `muta` (e.g. `muta start`,
  `muta stop`, `muta status`, `muta token`), with `muta daemon <subcommand>` retained
  for backward compatibility. Bare `muta` invocation now starts the daemon in foreground
  by default.
- **HTTP runtime endpoints and CORS support.** Added CORS support across all HTTP
  routes, along with authenticated REST API endpoints (`GET /api/v1/sessions`,
  `POST /api/v1/prompt`).
- **One-click desktop/shell launchers and release packaging.** Added `muta-ui.sh`
  (Unix) and `muta-ui.bat` (Windows) one-click scripts to launch the daemon and open
  the Web UI, packaged into release archives.

## [0.32.0] - 2026-08-24

### Changed

- **The project is now Muta, with core and terminal app split at the process
  boundary (ADR-0136).** `muta` contains only the daemon and related service
  commands; the terminal application is the separate `mutx` binary under the
  `apps/tui` subproject (`crates/mutx` plus its private
  `crates/mutx-engine`). `mutx` still discovers the per-user daemon and starts
  a sibling, `MUTA_BIN`, or `PATH`-resolved `muta` when needed. Crates,
  package metadata, instance paths, environment variables, install scripts,
  service files, CI, and release archives use the Muta names, and releases
  ship both binaries.
- **Composer completion is backend-owned for every frontend.** Protocol 2
  adds `CompleteInput` / `InputCompletions`; the daemon now owns slash and
  intent matching, aliases, trusted project commands, and `@path` discovery.
  Results carry exact replacement ranges and insertion text, leaving TUI and
  Web responsible only for cursor-unit translation, presentation, and
  applying the returned edit.
- **The Web app builds and deploys independently.** The Web-assets Rust crate,
  embedded bundle, daemon `panel` command, and static-file route are removed.
  The daemon keeps only the generic `/healthz` HTTP probe and adds
  `muta daemon token` for explicit operator access to the local TCP bearer
  credential.

## [0.31.0] - 2026-08-24

### Fixed

- **A round that finished its answer right as the next message landed is no
  longer mislabelled `▲ interrupted · new message`.** The reported case:
  the model's answer fully streamed, the user sent the next message, and
  the transcript grew an interrupt marker *over the completed round* — on
  resume it even sat below the newer round's answer. Three stacked defects:
  (1) the stream loop's `select!` is `biased` toward cancellation, so a
  cancel arriving between the final delta and the stream's terminal event
  won the next poll and unwound the round as `Err(Interrupted)` even though
  every delta was already rendered — a stream that has delivered output
  this turn now gets one bounded finish-drain window
  (`FINISH_DRAIN_GRACE`, 750 ms) to reach its natural end, so a settling
  stream commits as a completed round while a stream that stays silent is
  still genuinely interrupted; (2) the durable record's `at_ms` was stamped
  at *tail time* — after the superseding message's send — so the resume
  seam-merge dropped the marker below that message; the stop reason is now
  parked with its clock reading at the moment the stop is requested
  (`RoundLifecycle::record_interrupt` / `record_interrupt_at`, a supersede
  parks the superseding message's `sent_at_ms`); (3) the record's `round`
  read the live agent counter at tail time, which the superseding round
  had already bumped, stamping round N+1 over round N's stop — it now uses
  the interrupted round's own admitted number. Interrupt UX is unchanged
  for real interrupts (Esc Esc still cuts a generating answer at the next
  chunk; only an already-settling stream completes).
- **Interrupted/failed turns booked absurd completion-token counts (and TPS
  figures like 130 050).** The streamed-output estimator fed every delta
  into the exact BPE `StreamingCounter` but then *summed* the counter's
  return value across deltas — and `push` returns the **running total**, not
  a per-delta increment. Every early token was therefore re-counted once per
  later delta, growing quadratically: a real 4 000-delta interrupted stream
  settled as 14 786 219 "completion tokens" over 113 s → the 130 050 tok/s
  row in the Context Usage modal. Completed turns masked the bug only by an
  accident of ordering (`book_turn_usage` finished the counter and took the
  maximum); interrupted and failed attempts settled straight through the
  inflated sum. The count is now read off the counter (`tokens()` /
  `finish()`) — exact across delta boundaries — and the counter is finalized
  on the interrupt/failure path too, so an interrupted attempt books the
  true whole-text count of what it streamed. Existing poison is repaired in
  place: `TokenSourceLedger::restore_session` clamps physically implausible
  *estimated* completion counts (>10M tokens — nothing real streams that)
  to "prompt only, no completion" so resumed sessions and the durable
  `/usage` mirror stop showing seven-figure counts and fabricated rates.
  Provider-reported counts are never touched.

### Added

- **`[websearch]` is now a live wire-configurable setting** (was
  boot-time-only). Two additive `AgentRequest` variants —
  `QueryWebSearchConfig` (replies `WebSearchConfigSnapshot`) and
  `UpdateWebSearchConfig` (a PATCH; replies `WebSearchConfigUpdated`) —
  expose the search backend / fallback / reader / timeout / SearXNG URL
  and API-key presence over the session protocol. Replies carry **key
  presence flags only**; plaintext secrets never cross the wire. Updates
  validate at the boundary (unknown backend/reader names and `searxng`
  without a URL are rejected with pointing errors), persist behavior
  fields to `config.toml` and keys to `credentials.toml` (empty string
  clears), and hot-apply through a new shared
  `SharedWebSearchConfig` handle — the running `websearch`/`webfetch`
  tools rebuild their provider chain / HTTP client on the next call by
  signature comparison, no restart or toolset rebuild.
  `/settings reload` pushes an out-of-band `config.toml` edit through the
  same hot-apply path. Frontends: the TUI Settings view gains a **Web
  Search** category (`/settings`, cycle backends/reader, inline editing
  for the SearXNG URL and keys), and the web panel gains a `⌕ web` header
  dialog. Wire-compatible (additive variants; see
  `scripts/check-wire-compat.sh`'s `wire-compatible` label).

## [0.30.5] - 2026-08-23

### Added

- **Wire-protocol window negotiation (ADR-0134).** Client/daemon
  compatibility is now governed by a wire protocol number, not the
  product version. A client that sends `Select{protocol}` (the CLI, TUI,
  and web panel all do) is served anywhere in the daemon's window
  `[MIN_PROTOCOL_VERSION, PROTOCOL_VERSION]` — *whatever its product
  build* — so additive wire changes stop breaking version-pinned clients;
  outside the window it is refused before any session work with
  `Error{code:"protocol_mismatch"}` and a directional fix. Clients that
  predate the field keep ADR-0100 rule 4's exact version equality. Locally,
  an in-window daemon is served whatever its product build — a patch bump
  no longer interrupts a healthy daemon — while the one freshness gate
  that survives is the dev-drift lie: same version, *different binary*
  (`/proc/<pid>/exe` image check), the state where every version signal
  agrees but the client is still about to test a stale locally-rebuilt
  image.
  The wire envelope moved to `neenee-contracts::wire` (single serde source
  of truth next to the payload types), the discovery record mirrors the
  daemon's `protocol` for pre-handshake refusal, the web panel's
  `AttachAction`/`ControlRequest`/`SessionOverview` mirrors are now
  ts-rs-generated (fixing a real drift: the hand-written `AttachAction`
  lacked the `picker` variant), and `scripts/check-wire-compat.sh` (CI)
  fails a wire-surface change without a `PROTOCOL_VERSION` bump unless
  the PR is labeled `wire-compatible` — the bump decision is now a
  reviewable assertion, not memory.

- **Views are retained surfaces with a global quick switcher (ADR-0133).**
  Surfaces no longer reset when you leave them: hiding a view (Esc, outside
  click, Ctrl+C — all one shared dismiss verb) saves its scroll, selection,
  and follow state to a per-view registry, and reopening returns you
  exactly where you were, the same "leave and come back, nothing lost"
  contract daemon sessions already have. Migrated surfaces: Help,
  Activity/Todos, Tools, MCP, Skills, Permissions, Usage stats, Context
  report, `/btw` asides, Settings, the Models and Connections pickers,
  input history (Ctrl+R), the Queue overview, the session dashboard, and
  the sessions picker. Data-refresh side effects (the usage-stats query,
  session-context snapshots) run on a view's *first* open only. Switching
  sessions forgets retained view state — it belongs to the conversation,
  not the terminal.
- **Picker→editor navigation is a stack.** The model editor, the
  provider-template chooser, the custom-provider editor, and the OAuth
  sheet return to the surface they were opened from through one bounded
  navigation stack (the single-slot `editor_return_to` field and the two
  hard-coded "back to Connections" links are gone). Composer drafts parked
  by the pickers live in per-view slots, so a draft parked for Models can
  never be clobbered by one parked for history (the one global stash is
  gone; only the input-injection sheet keeps its own).
- **Queue auto-block is a view enter/exit pair.** Opening the Queue view
  blocks the outbox for safe editing; *every* path that leaves it (Esc,
  outside click, Ctrl+C, switcher) releases the block.
- **Dashboard and sessions picker keep their place.** Hiding the dashboard
  preserves the dock selection and the cockpit log; a data refresh while
  the sessions picker is open no longer snaps the cursor back to the top.
- **`Ctrl+L` opens the global view switcher** over every surface (usable
  even while another modal is up): open views first in most-recently-used
  order, then every other view as discovery; typing filters the list
  fuzzily against names and entry points; `Enter` switches (the origin
  hides with its state saved), `Esc` cancels back untouched.
### Changed

- **Retired top-level CLI spellings are removed outright.** `neenee
  serve`, `stop`, `status`, `resume`, and `exec` no longer get a
  dedicated teaching error (ADR-0119's interim shim): a retired word is
  an ordinary unrecognized command (exit 2), and a multi-word retired
  phrase parses as a positional prompt like any other unknown phrase.
  `neenee session ls` likewise loses its pointer error — it is an
  unknown `session` subcommand like any other. The canonical forms
  (`neenee daemon start/stop/status`, `attach`, `run`, `session rm`)
  are unchanged. Retirement now deletes rather than teaches
  (ADR-0135).

### Fixed

- **A normally completed round is no longer mislabelled as
  "interrupted".** Two stacked defects made the durable `RoundInterrupt`
  projection fire on rounds the server-side LLM finished cleanly.
  First, the stop sites park their interrupt reason *unconditionally* —
  Esc Esc or a session switch (`/new`, `/resume`, `/sessions <id>`) parks
  one even when no round is live — and `RoundLifecycle::begin()` never
  cleared the slot, so the parked reason leaked into the *next* round's
  tail. Second, the tail persisted a record whenever *any* reason was
  parked, without consulting the round's outcome, so a natural model
  convergence that returned `Ok(())` still wrote `RoundInterrupt` +
  `RoundEvent::RoundInterrupted` — projecting a false
  `▲ interrupted · Esc Esc / new message` warning into the transcript
  (live and on resume) and folding the dashboard row to `Interrupted`.
  `begin()` now clears the slot, and `execute_round` returns a
  `RoundCompletion` (`Completed` / `Unsent` / `NotStarted`) so the tail
  records only genuinely-stopped rounds: an unwind (`Err`), the
  generation-suppressed supersede arm, or the phase-1 unsend. A late Esc
  Esc that lands after the round passed its last cancellation checkpoint
  (the model already converged) is dropped, not recorded. Regression
  tests pin all four shapes: idle park → clean completion, live park →
  completion, real interrupt, real supersede.

- **Daemon lifecycle errors name the canonical CLI spellings.** Every
  user-facing client/daemon incompatibility, lock-contention, and
  daemon-started hint (`client.rs`, `serve.rs`, `host.rs`, CLI `main.rs`,
  `daemon status --diagnostic`) told the operator to run `neenee stop`,
  `neenee serve`, or `neenee status` — top-level spellings ADR-0116
  retired with an error that teaches the canonical form. Following the
  old advice now fails with "'neenee stop' is now 'neenee daemon stop'",
  so the fix a message names must itself work. All messages now name
  `neenee daemon stop` / `neenee daemon start` / `neenee daemon status`,
  and the pinned tests assert the canonical spellings.
- **Selectable modal bodies no longer drop text on wrapped continuation
  rows.** `render_row_line` computed its per-segment split points against the
  row's *full* concatenated text but sliced the *wrapped* slice with them, so
  on any visual row past the first, a range that ran past the slice's end was
  silently skipped (`hi > text.len()`) and mid-row text vanished. The visible
  symptom was a permission-sheet header like `bash  ls | head` rendering with
  characters missing (`l | he`) at narrow widths; the same defect affected
  every selectable body that soft-wraps a multi-segment row (permission
  sheet, help, history preview, session detail, usage stats, token report,
  activity). Split points are now intersected with the wrapped slice's byte
  window before slicing, and a regression test pins a wrapped
  `bash  ls | head` header to its full text.
- **The double-Esc interrupt confirmation no longer flashes and
  vanishes.** Two defects made the armed window unreliable. First, the
  window was a 20-iteration loop counter, but the event loop wakes on
  every keystroke, mouse move, stream delta, and dirty-notify — far more
  often than its 100ms animation heartbeat — so the intended ~2s window
  could burn through in a few hundred milliseconds, and the "Esc again
  interrupts" toast disappeared before a second press could land. Second,
  the keep-alive check read the runtime's global `is_responding` flag,
  which tracks only the *primary* session: inside a `/btw` aside view
  (where Esc arms from the aside's own running round) the armed window
  was zeroed on the very next frame whenever the primary sat idle — the
  "first press did nothing" symptom. The window is now wall-clock
  (`App::ESC_ARM_WINDOW`, 2s, matching the Ctrl+C quit window that
  already made this exact migration) and view-scoped: it lapses on the
  deadline or when the *viewed* session's round ends, whichever comes
  first. Arming is also dropped when switching views or leaving an
  aside, so one session's confirmation can never fire another session's
  interrupt, and a lapsed window re-arms instead of firing a stale
  confirmation.

### Added

- **Dashboard console is a command surface (ADR-0097 §2–§3, first
  slice).** The `/dashboard` upper region — previously an orchestrator
  placeholder — is now a cockpit log with a command grammar. Typing on
  the dashboard opens the composer directly (any printable key seeds it;
  `p` opens it empty, `n` opens it in create mode). The line speaks the
  ADR-0097 address syntax: `@3 refactor the retry loop` sends to session
  `#3`, `@2 @3 text` fans the same prompt out to several sessions, and
  bare text keeps the classic "prompt the selection" role (or creates,
  when the line was opened with `n`). Slash verbs manage sessions from
  the same line:
  `/interrupt` (`/stop`), `/suspend` (`/park`), `/kill` (`/x`, also the
  two-press `k` dock key), `/new [text]`, `/help` — each accepting an
  optional `@N` to act without moving the dock selection. Every dispatch
  (verbs and the `i`/`s`/`k` keys alike) writes a receipt line into the
  console — `› [#3] prompt …`, `✓ #3 queued`, `✗ #3 … is not hosted` —
  so the log answers "what did I ask the fleet to do" without a
  re-attach. New dock keys: `k` kills the selection (press `k` again to
  confirm; any other key or a selection move cancels), `s` suspends it.
- **`suspend_session` control verb.** The control plane gains a
  memory-reclamation verb that parks a hosted session in memory without
  ending it: the driver is torn down, `SessionEnd` hooks do not fire,
  and the next attach rebuilds the session from its durable transcript
  via lazy resume (the same path the idle reaper uses, now available on
  demand). Refused — with an actionable error — while a client is
  attached, a round is active, or the session has no persisted content.
  The web panel's session rows gain interrupt (⏹) and suspend (⏸)
  actions alongside rename / end / delete.

### Fixed

- **`neenee dashboard` no longer leaks into the carrier conversation on
  Ctrl+C.** The startup dashboard is the app while it is open: leaving it
  must exit the whole TUI. `Esc` already quit, but `Ctrl+C` hit the generic
  modal-close arm, dismissed the dashboard, and dropped the user into the
  carrier session's chat — a conversation they never asked for. The
  dashboard now owns Ctrl+C with the same double-press contract as the
  conversation view: the first press arms a 2s quit window (the "press
  Ctrl+C again to exit" toast), the second exits the entire TUI. With text
  staged in the dashboard's inline prompt (`p` / `n`), the chain is three
  presses like the composer (clear, arm, quit). The in-session `/dashboard`
  gets the same gesture; its second press declares the session end exactly
  as the conversation's double Ctrl+C does (ADR-0112), while the startup
  screen's quit stays detach-flavoured so the carrier session survives.

### Changed

- **Autopilot posture is now session-persisted (ADR-0132).** The
  unattended/attended flag moved from a process-local in-memory bool to
  session-scoped persisted state (`SessionEvent::AutopilotSet`), following
  the ADR-0048 Phase 2 pattern. A daemon that dies mid-unattended-task —
  crash, kill, upgrade, reboot — now reopens the session **unattended** when
  it is re-hosted (attach, lazy-resume, or boot rehost), instead of silently
  de-escalating to attended and parking the next side-effecting tool on a
  permission modal nobody is watching. Every write path persists
  (`/autopilot on|off`, the `--autopilot`/`-y`/`--yolo` startup flag —
  which previously never wrote the store or the command ledger, closing the
  widest recovery gap — and `/principal <role>` switches); every restore
  path restores, and the WS attach snapshot now publishes the real posture
  in its first frame instead of a hardcoded `false`. `/reset` starts the
  new session attended (the old session's posture is not inherited), a
  posture toggle alone never materialises an otherwise-empty session file,
  and de-escalations are as durable as escalations (`/autopilot off`
  persists; the last write wins across restarts). Sessions created before
  this change are recovered from the command ledger (last
  `Autopilot ON`/`OFF` ack) with a loud "Autopilot restored" notice naming
  the recovery source.

### Changed

- **Models picker list is now three labeled sections.** The `/models` /
  `Ctrl+M` modal — one row per (provider, model) pair across every connection
  — now groups its rows into **Favorites**, **Recent**, and **All models**,
  each announced by a dim uppercase label row (`FAVORITES` / `RECENT` /
  `ALL MODELS`) that the ↑/↓ cursor skips; an empty section renders no label.
  Ordering inside the sections: Favorites and All models sort ASCII by the
  model id (provider label as the tiebreaker); Recent sorts most-recently-used
  first. Precedence is favorite > recent > rest — a star is pinned user intent
  and beats the emergent recency signal. The currently-active pair is no
  longer pinned to the top of the list: it keeps its natural section position
  and is identified by its `●` glyph (the modal still opens with the cursor on
  it). To feed the Recent section, per-model usage recency
  (`ProviderModelInfo.last_used_ms`) is now surfaced in the picker snapshot —
  the signal was already tracked per model in the usage store, it just never
  reached the UI. Fuzzy search keeps the same grouping over the filtered rows.

## [0.30.4] - 2026-08-22

### Added

- **Selectable text in documentary modals — converged on one component.**
  All modal documentary bodies now render through
  `components/selectable_body.rs`, the single selectable-document path:
  Help (`?`), Usage Statistics (`/usage`), the Context Usage round drill-in,
  the Activity modal (both tabs, including todo items), the Sessions `i`
  info sub-view, the History `Tab` full-text preview, the permission sheet's
  body (tool description + arguments JSON), the OAuth pending sheet, and
  every in-modal `?` keymap sub-page. Drag across rows to highlight,
  `Ctrl+Shift+C` (or `Cmd+C`) to copy — the same interaction as the
  transcript. A press on modal text no longer dead-ends; it arms a drag.
  Outside-click dismiss, buttons, and all click affordances keep their
  previous behaviour. Decoration (indents, todo status glyphs) paints as row
  prefixes that stay out of copied text. Picker-style modal *lists* (Models,
  Connections, Tools, …) stay deliberately non-draggable — their rows are
  keyboard targets; the content-vs-control split is documented in the TUI
  reference. The OAuth sheet's previous hand-rolled region registration was
  retired in the process: it recorded one region per logical row while
  wrapping happened inside the engine, so wrapped continuation lines and
  scrolled views misaligned — regions are now anchored per visual row after
  application-layer wrapping, which also removed the now-unused
  `indented_wrapped_lines` pre-wrap helper.

### Fixed

- **`/retry` after a crash.** A session whose host process died mid-round
  (SIGKILL, panic, power loss) previously answered `/retry` with "Nothing to
  retry — the last round already completed": the resume point was armed only
  on stops the round path could observe, and the startup crash-residue check
  filtered for a status (`Abandoned`) that is never present in the session
  store on first reload — `TokenSourceLedger::restore_session` flips
  in-flight records only in its in-memory map. The driver now treats a
  still-`InFlight` usage record in the store as the crash signal, records the
  `Terminated` round interrupt, and arms a `/retry` resume point (round from
  the record, committed turns recovered from the transcript, history
  watermark from the durable window) before the idle harness snapshot is
  published — so a re-hosted session offers `/retry` from frame one. This
  covers graceful daemon kills and a crash during a `/retry` resume too (the
  resumed round keeps its number); only a round the session has already
  moved past is left un-resurrected.

## [0.30.3] - 2026-08-21

### Fixed

- Retry-checkpoint integration tests now compare provider-visible semantic
  history rather than request-projection timestamps, eliminating failures
  caused by coverage instrumentation and slower Windows runners.

## [0.30.2] - 2026-08-21

### Added

- **Native Windows support.** The local control plane uses a current-user-only
  Named Pipe, cross-process locks use `LockFileEx`, owned subprocesses are
  contained in kill-on-close Job Objects, private files receive protected
  user DACLs, and user scripts run through non-interactive PowerShell.
- Windows check/test jobs, an `x86_64-pc-windows-msvc` release zip, SHA-256
  release sidecars, and a checksum-verifying PowerShell installer.

### Changed

- Platform-neutral daemon, persistence, and process policy now depends on the
  small native capabilities in `neenee-platform`. XDG remains Linux's native
  placement and a portable override vocabulary; macOS and Windows use their
  native default directories.
- Unix/macOS/Linux installs now verify the release SHA-256 before replacing
  the executable.

### Fixed

- Non-Unix process locks and lock probes no longer report success without
  mutual exclusion. Windows atomic writes now replace existing files instead
  of failing on `rename` semantics, and subprocess timeouts terminate the
  whole descendant tree instead of only the direct process.

## [0.30.1] - 2026-08-21

### Fixed

- **Daemon auto-start works again on Unix.** Version 0.30.0 configured a
  spawned daemon with both `process_group(0)` and `setsid(2)`; the former
  made the child a process-group leader, which requires the latter to fail
  with `EPERM`. Both explicit `daemon start` and on-demand startup now share
  one detachment primitive that calls `setsid(2)` alone, with regression
  coverage for the resulting session and process-group identities.
- Updated `h2` to 0.4.16 and `webbrowser` to 1.2.2, resolving
  RUSTSEC-2026-0258 and RUSTSEC-2026-0257 without advisory exceptions.

## [0.30.0] - 2026-08-21

### Fixed

- **Doom-guard defaults were documented backwards in three places.** The
  guard has been **on by default** (`window: 16`) since ADR-0113 §5, but the
  `DoomGuardConfig` module docs, the `PrincipalConfig.nudge` field rustdoc,
  and `configuration.md` all still said "default disabled / opt in". The
  canonical TOML key is now `[principal.doom_guard]`; the historical `nudge`
  spelling still loads (serde alias — an explicit `enabled = false` under
  the old key survives, since silently dropping it would flip the user's
  opt-out back to blocking) and saves write the new key.
- **`read_file` → `read_text` across the docs.** The tool was renamed long
  ago (its own description says `read_text`); ~15 doc pages still taught the
  dead name.
- **Removed the phantom `search_history` tool from the docs** — deleted in
  0.24.0 but still fully documented (parameter table and a source file that
  no longer exists).
- `docs/reference/commands.md` documented the **hidden alias** `/config`
  and omitted the canonical `/settings` and `/retry`; the trigger-word
  table listed 3 of 8 rows. `paths.md` listed `model_usage.json` as a live
  State file while also listing it as removed, and never documented
  `themes/`. `cli.md` omitted `-p/-i/-y/-j`. The README key table omitted
  the queue family. A new test (`commands_reference_table_matches_builtin_registry`)
  pins the command table to `BuiltinCmd::ALL` so the markdown can no longer
  drift silently.
- Wrong ADR citations: the mid-turn save point was attributed to ADR-0035
  (an unrelated, superseded ADR) in five code comments and two ADR
  reference lists; it is specified by ADR-0048.

### Changed

- **Bounded log retention.** `tracing_appender::rolling::daily` rotates but
  never deletes — the state log directory grew forever. Replaced with an
  in-house daily-rolling writer that keeps the newest files only
  (`NEENEE_LOG_RETENTION`, default 14).
- **`route_settings` moved out of the cache.** The user's per-(instance,
  model) reasoning overrides lived in `$XDG_CACHE_HOME/models_discovery.json`
  — a cache is derived and deletable, user settings are not. They now live
  in `$XDG_STATE_HOME/neenee/route_settings.json`, with a one-shot,
  idempotent migration that folds the old cache map in and clears it there.
- **Web-search API keys moved out of `config.toml`.** The six `[websearch]`
  keys are secrets; `config.toml` is behavior-only and shareable. They now
  persist in `credentials.toml [websearch]` (merged at load; a one-shot
  migration moves keys found in a pre-split `config.toml`, where an explicit
  credentials entry wins). Serialization of `WebSearchConfig` no longer
  emits them, so a saved config is safe to share by construction.
- **`/search` is honest lexical ranking.** The embedding-index machinery
  (persisted vectors from a hash-based `MockEmbeddingProvider`, dedup set,
  union-merge save, full-file rewrite per search) was real cost with no
  semantics. `/search` now ranks the live transcript and command ledger
  with deterministic lexical scoring — no index file, nothing persisted —
  until a real embedding provider is wired in. Deleting a session now also
  prunes its entries from the project embedding index (they previously
  survived forever via the union-only merge).
- **Blob garbage collection.** The content-addressed blob store had no
  reclamation path: deleting, forking, or compacting sessions orphaned blobs
  forever. `BlobStore::collect_garbage` marks every `content_blob`
  reference reachable from all project buckets' snapshots (a conservative
  textual scan — no schema coupling) and sweeps the rest. The daemon's idle
  reaper runs it at most once a day on the blocking pool, alongside usage
  day-file retention (400 days).
- **Bounded `/debug trace` capture retention** (newest 50 per directory):
  each capture is the full request context of one round-trip, so an armed
  trace on a long session previously grew the data dir faster than every
  other path combined.
- **Per-persist costs no longer grow with session age.** `EventLog::high_seq`
  — called on every snapshot persist to stamp the `applied_seq` watermark —
  re-read and re-parsed the whole event log each time; it is now cached and
  maintained by `append`/`rewrite` (O(1) after the first call). The
  request-ledger diff in `set_request_usage_records` was quadratic
  (`any`-inside-`any` plus a `find` per record); it is now computed through
  a key→record index. `upsert_record` in the usage store binary-searches
  the key-sorted day file instead of scanning it.

### Added

- **`neenee config check`** validates `config.toml` against the schema:
  hard syntax/type errors that made a load silently fall back to defaults,
  unknown keys (a typo silently meant "default"), and known dead legacy
  spellings with what replaced them. The unknown-keys-ignored policy stays
  — this restores the signal it traded away.
- **`--config-dir` / `--data-dir` / `--state-dir` / `--cache-dir`** CLI
  flags, wiring the per-category tier ADR-0014 §3 specified but never
  shipped. Each is the CLI form of its `NEENEE_*_DIR` env var and wins over
  `--home` for its own category; the pre-parser restates them as env vars
  for child processes, exactly like `--home`.

### Removed

- Dead code the docs had already disowned: the pre-ADR-0096 per-project
  `serve/<bucket>.json` write path (the docs called it "harmless litter"
  while code and tests kept it alive), the unused `Dirs::project_lock_file`
  and `Dirs::project_migration_lock` (their consumers were removed by
  ADR-0116), and stale crate docs referencing `neenee-core`, a per-project
  single-instance flock, and a `neenee-trading-store` sibling.

### Changed (tests)

- The tokenizer corpus tests **fail on a missing corpus file** instead of
  silently skipping (`let Ok(..) = read else { continue }` had been hiding
  wrong paths *and* stale pinned counts for the entire life of the test —
  both were wrong and it stayed green).
- The untrusted-hardening test asserted the regex **source text** contained
  substrings; a real match test now lives where `regex` is (the agent
  crate), covering 14 injection payloads plus benign-command non-matches.
- The `/schedule` split-spec test helper propagated errors instead of
  defaulting to empty strings, which turned every parse regression into a
  vacuous green.
- The relative-XDG-var test pins which documented fallback actually ran
  (it previously accepted both outcomes with an `||`).

### Changed

- **The queue family moved off the F-row to the Ctrl row (ADR-0126).**
  `Ctrl+Q` opens the Queue modal (was `F2`), `Ctrl+P` blocks/resumes the
  outbox (was `F3`, *pause*), and `Ctrl+O` inserts into the running round
  (was `F4`, *open into the round*). Fn-dispatch is OS/terminal policy the
  app cannot rely on — terminals, window managers, and browser embedders
  freely remap or reserve the F-keys, which is why the old bindings silently
  failed to work on many setups. Ctrl chords are distinct bytes under raw
  mode, survive tmux/screen, sit one row above `Enter`, and carry mnemonics.
  `F5` (the `/btw` asides list) keeps its slot.
- **A mid-round insert (`Ctrl+O`) is a transcript entry from the moment it is
  sent (ADR-0126).** The message lands in the scrollback immediately as a
  user panel in the pending treatment (`⏸ Queued` header, dimmer band) —
  visibly blocked on the running turn — without interrupting the running
  turn's own streaming entry below it. It settles in place (same row, no
  duplicate): admitted at a safe turn boundary → delivered with `↳ insert`
  provenance; round ended first (natural completion *or* an `Esc Esc`
  interrupt) → `⏸ Held for next round`, and its content joins the outbox as
  a paused next-round item that ships as the next round's prompt. Inserts
  are no longer outbox items and accept **no queue operations** (no recall,
  edit, delete, reorder, or cancel) — they have already entered the
  conversation. The `Inserting` state and the `steer›` badge are gone with
  the shadow item they existed for.
- **`InsertUserInput` no longer drops staged images** — the old send path
  hardcoded `images: Vec::new()`, silently discarding anything pasted with a
  `Ctrl+O` steer; the staged payloads now ship with the request.

### Added

- **Round interrupts are now recorded durably — with the reason and the
  timestamp — and projected back into the transcript on resume
  (ADR-0127).** Every path that stops a round before it completes now
  writes one `RoundInterrupt` record to the session store and emits the
  live `RoundEvent::RoundInterrupted`: an explicit **Esc Esc** (or the
  control-plane `Interrupt`), a **superseded** round (a new message, a
  `!command`, or a session switch killed the running one — previously
  completely invisible), and **termination** (daemon stop/kill, or a hard
  crash inferred at next load from the abandoned in-flight request). The
  TUI renders each as its own warning entry — `▲ interrupted · HH:MM`
  over `round N · Esc Esc` / `new message` / `process exited` — the web
  panel shows an equivalent row, and `neenee -p` prints
  `[Interrupted · <reason>]` (or a `round_interrupted` JSON event). On
  resume the markers re-project at their timestamp seams, so a restored
  session answers "this round stopped, here is why and when — continue?"
  at a glance. The records are projection state like the command ledger:
  they never enter the model-visible context (the deliberate no-marker
  decision is unchanged) and cost zero tokens. The dashboard row now also
  lands on `Interrupted` (with the reason as its note) for *every*
  interrupt phase, not just the phase-1 unsend.
- **Queue pointer: `↑`/`↓` walk the outbox non-destructively, and `Enter`
  edits a queued message in place (ADR-0126).** With staged messages,
  `↑` now arms a *pointer* at the newest queue item and projects its content
  into the composer (further `↑` step toward older items; `↓` steps back and,
  past the newest, restores the stashed draft) — nothing leaves the queue, so
  the old pop-pop-pop recall dance is gone. `Enter` writes the edited content
  back into the pointed-at item **in its own slot**: editing `a` of
  `[a, b, c]` into `d` yields `[d, b, c]`, never `[b, c, d]` and never a
  duplicate. If the pointed-at item shipped while you were editing (its round
  completed behind your back), the pointer is treated as empty — your edit
  stays in the composer and `Enter` sends it as a fresh message (queued if
  the session is busy), so the gesture never dead-ends. The queue is walked
  *before* input history; only an exhausted queue hands `↑` on to history
  recall.
- **Autonomous sessions come back with the daemon (ADR-0125).** On boot the
  daemon scans every project's persisted sessions (header-only, no
  transcript decode) and rehosts each one that still has armed `/schedule`
  jobs through the ordinary lazy-resume path, so scheduled prompts keep
  firing across daemon restarts (crash, upgrade, reboot) instead of waiting
  for a human to attach. Rehosted sessions appear in the dashboard like any
  hosted session; a missing project root or a failed assembly leaves that
  session dormant (it still lazy-resumes on attach) and never blocks
  startup. Opt out with `[daemon] rehost_armed_schedules = false`.

### Changed

- **A detached daemon now calls `setsid(2)` (ADR-0125).** Both spawn paths
  (auto-start and `neenee daemon start`) previously used `process_group(0)`
  only — a new process *group*, not a new *session* — so the daemon stayed
  in the spawning terminal's session and was SIGHUPed (then drained, per
  ADR-0101) when the terminal or its compositor exited. The daemon now
  detaches the way tmux's server does: closing the terminal, the window, or
  the compositor leaves it and every hosted session running. `kill -HUP`
  semantics are unchanged.
- **Armed `/schedule` jobs are never idle-suspended.** A session with
  pending scheduled work is not idle in the meaningful sense: suspending it
  would park its tick loop and silently stop the schedule from firing. The
  idle-suspension sweep now exempts these sessions (their memory stays
  bounded by the suspension of everything else).
- **Phase-1 unsend restore no longer clobbers an in-progress draft.** The
  `UnsentInput` composer restore is asynchronous, so it now only adopts an
  idle composer: a half-typed draft the user was composing while the round
  ran wins, and the interrupted prompt stays recoverable from the input
  history (`Ctrl+R` / `↑`). Explicit gestures — queue recall and Ctrl+R
  insert — still replace the draft. `App::adopt_as_draft` takes an explicit
  `DraftAdoption` policy (`Replace` / `OnlyIfIdle`) so each path declares
  its overwrite semantics instead of sharing one unconditional behaviour.
- **The Phase-1 unsend guard is a named, unit-tested predicate.**
  `is_phase1_unsend` (`orchestration.rs`) replaces the inline sentinel
  check, documenting the boundary it enforces: the unsend window closes at
  the first observed content delta or tool call, whichever comes first —
  not at the first network response packet (transport noise arrives long
  before any model output) and not at "request still local" (wire state is
  unobservable at the harness layer). See the new boundary discussion in
  `docs/explanation/interrupt-semantics.md`.
- **TUI unsend restore now shows a "Prompt not sent" toast**, matching the
  web panel's feedback instead of silently refilling the composer after the
  transcript row was popped.
- **Web panel: the `UnsentInput` restore no longer clobbers in-progress
  typing either.** The composer reports its idleness to the daemon store
  (`composerIdle`); an unsend adopts only an idle composer, and when the
  user is mid-composition the optimistic echo stays in the transcript (the
  only visible copy to re-copy from) with a toast explaining where the
  prompt went. Same policy as the TUI's `DraftAdoption::OnlyIfIdle`.
- **`UnsentInput`'s contract doc now says the composer restore is
  advisory** (`events.rs` doc comment, `server-api.md`,
  `server.asyncapi.yaml`): the harness has already reverted the
  conversation, so clients own how to surface the prompt.

### Fixed

- **`/schedule` jobs are no longer consumed when their harness is gone
  (ADR-0125).** `run_schedule_tick` used to fire-then-mutate: when the
  session's driver channel was already torn down (suspended, killed, daemon
  draining), the send silently failed *after* the job had been advanced — a
  cron lost one interval per tick and a once-job vanished unrecoverably.
  Dispatch is now deliver-first/mutate-second: an undeliverable fire leaves
  the job armed for the next harness.
- **The `/schedule` scheduler task no longer leaks past session teardown
  (ADR-0125).** It was spawned fire-and-forget with no cancellation token,
  so suspension/kill left it ticking against a dead channel every 30s for
  the daemon's remaining lifetime. It now shares the hosted session's
  teardown token with the driver.
- **Stale `Ok(false)` in the Phase-1 unsend docs.** `execute_round` returns
  `Result<(), HarnessError>`; the code comment and
  `interrupt-semantics.md` still described a bool-returning signature from
  an earlier revision.

### Security

- **Per-hop SSRF guard for the web tools (ADR-0124).** Redirects are no
  longer followed by reqwest automatically: `guarded_get` follows them in
  async code and re-runs the SSRF pre-flight on every hop, so a public URL
  answering `302 → http://169.254.169.254/…` is refused mid-chain instead of
  being followed into the cloud metadata endpoint. The guard now also blocks
  IPv6 link-local (`fe80::/10`), TEST-NET-1/2/3, `192.0.0.0/24`, and
  `240.0.0.0/4`. Response bodies are streamed with an 8 MiB hard cap — the
  builtin reader previously buffered the entire body before truncating.
- **Untrusted-content boundary for web results (ADR-0124).** `webfetch`
  output is wrapped in `[BEGIN/END UNTRUSTED WEB CONTENT]` markers and a new
  `system.web_untrusted_content` prompt section (active whenever a web tool
  is admitted) teaches the model to treat fetched pages and search snippets
  as data, never as instructions — closing the prompt-injection surface of a
  shell-holding agent. The Tavily API key moved from the request body to
  `Authorization: Bearer`, keeping it out of error-body echoes.

### Changed

- **Web tool output is token-budgeted, never mid-entry chopped
  (ADR-0124).** `SearchProvider` now returns structured results
  (`ProviderOutput::Results`/`Blob`) instead of a pre-rendered string, and
  the tool layer owns the 4 000-token budget: titles and URLs are never
  truncated, snippets degrade to title+URL first, and only then are tail
  entries dropped with a notice naming the count. Exa requests 5 results
  instead of 10 (measured: 10 results ≈ 14k tokens, previously chopped to
  ~28% with the tail URLs lost). `webfetch` moves from a 16 000-byte cap to
  the same 4 000-token cap; its truncation notice now suggests a narrower
  URL/anchor instead of the misleading `raw=true` (which never raised the
  cap).
- `webfetch`/`snapshot` send a browser User-Agent (matching the search
  backends) instead of `neenee/0.1`, which anti-bot layers rejected more
  often. Unknown `[websearch] provider`/`fallback` names log a warning and
  fall back to Exa instead of failing silently. The `websearch`
  description's "current year" is computed per process, not per tool
  construction, so long-lived daemon sessions keep the right year.

## [0.29.1] - 2026-08-20

### Changed

- **Provider instances are state; routes are derived, never persisted
  (ADR-0123).** `config.toml` is now behavior-only: provider *instances* moved
  to a state store (`$XDG_STATE_HOME/neenee/providers.toml`), credentials are
  keyed by instance (`credentials.toml [providers.<id>]`, replacing
  `[builtins.<id>]` / `[user.<id>]`), and per-model routes (transport/
  endpoint/reasoning) are derived at runtime from each instance's template
  plus the discovery cache — the `[[providers.channels]]` concept and the
  legacy top-level `*_api_key` / `*_base_url` / `*_model` fields are gone. Two
  instances of the same template no longer duplicate or drift a route set;
  per-(instance, model) reasoning lives in the discovery cache
  (`route_settings`), not `config.toml`. A one-shot migration converts the
  legacy layout automatically on first launch; `neenee auth` / `neenee config`
  read the new stores.

## [0.29.0] - 2026-08-20

### Added

- **`/usage` — cross-session usage statistics that survive session cleanup
  (ADR-0122).** A new overlay shows daily token totals (with a two-week bar
  chart), a per-`(provider, model)` breakdown sorted by usage, and the
  recent terminal-request event log (state, model, tokens, local time).
  The data comes from a new day-partitioned append-only store at
  `data/usage/daily/<YYYY-MM-DD>.json` — a **sibling of `projects/`**, never
  inside a session file — so deleting sessions or pruning project buckets
  can never touch it: the report reflects each day's real consumption
  forever. Records are mirrored from the existing token-ledger settlement
  point (`TokenSourceLedger::settle_request` forwards every terminal
  attempt to a `UsageStatSink`), keyed idempotently by request identity
  (a reported replay upgrades an earlier estimate, never the reverse),
  written atomically and lock-serialised across processes. Interrupted and
  failed attempts are included (marked), so the daily number is honest
  about what was actually requested. Fetched over the control plane via
  `AgentRequest::QueryUsageStats` / `AgentResponse::UsageStatsReport`, so
  the web panel can reuse the same aggregate.

### Changed

- **The Context Usage modal now shows throughput at both scopes — per round
  and per session — instead of only the latest round's.** The round table's
  **Turns** column was replaced by a **TPS** column: this round's average
  output rate (the round's output tokens ÷ the generation time its attempts
  actually measured, `–` when nothing was timed). The turn count it replaced
  still lives in the drill-in's "Turns / attempts" row. The top **Output
  rate** row is now the *session-wide* average — Σ output tokens ÷ Σ measured
  generation time over every terminal attempt, a time-weighted mean rather
  than an average of per-round rates — and it no longer goes stale waiting
  for a `RoundCompleted` event: both figures are derived from the token
  ledger the modal already reads, so they agree by construction with each
  other and with the drill-in's per-attempt `tok/s` column. With turn
  (`tok/s` in the drill-in), round (`TPS`), and session (`Output rate`)
  rates all visible, the modal's `RoundSummary` plumbing was retired.

- **The input box's completion menu now behaves like every IDE
  autocomplete.** The moment the `/slash` or `@path` popup appears its
  **first candidate is selected by default** — the solid brand highlight
  band and the details flyout track it with no prior keystroke, `↑`/`↓`
  move the highlight, and `Enter` commits it. A new "anchor" pass keeps
  the selection coherent wherever the candidate list is re-derived (per
  keystroke, after each dispatched action, at the render gate): a fresh
  menu seeds `Some(0)`, a stale index clamps into range when the list
  shrinks, and no rendered menu (a resolved exact-match composer, a
  dismissed popup) clears the highlight — so the highlighted row can
  never point at a menu that is not on screen. The `Path` menu inherits
  the same Enter-commits-the-highlight contract the slash menu already
  had.

- **`Tab` is now the completion commit (same as `Enter`) and the other
  half of the `Esc` toggle.** While a menu is open, `Tab` commits the
  highlighted candidate instead of silently cycling to the next one
  (cycling is `↑`/`↓`'s job). After `Esc` dismissed a popup, `Tab`
  re-opens it — landing selected on the first candidate again — provided
  the composer still holds trigger text (a partial `/command` or a live
  `@mention`); a resolved exact command never resurrects its menu.

## [0.28.0] - 2026-08-20

### Changed

- **The command surface finishes its convergence (ADR-0119's principle
  applied to its own leftovers).** `session ls` is retired — the session
  table is the daemon's view, so `neenee daemon status` is its one home
  (`neenee session ls` now refuses with a pointer, and `neenee session`
  teaches `session rm <id>` instead of silently listing). `mcp` and
  `skill` gained the noun-verb shape before growing a second action made
  it a breaking change: `neenee mcp ls` / `neenee skill ls` (aliases
  `list`), with the bare nouns refusing and teaching. `panel` now says
  what it does: `neenee panel [url]` prints the URL (the historical
  behaviour, kept as the bare form), and the new `neenee panel open`
  additionally launches the platform browser (`$BROWSER`, else
  xdg-open/open). Help no longer advertises `--yolo` ahead of the
  canonical `--autopilot`.

- **`--remote` / `--token` are real: they no longer parse and then
  vanish.** Headless `neenee run --remote <host:port> --token <t>` now
  connects to the explicitly named daemon directly — no local discovery
  read, no on-demand spawn of a local daemon — over TCP+bearer, with the
  address accepted as `host:port`, `ws://host:port`, or a bare `:port`
  for loopback. A missing port or token is an actionable error rather
  than a silent well-known default (which would target the local daemon
  while appearing to be remote).

- **`neenee daemon start` (detached) no longer drops its own flags.**
  `--port`, `--public`, `--no-local-auth`, `--idle-exit`, and `--grace`
  now survive the detach and reach the foreground child — previously
  `daemon start --port 9809` silently bound the default (or its ephemeral
  fallback), because the supervisor re-invocation passed none of them.
  Found by the new CLI smoke; pinned there.

- **A CLI-surface smoke gate joins the e2e job**
  (`apps/web/e2e/cli-smoke.sh`): the binary's own contract — retired
  spellings teach, noun-verb shapes parse, `--remote` validates and
  connects — exercised against a live daemon in a throwaway instance
  root, complementing the protocol-level `daemon-smoke.mjs`.

- **Tokens are the first-class unit everywhere (ADR-0120).** With the exact
  BPE tokenizer in place, every char/byte-denominated budget, marker, and
  display in the pressure → prune → compact pipeline is token-native now:
  - `prune_tool_results` takes `protect_recent_tokens` /
    `min_reclaim_tokens` (the byte accumulator it replaced under-protected
    CJK sessions 3–4×); `PruneOutcome::reclaimed_tokens`; tier thresholds
    `TRUNCATE_MIN_TOKENS = 512` / keep-each-side 128 tokens.
  - Model-visible markers tell the truth in the model's own unit:
    `[cleared tool result: … (42 lines, 350 tokens)]` and
    `[… N tokens elided …]` (recognizers match a stable substring, so
    pre-ADR-0120 records still escalate truncate→clear).
  - The compaction summary pipeline is token-bounded end to end
    (`truncate_to_tokens`, the new exact token-boundary cut):
    `summary_char_budget` and its `target × 4` round trip are gone, the
    excerpt fallback / summarizer transcript / envoy caps are token
    budgets, and the binary-search `truncate_summary_to_token_budget`
    collapses into the exact cut.
  - `Compacted` (wire + persisted checkpoint) carries
    `window_tokens_before`/`window_tokens_after` — point-in-time samples
    of the active window around the projection, named subject-first so
    the pair sorts together and reads as one measurement. Previously
    byte values labeled chars, shown as bytes in the TUI and chars on
    the web: four ways of being wrong.
  - Transcript displays report tokens: `Thinking · N tokens` (TUI +
    docs), `[Output truncated: N tokens total]`, search truncation and
    `webfetch` framing, `Successfully wrote N tokens`, and the transport
    decode-error preview's omitted-tail count.
  - The `× CHARS_PER_TOKEN` conversions on the already-token
    `compaction_prune_protect_tokens` config are deleted (orchestration
    and bootstrap pass it through); `PRUNE_MIN_RECLAIM_TOKENS = 2_000`
    replaces the 8 000-byte floor.
- **The char-class estimator and `CHARS_PER_TOKEN` are deleted** (ADR-0120):
  the tokenizer is total, so the "cheap fallback" had no failure mode to
  fall back from and no production caller. `estimate_bytes` survives only
  as the `/debug preview` wire-size diagnostic, where bytes are the
  honest unit.
- **No compatibility aliases for renamed persisted/wire shapes** (the
  "erase over compat" policy, ADR-0120): the serde aliases that mapped
  old event tags (`compaction_committed`, `repeat_jobs_set`,
  `turn_counter_set`), old snapshot keys (`messages`,
  `archived_messages`, `last_relief`, `compaction`, `repeat_jobs`,
  `turn_counter`, `compaction_preserve_turns`), and the byte-era
  checkpoint fields onto current names are removed, along with the
  legacy `PRUNED_TOOL_PLACEHOLDER` string and the substring-matching
  truncation recognizer. Old records fail to parse by design: the event
  loader skips those lines with a warn, an unparseable session snapshot
  starts fresh (loudly warned), and pre-rename config keys fall back to
  defaults. Regression tests pin the skip/drop behavior so aliases
  cannot silently creep back.

### Added

- **Isolated instances for development and testing (`--home`,
  `NEENEE_HOME`, `NEENEE_PORT` — ADR-0121).** One selector with two
  entrances: the global `--home <dir>` flag and the `NEENEE_HOME`
  environment variable name the same **instance root**. Either gives
  neenee a completely separate footprint — config, credentials, sessions,
  skills, logs, and the daemon's socket/lock/discovery record under
  `<dir>/neenee/` — so a checkout's debug builds and test suites can run
  beside a live installed daemon without reading, writing, stopping, or
  spawning into its state. `NEENEE_PORT` overrides the well-known 9800
  default (below an explicit `--port`). A sandboxed client's auto-spawned
  daemon inherits the sandbox by construction, and `neenee daemon status
  --diagnostic` now leads with the resolved instance root and default port
  (making "two daemons, one discovered" a one-command diagnosis). With no
  root set, path and port resolution are byte-for-byte the previous
  behaviour.

### Changed

- **The CLI speaks one verb per action — the daemon noun owns its
  lifecycle (ADR-0119).** `neenee daemon start [--fg] | stop | status` is
  the canonical surface; the retired top-level `serve`/`stop`/`status`
  spellings are refused with an error naming the canonical form instead
  of silently accepting both forever. `daemon start` **detaches by
  default** (the verb asks for a daemon); `--fg` is the supervisor shape
  and is what `assets/neenee.service` and the on-demand self-spawn now
  use. `resume` merged into `attach`, `exec` into `run`, and `session`
  narrowed to `ls`/`rm` (joining a session is `attach`; the dashboard is
  `neenee dashboard`).

- **The command-line parser moved out of the session runtime.** What was
  `neenee_runtime::startup::parse_args` (with two hand-maintained flag
  tables for `serve` vs `daemon start`, hand-written help text, and three
  hand-maintained completion scripts) is now `neenee-cli`'s `cli.rs`: a
  declarative spec table that drives parsing, help, "did you mean", and
  the bash/zsh/fish completions from one source. The runtime's
  `StartupMode` (24 variants) shrank to `SessionStart` (4 — the shapes a
  session assembly actually consumes); `--single-instance`, a flag parsed
  and then discarded since the unified daemon (its registry call site
  hardcoded `false`), is now refused with an explanation and its plumbing
  is deleted.

- **`neenee attach` with no id opens the real session picker.** The
  daemon offers a new `AttachAction::Picker` that assembles a throwaway
  carrier session (no restore, no hooks); the client's TUI raises the
  sessions modal over it, and `/sessions <id>` switches to the chosen
  session through the ordinary re-attach path. Previously this path
  printed a session list on **stderr** and exited, leaving the user to
  copy an id by hand — the picker modal existed but was never wired to
  it. `Attach(None)`'s auto-bind of a lone session is unchanged.

### Fixed

- **`neenee daemon stop` no longer force-kills a daemon that is draining
  within its own budget.** The daemon now publishes its configured drain
  budget as `grace_secs` in the discovery record, and the stopper's tier
  pipeline (verb → SIGTERM → SIGKILL) waits *that* budget at each
  graceful tier instead of a hardcoded 2s: any signal arriving mid-drain
  escalates the daemon's `ShutdownGate` to a forced exit, so the old
  timing made a daemon with legitimately slow `SessionEnd` hooks
  (configured grace 10s, hooks up to 5s) skip the very teardown the stop
  requested. Records predating the field fall back to a 15s constant —
  generous against the 10s default, so a legacy record cannot cause the
  same regression.

- **The stop pipeline's Tier-4 cleanup no longer unlinks a successor
  daemon's UDS socket.** The discovery-record removal has long been
  pid-guarded (`remove_if_matching_pid`); the socket removal now is too
  (`uds_belongs_to_pid`): the file is removed only when the recorded
  daemon is dead *and* nothing answers on the path, so a daemon spawned
  during the stop window keeps its socket.

- **The single-instance lock wait tells the truth about its budget.**
  `wait_for_lock` now waits `max(grace, 10s) + 5s` instead of a hardcoded
  15s whose comment claimed it was "a fraction of that daemon's own grace
  budget" (it was larger than the default grace and too small for a
  long-grace daemon). The floor matters: the *predecessor's* grace is not
  knowable from the lock file, and the original 15s was sized to cover
  the general case.

### Added

- **Native `cl100k_base` BPE tokenizer for token prediction
  (ADR-0117).** The char-class estimator behind the context meter, the
  pruning/compaction triggers, `/context`, and the tool-schema overhead
  estimate is replaced by a real byte-level BPE implemented natively in
  `neenee-contracts` (`tokenizer.rs`), following OpenAI's `tiktoken`:
  a hand-rolled scanner for the cl100k pretokenizer regex (no `regex`
  dependency) plus tiktoken's `byte_pair_merge` over a compactly packed
  100 256-rank vocabulary embedded from `vendor/cl100k_base.packed`
  (1.04 MB, ≈35% smaller than the published `.tiktoken` file). On this
  repository's own CJK + Rust corpus the old estimator was off by
  −24…−54%; the tokenizer is exact for `cl100k_base` and cross-validated
  against an offline tiktoken reference (counts and pretokenization
  pinned in `tests/tokenizer_corpus.rs`). Message-level estimation now
  also charges chat framing (4 tokens/message, 2/tool-call). The
  char-class estimator remains as `count_tokens_heuristic` (documented
  fallback, ADR-0044). See
  [ADR-0117](docs/adr/0117-native-cl100k-bpe-tokenizer.md).
- **Exact incremental token counting for streamed output.** BPE is not
  additive across delta boundaries — merges span them, so summing
  per-delta token counts over-counts streamed completions by 2–100%
  (measured; worst on English with small deltas). The turn-accounting
  path that estimates completion tokens when a provider reports no usage
  now feeds every delta into a `StreamingCounter`
  (`neenee-contracts`, ADR-0117): it commits only pretokens that no
  future scalar can extend or re-split (holding back symbol runs ending
  in an apostrophe, which can still become contractions) and carries the
  open tail, yielding counts identical to whole-text tokenization for
  every chunking (property-tested over 9 scripts × 7 chunk sizes).
  Also fixed: `estimate_model_request` tokenized the non-system message
  subset a second time (and cloned the message list to do it), doubling
  the cost of every pressure estimate.


- **Two-stage web research: pluggable depth reader for `webfetch`.** The web
  tools are now an explicit breadth/depth pair
  ([ADR-0118](docs/adr/0118-two-stage-web-research-search-breadth-fetch-depth.md)):
  `websearch` stays breadth-only behind `SearchProvider`, while `webfetch`
  gains a mirrored `Reader` abstraction (`crates/neenee-agent/src/tools/reader/`).
  New `[websearch] reader` key: `"builtin"` (default, unchanged direct-fetch
  behaviour) or `"jina"` (r.jina.ai: JavaScript rendering, readability-style
  extraction, Markdown; optional `jina_api_key`; automatic fallback to the
  builtin path with a visible annotation on reader failure). Tavily now
  requests `search_depth: "advanced"` for richer breadth snippets.
  `webfetch` truncation is unified to the shared 16,000-byte cap (keeping
  half on truncation) and labels the reader/content-type in its truncation
  header. Live-network E2E tests pin the pipeline (`tests/webtool_e2e.rs`,
  `--ignored` by default).
- **Session lineage: forks and asides are first-class.** A new persisted
  `SessionForkKind` (`Trunk` / `Fork` / `Aside`) is stamped at fork time —
  `/fork` writes `Fork`, `/btw` writes `Aside` — and flows through
  `SessionSummary` → `SessionOverview` → `MonitoredSession`, so every
  observer (session picker, dashboard, web panel via the generated TS types)
  can see which sessions are branches and of what. A legacy snapshot with a
  `parent_id` but no kind degrades to `Fork`. The dashboard badges branch
  cards (`⑂aside` / `⑂fork`, muted name) while trunk cards stay plain —
  the main line is exactly one. See
  [ADR-0116-2](docs/adr/0116-2-session-lineage.md).

### Fixed

- **`/btw` aside views inherited the primary's activity bar and round
  state.** Chrome is now view-scoped: a per-session `SessionChrome`
  (activity text, responding flag, round/turn counters, elapsed-timer
  origin) is maintained for the primary and every live aside;
  `enter_side_view` parks the primary's chrome and swaps in the aside's own
  entry (a new aside starts idle — no inherited "responding" bar), and
  `exit_side_view` restores the primary's exactly as it was, so a primary
  round that kept streaming during the detour shows its own bar again. The
  activity bar, elapsed timer, Activity modal counters, and the redraw
  animation gate all read the viewed session's entry. Jumping aside → aside
  no longer re-snapshots the previous aside's state as the primary's.

## [0.27.0] - 2026-08-19

### Changed

- **`credentials.toml` keys credentials by provider instance, not by
  channel.** The old `[user.<id>]` table mapped `channel_label → api_key`,
  duplicating a shared key once per model route; a channel is a model
  route, not a security principal. The schema is now `[user.<id>] api_key`
  (one credential per instance, fanning out to every channel on load,
  OAuth channels excluded — their bearers live in `auth.toml`). The
  struct is `deny_unknown_fields`, so a legacy per-channel file fails the
  parse loudly (warn + empty, never a silent empty key) and is rewritten
  in the new shape on the next save. `docs/reference/paths.md` now
  documents both credential kinds side by side: token auth in
  `~/.config/neenee/credentials.toml`, OAuth token sets in
  `~/.local/state/neenee/auth.toml`, both keyed by provider instance.

- **`neenee-agent`'s catalog split into a module directory.** The
  3.5k-line `catalog.rs` is now `catalog/` with five focused files —
  `translate.rs` (config→`Channel` mapping), `migrate.rs` (one-shot config
  migrations), `discovery.rs` (live model discovery + template
  reconciliation + fitted-model sync), `picker.rs` (the picker snapshot
  the TUI renders), and `mod.rs` (facade + re-exports). All public symbols
  re-export from `catalog::`, so callers are unchanged. Unit tests moved
  to `catalog/tests.rs`.

- **Catalog discovery tests no longer write into the developer's real
  XDG cache.** `discover_provider_models` persists a `DiscoveryCache`
  through `paths::get()`, so a test whose discovery reported a change
  wrote `test-instance` rows into the real
  `~/.cache/neenee/models_discovery.json`. The affected tests now sandbox
  the process-wide `Dirs` via the new `test-path-override` feature on
  `neenee-persistence` (a dev-dependency feature, absent from production
  builds), which exposes `paths::set_test_default` to other crates' test
  suites — a crate cannot see another crate's `cfg(test)`.

- **Credential placement under XDG is now a recorded decision, not an
  accident.** New
  [ADR-0115](docs/adr/0115-credential-placement-config-vs-state.md): API
  keys are *config* (user-supplied, portable, hand-editable — the spec's
  own "important or portable enough" test says so) and OAuth token sets
  are *state* (daemon-rewritten on refresh, recoverable by re-login).
  `docs/explanation/persistence.md` and `docs/reference/paths.md` carry
  the reasoning; `docs/reference/paths.md` also gained a "Legacy stray
  files" section documenting the orphan files older releases left behind
  (`goals.db`, `session.json`, `model_usage.json`, `repeat.db`,
  `models-dev.json`) — none are read today, all safe to delete.

### Fixed

- **An unparseable `config.toml` fell back to defaults silently.** `load()`
  swallowed the parse error, so a single syntax typo made every provider,
  key, and preference vanish with no trace of why. The failure is still
  non-fatal (startup continues with defaults) but now logs an `error!` with
  the path, the parse error, and the stakes, matching the existing
  `credentials.toml` handling.

- **Google Antigravity OAuth login failed with HTTP 401 `invalid_client`.**
  The `google_antigravity_preset()` bundled the `1071006060591-…` client id
  with the wrong client secret: the two secrets shipped in the upstream
  Antigravity client were paired with each other's client ids, so every
  token exchange (and every later refresh) hit
  `oauth2.googleapis.com/token` with credentials Google rejects. The preset
  now carries the verified pairing — `1071006060591-…` authenticates with
  `GOCSPX-K58F…` (confirmed live: the endpoint answers `invalid_grant`
  instead of `invalid_client` for a dummy grant) — and the preset unit test
  pins the corrected secret so the pairing cannot silently regress.

- **429 quota errors lost their retry classification and dumped raw wire
  framing into the transcript.** Google's `clarify_error` appended its
  guidance text *after* the `[NEENEE_RETRYABLE]{…}` JSON envelope,
  corrupting the encoding: `parse_retryable_error` could no longer decode
  it, so the harness classified the 429 as terminal (`Other`) instead of
  retryable, the backoff loop never honored it, and the mangled envelope
  was shown verbatim to the user (`[NEENEE_RETRYABLE]{"message":"Google
  HTTP 429 …` with `\\n`-escaped JSON inside). `clarify_error` now folds
  the guidance *into* the envelope's message and — when Google embedded a
  `RetryInfo.retryDelay` in the error body — promotes that delay to the
  envelope's `retry_after_ms` (previously always `null`) so the backoff
  loop backs off for Google's own quoted window instead of a guess. The
  quota hint no longer invents a fixed "45–60 minutes" figure; it quotes
  Google's delay when present and otherwise says the window resets at the
  next period without a number. Terminal `RoundEvent::Error` consumers
  (TUI transcript notice, headless CLI, session monitor) now strip the
  envelope via `public_error_message`, so the framing never renders.

- **Google channels never offered per-model reasoning-effort
  configuration.** `channel_model_info`'s `Transport::Google` arm
  hard-coded `effort: None`, so Gemini models that advertise a ladder
  (`gemini-3.7-flash` → `thinkingLevel`, `gemini-2.5-pro` →
  `thinkingBudget`) showed no effort in the Models picker and the `e`
  per-model settings editor never opened. The arm now mirrors the OpenAI
  arms: the channel's explicit override wins, else the model's ladder
  default (`high` clamped to the ladder); models with no ladder stay
  inert.

## [0.26.1] - 2026-08-19

### Added

- **Unified Configuration Modal**: Consolidated separate configuration dialogs
  into a single multi-tab settings overlay for theme selection, custom colors,
  layout options, and MCP configuration.
- **Standalone Theme File Schemas**: Support loading `.toml` theme definitions
  from `$XDG_CONFIG_HOME/neenee/themes/` with component-level overrides for input,
  crates, diffs, and command cards.
- **Expanded Builtin Command Specifications**: Full metadata, category tags,
  usage definitions, examples, and intent keywords across all builtin slash commands.

### Changed

- **Turn-Band Layout**: Streamlined turn-band transcript layout with refined visual
  hierarchy and removed legacy layout engine.

## [0.26.0] - 2026-08-19

### Added

- **Input-box editing parity: `Del` forward delete and a selection caret
  relay.** Two long-missing editor behaviours now work in the composer (and
  every surface that borrows its line):
  - **`Del` deletes forward** — the character after the caret goes, the caret
    stays put. It respects grapheme clusters (a CJK glyph or emoji vanishes
    as one unit) and is chip-aware: deleting onto the `[` of an attachment
    chip removes the whole chip in one keystroke, mirroring the chip-aware
    `Backspace`. Inert on read-only surfaces, live in the `/host` inline
    prompt.
  - **A selection now relays the hidden caret.** Drag-selecting input text
    hides the block cursor, but its position is remembered — the point where
    the mouse was released. Any direction key (`←`/`→`/`↑`/`↓`/`Home`/`End`)
    breaks the selection and continues from that hidden position instead of
    the stale pre-drag caret (`↑`/`↓` restore it; `←`/`→` step one further;
    `Home`/`End` jump to the selection's edges). `Backspace`/`Del` and the
    delete-family chords (`Ctrl+W`/`U`/`K`, `Alt+D`) replace the selection in
    one stroke. A click inside the box breaks the selection at the click
    point. The relay stands down while a transcript step holds keyboard
    focus, so arrow-driven step navigation is unaffected.

- **Client-declared session end (ADR-0112)**: `/exit` and double-`Ctrl+C` in
  the TUI, a headless run's terminal round, and the web panel's new
  "end session" sidebar action now *end* the hosted session instead of
  leaving it hosted indefinitely — the daemon tears it down (cancels a
  running round, fires `SessionEnd` hooks, clears WIP declarations) and
  every dashboard drops the row immediately via `session_removed`. Disk
  history is kept (`/sessions` resume still works); detaching (`/host`
  switch, plain socket drop) still keeps the session running in the
  background as before.

### Changed

- **Command entries: the invocation aligns with its result body, and the two
  are distinguishable by weight and tone.** The concrete invocation (`/new`,
  `!cargo check`) inside a command entry (ADR-0111) previously started at the
  transcript's left edge while the result body beneath it was indented by the
  prose leading indent — the entry read as a hanging head over a shifted body.
  The invocation now shares the body's indent, so a completed entry renders as
  one aligned block, and the result body keeps the bold invocation visually
  distinct by rendering one step quieter (the muted `Role::Tool` prose tone):
  input and output stay separable at a glance without any chrome.

### Added

- **The turn header shows the reasoning effort the turn ran with** —
  `> turn 26 · glm-5.3 · high · 17:55` instead of
  `> turn 26 · glm-5.3 · 17:55`. The depth is stamped per message at turn
  time (thinking-gated per protocol, exactly like the hint bar's effort tag:
  Anthropic effort counts only while thinking is on, OpenAI-compatible
  channels whenever they expose a ladder, Gemini stays quiet), so the header
  shows what that turn *actually* used rather than today's live setting, and
  it survives resume: the harness stamps the resolved effort onto the
  persisted assistant message next to the provider/model attribution. A new
  `Provider::effort()` accessor exposes the resolved depth from each concrete
  transport; non-reasoning channels render the unchanged shorter header.

## [0.25.2] - 2026-08-18

### Added

- **Custom OpenAI provider template — any OpenAI-compatible endpoint.**  - A new entry in Connections → `＋ Add connection` for third-party relays, self-hosted gateways, and subscription bundles that expose an OpenAI-compatible `/v1/chat/completions` surface (e.g. `https://chatapi.weixin.qq.com/openai/v1/chat/completions`).
  - Unlike the curated templates it seeds no model list: the editor shows a free-text **Model** field (registry-known OpenAI ids as fuzzy suggestions, plus the raw typed id as a custom value), so the exact model id the endpoint expects becomes the seeded channel. More models are added afterwards from the Models picker.
  - Model ids travel verbatim: case-sensitive ids (the WeChat endpoint serves `GLM-5.2` / `Deepseek-v4-flash` and rejects the lowercase spellings with `invalid model`) round-trip unmodified through the editor, config, and requests. Baseline metadata (200K context) is registered for those two cased ids.
  - Instances stay pure-custom in reconciliation terms: the typed model is never re-seeded or replaced by a template snapshot at startup.
- **`neenee run` now prints streamed assistant text.**
  - Streaming providers (the common case) deliver text as `StreamDelta` events; the headless runner previously matched only the non-streamed `Text` backstop, so a streamed round completed with an empty `response`. Both JSON and plain modes now emit the deltas live and the final `round_completed.response` carries the full text.
- **`/fork` is a top-level command.** Forking the current conversation into a
  child session no longer hides behind `/session fork` — the form two docs
  already referenced. `/session fork` keeps working as legacy grammar.
- **`/config reload`.** The config hot-reload action (ADR-0085 §6) moved to
  `/config reload`, where its semantics are explicit. Bare `/config` still
  opens the Settings overlay; the retired `/reload` spelling resolves through
  the hidden-alias table. Completion offers the `reload` subcommand.

### Changed

- **Removed `/review` and the session-review subsystem.**
  - `/review` was the sole trigger for an otherwise-unreachable subsystem: the
    376-line `session_review` runner (a bounded read-only `REVIEW` envoy over
    a transcript snapshot), the reviewer prompt registry, the `REVIEW` envoy
    profile, the `SessionReview` round/agent events, and the review-banner
    plumbing in both the TUI and the web panel. Nothing else — autopilot,
    round-end diagnostics, the doom guard — reads its output (ADR-0034
    explicitly declined to wire it in), and the loop-stuck cases it watched
    for are covered by the deterministic read-loop guard plus `Esc`.
  - Ledger compatibility is preserved: `CommandResult::Review`,
    `ReviewVerdict`/`ReviewStatus`, and `InjectionKind::SessionReviewInput`
    stay in `neenee-contracts` so old session files still deserialize and
    their recorded review results still render on resume.
- **Command rows are cards; the disclosure glyph is `▸`/`▾` (ADR-0109).** A
  command component now paints a full-width band
  (`Theme::command_surface`) with a thick `┃` identity bar in the family
  tone — the card grammar the user-message panel, code blocks, and notice
  cards already speak — so an operation is separated from prose by *shape*
  before color is even read (a muted `⌘ /cmd` line could read as just
  another sentence). The band lifts to `command_surface_hover` under
  pointer/focus, and the `▸`/`▾` marker column is reserved even when empty,
  so a pending row settling with its reply never shifts horizontally.
  Alongside, **every** disclosure site (tool steps, reasoning traces,
  provider retries, command cards, sticky pins) migrates `+`/`-` → `▸`/`▾`:
  `+`/`-` is now reserved exclusively for diff signs and the `+1 -1` counts
  in edit summaries, removing the old collision where an expanded edit step
  began with `- Edit a.rs +1 -1` — four sign characters, three of them diff
  semantics. The triangle also matches the web panel's chevrons, so both
  front ends share one disclosure vocabulary. The inline/Disclose
  classifier now budgets the card chrome and the trailing `· HH:MM`, so a
  joined reply genuinely fits inside the card.
- **Commands no longer trigger the activity bar (ADR-0110).** A slash command is a synchronous control-plane operation outside the round state machine, so dispatching one performs no activity-bar arming: no transient `queued` label, no breathing dot, and no fabricated `Esc Esc interrupt` affordance over a dispatch that cannot be interrupted (previously every command flashed the bar while idle). Command handlers also stop borrowing `RoundEvent::Activity` for their progress (`/compact`'s "compacting context"), which — dispatched mid-round — could overwrite the running round's live label. The pending command row (`⌘ /cmd`, ADR-0108) is a command's in-flight feedback; round-owned emitters (including the automatic in-round `compacting context` step) keep the activity surface. The driver's post-dispatch reconcile (ADR-0092) stays as a frontend-agnostic safety net.
- **Retired `/resume` and `/session`; `/sessions` absorbs them.**
  - `/resume` was a verbatim duplicate of `/session resume` (identical help
    line, no ADR justifying a second spelling) whose arm *also* skipped the
    provider-pin reapply, so it silently misbehaved next to its twin. `/session`
    grew six subcommands that all duplicate better surfaces.
  - `/sessions` now takes an optional id: bare `/sessions` opens the picker,
    `/sessions <id>` opens that session immediately. The picker's Enter key
    drives the same path (it used to synthesize `/session open <id>`, now
    `/sessions <id>`), and the sessions-modal `n` key sends `/new` instead of
    `/session new`.
  - Both retired spellings remain hidden aliases (the `/host` → `/dashboard`
    pattern): `/resume <id>` and `/session open <id>` still open the session,
    `/session list` opens the picker, `/session new` / `/session fork` behave
    like `/new` / `/fork`. `/session status` is retired — its id/counts/
    timestamps live in the picker's `i` info view; the `/continue` trigger now
    suggests `/sessions`.
  - The `/session <Tab>` canned subcommand list is gone along with the command
    (the argument of `/sessions <id>` is a session id, discovered via the
    picker); `/sessions <Tab>` completes the bare command only.
- **Envoy task steps now show what they are doing, not just that they are busy.**
  - The live peek row (the `└`-edged second line of a running `[EXPLORE]`-style task step) now leads with `running` followed by the current activity — `running thinking`, `running Grep "session"`, `running waiting for model` — instead of a bare `starting`/tool name. A long model call no longer reads as "possibly stuck".
  - `EnvoyEvent::Activity` reports (e.g. `waiting for model`, `waiting to retry (3s)`) were previously discarded by the TUI; they now drive the peek during stretches with no child events, so the row provably advances even before the first tool call lands.
  - The activity and its elapsed time are joined by plain whitespace instead of a `·` glyph, per the join ladder (same-rank metadata, R2).
  - `awaiting approval` keeps its bare form — nothing is moving while the envoy waits on a human.
- **Command rows are one component with a lifecycle (ADR-0108).**
  - A slash command is no longer echoed as a separate `▌ cmd` user bubble: dispatching pushes one optimistic command row (`⌘ /cmd`, muted running tone), and the typed reply settles *that same row in place* — input and output are one component, so the transcript keeps one row per command and the reading flow is never split across two unrelated rows.
  - Command components now have two states: **pending** (`⌘ /autopilot on`, no marker — the output does not exist yet) and **completed** (`⌘ /new · Started new session: a1b2c3` inline, or the `+`/`-` disclosure for long/multi-line replies). Commands that never emit a reply (modal/picker/side-view commands) settle as cancelled rows instead of lingering as promises.
  - The disclosure layout no longer renders through the reasoning-trace renderer: `+`/`-` now leads the row *with* the command glyph and a muted timestamp (`+ ⌘ /permissions · 21:39`), matching the inline/plain layouts — one span grammar everywhere.
  - Resume folds durable slash/shell echoes into the ledger rows, so a restored session renders exactly one row per command at its turn seam (no duplicate invocation bubbles).
- **Redesigned the Connections → Add connection template chooser.**
  - Rows are now sorted alphabetically by title (previously a fixed curation order), so scanning for a provider reads like a directory.
  - Unfocused rows show their title only; the focused row reveals the template's one-line description beneath.
  - Selection is a full-width brand background highlight (the Connections/Models row standard) — the leading `›` cursor marker is gone, freeing the horizontal space it cost.
  - Removed the `· <protocol> · <N> models` meta suffix: the models an endpoint actually serves are only knowable with a working credential, and the wire protocol is an implementation detail of the locked template.
  - Each row instead carries a trailing auth-scheme badge — `⚿ oauth` for browser/device-flow subscriptions, `⚿ token` for API-key templates — separated from the title by whitespace, never a `·` glyph.
  - Renamed the `Google Antigravity OAuth` template title to `Antigravity OAuth` (its id, protocol, and behavior are unchanged).
- **Removed `!` shell command passthrough.**
  - `!`-prefixed inputs are no longer intercepted as host shell commands and are now dispatched directly as normal chat prompts to the agent, eliminating modality collisions and preventing false triggers on conversational phrases.
  - Cleaned up the TUI bottom HintBar to remove the `shell_active` state (`Enter run command`), retaining a clean two-state Enter action (`Enter send` when idle, `Enter queue message` when busy).
- **Added expand/collapse micro-affordances to NoticeCard headers.**
  - When a transcript NoticeCard has expandable details (e.g. pretty-printed JSON error response or multiline traceback), its header now renders a subtle, right-aligned indicator (`click to expand` when collapsed, `click to collapse` when expanded) in muted tone.
  - Notices without detail payloads remain cleanly unadorned to prevent false affordances.
- **Disclosure auto-scroll is now configurable and disabled by default (`[tui] expand_auto_scroll`).**
  - Toggling a disclosure (tool step, command result, thinking, provider-retry, or notice card) previously auto-scrolled — "shift the summary line to the top of the viewport" on expand, "keep the collapsed summary visible" on collapse. A toggle is a read interaction, not a navigation command, so the default is now **off**: the card grows or shrinks in place and the scroll offset stays exactly where the user put it. Set `expand_auto_scroll = true` to restore the content-maximizing behavior (only the sticky header's collapse still re-anchors, since its overlay row must land where the covered summary sits).
  - The old auto-scroll also flickered: its scroll target was computed against the previous frame's layout and painted immediately, then corrected by the post-draw clamp with no redraw scheduled — the terminal showed an un-clamped viewport until the next unrelated frame. With the behavior enabled, toggles now latch a one-frame settle request: the event loop stages the next frame (full layout to measure the new height, zero terminal bytes), validates the target offset against the fresh measurement, and paints only the settled viewport; when the staged offset is already final the staged grid is committed as-is (no second layout, no intermediate frame).

## [0.25.1] - 2026-08-17

### Changed

- **Provider retry moved from transcript disclosures to the Activity Bar and Activity Modal.**
  - Transient provider retry countdowns and running attempt timers (`retry 4/15 · next in 6.6s`)
    are now rendered dynamically on the Activity Bar instead of inserting synthetic mutating
    messages into the conversation transcript.
  - The Activity modal (`Tab` / click Activity Bar) now displays the full "Last failure:" error
    details and diagnostics under the Status section.
  - Increased default `provider_retry_max_attempts` from 6 to 30 (and clamped limit from 10 to 60)
    for resilient long-horizon autonomous runs during upstream load spikes.
  - Lowered default `provider_retry_max_ms` from 30s to 10s to keep backoff polling frequency
    responsive once upstream providers recover.
  - Aligned `EnvoyTool` (subagent tasks) to inherit the session's provider retry policy,
    equal jitter, and minimum backoff floor guard, replacing the previous hardcoded 3-attempt limit.
  - Replaced bulky "Gave up after X attempt(s)..." boilerplate with raw clean error messages upon retry budget exhaustion.
  - Introduced expandable Notice components in the TUI: provider HTTP errors (e.g. 429/5xx) default to a concise header and expand to formatted, indented JSON details.
- **Command result rows now render a lead symbol and timestamp.**
  - Slash command rows lead with `⌘` (info tone) and shell command rows (`!cmd`) lead with `❯` (ok tone).
  - Both layouts (disclose and inline) append a muted `· HH:MM` sent-time label when a timestamp is present.
  - Command rows never render the `▌ Sent` fallback marker.

## [0.25.0] - 2026-08-16

### Fixed

- **Tests no longer write to the user's real state directory.** Several test
  suites constructed production persistence objects without path isolation,
  so a plain `cargo test` leaked into (and damaged) the developer's real
  `$XDG_STATE_HOME` / `$XDG_DATA_HOME`:
  - `neenee-tui`'s `App` test constructor now keeps
    `input_history_persist = false`, so `record_input_history` /
    `clear_input_history` never merge synthetic rows into — or truncate —
    the real `history.json`. (This leak is why the Ctrl+R picker once filled
    with `prompt 0..39` rows stamped `session-a`: the attachment-cache test
    wrote its synthetic prompts straight into the user's file, and the
    clear-history test then truncated that same file outright.)
    Production keeps persistence on (the `run_tui` constructor sets the flag
    to `true`), and a new regression test asserts the real file is
    byte-for-byte untouched while the flag is off.
  - `neenee-persistence`'s `TrustedProjects`/`TrustGate` tests now sandbox
    `paths::get()` via the sanctioned `set_test_default` +
    `TEST_OVERRIDE_GUARD` pattern, instead of replacing the real
    `trusted_projects.json` with an empty set (which silently revoked every
    project trust grant).
  - `neenee-runtime` / `neenee-agent` session tests now use
    `SessionStore::for_path(<tempdir>)` instead of
    `load_for_project(tempdir)`; the latter resolves the real XDG project
    bucket and minted `sessions/<id>.json` + blob dirs under
    `~/.local/share/neenee/projects/`.
- **The ↑/↓ input history is bound to the session, not the client window.**
  Three cross-session leaks are closed. First, the origin stamp lagged a
  mid-run switch: the session id used to tag (and filter) history entries was
  read from the handshake-time session source, frozen for the process
  lifetime — after `/session open`, `/resume`, or `/fork`, prompts kept being
  stamped (and recalled) under the *retired* session's id. The id now comes
  from a live cell the response listener updates on every
  `ConversationReplaced`/`ConversationCleared`. Second, a switch no longer
  carries composer state across the boundary: the ↑/↓ cursor, the stashed
  draft, and staged attachments are reset when the viewed session changes
  (entering/leaving a `/btw` aside included), so nothing typed into one
  conversation leaks into another. Third, `/new` now reports the freshly
  minted session id (`ConversationCleared { session_id }`) so clients track
  the switch the same way they track `ConversationReplaced`.
- **Resuming a session restores its prompt history.** The inline ↑/↓ recall
  now walks the union of the tagged persisted history and a
  **transcript-derived backfill**: the genuine chat prompts of the
  conversation on screen, rebuilt incrementally from the transcript each
  frame. A resumed session recalls its own earlier turns immediately —
  including ones typed in another client or before this client's
  `history.json` existed — instead of coming up empty until new prompts are
  sent. The backfill is derived state and never persisted (the session file
  is the durable record, ADR-0018); slash commands and `!shell` passthroughs
  are excluded, and `Ctrl+R` remains the global cross-session search surface.

### Added

- **Command rows interact by shape (ADR-0106, revising ADR-0091 D4).** The
  one-size `+ ⚙ /new` disclosure block is gone: a command row now picks its
  layout from the reply's shape at render time. A short single-line reply
  (`/new`'s confirmation, acks, `/schedule`) joins inline as
  `/new · Started new session: a1b2c3` — no `+` marker, since there is no
  second view worth opening, and the R1 ` · ` join reads the reply as the
  outcome of the invocation; a result-less record (shell passthroughs,
  legacy folds) renders as a plain dimmed row; only genuinely long or
  multi-line replies (`/search`, `/session status`, `/review`) keep the
  `+`/`-` disclosure with the expandable body. The `⚙` glyph is retired
  everywhere — the `/` already says "command". The inline classifier is
  width-aware: a reply that would truncate beside its invocation falls back
  to the disclosure layout, so an inline reply is never a fragment. The web
  panel's command blocks follow the same rule (short Text replies and acks
  render as one flat confirmation row), closing the gap where its acks were
  flat but the TUI's were expandable blocks.
- **Command rows restore at their turn seams (ADR-0106 §2).** On resume,
  `/session open`, `/resume`, fork, and side-view opens, the rebuilt command
  rows no longer append to the dialogue's tail: each merges before the first
  prompt sent after its invocation, so a `/compact` run between two rounds
  renders between those rounds — where it appeared live. Dialogue order is
  never disturbed; records older than every prompt, or dialogue without
  message timestamps, keep ledger order at the tail.
- **The daemon serves the web panel itself, on the same port (ADR-0105).**
  The TCP listener now peek-splits plain HTTP from WebSocket upgrades:
  `GET /` serves the panel bundle embedded at build time from `apps/web/dist`
  (new `neenee-web-assets` crate; a placeholder page is embedded when the
  dist was never built, so pure-Rust builds never need the Node toolchain),
  and `GET /healthz` answers an unauthenticated `{version, auth, panel}`
  probe. The CLI default port is now fixed at 9800 (falling back to an
  ephemeral port, recorded in the discovery file, when taken), `neenee
  panel` prints the panel URL including the token, and `Wire::Error` gains
  an optional machine-readable `code` (`"version_mismatch"` first).
- **Control-plane hardening (ADR-0105).** The loopback TCP listener now
  requires a bearer token by default — `[daemon] local_auth` (CLI
  `--no-local-auth` opts out) — generated per daemon start and published in
  the owner-only discovery record, which Rust clients already read and
  present. Browser clients authenticate with a `Sec-WebSocket-Protocol:
  bearer.<token>` offer (the one channel `new WebSocket()` can customize),
  echoed on accept; and every loopback handshake carrying a browser `Origin`
  is refused (403) unless the origin is itself loopback-hosted, closing the
  drive-by hole where any visited page could drive the daemon (WebSocket is
  not same-origin-protected).
- **Project trust now covers skills and slash commands (ADR-0107).** The
  ADR-0085 §5 gate previously stopped at `.neenee/config.toml` (MCP servers
  and hooks); project skills (`.neenee/skills`, `.agents/skills`,
  `.claude/skills` — the highest-priority scope) and project slash commands
  (`.neenee/commands/`) loaded from any cloned repo unconditionally. For an
  agent holding tools, project-supplied prompt text is execution by proxy,
  so both now load only after `/trust`, enforced inside the scan path itself
  (startup, periodic refresh, and `/skills reload` share the one gate, and
  command discovery is anchored to the session's project root rather than
  the daemon's cwd). `/trust` enables them mid-session, `/untrust` drops
  them immediately, and a project entry that shadows a same-named user-scope
  skill or command now emits a one-time warning notice naming the winner —
  silent overrides are impossible.
- **Sessions can be renamed over the wire.** `Request{RenameSession{id,
  title}}` sets a session's manual title (ADR-0022: AI titling never
  overwrites it) or clears it back to the AI/first-prompt fallback with
  `null`, resolving live and archived sessions by full id or short-id prefix
  exactly like `DeleteSession`; monitor rows republish with the new title.
  The web panel exposes it as inline rename in the session sidebar.
- **The web panel's protocol types are generated, not transcribed.** The 58
  payload types of the daemon wire protocol now carry `ts-rs` derives in
  `neenee-contracts` and export `apps/web/src/lib/generated/wire.gen.ts`
  (`cargo test -p neenee-contracts`); `types.ts` is a thin façade keeping
  only the serve-layer envelope and helpers. CI fails on drift, so the
  "client scaffolded against an imagined protocol" bug class is closed by
  construction.
- **Empty-state help carousel** (ADR-0104). A blank conversation now shows
  a rotating help line beneath the logo — one durable capability hint at a
  time (`/btw`, `Ctrl-R`, `F1`/`?`, `Ctrl-M`, `!` shell, `@` mentions,
  queue-on-Enter), advancing every 8 s on a wall-clock cadence — a single
  self-explaining line, no position indicator, and no static tagline above
  it: the carousel's first page already answers "how do I start" ("Send a
  message, or `/` command — try `/help`"), so the retired "Type a message
  below to begin." line was a duplicate. When no keyed LLM provider is configured the carousel
  is replaced by the pinned `/connections` setup blocker (consuming
  ADR-0057's `NeedsProvider` guidance), so the setup nudge no longer
  rotates away.

### Changed

- **The Models picker sorts in two weighted tiers, then ASCII.** The flat
  (provider, model) list now ranks by status first — the currently-active pair
  leads, favorites come next, everything else follows — and inside each tier
  rows sort by the model id in plain ASCII (byte order), with the provider
  label as the tiebreaker for the same id served by multiple instances. The
  per-model recency signal no longer participates: the list is deterministic
  regardless of usage history, so models no longer shuffle after every switch.
  `models_flat_filtered_from` now takes the current pair
  (`current_provider`, `current_model`) to compute the tier; the provider-level
  recency sort of the **Connections** list is unchanged.
- **Model surfaces are id-first: the wire model id is the label, everywhere.**
  The curated display-name mapping is gone, root and branch. Upstream
  discovery cannot provide one consistently — OpenAI-compatible and Google
  `/models` return only `{id, …}` while Anthropic/Kimi advertise a
  `display_name` and Copilot a `name` — so the picker used to show a mix of
  brand names ("Claude Opus 4.8") for registry-known ids and raw ids
  ("acme-7b") for everything else. Every surface now renders the wire id:
  the Models picker rows, the hint bar, the model editor title, the custom
  provider form and its suggestion list, transcript turn headers, and the
  activity modal. Removed with it: `model_display_name`, `Model::name`,
  `ModelCapabilities::display_name`, `FittedModel.display_name`,
  `FittedModelInfo.display_name`, `RemoteModelMetadata.display_name`, and
  `DiscoveredModel.display_name` (a `display_name` arriving in a discovery
  payload is simply ignored). `ProviderModelInfo.last_used_ms` is also gone
  from the wire — the picker no longer sorts by recency (the provider-level
  `last_used_ms` stays; the Connections list still sorts by it). The web
  panel's model picker mirrors the same two-tier ASCII ordering, and
  `wire.gen.ts` is regenerated.
- **The head band's second row is demand-driven** (ADR-0104, refining
  ADR-0103 §3). The affordance legend now renders only while the view has
  page-specific shortcuts to announce: the main view shows it only while
  asides are live (`btw 2 · 1 running  F5 asides`), `/btw` keeps it always
  (`Ctrl-C back` is its single exit), and the Envoy page drops it entirely
  (its permanent footer already carries the same legend — the two copies
  were exact duplicates). The unconditional `F1 help` pair is gone from
  every page (modal footers own that discovery), and so is the main view's
  `Esc interrupt` pair, which both duplicated and contradicted the activity
  bar's correct `Esc Esc interrupt` hint. The reclaimed row returns to the
  transcript.
- **The Envoy footer legend drops its global pair.** The page's permanent
  footer trims to its own navigation — `Esc back` (plus `[ prev` / `] next`
  while the focused task has siblings) — because `F1 help` is a global
  capability, not a property of any view, and every modal footer's
  mandatory `? help` chip already owns that discovery. `F1` itself still
  opens help from anywhere, including the Envoy page.

- **The web panel covers the full round-event surface.** Slash-command
  replies render as distinct command blocks (`RoundEvent::CommandResult`,
  ADR-0091) instead of vanishing — the composer's `/help` `/status` `/mcp`
  pills finally show their output; the non-streamed `Text` fallback
  (interrupted rounds, hook-blocked prompts) appends assistant prose;
  `HarnessState`/`AutopilotChanged` drive a live round counter and an
  autopilot badge; `Activity`/`TurnStarted` surface in the header;
  `RoundCompleted` shows honest per-round throughput (`RoundSummary`'s
  generation-time TPS); `Compacted`, `RetryScheduled`, and review alerts
  (`SessionReview` — a retained, dismissible banner) are all visible.
- **The web panel unblocks envoy approval walls.** `RoundEvent::Envoy`
  events fold into a nested, expandable envoy view inside the parent `task`
  tool card (profile, activity, streaming text, nested tool calls), and
  envoy-raised `PermissionRequest`/`UserQuestionRequest`/`InputRequest`
  prompts render with their origin label and route the reply's
  `parent_call_id` back to the parked envoy (ADR-0029) — previously these
  hung the session silently.
- **Web panel connection settings.** The daemon endpoint and project scope
  resolve from `?ws=`/`?host=`+`?port=`/`?project=` query params, then
  localStorage, then the `ws://127.0.0.1:9800` default, editable through a
  connection dialog (click the Online/Offline badge) that persists and
  reconnects. The version handshake now sends the build-time-injected
  `package.json` version, which CI pins to the workspace `Cargo.toml`
  version — the previous hardcoded `"web-0.24.0"` never matched the daemon's
  `"0.24.0"` and was refused before any session work.
- **Web panel session and model management.** Sessions can be deleted from
  the sidebar (two-click confirm, `DeleteSession`); the header shows the
  active provider/model from `Welcome` and opens a Models picker rendered
  from the `ProviderPicker` snapshot (favorites, effort/thinking flags,
  key-readiness) that switches via `SetDefaultModel`; `ProviderSwitched`
  updates the header live.
- **Web panel todo panel and image support.** `TodosUpdated` renders as a
  collapsible sticky task list above the composer; the composer accepts
  pasted/attached images (base64 `ImagePart`s, 10 MB cap) and message
  history renders `Message.images` and collapsible `reasoning_content`;
  `UnsentInput` now restores the interrupted prompt (with images) into the
  composer and drops the optimistic echo, and mid-round
  `UserInputInserted`/`NextRoundStarted` inserts append with dedupe.
- **Web panel tests and CI gate.** A vitest suite (happy-dom + a scripted
  fake WebSocket) covers config resolution, frame shapes, stream/command/
  tool/envoy folding, reconnect, and markdown sanitization; the CI web job
  runs it and asserts the web/workspace version match.

### Changed

- **Web panel markdown pipeline hardened.** The hand-rolled sanitizer (which
  missed `xlink:href`/`srcset`-style vectors) is replaced by DOMPurify's
  default profile with styles and form controls forbidden; code blocks are
  highlighted with highlight.js *after* sanitization, and surviving links open
  in a new tab with `rel="noopener noreferrer"`. Transcript items use
  `content-visibility: auto` so long sessions stay responsive, and the layout
  collapses to an overlay sidebar below 900px.
- **The web panel now speaks the daemon's actual wire protocol.** The client
  under `apps/web` was scaffolded against an imagined protocol and could not
  exchange a single valid frame: it read `Monitor` frames through a
  nonexistent `.event` field, expected `Response` payloads nested under
  `.response` (they are flattened into the envelope), matched internal
  `AgentEvent` names (`AssistantDelta`/`AssistantEnd`) instead of the wire's
  `StreamDelta`/`StreamEnd`, sent requests as `{type:"Request",request:{…}}`
  instead of the flattened `{type:"Request","Chat":{…}}` (omitting the
  required `images` field), and modeled enums with the wrong casing
  (`"user"` vs `"User"`, `"allow_once"` vs `"Once"`). `src/lib/types.ts` is
  now transcribed from `neenee-contracts` (`events.rs`, `monitor.rs`,
  `message.rs`, `todos.rs`) and `neenee-runtime`'s `Wire`/`AttachAction`/
  `ControlRequest`, and the store sends and parses the serde shapes exactly.
- **The web panel can now answer blocking prompts.** `PermissionRequest`
  (with `Once`/`Always`/`Reject`, honoring `one_off` and `elevation`),
  `UserQuestionRequest`, and `InputRequest` render an inline banner wired to
  `PermissionReply`/`UserQuestionReply`/`InputReply` — previously a session
  needing approval hung silently forever. Notices and errors (provider
  retries, turn errors, `UnsentInput`) surface as dismissible toasts instead
  of vanishing into the console; `Welcome`/`ConversationReplaced` filter
  `hidden` harness-injected messages out of the transcript; streamed
  markdown is sanitized (event handlers, dangerous elements, and non-http
  URLs stripped) before `{@html}`; message timestamps convert Unix seconds
  correctly (they previously rendered as 1970); tool cards show live
  stdout/stderr streams and a real failed/cancelled state; and the session
  sidebar uses the monitor row's actual fields (`overview`, all six
  statuses, current tool) instead of the nonexistent `title`/`provider`/
  `model` triple.
- **The web panel's Node workspace is now governed.** The committed lockfile
  moved from `apps/web/pnpm-lock.yaml` to the root `pnpm-lock.yaml`
  (`pnpm-workspace.yaml` already declared `apps/*`, so the nested lockfile
  was dead weight that workspace installs never read); `@types/marked` (a
  deprecated stub) was dropped; template leftovers (`Counter.svelte`,
  `hero.png`, `svelte.svg`, `vite.svg`, `icons.svg`) were removed; and the
  project README was replaced with the actual client's docs. CI gained a
  `web` job running `pnpm install --frozen-lockfile` + `check` + `build`,
  closing the gap where the panel had no gate at all.
- **Root `.gitignore` no longer shadows project-shared editor config.**
  `.vscode/` ignored whole directories, defeating `apps/web/.gitignore`'s
  `!.vscode/extensions.json` exception; it now ignores `.vscode/*` with
  explicit allow-listed exceptions.

### Fixed

- **The web panel reattaches after a disconnect.** A dropped session socket
  previously left the panel stuck on "Session detached" until a manual click
  (the monitor snapshot only auto-attached when no session was active); the
  session channel now reconnects with capped exponential backoff and replays
  the transcript from `Welcome`, and a fresh monitor snapshot re-triggers
  attachment when the channel is down. A global error or unhandled rejection
  surfaces as a toast instead of dying silently.
- **AsyncAPI contract drift (part 1 — mechanical defects).** The contract
  had a duplicate `SessionOverview` schema key (YAML last-wins), a
  `WelcomeEnvelope` that omitted the `provider`/`model` fields the daemon
  actually sends (real frames failed its `additionalProperties: false`), a
  `Message` schema that required fields the Rust struct does not have
  (`images`/`hidden`/`compacted`/`children` as mandatory, plus phantom
  `tool_name`/`tool_arguments`/`tool_duration_ms`), phantom
  `UpdateDoomGuardConfig` request/response variants whose Rust enum
  counterparts were deleted, a `PermissionRequest` missing `elevation` and
  `one_off`, a `MonitoredSession` missing `project_root` and `wip` (and the
  whole `WipStatus` schema), and a `ChannelAuth` enum missing
  `CopilotOAuth`/`AntigravityOAuth`.
- **Stale `neenee-server` binary references removed from docs.** ADR-0102
  deleted that binary; `server-api.md`, `cli.md`,
  `session-daemon-and-control-plane.md`, and both daemon how-tos now
  describe the single `neenee serve` entrypoint. `crates/neenee-runtime/
  README.md` (still titled `neenee-transport`, two renames ago, and claiming
  the session registry is a stub) was rewritten to the current architecture.
  `apps/web` is now registered in `docs/dev/workspace-layout.md` and the
  crate-layering guide (previously mentioned nowhere with the wrong path),
  and `docs/dev/documentation/contracts.md` points the `api.md` rule at the
  real server-API surface.

### Changed (prior)

- **`/btw` is now a background aside conversation (ADR-0103).** Leaving an
  aside view (`Ctrl+C`) detaches without interrupting — the aside keeps
  running, stays in the new asides list (`F5` / `/btw list`), and can be
  re-entered with its full transcript. The single live side of ADR-0017 is
  lifted to a multi-slot registry, so several asides can run concurrently;
  `/btw <text>` still auto-sends its first turn. An aside opened but never
  used is discarded on detach (registry entry and session files) so it never
  litters the list or `/sessions`. Entering an aside now back-fills the view
  from its full persisted transcript (inherited parent context included) —
  the model always saw that context; now the pixels match.
- **`Esc` interrupts, `Ctrl+C` leaves.** Esc inside an aside view now
  interrupts the viewed aside's round (armed twice, exactly like the main
  view's Esc interrupt) instead of exiting, and `Ctrl+C` is the single
  leave-the-view key — matching shell/REPL muscle memory. The aside's
  interrupt is scoped to its own session and never closes it.
- **The head band is two rows.** Row 1 keeps identity + status (the old head
  row); row 2 is a view-level affordance legend: the main view shows the
  live aside chip (`btw 2 · 1 running`) plus `F5 asides` / `Esc interrupt`,
  and the aside view shows `Ctrl-C back` / `F5 asides` /
  `Esc interrupt aside`. View-level shortcuts moved off row 1 (the old
  `Esc back` right hint is gone).
- **`F5` opens the asides modal.** One row per live aside (run/open badge,
  relative time, title), `Enter` to jump back in, `D` to close + discard
  outright, `F5` to refresh in place. Running badges derive from the
  per-session running set, so a background round finishing flips them on the
  next frame without a refetch.
- **The input box owns a dedicated two-color background pair.** The live
  input's background was one flat tone that differed from the app background
  and from the (borrowed) unfocused tone by only ~1/255 of luminance — so the
  activated and deactivated states were effectively indistinguishable from
  each other and from the page behind them. Both frontends now give the input
  component two related but independent tokens, tuned as a unit and shared
  with no other surface. TUI: `input_bg_active` (26,28,27) — the brightest
  interactive surface, "typing lands here" — and `input_bg_inactive`
  (16,17,17), a recessed inert band that is no longer borrowed from the
  sent-user-message panel; both derive per color scheme in `from_semantic`
  and are guarded by a test asserting a ≥4-point luminance margin (from the
  app background and from each other) in every preset. Web panel:
  `--input-bg-active` / `--input-bg-inactive` replace the single `--bg-input`,
  with the composer lifting to the active tone on `:focus-within` (previously
  only the border changed); the sidebar pill, markdown code blocks, and tool
  cards that incidentally reused `--bg-input` now sit on their own surface
  tokens so retuning the input never ripples into them.
- **The head row now spans the terminal's full width.** The `SESSION` /
  `/btw` / `ENVOY` identity strip at the top of every view used to be inset
  by the shared 2-col transcript gutter, which punched two `app_bg`-colored
  notches into its `body` band at either edge — reading as a rendering gap
  rather than a deliberate margin. The head is top-level chrome pinned to the
  top edge (the counterpart of the Envoy key-legend band at the bottom edge,
  which already spans the full width), so its background now owns every cell
  of the row. The text keeps the same 2-col inset, rendered as padding, so
  the identity still lines up with the transcript band and the footer bars.

## [0.24.0] - 2026-08-15

### Added

- **Unified single-binary architecture (ADR-0102).** The workspace now compiles
  to exactly one executable binary artifact: `neenee` (produced by `neenee-cli`).
  Running headless background session daemons is unified via `neenee serve` and
  `neenee serve --detach`, with automatic client on-demand self-spawning via
  `current_exe()`.
- **Daemon shutdown correctness (ADR-0101).** SIGTERM and SIGHUP now run
  the same graceful drain as Ctrl-C (previously only SIGINT was handled —
  `kill`/supervisor stops killed the daemon with no SessionEnd hooks and a
  stale discovery record). The drain is budgeted (`[daemon]
  shutdown_grace_secs`, default 10s): stop accepting → close live
  connections (watch clients get a `daemon_draining` monitor frame first) →
  tear every session down concurrently with per-`SessionEnd`-hook deadlines
  → remove the discovery record → exit 0. A second signal, or the budget
  expiring, forces the exit (still 0) and names the stragglers in the log.
- **`neenee stop`** — remote graceful stop through the new `Shutdown`
  control verb (`neenee status` shows the pid; `kill <pid>` now drains too).
- **Idle exit (ADR-0100 rule 3).** The daemon exits on its own after
  `[daemon] idle_exit_minutes` (default 5) of hosting zero sessions with
  zero attached clients; `0` disables it. Also surfaced as `--idle-exit` on
  `neenee serve`.
- **Version negotiation (ADR-0100 rule 4).** The discovery record carries
  the daemon's `version`; `Select` carries the client's. A mismatch is
  refused with a both-versions error naming the fix. Older records/clients
  are tolerated per the field defaults; a versionless record counts as
  mismatched (stop the old daemon once after upgrading).
- **Single-instance lock (ADR-0101).** The daemon holds a `flock` on
  `daemon.lock` for its lifetime; a second daemon spawned during a drain
  waits (bounded) instead of stealing the UDS socket.
- **`assets/neenee.service`** — a documented systemd *user* unit for
  always-on deployments (`--idle-exit 0`, `TimeoutStopSec` above the
  daemon's own grace).
- `MonitorEvent::DaemonDraining` on the monitor stream; `[daemon]` config
  table; lifecycle integration tests driving the real run loop with
  injected shutdown triggers.

### Changed

- **Crate renamed: `neenee-host` → `neenee-runtime` (ADR-0102).**
  The session harness, control-plane wire protocol (`serve`), and daemon runtime
  have been renamed to `neenee-runtime` to clearly denote the session state and
  IPC runtime engine.
- **Startup failures are readable.** A TCP bind failure surfaces as the
  actual `io::Error` (exit 1) instead of a bare `RecvError`; a contended
  single-instance lock reports the other daemon.
- **Deterministic teardown.** Accept tasks are joined (named per task) on
  shutdown — the UDS socket file removal no longer races the process exit
  (the old integration test slept 100ms to mask it), accept errors back off
  exponentially instead of hot-spinning, and session teardown is concurrent
  rather than serial.
- **Detached daemons get their own process group** (`process_group(0)`):
  a terminal Ctrl-C no longer kills a `--detach`-spawned daemon.
- `SessionRegistry::publish_for_test` is now the production
  `publish_host_event`.

### Removed

- **`crates/neenee-server` crate (ADR-0102).**
  The standalone binary crate has been removed and its daemon execution logic
  consolidated into `neenee serve` in `neenee-cli`.
- **`neenee-server --project`** — a silent no-op since the ADR-0096
  project-agnostic daemon; it was previously a hard usage error and is now
  completely removed with the binary consolidation.
- **`neenee completions <bash|zsh|fish>`** prints a static shell completion
  script.
- **Friendlier usage errors.** Misuse now prints a short error plus a
  `--help` pointer to stderr with exit 2 (previously a full usage wall);
  unknown options are no longer misreported as unknown commands, and a
  near-miss command earns a `tip: a similar command exists: '…'` suggestion.
- New reference page: [Command line](docs/reference/cli.md).

### Changed

- **Vocabulary normalized on "session daemon" (ADR-0099).** The process is
  the *(session) daemon* in prose and user-facing strings; `host` stays the
  code namespace, `serve` the verb, `neenee-server` the binary. The how-to
  guide moved to
  `docs/how-to/track-sessions-with-a-session-daemon.md`, and the client
  identifiers followed (`ensure_daemon`, `DaemonInfo`). No artifact was
  renamed.
- **Workspace topology cleanup (ADR-0098).** Crate renames and extractions;
  no user-visible behavior change (the `neenee` binary, CLI surface, config
  schema, and wire protocol are unchanged):
  - `neenee-core` renamed to `neenee-contracts` (its admission rule is in the
    name now), with module names normalized to snake_case
    (`color_scheme_config`, `doom_guard_config`, `skills_config`,
    `web_config`, `channel_auth`).
  - `neenee-transport` renamed to `neenee-host` — it is the session host, not
    a transport; it also gains the control-plane `client` module (moved from
    the CLI), so the `serve::Wire` protocol's client and server live in one
    crate.
  - New `neenee-tui` library crate: the entire terminal frontend (app shell,
    view tree, `showcase`) extracted from the `neenee-cli` binary, which is
    now a thin shell (~840 lines, down from ~68k).
  - New `neenee-mcp` crate: the MCP connector re-extracted from
    `neenee-agent` (restoring ADR-0060); the agent has no MCP protocol
    dependency.
  - Removed the dead `neenee-persistence::search_tool` module and the unused
    `dirs` dependency from `neenee-persistence`.

## [0.23.0] - 2026-08-13

### Changed

- **Tool dispatch runs on the task-level scheduler (stage-3 switchover).**
  The concurrent fan-out moved from conflict-free sub-batches
  (`group_by_conflict`, batch-parallel / batch-serial) to the ported
  `ToolScheduler` state machine (kimi-code model: FIFO + anti-starvation,
  full re-scan on every completion). A queued call now starts as soon as its
  own conflicts clear instead of waiting for its whole predecessor batch.
  `dispatch_tool_calls` is decomposed into named preflight / schedule /
  finalize stages (`dispatch_pipeline.rs`); the permission chain still
  evaluates inside each task, so hook concurrency and permission parking are
  unchanged. Interrupt semantics (cooperative drain with
  `ENVOY_DRAIN_GRACE`, terminal `ToolCancelled` per unproduced call,
  input-order recording) are preserved, and a panicking tool task now
  resolves as an ordinary error instead of stranding the scheduler queue.

### Removed

- **The ADR-0095 mirror channel leaves the wire (ADR-0096 follow-through).**
  `Select{action: Mirror}`, the `Wire::Mirror` / `Wire::MirrorUpdate`
  envelope variants, `MirrorHello`, and `SessionHosting::Mirrored` are
  deleted, along with the daemon-side machinery that served them
  (`serve::run_mirror`, the registry's mirror-row store and its
  `mirror_adopt` / `mirror_upsert` / `mirror_remove` methods) and the
  dashboard's mirrored-row rendering. Under unified daemon ownership every
  session is `hosted`; the `hosting` field stays on monitor rows
  (serde-defaulted) so older peers still deserialize. No shipped client ever
  sent mirror frames — the standalone-TUI path they served no longer exists.

### Fixed

- **Daemon control-plane token is now cryptographically random.** The
  `--public` bearer token was derived from clock nanoseconds XOR the pid —
  guessable enough to brute-force on a LAN. It is now 256 bits from
  `getrandom` (two UUIDv4 halves), compared in constant time, and no longer
  printed to stderr at startup (the discovery-file path is printed instead;
  the file itself stays 0600). `neenee-server`'s usage text now matches its
  parser (`--public`, not the stale `--expose`).
- **The daemon fires SessionEnd hooks.** `kill_session` (including the idle
  reaper) and daemon shutdown now run each hosted session's SessionEnd hooks;
  previously they only fired on the in-process exit path, which ADR-0096 made
  unreachable.
- **Attach-mode sessions now land in the calling client's project, not the
  daemon's.** The unified daemon inherits its working directory from
  whichever client first spawned it and used that directory to scope
  fresh-session creation, auto-attach, and lazy resume — so attaching from
  another project grouped the session under the wrong one. The attach
  handshake's `Select` frame now carries the client's working directory as an
  optional `project` field; older clients omit it and keep the daemon-cwd
  fallback.
- **`/serve` no longer quits the TUI.** Starting, stopping, or re-issuing
  `/serve` in a standalone session ran the whole interception and then exited
  the event loop; it now returns to the conversation.
- **The LLM client no longer waits forever on a stalled endpoint.** The
  pooled client gets a 15 s connect timeout, and non-streaming chat requests
  get a 300 s overall timeout (streaming stays unbounded by design; the
  harness owns the 120 s idle-stall guard). Stall errors classify as
  retryable, so the turn retries with backoff instead of hanging.
- **`#[derive(ToolSchema)]` emits field descriptions again.**
  `#[tool(desc = "...")]` was parsed as a bare string literal, so every
  field's description was silently dropped from the generated JSON Schema —
  the model saw bare types. The attribute is parsed properly now, unknown
  `#[tool(...)]` keys are compile errors, `Vec<T>` maps to a typed array, and
  generic structs derive correctly.

## [0.22.6] - 2026-08-13

### Changed

- **Effort vocabulary is now open, not closed.** The seven `Effort` rungs are
  the words providers use, not a ceiling. A new `EffortLevel` type
  (`Known(Effort)` | `Other(String)`) lives on the runtime per-channel view
  (`ModelCapabilities` / `RemoteModelMetadata`) so a provider-advertised tier
  the vocabulary does not name is **preserved and stamped through to the wire
  verbatim**, rather than silently dropped. The closed `Effort` enum stays
  `Copy` on the static registry; openness lives only where live discovery lands.
  This honors ADR-0065 (live discovery is authoritative) end-to-end: a provider
  adding a tier reaches the wire with no neenee release.

### Fixed

- **`Effort::parse` no longer silently drops unknown upstream tiers.** Live
  discovery (Kimi K3, Copilot) advertised effort values were filtered through
  the closed enum and any unknown tier vanished without a trace. The ingress
  now uses the non-dropping `EffortLevel::parse`; the fitted-overlay →
  static-registry bridge (which must narrow to the `Copy` `Effort` vocabulary)
  logs any tier it cannot carry instead of dropping it quietly. Unknown tiers
  still survive on the channel's runtime metadata path.

## [0.22.5] - 2026-08-13

### Added

- **DeepSeek migrated to the OpenAI Responses API.** The built-in `deepseek`
  template now seeds the Responses endpoint (`api.deepseek.com/v1/responses`)
  instead of chat completions, and adds the `deepseek-v4-pro-0813` model (Pro
  gained the Responses surface with the 0813 release; Flash 0731 GA already
  had it). A new API-key `openai-responses` transport path carries the channel
  `effort` override onto DeepSeek's `reasoning.effort`, and Responses parsing
  surfaces DeepSeek's raw chain-of-thought via `reasoning_text` parts.

### Changed

- **Reasoning-effort abstraction unified.** `Effort` is now documented as the
  single provider-independent depth concept, in two explicit layers: the
  *abstraction* (`Effort` → each public API specification's depth field) and the
  *baseline value-sets* (the `EFFORT_*` consts). Every `EFFORT_*` const now
  states whether upstream advertises tiers (Kimi K3, Copilot — live-discovered,
  the const is a pre-fetch seed) or advertises nothing (OpenAI/xAI/DeepSeek/Z.AI
  /Google — the const *is* the effective ladder, sourced from prose docs), with
  citations. This makes the precedence chain — live discovery → static baseline
  → `&[]` — explicit instead of implicit. Added DeepSeek, GLM-5.2, and Gemini
  (`thinkingLevel` for 3.x / `thinkingBudget` for 2.5) effort ladders; merged the
  byte-identical Kimi-K3 / DeepSeek consts into the shared `EFFORT_LOW_HIGH_MAX`.
  New reference: [docs/reference/effort.md](docs/reference/effort.md).

### Fixed

- **Gemini 2.5 `thinkingBudget` doc bug.** `gemini_thinking_budget` documented
  `High` as returning `-1` (dynamic); it actually pins the model's full cap (a
  deliberate request is never "let the server decide"). Doc corrected to match
  the code.
- **ADR-0065 / Copilot fitting discrepancy.** The ADR claimed fitting was
  enabled for "only `kimi-code`", but `copilot-oauth` also sets `fitting: true`
  (it advertises `supports.reasoning_effort`). ADR updated to name both.
- **Release workflow cross-compile target install.** The `rust-toolchain.toml`
  pin must carry the cross target; installing it into `stable` via the action's
  `targets` input left the build toolchain without it (E0463).

## [0.22.4] - 2026-08-13

### Added

- **Google Antigravity OAuth subscription provider.** A new
  `antigravity-oauth` channel exposes Google-native models over native Google
  REST, resolving the live OAuth access token from `auth.toml` (key
  `google-antigravity`) and refreshing it at activate/switch time. Registered
  with a public client id and an offline-access consent scope, it uses the
  browser login flow by default. The OAuth bearer resolution in the agent
  catalog and model discovery was collapsed from three per-provider match arms
  into a single `oauth_provider_id()`-driven lookup, so future OAuth channels
  no longer need bespoke resolution code; a matching `antigravity-oauth`
  template appears in the TUI provider picker with the correct labels.

## [0.22.3] - 2026-08-12

### Added

- **Effort ignition: selecting a model's top reasoning tier fires a
  footer-wide celebration (codex `ultra` port).** When the active model's
  effective effort reaches `max` (Kimi K3's top tier — via the Models
  picker's `e` editor, or by switching to a model already pinned to `max`),
  the composer band and hint bar ignite for ~1.3s: two warm fire-wave
  crests sweep left→right across the panel's background (a staggered chase
  crest makes it read as more than a single pass), a violet `· → ✦ → ✧`
  spark lands on the band's top-right corner once the wave has landed, the
  hint bar's identity cluster collapses into a converging `M A X` label
  whose letters fall inward from wide-at-the-edges gaps to a tight centered
  row, and the `›` prompt tint charges toward the fire accent in a 150ms
  ramp. The glow only ever repaints cell *backgrounds*, so a half-typed
  draft rides the wave untouched, and the prompt glyph itself never changes
  — it stays `›`, returning to its ordinary color once the animation ends.
  The animation is timed against a single wall-clock epoch (like the
  breathing dot), so its cadence is immune to the event loop's irregular
  wakeups, and it is dropped from the `animating` set the frame it
  finishes. `neenee showcase ignition` runs it standalone (Space
  re-ignites).

- **`neenee dashboard`: the full-screen session dashboard, straight from the
  shell.** Reaches the same in-terminal control panel as TUI `/dashboard`
  (ADR-0096) without first entering a session. The client attaches to the
  daemon's most-recently-active hosted session purely as the underlying TUI
  carrier and raises the dashboard over it on the first frame — the
  dashboard's monitor stream and its control verbs (interrupt / prompt /
  create) ride their own daemon connections, so the panel never depends on
  that carrier. Esc from the opening dashboard quits (there is no
  conversation the user asked for behind it); `a` on a dock card attaches
  into that session through the ordinary re-attach loop (Enter previews).
  Like `neenee status`
  (ADR-0093), it never spawns a daemon: a missing daemon or an empty host is
  a clean error, not an excuse to spawn one or fabricate a session. Also
  fixes the dashboard/`status` monitor stream to prefer the daemon's Unix
  domain socket (previously TCP-only, so the panel could come up empty
  against a UDS-only daemon).

- **`F4` steers the running round: insert the composed message at the next
  safe turn boundary.** Where a busy `Enter` stages the message in the outbox
  to wait for a fresh round, `F4` hands it to the *currently running* round
  via `AgentRequest::InsertUserInput` — the agent admits it as a visible user
  message between its ReAct turns, so a correction or a missing piece of
  context lands in this round instead of queuing behind it. While admission
  is pending the item stays in the queue bar marked with a `steer›` badge; a
  race loss (the round ended first) returns it to the outbox as a paused
  next-round entry, so nothing is ever dropped. In-flight steers are pinned
  out of the Queue modal's edit/delete/reorder range until they resolve.

### Changed

- **The queue bar condenses to a single row.** The outbox summary is now one
  line — `QUEUE N · inline next-item preview … F4 insert  F3 block  F2 expand`
  — instead of a two-row header + preview stack (`QUEUE_BAR_ROWS = 1`). The
  preview truncates under width pressure before the legend sheds its labels
  and clusters, so the identity always survives; an empty outbox still hides
  the row entirely, and an idle session gains one more transcript row.

- **Footer chrome packs three rows tighter.** The activity bar now sits flush
  against the composer (`ACTIVITY_COMPOSER_GAP_ROWS = 0`), the hint bar flush
  against the composer's bottom edge (`COMPOSER_HINT_GAP_ROWS = 0`), and the
  bottom viewport margin is gone (`VIEWPORT_BOTTOM_MARGIN = 0`) so the hint
  bar pins flush against the terminal's bottom edge — the composer's own
  top/bottom panel-bg padding rows already provide the visual separation, so
  the gap/margin rows only burned transcript space. The 1-row top margin
  stays (the transcript still breathes at the top when no head row is
  shown); a round in flight now leaves three more rows for the conversation.

- **Provider-switch confirmation is a toast, not a transcript row.** The
  `ℹ Provider switched to …` line is no longer appended to the transcript as
  an inline notice (ADR-0088: a status confirmation is acknowledgment, not
  conversational content). A genuine user-initiated switch (`/models`, the
  Models picker, `/provider`) now surfaces a transient toast — emitted by the
  harness wrapped in `RoundEvent::Notice` so every attached client (in-process
  TUI, `neenee attach`, `/serve`) sees it — while the hint bar keeps the
  long-lived "still in effect" indicator and the durable command ledger
  records an `Ack` for audit (ADR-0091). Startup/attach synthetic
  `ProviderSwitched` replays only re-hydrate the hint bar: no toast, no
  transcript row. Provider *rebuilds* (edit/delete/reasoning/reapply) stay
  silent and unrecorded, exactly as before.

- **Bare `neenee` always starts a fresh session.** Previously the daemon
  auto-bound the caller to an existing session whenever exactly one was
  hosted (so `cargo run` / `neenee` silently continued the previous
  conversation). Resuming is now always explicit: bare `neenee` sends
  `AttachAction::New`; `neenee resume` picks from the daemon's sessions;
  `neenee resume <id>` attaches to that id directly. Also completes the
  Ctrl+C quit-window migration to a wall-clock deadline
  (`ctrl_c_armed_until`), so the ~2s double-press window no longer stretches
  to ~20s when the loop idles at its 1s heartbeat.

### Added

- **Hint bar shows the serving instance and Kimi K3's effort tier.** The
  right cluster now renders `Model effort @instance  context` — e.g.
  `Kimi K3 max @kimi-code  89.2k (8%)`. The muted `@<instance>` suffix
  carries the provider instance's display name so identical models served by
  different instances stay attributable (mirroring the `· <provider>` suffix
  in the Models picker); it drops before the context meter on narrow
  terminals. Kimi K3's registry entry advertises its `low`/`high`/`max` effort
  ladder (the shared `EFFORT_LOW_HIGH_MAX` const; refreshed live from the
  platform's `think_efforts.valid_efforts`), so requests pin
  `reasoning_effort` and the effort tag shows for it; models whose effort
  ladder tops out below `high` now default to their deepest tier instead of an
  unsupported `high`.

- **Unified session daemon and the control plane (ADR-0096).** neenee is now  a client of one user-level daemon (`neenee serve`, or the `neenee-server`
  binary) that owns **every session across every project**. The daemon
  exposes a session-management **control plane** — create a session, send a
  prompt, interrupt, answer a permission, kill — alongside the existing
  attach and monitor roles, all over one WebSocket handshake. It listens on
  a **Unix domain socket** by default (`$XDG_RUNTIME_DIR/neenee/daemon.sock`,
  0600 in a 0700 dir; filesystem permissions are the auth boundary, no token)
  and additionally on TCP with a mandatory bearer token when `--expose` /
  `--public` is given (ADR-0054 model). Discovery is now a single global
  record (`daemon.json`) carrying the UDS path. `neenee serve --detach`
  forks the daemon into the background (and refuses to start a second one).
  The TUI gains a **`/host` control panel**: a live view over all daemon
  sessions with per-row status and a selected-row preview; Enter switches
  the TUI to a hosted session (detach + re-attach — the previous round keeps
  running in the daemon). See
  [ADR-0096](docs/adr/0096-unified-session-daemon.md).

- **Daemon-architecture documentation sweep.** A new concept page,
  [The session daemon and the control plane](docs/explanation/session-daemon-and-control-plane.md),
  explains the process topology, ownership, transports, and lifecycle;
  `crate-layering.md` is rewritten for the unified model; `commands.md`
  documents `/host` and marks in-TUI `/serve` superseded; `glossary.md`
  updates the cli/server/attach entries and adds session-daemon /
  control-plane / `/host` terms (plus a legacy Mirror entry); `paths.md`
  records the global `daemon.json` + `daemon.sock`; `server-api.md` and
  `server.asyncapi.yaml` gain the Control verbs and the UDS transport with
  all Mirror remnants removed; READMEs list the daemon/control-plane as a
  headline feature; ADR-0081/0089 are marked partially superseded.

### Changed

- **Every interactive session is daemon-held (ADR-0096).** Bare `neenee`
  and `neenee resume [id]` now attach to the unified daemon (spawning it if
  needed) instead of assembling an in-process harness. Two behaviour changes
  follow directly and are intended: **closing the TUI no longer ends the
  round** (the session lives in the daemon; re-attach any time), and
  **switching sessions from the `/host` panel never kills running work** —
  it is detach + attach, not the old `/session open` supersede. The ADR-0095
  mirroring bridge is removed: with single ownership every session is
  `hosted`, so the mirrored/hosted distinction and the mirror channel are
  gone. This is a hard behaviour change on the still-unreleased serve/attach
  surface.

### Removed

- **Session mirroring (ADR-0095) and the in-process harness path.** The
  mirror tap, supervisor, `Mirror`/`MirrorUpdate` frames, and
  `SessionHosting::Mirrored` are deleted; the unified daemon holds every
  session, so a TUI no longer needs to report a session it owns. The
  `Wire::Mirror` / `Wire::MirrorUpdate` envelope variants are retained on
  the wire for one release as a parsing no-op for older clients, then
  removed.

### Fixed

- **`neenee daemon` now actually parses.** ADR-0089 accepted the subcommand
  and `main.rs` handled it, but `parse_args` never produced
  `StartupMode::Daemon` — the branch was unreachable and `neenee daemon`
  exited with "Unknown command 'daemon'". The parser now has the arm, and
  the usage text lists the full daemon surface (`daemon`, `attach`,
  `status`).

### Changed

- **Hint bar model identity: `(effort)` parentheses and an `@ instance` tag
  replace the `◆` diamond.** The reasoning-effort attribute now hugs the
  model name as a plain parenthetical (`Kimi K3 (max)`) instead of the `◆
  max` glyph form, and the bar can finally tell you *which* provider instance
  is serving the model: when the active model is offered by more than one
  configured instance, an `@ {instance}` tag joins the identity group in
  muted (`Kimi K3 (max) @ 官方中转`); single-instance setups stay quiet.
  Under width pressure the instance tag drops first, then effort, context,
  and finally the model name — unchanged priorities otherwise.
  (`HintBarView::provider_label`; the `/models` picker's `◆ think on ·
  <effort>` reasoning tag is untouched.)

## [0.22.2] - 2026-08-07

### Added

- **Interrupted envoys keep their work.** When you stop a turn (Esc) while a
  research/coding envoy (`envoy` / `envoy_code`) is mid-flight, the partial
  transcript is no longer discarded with the dropped future. The envoy is
  cancelled *cooperatively*: it stops at its next safe boundary, returns the
  half-finished transcript, and the parent records it as an **Interrupted**
  tool step — with its partial `children` persisted, a `↳ Interrupted · N tool
  calls` status line, and a model-visible `Interrupted: …` summary that
  preserves the findings so far. On resume the step rebuilds with its nested
  transcript and true classification, and a follow-up like "pls go on" lets
  the model continue from where the envoy stopped (or re-delegate) instead of
  cold-restarting. Failure classification is untouched — an envoy that errors
  on its own still reads `Failed`. (Cooperative cancel: `Tool::
  supports_cooperative_cancel` / `Tool::request_cancel`; executor drain with
  a bounded grace; `ToolOutput::Envoy.interrupted` /
  `EnvoyMeta.interrupted`; `ToolStepStatus::Interrupted`.)

- **Colored attachment chips in the composer.** Pasted text blocks and images
  now render as distinct tinted "pills" inside the input box instead of plain
  prose: a large-text paste chip (`[Pasted text #N +M lines · size]`) paints
  calm blue, an image chip (`[Image #N · size]`) paints warm amber, each bold
  on a tinted band derived from the theme. The chip label is now a real
  identifier — it reports the hidden payload's line count **and** byte size
  (`[Image #1 · 24.1 KB]`), the composer recolors the pill when a chip wraps
  across rows, and selection keeps the identity color so you can always tell
  which block is selected. Sizes are re-derived from the staged payload on
  every reconcile, so a relabeled chip never reports a stale byte count.
  (`Theme::chip_paste_fg/bg`, `Theme::chip_image_fg/bg`.)

- **Chips are only colored when a payload is really staged.** The pill is
  applied per the actual staged state (`pending_images` / `pending_text_pastes`),
  not the label text alone: an image or paste chip whose `#N` has no backing
  payload — typed by hand, or left over after the paste was undone — renders
  as ordinary text, so a literal `[Image #1]` never reads as an attachment
  that isn't there. This mirrors the submit path, which already drops unbacked
  chips before the model sees them.

### Changed

- **`/autopilot` with no argument now toggles, and `on|off` complete.**
  Previously a bare `/autopilot` was an error ("Unknown value ''. Use
  `/autopilot on|off`.") even though the dispatch carried an (unreachable)
  toggle branch; and once the space was typed after `/autopilot` the
  completion menu dead-ended because no subcommand candidates existed. The
  missing-argument case is now wired to flip the current state (`on`/`true`/`1`
  and `off`/`false`/`0` still set explicitly), the error hint names the toggle
  form, the command vocabulary says "no argument toggles", and the completion
  menu offers `/autopilot on` / `/autopilot off` after the space. The toggle
  is surfaced exactly like the explicit forms: an `Ack` toast plus an
  `AutopilotChanged` event, so the status-bar badge always reflects the flip.
  (`parse_autopilot_arg`, `App::completions`.)

- **Todo and queue bars de-cluttered.** The one-row todo summary no longer
  leads with a `📌` pin glyph, and the two-row queue bar no longer leads with a
  `📤` tray glyph or a next-item send time (`HH:MM`); neither sits on a raised
  tint anymore. Both render as quiet metadata on the plain frame surface —
  brand-colored `TODOS` / `QUEUE` tags, counts, and content previews (the
  per-item send time still lives in the Queue modal). Their right-pinned keycap
  legends (`Ctrl+T expand`, `F3 block  F2 expand`) now keep a guaranteed
  `BAR_LEGEND_GAP_MIN` (6 cols) of breathing room from the content, so a
  truncated item's `…` never butts against a keycap. (`draw_todo_bar`,
  `draw_queue_bar`.)

- **A join ladder now governs relationship spacing.** The `·` middle dot was
  drifting into a universal separator — joining same-rank peers, different
  levels, and different granularities with the same glyph, so it carried no
  information. The new [visual-language](docs/reference/tui/visual-language.md)
  reference defines one rule — *the tighter the relationship, the quieter the
  join* — with a four-rung ladder: R0 atomic values (`1.5 KB`, `F3 block`),
  R1 attribute joins (`Thinking · 120 chars`, `[Image #1 · 24.1 KB]` — the one
  sanctioned use of `·`), R2 same-rank peers (2 columns of whitespace: modal
  footer hints, empty-state suggestions, queue-bar legend), and R3 cross-group
  segments (`BAR_LEGEND_GAP_MIN`). Different *levels* use the ` › ` breadcrumb
  (`round 3 › turn 2`, `Connections › keybindings`) instead of `·`. The ladder
  is a single source of truth in `design.rs` (`JOIN_MODIFY`,
  `JOIN_ENUMERATE_COLS`, `JOIN_BREADCRUMB`), and all migrated surfaces now
  render through it.

- **Input history is now configurable and de-cluttered.** A new `[input_history]`
  table in `config.toml` controls the Ctrl+R prompt picker and the persisted
  `history.json`:
  - `dedup` (default `true`) — the same prompt text collapses to a single
    entry **across sessions and workspaces**; re-sending bumps it to the top
    of the newest-first list instead of adding a duplicate row. Set `false`
    to keep per-session entries as before.
  - `record_commands` (default `false`) — `/slash` command invocations
    (`/model`, `/clear`, …) are no longer recorded, and any legacy ones stop
    showing in the picker; they are UI gestures already visible in the
    transcript. Set `true` to make them recallable again.
  - The picker's footer no longer trails the selected row's `~/project ·
    #session… · time` origin strip — redundant noise when the row number and
    prompt text already anchor selection (the workspace/session stamp is still
    stored per entry and still drives the per-session ↑/↓ recall).
  - `Ctrl+X` inside the picker clears the **entire** history: it arms a
    one-key confirmation (`y` wipes, any other key cancels) so a stray
    keystroke can never wipe it. (`InputHistoryConfig`,
    `neenee_core::merge_history` dedup identity, `App::clear_input_history`.)

### Fixed

- **↑/↓ and Ctrl+R recall now restore a message's image and large-paste
  attachments.** Previously a message sent with pasted images was recorded to
  input history as text-only, so interrupting it and re-sending via history
  recall shipped the bare `[Image #N]` chip label with no payload — the model
  received a phantom label it could not see. History entries now cache their
  staged attachments in memory (keyed by the entry's `(text, session_id)`
  identity, capped FIFO, never persisted — `history.json` stays rebuildable
  cosmetic telemetry), the ↑/↓ walk and the Ctrl+R insert restore them into
  the composer, the first-↑ draft stash keeps them for the ↓-past-newest
  round-trip, and an orphaned chip with no staged payload is stripped from the
  text at dispatch instead of leaking to the model as a literal placeholder.

- **↑/↓ keep working once the composer holds a fully-typed `/command`.**
  Previously, a completed slash command pinned the arrow keys: the completion
  menu (whose only exact-match row was the text already in the box) captured
  every ↑/↓ as a no-op suggestion move, so input-history navigation became
  unreachable right after switching to a command. A fully-typed command is now
  treated as a *resolved* state — its popup is hidden, Tab stops invisibly
  cycling sibling candidates, and ↑/↓ resume their ordinary history role. A
  partially-typed command keeps the interactive menu unchanged.

- **Inline ↑/↓ now walk input history in the correct direction.** The two
  directions were swapped: the first ↑ stashed the draft and loaded the newest
  entry, but a second ↑ clamped at position 0 instead of moving to the older
  entry ("只能往上翻一个"), while ↓ walked *older* rather than back toward the
  newest. ↑ now advances oldest-ward (clamping at the oldest entry) and ↓
  walks back toward the newest, restoring the stashed draft — text and any
  staged image / paste attachments together — once it passes the newest entry.
  Navigation logic moved into `App::history_prev` / `App::history_next` so the
  two keys can never drift, and history stamps are now strictly-increasing so
  a same-millisecond send burst still sorts newest-first instead of degrading
  to oldest-first on a timestamp tie.

- **Input-history recall now follows an explicit pointer model.** The composer
  is a pointer over two slot kinds: the **draft** — the newest position, the
  input that has *not* been successfully sent (still being composed, restored
  by a Phase-1 unsend, inserted from Ctrl+R, or recalled from the queue) —
  which is editable and remembered; and the **history rows**, which are
  read-only snapshots (edits on a row are temporary and discarded when the
  pointer moves). The draft slot is now correctly cleared when a send succeeds
  (the input is historicised, so a later ↓ never resurrects an already-sent
  prompt) and correctly **replaced** whenever new input is adopted as the
  newest unsent slot — a Phase-1 unsend restore, a Ctrl+R insert, or a queue
  recall re-seeds the draft, so ↓ past the newest history row restores the
  current input, never a stale earlier draft. (`App::adopt_as_draft`,
  `App::clear_history_draft`, `App::restore_dispatch`.)

## [0.22.1] - 2026-08-01

### Added

- **DeepSeek V4 Flash (0731) support.** Added `deepseek-v4-flash-0731` model to built-in provider templates (`deepseek`, `opencode_go`), capability registry, baseline fidelity tests, and catalog tests.

- **Project trust gate extended to the whole package (hooks + commands),
  git-root alignment, and untrusted bash hardening.** The per-project trust
  gate (ADR-0085 §5) previously gated only project-scope `[mcp.*]` servers. It
  now matches the codex "project-local config, hooks, and exec policies" model:
  - **`[[hooks]]` declared in a project `.neenee/config.toml`** now load only
    under the same trust grant. A project hook runs a repo-supplied shell
    command (`.neenee/hooks/*.sh`), so it is the same class of prompt-injection
    hazard as a project MCP server and must not auto-execute from a
    cloned/vendored tree. (`Config::load_project_hooks` + `merge_project_hooks`.)
  - **`.neenee/commands/` slash-command templates** are skipped while a project
    is untrusted; user-global commands always load. (`discover_commands_trusted`.)
  - **Trust is now git-aware.** A grant resolves to the repository root via a
    pure-filesystem `.git` walk (including linked worktrees), so one trust
    decision covers every subdirectory and worktree of a repository — no more
    re-prompting when `cwd` is a subdirectory or a linked worktree.
    (`resolve_trust_root`.)
  - **Bash hardening for untrusted projects.** While a project is untrusted,
    `bash_policy` gets `autopilot_confirm` locked to `deny` and a `confirm`
    rule prepended for fetch/install/pipe-to-shell commands (`npm install`,
    `pip install`, `curl … | sh`, …) — the classic prompt-injection payloads.
    `/trust` re-seeds the raw (un-hardened) policy.
  - `/trust` and `/untrust` now apply/revoke the whole package (MCP + hooks)
    and re-seed the bash policy accordingly; their help text reflects this.

### Changed

- **TUI breadcrumb navigation for modal headers.** Updated connection chooser, provider editor, and model settings modals with breadcrumb title headers.

## [0.22.0] - 2026-07-31

### Added

- **Scheduled prompts: one-shot timers via `/schedule`, unifying the cron
  scheduler.** The cron-driven `/repeat` scheduler is generalized into a
  unified scheduled-prompt system. The new `/schedule <when> <prompt>` command
  accepts a five-field cron (recurring), a relative countdown (`10m`,
  `2h30m`, `in 2 hours 30 minutes`), or an absolute time (`14:00`,
  `tomorrow 09:00`, `2026-03-15 14:00`). Recurring jobs fire their first run
  immediately and continue on schedule; one-shot jobs fire once at their
  scheduled instant and are then removed. This enables the "schedule a future
  input" / countdown scenario (e.g. a quota reminder that fires a new round
  after a delay) alongside the existing recurring-cron use case. `/repeat` is
  retained as a cron-only alias. The session state type `RepeatJob` is
  generalized to `ScheduledJob { trigger: Schedule::Cron | Schedule::Once }`,
  persisted as session-schema v9 with full back-compat: legacy flat
  `repeat_jobs` snapshots and `repeat_jobs_set` event-log lines load unchanged
  via serde aliases and a manual legacy-shape deserializer. See ADR-0090.

- **`--autopilot` now works in attach mode.** Previously `--autopilot` parsed
  alongside `attach`/`--attach` but was silently ignored (the attach client
  drives a `neenee-server`-hosted session, which hardcoded autopilot off).
  Now a client that spawns the server forwards `--autopilot` to
  `neenee-server` (new server flag), and — to also cover an already-running
  server it did not spawn — re-sends the intent over the wire as
  `/autopilot on` immediately after the handshake. The slash command is the
  existing, idempotent path (a freshly spawned autopilot server ignores the
  redundant re-affirmation; an unrelated live server flips on), so the
  status-bar `autopilot` badge reflects the flip exactly like a hand-typed
  `/autopilot on`. Revises ADR-0081 consequence #8.

- **ADRs are linked from the documentation index.** `docs/index.md` now lists
  Architecture Decision Records alongside the How-to, Reference, Explanation,
  and Contributor sections, so the decision-record catalog is reachable from
  the entry point instead of only via in-text cross-references.

### Changed
- **History dropdown is now an extension of the composer.** The Ctrl+R input-
  history panel is restyled to share the composer's surface language — a solid
  panel fill bracketed by half-block `▄`/`▀` transition rows — so it reads as
  continuous with the input box rather than a separately-bordered floating
  window. The previous full-height brand-colored left accent bar (which read
  as selection/severity) is gone. The panel now collapses to its actual row
  count (plus the edge/header/footer rows) instead of reserving a fixed
  minimum, and is capped at ten entries beyond which the body scrolls — a
  Ctrl+R picker is a glance surface, not a full browser. It also reserves the
  transient activity bar's rows as a ceiling: the panel never grows into the
  activity bar above the composer, so the live status surface is always visible
  and always reads as above the history list.

- **The activity bar signals a pending permission request.** While a tool-
  permission request awaits the user's decision, the activity bar above the
  input box is forced on (even if the loop has nominally gone idle) and its
  label is rendered in a steady warning hue rather than the ordinary shimmer,
  so it reads as a distinct "paused on your decision" attention state. The
  permission sheet replaces the input box and the bars beneath it; this bar is
  the one piece of live status that survives, so marking the state here is
  what tells the user why the round is waiting.

- **Multi-session daemon and select-then-attach protocol (ADR-0089).** A
  single `neenee-server`/`neenee daemon` process now hosts any number of
  sessions for one project via an in-process `SessionRegistry` (each
  session remains its own writer under the ADR-0018 invariant). The attach
  wire format gains a select-then-attach handshake: the client sends
  `Select { action }` (`New` / `Attach(None)` / `Attach(Some(id))`) and the
  daemon replies `Welcome` (bound, with transcript), `Pick` (several
  candidates — list them), or `Error`. `Wire::History` is removed.
  `neenee daemon` starts the host in the foreground; `neenee attach [id]`
  binds to the daemon (spawning one if none runs). The discovery record
  drops its `session_id` field (a daemon is multi-session).

- **`unattended` renamed to `autopilot`.** The no-human-intervention mode flag is renamed
  across the agent, permission store, envoy/principal profiles, events, server wire schema,
  CLI flag, slash command, and docs. The flag's *behaviour* is unchanged (auto-approving
  tool permissions, reclaiming `ask_user`, closing interactive stdin); only the name and
  user-facing surfaces move to a clearer, positive term: `--autopilot`, `/autopilot [on|off]`,
  a status-bar `autopilot` badge, the `AutopilotChanged` events, and `HarnessState.autopilot`
  on the wire. `unattended` → `autopilot` everywhere; the `BashPolicyConfig.unattended_confirm`
  field and `BashPolicyUnattendedAction` enum become `autopilot_confirm` /
  `BashPolicyAutopilotAction`. `docs/explanation/agent-design/unattended.md` is renamed to
  `autopilot.md`; ADR-0087's file follows. Note: this supersedes the earlier `auto_approve`
  → `unattended` rename — the wire field key and event names change again, so clients must
  update.

- **`CODE` envoy runs on autopilot.** The coding envoy profile
  (`envoy_code` dispatch tool) now runs `autopilot: true`, matching every
  other built-in envoy: the principal's act of calling `envoy_code` *is* the
  authorization for the delegated task, so the child's writes and commands
  no longer surface as per-call permission requests through the broker. The
  permission sheet stays the principal's gate; the principal reviews the
  envoy's handoff and stays accountable for the result. `ask_user`
  supervision is preserved via the full-duplex channel. Decided in
  [ADR-0087](docs/adr/0087-code-envoy-runs-autopilot.md), which supersedes
  ADR-0086's `autopilot: false` decision.
- **`envoy_code` renders as an envoy step in the TUI.** A coding delegation
  now renders through `draw_envoy_inline_step` with the `EnvoyPresenter`
  summary — one navigable line plus a live status line, `Enter` to drill
  into the child transcript — identical in shape to an `EXPLORE` run. Fixes
  `is_envoy_task()` / `presenter_for` matching only `envoy`, which left
  `envoy_code` falling through to the generic expandable tool-step
  ("disclosure") renderer.

### Removed

## [0.21.3] - 2026-07-30

### Added

- **Project-scope MCP via `.neenee/config.toml`.** A project may now declare
  `[mcp.*]` servers in its own `.neenee/config.toml`, layered on top of the
  global config: a project entry overrides a same-named global one and adds new
  servers, so a repo ships its own connectors alongside the user's global set.
  Read through a narrow TOML projection (`Config::load_project_mcp` /
  `merge_project_mcp`) that ignores unrelated keys, so a partial/incomplete
  project file never fails the load. Decided in ADR-0085, implemented in
  `crates/neenee-persistence/src/config.rs`; layered at bootstrap and on
  `/reload`.
- **Project trust gate.** Project-supplied MCP commands execute processes, so
  they load only after the user explicitly trusts the project root. Trust
  grants live in a JSON set under `$XDG_STATE_HOME/neenee/trusted_projects.json`
  (program-generated state alongside `history.json`/`provider_usage.json`, safe
  to lose). Untrusted projects load nothing and get a one-time hint. Global
  config remains trusted unconditionally. New `TrustGate` in
  `crates/neenee-persistence/src/trusted_projects.rs`; surfaced onto the
  session driver and consulted by bootstrap and `/reload`.
- **`/trust` and `/untrust`.** Grant or revoke trust for the current project's
  `.neenee/config.toml` external tools. `/trust` activates the project's MCP
  servers immediately; `/untrust` disconnects them. Registered as built-in
  commands in `crates/neenee-transport/src/startup.rs`; handled in
  `crates/neenee-transport/src/handlers_slash.rs`.
- **`/reload`.** Re-read `config.toml` and apply the diff live — no restart
  needed to add/remove/edit MCP servers, permissions, bash policy, hooks,
  principal settings, tool variants, or prune threshold. MCP is diffed and
  (re)connected/disconnected via the new `McpRuntime::reconfigure`
  (`ReconfigureReport`), which also re-layers project MCP gated by trust. The
  agent-scoped config sections that are otherwise seeded only at startup are
  re-applied via the existing replace-style setters. Registered in
  `crates/neenee-transport/src/startup.rs`; handled in
  `crates/neenee-transport/src/handlers_slash.rs`.

### Changed

- **Status bar layout flipped.** The status bar now leads with the `autopilot`
  flag on the left and trails with the tilde-shortened workspace path on the
  right — previously the workspace led and the flag trailed. A silent agent
  running is the most glance-worthy session state, so the warning-toned flag
  now sits next to the input where the eye lands. The workspace path is still
  always rendered; on narrow terminals it is now truncated from the left
  (`…suffix`) so its most specific tail (the project directory) stays pinned
  to the right edge, and the flag drops before the path disappears. Updated in
  `draw_status_bar` (`crates/neenee-cli/src/tui/chrome.rs`).
- **`Enter send` keycap now uses the unified keycap style.** The hint bar's
  left action sentence ("Enter send" / "Enter queue message" / "Enter run
  command") hand-rolled its `Enter` keycap as `fg + bold`, diverging from every
  other keycap in the app (the activity bar's `Esc Esc to interrupt`, the
  queue bar's `F2`/`F3` legend, the modal footers). It now routes through the
  shared `keycap_style` (brand color + bold), so the affordance reads
  consistently across surfaces. Updated in `input_action_spans`
  (`crates/neenee-cli/src/tui/chrome.rs`).
- **`McpRuntime::configs` moved behind a `RwLock`.** `reconfigure` replaces the
  whole server map while readers (`set_enabled` / `reconnect` / `refresh_all` /
  `Drop`) only need a borrow, so the config map is now `RwLock`-guarded to
  support live hot-reload. (`crates/neenee-agent/src/mcp/runtime.rs`.)

### Removed

- **Idle progressive-disclosure machinery retired.** The `disclosure_ledger.rs`
  and `disclosure_bridge.rs` modules — ported from kimi-code but never wired
  behind a `select_tools` meta-tool, and carried under
  `#[allow(dead_code)]` — are deleted. ADR-0085 records the decision to scope
  external tools at config time (eager full-schema loading, `/reload` hot
  reload) rather than via runtime on-demand discovery. Files removed from
  `crates/neenee-agent/src/`; the module declarations dropped from `lib.rs`.

## [0.21.2] - 2026-07-29

### Added

- **`[principal] skip_interactive_input` config option.** When a `bash` command
  the interactive classifier matches (`sudo`/`gpg`/`passwd`/TUI editors/`read`/…)
  needs stdin, neenee normally pops an inline input panel (the command + a
  masked/plain field) to collect the response. Set `skip_interactive_input = true`
  under `[principal]` to **never** pop that panel: the command runs with stdin
  closed instead, reads EOF immediately, and fails fast with the existing
  non-interactive remedy hint — exactly as under autopilot mode, but without
  turning the principal itself autopilot (ordinary tool confirmations still
  apply). For users who find the prompt disruptive and prefer to retry the
  command themselves (or let the model retry with a non-interactive form). Added
  in `crates/neenee-persistence/src/config.rs` (`PrincipalConfig`), mirrored in
  `neenee_core::PrincipalRuntimeConfig`, threaded through `Agent` as an
  `AtomicBool` consulted by `decide_bash_stdin`, seeded in bootstrap and
  `apply_principal_profile`; documented in `docs/reference/configuration.md`.

- **`[tui] click_outside_dismiss` config option.** Clicking outside a modal
  to close it (mirroring Esc). **On by default**: a click on the backdrop of a
  dismissable overlay (Help, Tools, Sessions, Config, …) closes it like Esc —
  the composer draft is parked so nothing is lost. Modals that hold precious
  in-progress input (API-key editor, permission/question sheets, …) are never
  click-dismissable regardless of this flag, and the `neenee resume` startup
  picker's click-outside still quits the program. Set
  `click_outside_dismiss = false` under `[tui]` to require Esc / Ctrl+C for
  every close. Added in `crates/neenee-persistence/src/config.rs` (`TuiConfig`),
  surfaced onto `App`, and gated in the event-loop click handler; documented in
  `docs/reference/configuration.md`.

- **Session-info sub-view (`i`).** Pressing `i` in the `/sessions` picker (or
  the `neenee resume` startup picker) drills into a read-only detail view of
  the highlighted session, showing the breadcrumb header `Sessions › Info`: its
  id, stored title (if any), absolute creation and last-active timestamps,
  message count, active-state, and the **complete** last effective user prompt
  (not the truncated preview). The detail is fetched on demand
  (`QuerySessionDetail`) so it carries the full prompt without leaking the
  whole transcript to the TUI; `Esc` backs out to the list (a second `Esc`
  closes the modal). The list-only keys (`d`/`n`/`i`) are inert while in the
  sub-view. Added across `neenee-core` (`SessionDetail`, request/response
  variants), `neenee-persistence` (`SessionStore::detail`),
  `neenee-transport` (dispatch + handler), and `neenee-cli` (input, event loop,
  `App` state, and the `draw_sessions_modal` detail branch).

### Changed

- **Sessions are not persisted until they gain real content.** A session is
  now deferred to disk until the user sends their first message or runs a
  command — opening a session and exiting (e.g. via `neenee resume`, viewing
  `/sessions`, switching a provider, or a no-op `/clear`) no longer creates an
  empty session record that pollutes the history. Previously several metadata
  mutators (`set_title`, `set_provider_selection`, `replace_messages`,
  `mutate_messages`) wrote a brand-new empty session to disk unconditionally,
  so `set_provider_selection` (triggered by `/models`) was the common cause of
  empty-session litter. All write paths now carry the `empty_unpersisted` guard
  (skip persist when the session is empty AND never yet on disk); the
  `load_or_seed` constructor no longer seeds a `.jsonl` for a missing snapshot
  either. The first real message or command echo persists as before. Changed
  in `crates/neenee-persistence/src/session/mod.rs`.

- **Unified hierarchical (breadcrumb) modal headers.** Drill-in sub-pages now
  share one component-level convention instead of ad-hoc per-modal styling. A
  new `breadcrumb_parts(parent, child)` primitive (with a centralized ` › `
  separator) renders the standard `Parent › Child` header — a muted parent, the
  separator, then a bold child — and documents the modal-hierarchy rule it
  encodes: a sub-page keeps the *same* `Modal` variant as its parent (one modal
  drilling into a secondary view), the breadcrumb is how the user sees where
  they are, and `Esc` navigates one level up. Applied to `Sessions › Info` and
  used to align the existing Config sub-pages (`Settings › Layout`,
  `Settings › Appearance`, `Appearance › Custom palette`), which previously
  used a hand-written `"Settings  /  "` prefix with a different glyph. Changed
  in `crates/neenee-cli/src/tui/primitives.rs` (`breadcrumb_parts`,
  `BREADCRUMB_SEP`) and the `overlays/{session,config_layout,config_theme,
  config_theme_custom}.rs` headers.

- **Session delete / picker no longer re-parses every transcript on disk.**
  Deleting a session from the `/sessions` picker (and refreshing the picker
  after each delete) used to read and *fully deserialize* every session file —
  the whole recursive transcript (envoy `children`, tool calls, content blobs,
  provider meta, …) — just to extract a header row. On a project with hundreds
  of multi-megabyte sessions that was seconds of work per delete, so rapid
  deletes piled up and the picker visibly lagged or froze. Two fixes:

  - `SessionStore::list` now parses session snapshots with deferred
    `Box<RawValue>` message bodies — it records each message array's byte range
    but only decodes the one field it needs (the first user message's `content`
    for the preview) and counts the rest without allocating it. The picker now
    scales with the *number* of sessions, not their total content size.
  - `SessionStore::resolve_session` (called by every `delete` and `/session
    open`) now resolves an id prefix by matching directory **filenames** — a
    session's filename *is* its id under ADR-0018 — so it opens zero files on
    the common path. The old code fully deserialized every session's
    `SessionData` on each resolve. Legacy snapshots whose stored `id` differs
    from their filename are still reachable via an id-only fallback deserialize.

  Measured on a 618-session / ~2 GB project: per-delete resolve dropped from
  ~1.4 s (full parse of all files) to ~0 ms (filename match, no I/O), and the
  picker `list` from ~1.1 s to ~0.5 s. Changed in
  `crates/neenee-persistence/src/session/mod.rs` (`SessionHeader`,
  `SessionStore::list`, `SessionStore::resolve_session`); the `raw_value`
  serde_json feature is now enabled for `neenee-persistence`.

- **Sessions picker preview is the last real prompt, and the row shows only the
  active time.** Two related refinements to what each picker row displays:

  - The left-hand preview is now the **last effective user prompt** — the most
    recent user turn that is *not* a non-driving command echo. Slash commands
    (`/autopilot on`, `/session open …`) and `!shell` passthroughs (ADR-0050)
    are agent operations, not AI-conversation turns, so a row like
    `/autopilot on` was meaningless as a preview; it is now excluded. The
    deferred header parse was extended to read each message's `origin` (it
    previously decoded only role + content) so echoes are filtered by their
    `CommandEcho` provenance, not by fragile `/` string-sniffing.
  - The row meta now shows only the **active** time (compact relative form),
    dropping the duplicate `created` column — the full creation timestamp lives
    in the new `i` info sub-view.

  Changed in `crates/neenee-persistence/src/session/mod.rs`
  (`session_overview_header`, `last_effective_prompt`, `MessagePreview`) and
  `crates/neenee-cli/src/tui/overlays/session.rs`.

- **Write/operation-scope gate is soft when attended.** An out-of-scope tool
  call is no longer blocked outright when a user is reachable; it falls
  through to the permission broker for the operator to approve / always-allow /
  reject — handing the right to decide back to the user. It still hard-denies
  under autopilot (no human to answer), preserving the safety floor for
  autonomous runs. See [ADR-0084](docs/adr/0084-soft-write-scope-gate.md)
  (supersedes ADR-0028). Changed in `crates/neenee-agent/src/permission_policy.rs`
  (`ScopeGatePolicy`); docs updated in
  `docs/explanation/agent-design/{autopilot,rounds-and-turns,harness}.md` and
  `docs/reference/glossary.md`.

### Fixed

- **Ctrl+C at the `neenee resume` startup picker quits instead of dropping
  into an empty session.** Esc and an outside click already quit the program
  when the startup picker is showing (there is no conversation behind it), but
  Ctrl+C took its own arm that only cleared the modal (`active_modal = None`)
  and never set `should_quit` — landing the user in a bare empty chat. Ctrl+C
  now quits at the startup picker too, matching Esc / outside click. Fixed in
  `crates/neenee-cli/src/tui/event_loop.rs` (`CtrlC` arm).

- **Empty sessions are no longer persisted by a provider pin.** Pinning a
  provider/model on an otherwise message-less session (`/models` switch) used
  to write the snapshot, surfacing empty sessions in the picker. This was the
  common path: Ctrl+C at the startup picker dropped into an empty session (see
  above), and opening `/models` then persisted it. `set_provider_selection`
  now carries the same `empty_unpersisted` guard the other metadata mutators
  (`set_todos`, `set_disabled_tools`, …) already had, so a brand-new empty
  session stays unpersisted until it gains real content. Fixed in
  `crates/neenee-persistence/src/session/mod.rs`.

- **Footer `? help` chip never hides its label.** Under width pressure the
  modal footer chip used to degrade `? help` → `? …` → `?`, hiding the "help"
  label so the user could not tell what the chip offered (it is the only way to
  discover the hidden keymap). The label is now non-negotiable: another hint is
  dropped to make room for the full `? help` rather than truncating the label.
  Only a terminal narrower than `? help` itself (under ~6 columns) collapses to
  `…`. Fixed in `crates/neenee-cli/src/tui/components/footer.rs`.

- **Sessions picker keeps the cursor on the same line after a delete.**
  Deleting a row used to snap the selection back to the top of the list. The
  delete handler already removed the row optimistically and kept `modal_index`
  on the same line, but the backend then pushed a fresh sessions overview whose
  "open the picker" signal reset `modal_index` to 0 and `session_scroll` to 0 —
  fighting the optimistic update on every delete. The refresh path now resets
  the cursor/scroll only when the modal is genuinely *opening* (transitioning
  into the sessions picker); a data refresh while it is already open (such as
  the post-delete overview) preserves the cursor and scroll. Fixed in
  `crates/neenee-cli/src/tui/event_loop.rs` (sessions-overview refresh branch).

- **Sessions picker no longer hitches / freezes while open on large projects.**
  On a project with hundreds of sessions the picker did O(n) work on the
  render thread *every drawn frame*, and an unconditional per-frame clone of
  the whole overview — so navigating or deleting felt laggy and the UI could
  stall briefly. The delete path itself was already offloaded to background
  threads; the bottleneck was synchronous per-frame work on the event loop.
  Three fixes, all in `crates/neenee-cli/src/tui/`:

  - **Windowed row rendering.** `draw_sessions_modal` (`overlays/session.rs`)
    used to build a styled `Line` (several allocations each) for *every*
    session on disk every frame, even though only ~20–40 rows are visible.
    It now resolves the scroll against the true list length (factored into
    `primitives::resolve_scroll`) and builds only the visible window; the
    scrollbar still reflects the full list via the resolved `max_scroll`.
    Cost drops from ~n rows/frame to ~visible-rows/frame.
  - **Revision-gated overview clone.** The event loop mirrored the shared
    `sessions_overview` into `App` with a deep `Vec::clone()` (two `String`s
    per row) *every iteration*, whether or not it had changed. A new
    `sessions_overview_rev` counter (bumped by the response listener on each
    replacement) gates the clone so it happens only when the picker data
    actually changed. (`event_loop.rs`, `tui/mod.rs`.)
  - **One wall-clock read per frame.** Each picker row formatted its relative
    time with its own `SystemTime::now()` syscall (≈600 syscalls/frame on a
    large project). The now-computed-once value is threaded through a new
    `relative_time_at(ts, now)` (`overlays/common.rs`), replacing the
    per-row `relative_time_compact`.

### Removed

## [0.21.1] - 2026-07-27

### Added

- **Queue bar reference page.** `docs/reference/tui/queue-bar.md` documents the
  two-row outbox summary pinned above the input box, mirroring the todo-bar
  page.
- **Status bar.** A new one-row strip caps the footer directly below the hint
  bar, dedicated to ambient session state: the tilde-shortened workspace path
  (e.g. `~/projects/xx`) on the left, and persistent status flags
  (`autopilot`) on the right. It is always present while chrome is visible,
  so the workspace is always glanceable. `docs/reference/tui/status-bar.md`
  documents it.
- **Activity bar reference page.** The footer's transient breathing-dot
  liveness row now has its own `docs/reference/tui/activity-bar.md` page
  (formerly documented under `status-bar.md`, whose name is now used by the
  new session-status bar).

- **Resume is a hard error on a missing target, with an exit hint.**
  `neenee resume <id>` for a session id that does not exist now fails loudly
  instead of warning and silently starting a fresh session, so the operator
  learns the resume never happened. `neenee resume` with no id opens the
  sessions picker overlay (the same as before). When the TUI exits cleanly,
  it now prints `Session <id> ended. To continue it later, run: neenee resume
  <id>` — but only if the session actually gained content, so empty
  conversations don't advertise resuming.

### Changed

- **Tiered recursive-`rm` policy.** The bash policy's single `rm -rf /` deny
  rule and blanket `recursive force remove` confirm treated every recursive
  `rm` alike, so routine project cleanup (`rm -rf target/`) hit the same
  confirm as wiping a sibling repo. The built-in rules are now split into
  three tiers: **deny** (recursive `rm` of `/`, the home directory, or a
  system directory like `/etc`/`/usr`), **confirm** (recursive `rm` of any
  other absolute path or a parent-traversal target like `../sibling` — i.e.
  anything leaving the cwd), and **allow** (recursive `rm` of a relative path
  inside the cwd, plus `/tmp`). A built-in allow never overrides a built-in
  deny. The matchers require a real path token after the flags, so a quoted
  `"rm -rf"` substring inside another command (e.g. an `rg` pattern) no longer
  trips the rule. The permission chain's deny contract was also simplified:
  the `PolicyDecision::Deny` `collective` flag (and the unused
  `PermissionContext::autopilot`) were dropped — collective-abort of a
  parked batch is owned by the permission store's `reply`, keyed on a
  `PermissionDecision::Reject`.

- **Hint bar split into input-focused hint bar + session status bar.** The
  bottom hint row no longer carries the `autopilot` flag alongside the model
  and context meter. It is now purely input-focused — next-`Enter` action on
  the left, model + reasoning + context on the right — and a new
  **status bar** sits one row below it carrying session-level state (workspace
  path on the left, `autopilot` on the right). The layout constant that
  drives the transient activity bar was renamed `STATUS_BAR_ROWS` →
  `ACTIVITY_BAR_ROWS` to free up the `STATUS_BAR_ROWS` name for the new bar.
- **Queue bar gets a todos-style identity.** The persistent outbox bar now
  leads with a `📤` tray glyph + a brand-coloured uppercase `QUEUE` label on a
  raised surface, paralleling the todo bar's `📌 TODOS` treatment so it reads
  as a distinct pinned panel rather than a plain footer strip. Row 1 is now
  `📤 QUEUE N · HH:MM` (count, next-item send time) with a `F3 block · F2
  expand` legend; row 2 is the one-line next-item preview. The previous flat
  `queue · N` tag and the right-pinned `insert`/`next` send-target badge are
  gone.
- **Queue block/resume (`F3`) and modal outbox management.** The queue bar's
  `Esc recall` affordance never worked as advertised — `Esc` outside the modal
  didn't recall — so it was replaced with a real block/resume model. `F3`
  hard-blocks the viewed session's outbox: while blocked, **no** queued message
  auto-drains, not even after the round completes and the harness goes idle.
  The count turns error-coloured and gains a `blocked` tag; the legend flips to
  `F3 resume`. Opening the Queue modal (`F2`) auto-blocks the outbox for safe
  editing; closing it (Esc / outside-click) resumes normal auto-drain. Inside
  the modal, `↑`/`↓` select, `Enter` re-edits the **selected** item (not always
  the newest), `D` deletes it, and `J`/`K` reorder it toward the front or tail.

### Removed

- **Tab send-target toggle.** The `Tab` key no longer flips the next busy
  `Enter` between injecting mid-round (`Insert`) and waiting for a fresh round
  (`NextRound`). A busy `Enter` now always stages the message to wait for the
  running round to finish naturally — the mid-round insert path was rarely used
  and made the queue harder to reason about. The `Tab` keycap dropped out of
  the queue legend accordingly (Tab still accepts completions and cycles
  history/modal fields elsewhere). The core `InsertUserInput` capability
  remains for other frontends; it is just no longer reachable from the TUI.
- **`/repeat` moved onto the session and the `rusqlite` dependency dropped.**
  The `/repeat` cron-job schedule moved out of a standalone SQLite-backed
  `RepeatStore` and onto the session itself, persisted through its event log
  (`SessionEvent::RepeatJobsSet`, session-schema v8) alongside todos and the
  provider pin. A job is now owned by the session that created it, so
  resume/fork carries the schedule; the background scheduler polls the live
  `SessionStore` instead of a separate database. This removes the last SQL
  surface from the workspace: `neenee-persistence::repeat` (`RepeatStore`)
  and `db` (migration helpers) are deleted, `rusqlite` is dropped from the
  workspace and `neenee-persistence` manifests, and bootstrap no longer opens
  `repeat.db`. No data migration — `/repeat` jobs are rebuildable scheduler
  state.
- **`Esc recall` queue-bar legend.** The bar advertised `Esc` as a recall
  shortcut, but `Esc` outside the modal did not recall (it only worked inside
  the open Queue modal, and via `↑` in an empty composer). The misleading
  legend was removed; recall is now done by selecting an item in the Queue
  modal and pressing `Enter`, or by `↑` in the empty composer (unchanged).
  The modal's `Esc`/`Enter` always-recall-newest behavior became `Enter`
  recalls-the-selected.

## [0.21.0] - 2026-07-27

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
  is preserved (no LLM judge). See ADR-0069 and its accounting refinement,
  ADR-0083 — both superseded before a file was written, and folded into
  [ADR-0082](docs/adr/0082-remove-pursuit-stop-gate.md).

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
  completion and re-arm clear stale reasons. See ADR-0083, superseded
  before a file was written and folded into
  [ADR-0082](docs/adr/0082-remove-pursuit-stop-gate.md).

- **Pursuit contained behind the stop-gate; pursuit module slimmed to its
  domain values (ADR-0082).** Pursuit now has a written containment
  invariant: it may interact with the round loop only through the
  `stop_gate` composition point (the gate chain shared with `Stop` hooks),
  and any new touchpoint outside that chain needs its own ADR. The
  `neenee_core::pursuits` junk drawer was emptied — `TokenUsage` moved to
  `neenee_core::usage` (the `neenee_core::TokenUsage` re-export is
  unchanged), `RoundOutcome` moved into `neenee-agent` — leaving only
  `Pursuit` and `PursuitBudget`. No user-visible behavior change. See
  [ADR-0082](docs/adr/0082-remove-pursuit-stop-gate.md).

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
  rather than between the activity bar and the input, so `autopilot` reads as
  an attribute of the composer area. The flag is rendered lowercase
  (`autopilot`, warning tone + bold). The row still costs zero vertical space
  while no indicator is active and remains the designated home for future
  ambient state (workspace, etc.). See the
  [status bar reference](docs/reference/tui/status-bar.md).

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

- **The `/pursue` command and the pursuit stop-gate primitive (ADR-0082).**
  `/pursue <condition>` and all its subcommands (`status`, `stop`, `done`,
  `clear`, `edit`, `budget`) are gone, along with the forced-continuation
  stop-gate that drove a single turn until the model signaled completion, hit
  a 50-pass cap, tripped a budget, or was interrupted. The default round model
  is now the simplest one: **a round ends when the model stops calling tools,
  and that is treated as completion.** Forced continuation is the model's
  responsibility to need, not the client's to perform. For running on autopilot
  on a schedule, use `/repeat`. The marker (`[NEENEE_PURSUIT_COMPLETE]`), the
  durable `Pursuit`/`PursuitBudget`/checkpoint types, the `SessionData` fields,
  and the `pursuit`/`pursuit_runtime`/`loop_checkpoint` session events are all
  removed. See
  [ADR-0082](docs/adr/0082-remove-pursuit-stop-gate.md).

- **Dead pursuit types and the expired legacy pursuit migrations
  (ADR-0082).** The unused `RoundTimer` and `ThreadPursuit` types are
  deleted, and the one-shot migrations that folded a pre-ADR-0032
  `pursuits.db` or pre-ADR-0010 `harness_goal*` config keys into
  `SessionData.pursuit` are gone — the migration window (~1 month, 10
  releases) has closed. The old file and config keys are left on disk but
  never read; upgrading across the window means re-setting the objective
  with `/pursue`. See
  [ADR-0082](docs/adr/0082-remove-pursuit-stop-gate.md).

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
  terminal-bell notification so a long-running task that goes on autopilot still
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
  `[principal] allow_model_stdin` (default `false`) lets an autopilot flow let
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

- **`auto_approve` renamed to `autopilot`.** The no-prompt permission flag is renamed
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

[Unreleased]: https://github.com/ming2k/muta/compare/v0.36.1...HEAD
[0.36.1]: https://github.com/ming2k/muta/compare/v0.36.0...v0.36.1
[0.36.0]: https://github.com/ming2k/muta/compare/v0.35.7...v0.36.0
[0.35.7]: https://github.com/ming2k/muta/compare/v0.35.6...v0.35.7
[0.35.6]: https://github.com/ming2k/muta/compare/v0.35.5...v0.35.6
[0.35.5]: https://github.com/ming2k/muta/compare/v0.35.4...v0.35.5
[0.35.4]: https://github.com/ming2k/muta/compare/v0.35.3...v0.35.4
[0.35.3]: https://github.com/ming2k/muta/compare/v0.35.2...v0.35.3
[0.35.2]: https://github.com/ming2k/muta/compare/v0.35.1...v0.35.2
[0.35.1]: https://github.com/ming2k/muta/compare/v0.35.0...v0.35.1
[0.35.0]: https://github.com/ming2k/muta/compare/v0.34.5...v0.35.0
[0.34.5]: https://github.com/ming2k/muta/compare/v0.34.4...v0.34.5
[0.34.4]: https://github.com/ming2k/muta/compare/v0.34.3...v0.34.4
[0.34.3]: https://github.com/ming2k/muta/compare/v0.34.2...v0.34.3
[0.34.2]: https://github.com/ming2k/muta/compare/v0.34.1...v0.34.2
[0.34.1]: https://github.com/ming2k/muta/compare/v0.34.0...v0.34.1
[0.34.0]: https://github.com/ming2k/muta/compare/v0.33.1...v0.34.0
[0.33.1]: https://github.com/ming2k/muta/compare/v0.33.0...v0.33.1
[0.33.0]: https://github.com/ming2k/muta/compare/v0.32.2...v0.33.0
[0.32.2]: https://github.com/ming2k/muta/compare/v0.32.1...v0.32.2
[0.32.1]: https://github.com/ming2k/muta/compare/v0.32.0...v0.32.1
[0.32.0]: https://github.com/ming2k/muta/releases/tag/v0.32.0
[0.31.0]: https://github.com/ming2k/neenee/releases/tag/v0.31.0
[0.30.5]: https://github.com/ming2k/neenee/releases/tag/v0.30.5
[0.30.4]: https://github.com/ming2k/neenee/releases/tag/v0.30.4
[0.30.3]: https://github.com/ming2k/neenee/releases/tag/v0.30.3
[0.30.2]: https://github.com/ming2k/neenee/releases/tag/v0.30.2
[0.30.1]: https://github.com/ming2k/neenee/releases/tag/v0.30.1
[0.30.0]: https://github.com/ming2k/neenee/releases/tag/v0.30.0
[0.29.1]: https://github.com/ming2k/neenee/releases/tag/v0.29.1
[0.29.0]: https://github.com/ming2k/neenee/releases/tag/v0.29.0
[0.28.0]: https://github.com/ming2k/neenee/releases/tag/v0.28.0
[0.27.0]: https://github.com/ming2k/neenee/releases/tag/v0.27.0
[0.26.1]: https://github.com/ming2k/neenee/releases/tag/v0.26.1
[0.26.0]: https://github.com/ming2k/neenee/releases/tag/v0.26.0
[0.25.2]: https://github.com/ming2k/neenee/releases/tag/v0.25.2
[0.25.1]: https://github.com/ming2k/neenee/releases/tag/v0.25.1
[0.25.0]: https://github.com/ming2k/neenee/releases/tag/v0.25.0
[0.24.0]: https://github.com/ming2k/neenee/releases/tag/v0.24.0
[0.23.0]: https://github.com/ming2k/neenee/releases/tag/v0.23.0
[0.22.6]: https://github.com/ming2k/neenee/releases/tag/v0.22.6
[0.22.5]: https://github.com/ming2k/neenee/releases/tag/v0.22.5
[0.22.4]: https://github.com/ming2k/neenee/releases/tag/v0.22.4
[0.22.3]: https://github.com/ming2k/neenee/releases/tag/v0.22.3
[0.22.2]: https://github.com/ming2k/neenee/releases/tag/v0.22.2
[0.22.1]: https://github.com/ming2k/neenee/releases/tag/v0.22.1
[0.22.0]: https://github.com/ming2k/neenee/releases/tag/v0.22.0
[0.21.3]: https://github.com/ming2k/neenee/releases/tag/v0.21.3
[0.21.2]: https://github.com/ming2k/neenee/releases/tag/v0.21.2
[0.21.1]: https://github.com/ming2k/neenee/releases/tag/v0.21.1
[0.21.0]: https://github.com/ming2k/neenee/releases/tag/v0.21.0
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
