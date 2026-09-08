# 0197. One frontend truth: the shell is a client of the engine and of the protocol

- **Status:** Accepted (implemented 2026-09-08; M1–M6 landed)
- **Date:** 2026-09-08

> **Implementation addendum (2026-09-08, completed).**
>
> - **M1 landed** (`Translator seam`): the response listener and the monitor
>   client are pure translators producing typed [`AppMutation`] values
>   (`event_loop/mutations.rs`, ~45 semantic variants) onto a bounded
>   channel; `event_loop/apply.rs` is the **sole `App` writer**. The ~45
>   shared mirror cells, the per-frame `sync.rs` hydration, both
>   `Versioned` transcript buffers, the whole `TranscriptPatch` replay
>   machinery, and both rev counters are **deleted** (~2,300 net lines out
>   of the shell). The loop ↔ translator coordination facts that genuinely
>   flow the other way (viewed session, stream-coalescing hint, OAuth
>   add-flow mirror, trust-gate latch) are documented on `UiRuntime` — six
>   fields, zero of them rendered state. The transcript document lives on
>   `App` and the applier executes semantic edits (settle/hold/lazy-create/
>   finalize/discard) with the same identity-addressed helpers, keeping the
>   height-invalidation discipline honest (targeted evictions instead of
>   blanket guard-based invalidations).
> - **A dead-mirror defect found and fixed in passing**: the
>   `SessionTreeSnapshot` response was written into a shared cell that no
>   sync ever read — the `/tree` panel rendered `SessionTree::default()`
>   forever. The applier now actually lands the tree.
> - **M5 landed** (`Dead-link states`): all 52 swallowed
>   `let _ = app.tx.send(...)` sites in the event loop now go through
>   `App::send_intent` (`app/link.rs`), which latches `App::link_down` and
>   logs an error; completion requests use the same path. Protocol sends
>   that occur before or outside the interactive `App` (initial prompt,
>   headless replies, session teardown, and the exit history flush) now
>   propagate or explicitly log delivery failure. The activity bar renders
>   "daemon link lost" as the visible chrome state (it stays until process
>   exit — with the driver gone, nothing can acknowledge recovery, and
>   claiming "reconnecting" would be a lie).
> - **M6 landed** (`One retained system`): `mutx-engine::render_tree`, both
>   `draw_tree` methods, and the engine integration tests were deleted;
>   ADR-0195 is marked partially superseded. The widget + scene pipeline is
>   the one retained model.
> - **M3 landed** (`Client facade`): the new `muta-client` crate re-exports
>   the full client surface (discovery, ensure/connect/control, monitor
>   stream, posture, remote daemon, tracing init, command catalog,
>   completion helpers, clipboard SPI) and carries the *edge-enforcement
>   test*: `mutx`'s Cargo.toml may name `muta-contracts` and `muta-client`
>   and never `muta-runtime`. `mutx` sources now import only the facade.
>   **Recorded edge debt** (same class, next tranches): `mutx` still depends
>   on `muta-persistence` (config.rs reads XDG paths and opens the
>   database directly for local config reads) and on `muta-providers`
>   (static provider-preset constants rendered by the provider table).
>   Both are read-only data/utility reaches, but they are reaches; the
>   clean end state moves the preset constants into `muta-contracts` and
>   routes config reads through the protocol.
> - ADR-0196 (same-day) landed the backend half of D6's context: durability
>   health is on the wire (`MonitorEvent::PersistenceHealth`), `muta status`
>   and the TUI render degradation.
> - **M4 landed** (`Harness dispatch`): the follow-up queue authority moved
>   into the session driver. The frontend sends one verb
>   (`AgentRequest::FollowUp`); the daemon starts an idle target
>   immediately (`FollowUpStarted`) or enqueues it (`FollowUpQueued`) and
>   ships automatically at the round boundary, driven by a completion wake
>   added to `RoundLifecycle` (`finished()` + `was_interrupted()`). Every
>   queue change emits an authoritative `QueueUpdated` snapshot; the queue
>   modal's delete/recall/reorder/pause verbs mutate the daemon queue and
>   mirror locally. The frontend auto-dispatcher
>   (`auto_dispatch_ready_round` + the `idle/naturally_completed` sets) is
>   **deleted** — the double-ship hazard is gone by construction.
>   Deliberate semantic: a round the operator interrupted parks the queue
>   (`was_interrupted` → paused) — Esc means stop, and auto-firing more
>   prompts after it is never the intent; `Ctrl+P` or a fresh send re-arms.
>   Covered by `FollowUpQueue` unit tests (FIFO/target-scoping/reorder
>   clamping/pause gating) and the lifecycle boundary test.
> - **Edge debt cleared (recorded tranches landed 2026-09-08).** The
>   recorded dependency reaches of `mutx` are gone, and the facade's
>   boundary test now bans them by name:
>   `muta-providers` — the preset model lists moved to
>   `muta_contracts::provider_presets` and the OAuth client profiles to
>   `muta_contracts::provider_auth` (pure serde data both sides need);
>   `muta-providers` keeps the OAuth *engine* and re-exports the profile
>   data. `muta-persistence` — the application filesystem infrastructure
>   (XDG `paths`, `fsutil`, `lock`) moved to the new `muta-paths` crate
>   (muta-persistence re-exports for the daemon-side crates), and the two
>   direct database reaches were replaced by protocol services: the daemon
>   is the source of truth for prompt input history
>   (`QueryInputHistory` / `RecordInputHistory` / `DeleteInputHistoryEntry`)
>   and route capability overrides (`QueryRouteSettings` →
>   `AgentResponse::RouteSettings`). Semantic change, deliberate: history
>   hydration is now asynchronous (an `InputHistory` mutation lands one
>   round-trip after startup or a picker open instead of a synchronous
>   local read), and the model editor's override prefill arrives the same
>   way, guarded against the operator having moved on. The exit-time
>   history flush is a fire-and-forget `RecordInputHistory` intent — each
>   prompt was already recorded during the session, so a lost flush at most
>   drops the last dedup pass. `mutx` now depends only on `muta-contracts`,
>   `muta-client`, and `muta-paths`.
> - **M2 landed** (`Scene-owned modality`): keyboard families are
>   load-bearing from mount through dispatch. The engine's
>   `Component::scope(claims)`, `foreground_for`, and
>   `keyboard_path_for` contracts select one route for every event; the
>   transcript, composer, completion menu, and sheets declare their family
>   claims at mount time. The permission sheet claims `SHEET`, `COMPOSER`,
>   and `COMPLETION`, while leaving `TRANSCRIPT` unclaimed, so transcript
>   navigation resolves to the component below it. Exclusive sheets and
>   modals terminate every route. The event loop classifies each terminal
>   event, resolves the committed scene's keyboard path, and offers it to
>   the component handler on that path. The config dropdown,
>   provider-delete confirmation, and composer selection relay no longer
>   form a pre-dispatch side channel. The former `process_event` API is
>   deleted; `input::router` retains terminal normalization, global chords,
>   the Esc precedence ladder, and shared readline affordances, while modal,
>   sheet, view, and component handlers own their surface verbs through
>   small sub-state structures. Contract tests cover family fallthrough and
>   exclusive barriers; the focused input regression suite covers the
>   preserved key behavior.

