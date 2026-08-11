# 0097. Session addressing and the orchestrator console

- **Status:** Proposed
- **Date:** 2026-08-12
- **Builds on:** ADR-0093 (monitor protocol), ADR-0094 (serve vocabulary),
  ADR-0096 (unified session daemon and control plane)

## Context

The session dashboard (full-screen successor of the `/host` modal) is today a
*passive observability* surface with five one-shot control verbs
(`ControlRequest`: `CreateSession`, `SendPrompt`, `Interrupt`,
`ResolvePermission`, `KillSession` —
`crates/neenee-transport/src/serve.rs:69-90`). Driving several sessions from
it exposes three structural gaps:

1. **No handle to talk about.** Sessions are full UUIDs truncated to 8 chars
   for display (`dashboard.rs:283`, `status.rs:181`); selection is positional
   over a newest-first list, so the row you mean shifts as `updated_at`
   changes. A prefix cannot even be attached to:
   `SessionRegistry::resolve_id` is exact-match only
   (`crates/neenee-transport/src/registry.rs:286`). Any textual "send to
   session X" grammar — human or model-authored — needs a stable short
   address.
2. **No conversation.** The monitor stream deliberately carries no content
   (`crates/neenee-core/src/monitor.rs:8-10`); the only way to read a
   transcript is a full re-attach (`Wire::Welcome`). The inline prompt (the
   `p`/`n` footer line) is single-shot fire-and-forget: no reply, no
   follow-up, no iteration.