## Context

The engine (`mutx-engine`) is clean: a retained cell grid, a diff backend,
and a declarative scene (`Scene<K>`, `UiRuntime<K>`) that carries a complete
input/pointer policy model — `InputPolicy::Bubble/Scope/Modal`,
`PointerPolicy::Barrier`, `keyboard_path`, focus and capture
(`apps/tui/crates/mutx-engine/src/ui/scene.rs`), explicitly free of
application vocabulary (ADR-0038). The shell (`mutx`), however, does not
*use* most of it. An audit found seven structural leaks, none annotated,
all producing workarounds that now define the codebase's shape:

1. **Modality is re-implemented beside the policy model.** The shell passes
   `app.active_modal()` *into* `ui.begin` (`mutx/src/render.rs:30`) and maps
   `scene().foreground()` back into its own `Modal` enum
   (`event_loop/mod.rs:453-464`) — modality round-trips through two
   vocabularies. The engine's family-scoped `keyboard_path` /
   `foreground_for` are referenced only by engine tests. Keyboard routing is
   duplicated in a ~25-field `InputContext` god-struct rebuilt per keypress
   (`mutx/src/input/mod.rs:9`), plus three hard-coded pre-dispatch
   interceptors (`probe_config_dropdown` / `probe_delete_overlay` /
   `probe_input_selection_relay`, `event_loop/mod.rs:528-534`) living
   outside any policy model.
2. **Two sources of truth, reconciled per frame.** The response-listener
   task mutates ~45 shared cells in `UiRuntime`
   (`event_loop/runtime.rs:83-142`); `sync.rs:16-295` mirrors nearly all of
   them into `App` every frame, with rev-counter staleness plumbing
   (`sessions_overview_rev`, `host_sessions_rev`) patching the
   disagreement windows, and fields documented as "mirror of `App::…`"
   (`lib.rs:501-505`).