3. **Nobody home.** The user orchestrates by hand. An AI that orchestrates
   needs a place to live (it cannot be a child envoy of some chat session —
   its lifecycle is the daemon's), a toolset aimed at sessions rather than
   code, and its own model pin, since the orchestration workload (routing,
   decomposition, cross-session synthesis) is not the coding workload.

## Decision

### 1. Daemon-assigned short session numbers

Every session receives a monotonically increasing, daemon-scoped short
number at creation (in `SessionRegistry::create_session`), displayed
everywhere as `#N` and persisted in the registry's state file so numbers
survive daemon restarts and never repeat.

- `resolve_id` gains a `#N`/bare-number resolution path in addition to exact
  UUID match. Display truncation to 8 chars remains for UUID-only contexts,
  but `#N` is the canonical human handle.
- Numbers are scoped to the daemon (there is exactly one user-level daemon,
  ADR-0096), so `#3` is unambiguous for every client of that daemon. They are
  *not* per-project: the dashboard is the all-projects surface.
- `MonitoredSession` gains the field; `neenee status` and the dashboard list
  render `#N` in place of the 8-char truncation as the leading column.

### 2. Address grammar: `@<n> <text>`

The dashboard's input adopts one explicit address token, game-chat style:

- `@3 refactor the retry loop` — send `refactor the retry loop` to session
  `#3` (`ControlRequest::SendPrompt`).
- `@2 @3 summarize your findings` — fan out the same prompt to several
  sessions.
- Bare text with no `@` token goes to the **orchestrator** (§3).
- The address applies to the whole line; there is no per-sentence mid-line
  retargeting in v1 (keeps the grammar regular and the parser trivial — one
  leading run of `@n` tokens, then the payload).

### 3. The dashboard console: orchestrator transcript + addressing input

The dashboard becomes an orchestration *console* with a two-zone layout
(the first half — the dock — is implemented):

- **Console** (upper, flexible region): the AI-interaction surface — the
  orchestrator conversation (user directives, orchestrator replies, action
  receipts) plus system receipt lines for direct `@n` dispatches
  (`→ #3 queued`). It is the user's cockpit log, not any session's
  transcript — per-session content stays one attach away. Until the
  orchestrator lands, this region hosts a placeholder plus the selected
  session's live monitor read-out.
- **Sessions dock** (bottom strip): every session as a compact card —
  sequence number, workspace name, uptime since the session opened, and
  lifecycle status (`running` vs `done`, blocked states kept distinct).
  Cards tile into as many columns as the width affords (one per ~36 cells,
  capped at four) and are ordered by sequence number — creation order, so
  positions are stable while statuses flip around them. The workspace name
  is the directory basename, falling back to the full path only when two
  sessions share a basename. `MonitoredSession` carries `project_root` for
  this (registry projects it down from the two-level index; mirrors and
  prehosts leave it empty).

- The composer (next step) is a real input (the existing composer
  component, not the borrowed single-line `app.input` footer hack), with
  history and multi-line editing. Enter submits; `@n` routing is applied
  at submit.

Interaction model (implemented):

- **Focus defaults to the console/input region.** The dashboard opens with
  the keyboard on the upper region (`DashboardFocus::Detail`), not the
  dock, so the future orchestrator composer owns typing without a Tab
  first; `Tab` drops to the dock for selection.
- **Selecting a session is inert; Enter previews.** Moving the dock
  highlight only moves the highlight. Enter on a selection opens a
  centered, read-only **session preview modal** (the full monitor
  read-out, scrollable; Esc closes back to the dashboard). Attach moved to
  `a` — Enter no longer yanks the user out of the dashboard just for
  peeking. The preview's content today is the monitor read-out; a real
  transcript awaits the read-only `FetchTranscript` verb (§ below, its own
  ADR).
- **No session transcript pane in v1.** Reading a session's conversation
  requires a new read-only control-plane verb (`FetchTranscript`); that is
  deliberately its own ADR. The console transcript answers "what did I ask
  the fleet to do", not "what is session 3 doing".

### 4. The orchestrator is a daemon-level agent

The orchestrator runs as a first-class agent owned by the daemon — one per
daemon, attached to the dashboard console rather than to any chat session.
It is constructed directly (`Agent::new` with a restricted `ToolSet`, the
pattern `run_envoy_outcome` uses at
`crates/neenee-agent/src/envoy_tool.rs:470-546`), not spawned through an
`EnvoyTool`: it is nobody's child.

Its toolset is orchestration-scoped, following the `EnvoyProfile` allowlist
precedent (`QUANT` proves a non-coding allowlist works,
`crates/neenee-core/src/envoy.rs:394-440`). The initial set maps onto the
existing control plane — no new daemon capability is required:

- `list_sessions` — monitor snapshot (status, round, tokens, note).
- `send_prompt { session, text }` — address by `#N` or UUID.
- `interrupt_session { session }`.
- `create_session { project, prompt? }`.
- `resolve_permission { session, request_id, decision }` — gated: only
  forwarded when the orchestrator profile's operation scope allows, and
  always logged to the console transcript as an action receipt.
- `get_session_overview { session }` — returns the §7 summary blob when
  present (and, in v1, `MonitoredSession.overview`), explicitly *not* the
  full transcript.
- Plus the coordination answers of §5 (responding to `check_wip`, and
  volunteering overlap nudges) — these are the orchestrator reading its
  coordination state, not tools it exposes to sessions.

Every tool call is recorded in the console transcript as a receipt line, so
the human always sees what the orchestrator did to which session.

### 5. WIP coordination: `check_wip` and `declare_wip`

Sessions working in the same workspace can collide: one is mid-refactor
(WIP, tree doesn't build) while another, unaware, runs the full suite or
launches the app and concludes the project is broken. The orchestrator is
the natural coordinator — it already sees every session's status and
workspace — so coordination is a *tool-mediated query/declaration between a
session agent and the orchestrator*, not new session-to-session protocol.

**`check_wip`** (read-only, on the session's toolset):
- **Args**: `{ paths?: string[], concern?: string }` — the paths the
  session is about to touch, and/or what it's about to do (run tests,
  build, launch).
- **Mechanism**: routes through the session↔orchestrator channel to the
  orchestrator, which inspects the other sessions in the *same workspace*
  (the registry already indexes `project_root → sessions`, the
  `resolve_auto` filter at `registry.rs:257`) and their declared-WIP set.
- **Returns**: a verdict the session acts on:
  `{ wip_conflicts: [{ session, paths, summary, overlap }], advice }`,
  where `advice ∈ { "proceed", "proceed_scoped", "defer" }`. On
  `proceed_scoped` the session narrows to non-overlapping paths and skips
  whole-tree verification (no full `cargo test` / no direct run while a
  conflicting WIP exists); on `defer` it waits or asks the human.
- **Advisory, not a lock**: it is a consult, not a mutex. A session may
  proceed against the advice at the human's word, but the default steer is
  "focus on your own scope; don't do global verification under a conflict."

**`declare_wip`** (on the session's toolset): a session *registers* its own
active WIP so others' `check_wip` can see it —
`{ paths: string[], summary: string }`, auto-scoped to the calling session
and its workspace. Registrations live in the orchestrator's in-memory
coordination state (v1: volatile; persistence is a follow-up) and clear on
session end, explicit `wip_done`, or when the session goes `Idle` with its
round naturally complete.

**Orchestrator-side**: the orchestrator answers `check_wip` from facts
(monitor rows + the declared-WIP registry), and may volunteer a nudge when
it *detects* overlap (two sessions editing the same file) without being
asked — the "向上层…进行询问" flow is the pull half, this is the push half.
Its answers are injected back into the asking session as a tool result,
keeping the session's transcript self-contained.

**Trust boundary & failure semantics**:
- The orchestrator *advises*, the session (and its human) *decides*. No
  WIP state can force-block another session's tools.
- Orchestrator unavailable / no orchestrator configured → `check_wip`
  returns a clean "no coordination data" verdict and the session proceeds
  exactly as today (absence never breaks a session).
- `check_wip` is read-only and idempotent; `declare_wip` only mutates the
  coordination registry, never files or other sessions.

**OperationScope note**: `declare_wip`/`wip_done` are not file/command
operations, so they sit outside the `write_paths`/`command_allowlist`
axes (`capability.rs:592`). The orchestrator profile's tool allowlist is
the admission axis for the orchestrator side; on the session side these
two tools are ordinary builtins gated by the session's own model-visible
toolset, exactly like `todo`/`websearch`.

### 6. Role-scoped model selection: `[orchestrator]` and `[summarizer]`

Config gains two role tables shaped exactly like `ProviderSelection`
(`{ provider: String, model: Option<String> }`,
`crates/neenee-persistence/src/session/mod.rs:45`), living in the global
`config.toml`:

```toml
[orchestrator]
provider = "anthropic"
model = "claude-opus-…"

[summarizer]
provider = "openai"
model = "gpt-…-mini"
```

- Each resolves through the existing factory
  (`catalog::build_provider_for_model`,
  `crates/neenee-agent/src/catalog.rs:1133`) into a dedicated
  `Arc<dyn Provider>` held by the daemon for the orchestrator, and used
  per-invocation for the summarizer. `session_id` is `None` for these
  clients (they are not session-keyed; prompt-cache keying follows
  ADR-0067).
- The C6 session overlay (`bootstrap.rs:325-330`) touches only
  `default_provider`/`default_model`; role pins are unaffected by any
  session's `/models` switch — this is the point: roles are independent of
  session-level selection.
- Unset (the default) means the orchestrator console still works for `@n`
  dispatch but has no AI, and the summarizer is simply off. No silent
  fallback to some session's model — a role model is configured, not
  inferred.

This revises the ADR-0022-era axiom recorded in
`crates/neenee-agent/src/session_title.rs:56-59` ("neenee's catalog has no
'small model' concept, so a dedicated cheap channel is out of scope") for
the two daemon-level roles only. Session-internal calls (compaction, title)
keep using the session's own provider; making *them* role-routable is a
separate, later decision.

### 7. The summarizer: post-round session summaries with a direction tree

A daemon-side summarizer regenerates a session's structured summary when
the session settles to `Idle` after a completed round — the exact seam
where `SessionMonitorTracker` flips `RoundCompleted → Idle`
(`crates/neenee-transport/src/monitor.rs:147-152`) — debounced so an active
back-and-forth session is not re-summarized every round (a settle delay on
the order of a minute, configurable).

- **Content** (a single structured blob): task summary, implementation
  overview, expected effect, and a **direction tree** — a small number of
  candidate next-step branches, each one or two predicted steps deep,
  rendered in the dashboard detail pane. For interrupted/failed sessions
  the same shape carries "where it stopped and why" plus resume directions.
- **Input**: the session's `CompactionCheckpoint` summary when one exists
  (it already distills objective/progress/next-steps), plus the tail of the
  transcript since it; full raw transcript only when no checkpoint exists.
  This bounds summarizer cost per round.
- **Storage**: a new `#[serde(default)]` field on `SessionData` (schema
  bump, the `title` precedent at
  `crates/neenee-persistence/src/session/mod.rs:29-36,117-120`), surfaced
  through `MonitoredSession` so the dashboard renders it live in the detail
  pane — replacing the current static `overview` preview.
- **Model**: the `[summarizer]` role client (§5). Absent the pin, the pane
  keeps today's `overview` text and no LLM call is ever made.

## Alternatives considered

- **Orchestrator as an `ORCHESTRATOR` envoy profile spawned from a chat
  session.** Rejected: the orchestrator's lifecycle is the daemon's, not a
  round's; a child envoy dies with its parent's round, forces the user to
  keep a "master session" alive, and its transcript would be buried inside
  that session. The profile *machinery* (allowlist + system prompt + scope)
  is reused; the *dispatch* machinery is not.
- **8-char UUID prefixes as the address grammar.** Rejected as the primary
  handle: 8 hex chars are not speakable, not memorable, and collide more
  readily as session counts grow; prefix resolution is still added under
  `resolve_id` for convenience, but `#N` is the canonical grammar token.
- **A transcript pane per session inside the dashboard.** Deferred, not
  rejected: it needs a read-only `FetchTranscript` verb and real bandwidth
  discipline on the wire; the orchestrator console plus summaries cover the
  80% case (knowing what to do next) without it.
- **Summarizer reuses the session's own provider** (like compaction).
  Rejected: it would run N different models across N sessions with
  unpredictable cost, and it couples a fleet-wide read-model to whatever a
  given session happens to be pinned to. A dedicated cheap role model is
  the entire point.
- **Summarize only on `SessionEnd` / attach-detach.** Rejected: SessionEnd
  fires on host shutdown, and detach is not completion — users want the
  summary while the session is merely idle, ready to be re-entered.
- **WIP conflicts as a hard lock / reservation system.** Rejected: a mutex
  across sessions serializes work the user explicitly parallelized, and a
  stale lock bricks a workspace when a session dies mid-WIP. Advisory
  verdicts with a human override give the safety benefit (sessions stop
  tripping over each other's broken trees) without the liveness hazard.
- **Sessions detect conflicts themselves** (read each other's
  git/dirty-tree state). Rejected: sessions have no business inspecting
  each other's files, the monitor stream already centralizes the facts, and
  the *judgement* (is this overlap a real conflict for what you're about to
  do?) is exactly the orchestrator model's job — centralizing it avoids
  baking coordination heuristics into every session.

## Consequences

Positive:

- `#N` gives every client (status, dashboard, orchestrator tools, future
  web UI) one stable, speakable session handle; the grammar `@3 …` is
  trivially parseable by humans and models alike.
- The control plane needs no new write verbs for the orchestrator — its
  tools are thin adapters over `SessionRegistry`, so the trust boundary
  stays exactly where ADR-0096 put it.
- Role pins are additive config with safe defaults (unset = feature off);
  no existing behavior changes when the tables are absent.
- The summarizer lands on an already-observed lifecycle seam
  (`RoundCompleted → Idle`) and reuses the compaction checkpoint as its
  primary input, so incremental cost is one bounded LLM call per settled
  round.
- WIP coordination makes same-workspace parallelism *safer to default to*:
  sessions stop running global verification against each other's broken
  trees, which is the precondition for the fleet model (many sessions, one
  user) to work without constant human traffic control.

Negative / costs:

- Registry state becomes versioned persisted data (number counter +
  id→number map); a corrupted or reset registry file renumbers nothing but
  must not fail session resolution — exact-UUID resolution remains the
  fallback everywhere.
- A daemon-resident agent is a new always-on component: it needs its own
  failure isolation (an orchestrator error must surface as a console
  receipt, never as a session failure) and its own token accounting bucket
  (it must not pollute any session's round totals — cf. the principal-only
  rule in the token report).
- The WIP registry is in-memory in v1, so a daemon restart silently drops
  every declared WIP — sessions simply stop seeing conflicts until their
  peers re-declare. Acceptable for an advisory mechanism (never a
  correctness hazard by design), but it argues against ever letting the
  verdict become a hard gate.
- Two more model clients to construct, hold, and hot-swap on config
  reload.

Migration / sequencing (each step independently shippable):

1. ~~Layout~~ **done**: console/dock two-zone split, sequence-numbered dock
   cards, `project_root` on `MonitoredSession`; console-default focus,
   Enter-to-preview modal, `a`-to-attach.
2. Daemon-assigned `#N` numbering (today the numbers are client-side
   creation-order; daemon assignment makes them addressable through
   `resolve_id`) + `resolve_id` prefix/number path.
3. Dashboard composer + `@n` routing on top of `SendPrompt` (no AI yet —
   this is already the full "game chat" UX).
4. `[orchestrator]` role config + daemon-level orchestrator agent with the
   five control-plane tools, **plus the WIP coordination channel** (§5):
   session-side `check_wip`/`declare_wip` tools and the orchestrator's
   coordination registry. This is the step that makes the fleet *cohesive*,
   not just addressable.
5. `[summarizer]` role config + post-round summary pipeline + console-pane
   rendering.
6. (Separate ADR) read-only `FetchTranscript` and a transcript pane.

## References

- ADR-0022 (session-level AI title — the "no small model concept" axiom
  revised here), ADR-0089/0093/0094/0096 (daemon line), ADR-0027/0033
  (profile-as-agent precedent).
- `crates/neenee-transport/src/serve.rs:47-90` (control-plane vocabulary),
  `crates/neenee-transport/src/registry.rs` (session registry),
  `crates/neenee-cli/src/tui/overlays/dashboard.rs` (dashboard WIP),
  `crates/neenee-core/src/envoy.rs` (profile/tool-allowlist machinery),
  `crates/neenee-persistence/src/session/mod.rs` (SessionData schema).