3. **Business logic lives in a ~2,200-line listener closure** (`lib.rs:633+`):
   steer/follow-up settlement (`lib.rs:739-812`), aside routing
   (676-704), inline `ChromeUpdate` bookkeeping (550-560), and *notice
   surface policy* — toast vs inline (864-889) — decided inline. The loop
   feeds two competing signal paths: direct buffer writes and the
   `OutboxSignal` queue (`runtime.rs:41-64`, drained in `sync.rs:477-533`).
4. **Dispatch orchestration is frontend policy.** `auto_dispatch_ready_round`
   (`event_loop/mod.rs:151-200`) decides when queued follow-ups ship and
   sends `AgentRequest::FollowUp` straight onto the wire; the dispatch-queue
   state machine (`pending_dispatch`, `idle_sessions`,
   `naturally_completed_sessions`, …) is a harness concern duplicated in
   `App`.
5. **The frontend reaches into daemon internals.** `mutx/src/lib.rs:582-630`
   spawns its own monitor stream and calls `muta_runtime::client::*`
   directly; `Cargo.toml` depends on `muta-runtime`, breaking the
   "TUI is a pure frontend over `muta-contracts`" boundary the view layer
   otherwise obeys.
6. **Send failures are swallowed.** 52 `let _ = …send(AgentRequest…)`
   sites across `event_loop/` (actions.rs:31, modals.rs:12, commands.rs:8,
   mod.rs:1); two check. A dead daemon link leaves the UI queueing optimistic
   state forever.
7. **The engine carries a second, unused retained system.** `render_tree`
   (ADR-0195) is exported and reachable via `Terminal::draw_tree`
   (`frame.rs:343-349`) but has zero non-test callers; the live app uses
   only the `ui/scene` + widget path. A promise without a consumer is a
   defect of the class ADR-0190 D7 names.

None of this is the engine's fault — the leakage is that the shell *bypasses
the mechanisms the engine already provides* and *reaches past the protocol
boundary*. Patches (another rev-counter, a fourth probe) compound the
disease. Break clean.

## Decision

The shell is rebuilt as a strict client: of the engine (it declares scenes
and obeys scene-derived routing) and of the protocol (it speaks
`muta-contracts` only). Seven sub-decisions:

### D1. One source of truth: `App`

The listener task stops owning state. It becomes a **translator**: consume
`AgentResponse` → produce typed `AppMutation` values (append notice, settle
steer, update chrome, set phase …) onto a bounded channel. The event loop
applies mutations to `App` between input batches and renders from `App`.

- `UiRuntime`'s ~45 mirror cells, `sync.rs`'s per-frame mirroring, and the
  rev counters are **deleted**.
- The engine's `UiRuntime<K>` (scene instances, per-node typed state,
  focus/capture) remains the only engine-owned state; the seam stays
  `render_frame(&App) -> Scene`.
- Concurrency invariant: `App` is touched only by the event loop task.
  Listener-side data the loop needs (transcript patches) travels as data in
  mutations, not as shared cells.

### D2. The scene owns modality

Keyboard and pointer routing flow exclusively through the engine's policy
model:

- Overlays/modals declare `InputPolicy::Scope/Modal` and layer at mount
  time; `ui.begin` no longer receives `active_modal`. Modality is read
  *from* the committed scene where the shell still needs it (one direction,
  scene → shell, for chrome decisions).
- The `InputContext` god-struct and the three `probe_*` interceptors are
  deleted. What remains is: scene-resolved keyboard target → the target
  component's declared policy → component handler. Completion menus,
  permission sheets, and info sub-views express their inert/exclusive
  behavior as policy on their components, where it can be hit-tested and
  tested like everything else.
- Engine gap fix (the reason the model went unused): `foreground_for` /
  family-scoped `keyboard_path` move from `u64::MAX`-stubbed to real family
  identifiers, with engine tests promoted from unit tests to the routing
  contract.

### D3. One signal path, policy in one place

The dual path (direct `Versioned` transcript writes + `OutboxSignal` queue)
collapses into the mutation stream of D1. Notice *surface policy* (inline
notice vs toast vs banner) becomes a declared property of notice kinds,
decided in one module (`view::notices`), not in the listener's match arms.

### D4. The protocol boundary is compiler-enforced

`mutx` depends only on `muta-contracts` (+ engine + shared utility crates).
Daemon discovery, lifecycle, and the monitor stream move behind a thin
`muta-client` facade (either a new crate or the one permitted
`muta-runtime::client` import point, chosen when the split is priced).
`cargo-deny`/ban rules enforce the edge so the boundary cannot rot again.

### D5. Dispatch policy belongs to the harness

Queued follow-up shipping moves into the session driver (the actor loop of
ADR-0190 — the driver already owns round-boundary queueing, ADR-0126). The
TUI's `auto_dispatch_ready_round`, the queue state machine, and the
`FollowUp` wire sends are deleted from `mutx`; the TUI sends user intent
(submit, enqueue, cancel) and renders the queue state the backend reports.

### D6. A dead link is a state, not noise

Every outbox send is checked. Connection loss enters an explicit
disconnected chrome state; queued user intents either deliver or surface.
Combined with ADR-0196's health signal, "the backend cannot hear me" and
"the backend cannot save" become first-class, visible states.

### D7. One retained system

`render_tree` is deleted from `mutx-engine` (the widget + scene pipeline is
the retained model); ADR-0195's render-tree half is recorded as superseded
by this decision, its lifecycle/event learnings having already been folded
into `ui/runtime.rs`. If a Flutter-style tree is ever needed, it returns as
a new decision with a consumer, not as resident dead code.

## Alternatives considered

- **Elm-style everywhere (App is also a reducer over input events):**
  attractive symmetry, but input needs per-component focus/capture geometry
  the scene already owns; making input flow through a global reducer would
  re-create `InputContext` under another name. Rejected; D2 is the Elm
  discipline where it actually applies — routing.
- **Keep the mirrors, add finer dirty-tracking:** patches the disagreement
  windows instead of removing the two truths; every future field re-pays.
  Rejected.
- **Keep `render_tree` for a future roadmap:** dead code that compiles,
  drifts, and must be read by every engine contributor — ADR-0190 D7 names
  this class a defect. Rejected.
- **Keep the probes as "fast paths":** they are correct today and cost
  nothing at runtime, but they are un-modelled modality; each new overlay
  adds another probe. Rejected.
- **Move dispatch to a frontend service crate instead of the harness:**
  keeps scheduling policy out of `App` but still duplicates the backend's
  queue authority across the process boundary — two machines guessing about
  the same queue. Rejected.

## Consequences

### Positive

- One state machine per layer: engine owns geometry/policy, `App` owns
  application truth, the harness owns dispatch. The mirror/sync/rev-counter
  apparatus (~300 lines of sync.rs plus cells) is deleted outright.
- Overlay/modality work stops being probe archaeology: a new overlay is a
  component with a policy, testable in engine terms.
- The frontend is portable to any transport speaking `muta-contracts`
  (WS today, in-process tomorrow) — verifiable by the dependency graph.
- Queue/dispatch behavior has one owner and one test surface; frontend
  follow-up bugs stop being heisenbugs across a process boundary.

### Negative / trade-offs

- This is a rewrite of the shell's spine (listener, sync, input dispatch,
  outbox), not a cleanup: large diff, and it must land while the product is
  live. The milestone order below keeps every step shippable.
- Mutation indirection adds one hop between wire event and UI update;
  bounded channel + per-iteration draining keeps latency at one frame.
- Some scene-policy refactoring lands in the engine (real input families),
  touching a codebase that is currently stable — accepted, it is the price
  of the model finally being load-bearing.
- Dispatch moving to the harness changes follow-up timing ownership; wire
  additions for queue state are additive per ADR-0134 but touch every
  frontend.

### Implementation milestones (each lands independently)

1. **M1 — Translator seam.** Listener produces `AppMutation`s; event loop is
   the sole `App` writer. Mirrors/rev-counters deleted. (L2, L3 signal
   half)
2. **M2 — Scene-owned modality.** Real input families in the engine;
   `active_modal` input removed; `InputContext` and probes deleted; routing
   tests on the scene. (L1)
3. **M3 — Client facade.** `muta-client` boundary + dependency ban.
   (L5)
4. **M4 — Harness dispatch.** Queue authority moves to the driver; frontend
   queue machine deleted. (L4)
5. **M5 — Dead-link states.** Checked sends + reconnecting chrome, with
   ADR-0196 health banners. (L6)
6. **M6 — Engine deletion sweep.** `render_tree` removed; ADR-0195 marked
   partially superseded; docs updated. (L7)

## References

- ADR-0038 (in-house grid/diff engine — the vocabulary-free engine this
  decision finally uses fully), ADR-0195 (retained TUI runtime and
  component lifecycle — scene/runtime half stands; render-tree half
  superseded by D7), ADR-0045 (the three-layer engine/view/shell topology
  this decision completes), ADR-0190 (task fabric; its D7 discipline names
  the dead-code class; its driver loop receives dispatch in D5), ADR-0126
  (round-boundary queueing), ADR-0103 (asides), ADR-0134 (wire
  compatibility), ADR-0196 (persistence health — the backend half of D6).
- Prior art: Elm architecture (model/update/view with one owner);
  Flutter's element tree (per-node state owned by the framework, widgets
  re-declared per frame — the shape `ui/runtime.rs` already has);
  Redux/devtools single-source-of-truth discipline; MVC's original
  one-way data flow.
