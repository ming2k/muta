# 0133. Views as retained, buffer-like surfaces with one shared lifecycle and a global quick switcher

- **Status:** Proposed (phases 1–2 implemented; 3–5 open)
- **Date:** 2026-08-22

> **Implementation status.** Phase 1 (the `ViewRegistry`, first-open
> initialisation, the hide/close/switch vocabulary on `App`) and phase 2
> (the browse surfaces: Help, Activity/Todos, Tools, MCP, Skills,
> Permissions, UsageStats, TokenReport, Btw, and the Settings view) are
> implemented, together with the quick switcher (`Ctrl+L`) over every
> migrated view. `views.rs` is the registry; `App::open_view` /
> `save_view_state` / `dismiss_surface` are the verbs; the Esc,
> outside-click, and Ctrl+C paths share `App::dismiss_surface`. Settings
> keeps its cursor in its own fields (`config_category` /
> `config_detail_index` / `config_focus`), so its retention is field-native
> and the registry stores no scroll for it. Phases 3 (picker→editor chains,
> per-view drafts), 4 (Host/Sessions sub-layers onto the stack), and the
> editor-chain return consolidation remain open.

## Context

The TUI has one fieldless discriminant — `Modal`
(`crates/neenee-tui/src/modal.rs:10`) — and a single global slot
`App::active_modal` (`crates/neenee-tui/src/app.rs:425`). Everything else
about a surface is a set of flattened per-modal fields on `App`
(`*_scroll`, `modal_index`, `skills_expanded`, `host_*`, `config_*`,
`btw_*`, …) plus boolean "sub-layer" flags (`host_preview`,
`token_report_detail`, `config_custom_editing`, `modal_keymap_open`, …).

Three observations motivate this ADR:

1. **No hide/close distinction, and no shared lifecycle.** "Closing" a modal
   is `active_modal = Modal::None` (`event_loop/actions/modals.rs:373`).
   Close preserves scroll/cursor state only *accidentally* — every open
   action resets it (`modal_index = 0`, `scroll = 0`, follow = true; e.g.
   `actions.rs:589-655`, `event_loop.rs:1287-1323`). So the user's reading
   position in Help/Activity/UsageStats/Todos is destroyed on every
   close-reopen cycle, and each modal re-implements its own reset ritual in
   its open arm. Some surfaces even *discard data* on open (`UsageStats`
   clears its report, `actions.rs:626`; Host clears `host_console_log`,
   `event_loop.rs:1304`).

2. **Navigation is a flat replace plus hand-written return links.** There is
   no stack. Escape semantics live in one giant "deepest sub-layer first"
   chain (`modals.rs:258-375`) that the outside-click path mirrors by hand
   (`actions/mouse.rs:93-183`), plus one single-slot back-pointer
   (`editor_return_to`, `app.rs:942`) and two hard-coded returns
   (ProviderTemplate/CustomProvider → Connections). Every new layer adds
   another arm to that chain and another chance for the two paths to drift
   apart.

3. **Sessions already behave like buffers; views do not.** ADR-0096 made
   sessions daemon-owned: closing the TUI detaches rather than kills, and
   re-attach resumes state. Users correctly model that as "switch away, come
   back, nothing lost". Views get the opposite contract — leave and
   everything you were looking at is gone — which is inconsistent with the
   product's own mental model. Terminal users know this pattern well
   (tmux/Vim/Emacs windows/buffers): *hide* by default, *quit* only when
   explicitly asked.

Two escape hatches already exist and prove the design point:
`modal_keymap_open` is a modal-scoped "sub-view swap" that keeps the parent
alive (`app.rs:605-609`), and the envoy `focus_stack` (`app.rs:408`) is a
real push/pop view stack for one specific surface — but neither is
generalised.

The seam where this lands already exists and is already duplicated by
hand in three places: `App::modal_scroll_field` (`app.rs:1419-1477`),
`Modal::recess` / `dismissable_by_outside_click` / `owns_caret`
(`modal.rs`), and the render dispatch `match app.active_modal`
(`event_loop/render.rs:592-1050`).

## Decision

Adopt a **buffer-like view model** for surfaces the user *browses*:

1. **Every browse surface becomes a retained `View` with per-view state.**
   Move each modal's flattened state out of `App` into a `View` value with
   the standard fields — `scroll`, `index`, `follow`, plus its own sub-state
   — kept alive in a registry keyed by view id (roughly: the current `Modal`
   variants, minus the request-driven ones). `App` keeps
   `active: ViewId` plus `registry: map<ViewId, ViewState>`. Views are
   created on first use and **never dropped** for the lifetime of the App —
   the current "reset on every open" arms (`actions.rs:589-655`,
   `event_loop.rs:1287-1323`) are replaced by *first-open* initialisation,
   so reopen returns to the exact scroll/index/sub-layer the user left.

2. **Navigation becomes a real MRU stack.** `App` gains
   `nav: Vec<ViewId>` (most-recent-first). Opening a view pushes it;
   Esc/outside-click pops it. This *replaces* the deepest-first chain and
   the hand-mirrored outside-click copy: drilling into a sub-layer pushes a
   sub-view, so "return" is the same pop everywhere. `editor_return_to`,
   the ProviderTemplate/CustomProvider hard-codes, and `host_preview` /
   `token_report_detail` / `session_info_detail` / `config_custom_editing`
   booleans become pushes onto the same stack. The stack is bounded (cap at
   ~16; the eldest entries are dropped).

3. **One lifecycle vocabulary, three verbs.** `hide` (Esc — state retained,
   view stays in the registry), `close` (explicit exit — view removed from
   the registry and its data dropped), and `switch` (quick switcher or
   navigation — pure focus move, nothing reset). Every view gets all three
   through the shared machinery, not through per-modal arms. Request-driven
   sheets (`Permission`, `Question`, `InputInjection`) keep their
   queue-driven lifecycle but ride the same stack so their return target is
   well-defined.

4. **A global quick switcher** (the view-level analogue of the session
   dashboard): a `Gate::Always`-style binding — **Ctrl+L**, currently free
   (checked against `GLOBAL_BINDINGS` and the readline family; the F-row was
   rejected for the queue family in ADR-0126 for the same portability
   reasons, and F5's comment already records that Ctrl+G collides with
   readline) — opens a fuzzy MRU list of views (open-views first, then
   not-yet-opened ones), reuse the Models/Connections picker's
   browse/search two-mode design, Enter switches (hide current, focus
   target). If no view is open it lists all views.

5. **Composer-line views park drafts into their own view state.** The
   single global `stashed_input` slot (`app.rs:922`) becomes per-view:
   parking the composer on view-entry stores the draft in the *entering*
   view's state, restoring on return, so nested flows can no longer
   overwrite each other's parked draft.

Non-goals: no change to `Recess` (it is orthogonal and already a clean
single source of truth); no persistence of view state across App restarts
(views are TUI-process state; sessions are the durable unit); `neenee
dashboard`'s startup overlay keeps its quit-on-Esc contract (ADR-0096's
CLI entry semantics are unchanged).

## Alternatives considered

- **Status quo (flat discriminant + per-modal reset arms).** Rejected: every
  new modal re-implements open/close/reset, the deepest-first chain grows
  linearly with surfaces, and the reading-position loss is a daily UX tax
  the session model already taught users not to expect.

- **Full SPA-style page tree (every view a first-class `Page` with parents,
  routing, history).** Rejected: the terminal surface is one viewport; a
  tree + router is heavy machinery for what is, today, ~15 browse surfaces,
  and it would force rewrites of every renderer signature at once. The MRU
  stack gives the navigation semantics (bounded back-chain) without a
  router.

- **Keep the discriminant, just fix the resets (make open actions not reset
  state).** Rejected as a half-measure: it removes the reading-position tax
  but leaves three hand-mirrored lifecycle sites (`modal_scroll_field`,
  the Esc chain, the outside-click copy) and still has no answer for
  "return to where I was" across a two-level drill (ModelEditor from
  Models vs from Connections).

- **Modals as a stack of *drawn* layers (nested rendering).** Rejected: the
  terminal cannot usefully stack more than ~2 layers of content; today's
  `Recess` model already handles the visual story. This ADR is about
  *state and navigation*, not paint order.

- **Ctrl+Tab / Ctrl+Space / F-row for the quick switcher.** Rejected:
  Ctrl+Tab is browser/tmux-reserved and often swallowed; Ctrl+Space is IME
  territory on many desktops; the F-row was already rejected in ADR-0126
  for portability. Ctrl+L is a distinct byte sequence, works in raw mode,
  tmux, and screen, and is not in the readline family the composer uses
  (`Ctrl+A/E/W/U/K`, `Alt+B/F/D`).

## Consequences

Positive:

- Reopen preserves scroll/index/sub-layer — parity with the session
  "detach/attach" mental model; no more "Help forgot where I was".
- The Esc deepest-first chain and its outside-click twin collapse into one
  pop; every new surface gets hide/close/switch for free instead of new
  arms in three files.
- The quick switcher makes the full view set reachable from anywhere in
  two keystrokes, and gives the same "switch, don't destroy" promise
  sessions already have.
- Per-view drafts remove a real (if rare) data-loss window in nested
  picker→editor flows sharing one `stashed_input`.

Negative / costs:

- A real refactor: `App` loses ~40 fields to the registry; every open arm,
  the close chain, the outside-click mirror, and `modal_scroll_field`
  rewrite onto the new vocabulary. Land it incrementally (phase plan
  below), not as one big-bang PR.
- State that *should* refresh on reopen (e.g. `UsageStats` re-query) must
  become an explicit per-view `refresh_on_show` flag rather than a free
  side effect of the reset ritual. Same for Queue's block/resume pairing
  (`block_queue`/`resume_queue`) which must move to the view's
  enter/exit hooks.
- Memory: view state lives for the App's lifetime. Bounded and small
  (scroll/index/sub-layer), but worth a cap on per-view caches (the
  `host_console_log` cap policy generalises).

Migration (incremental, each step shippable):

1. Introduce `ViewId`/`ViewState` + the nav stack beside `active_modal`;
   convert one surface (Help — trivial, no data, no draft) end-to-end
   including its quick-switcher entry.
2. Convert the read-only browse surfaces (Activity/Todos, UsageStats,
   TokenReport, Btw, Tools/Mcp/Skills/Permissions, Config) — these are
   pure wins (scroll/index retention).
3. Convert the picker→editor chains (Models, Connections,
   ProviderTemplate, CustomProvider, ModelEditor, OauthPending) — this is
   where per-view drafts and the unified return chain pay off; delete
   `editor_return_to` and the two hard-coded returns.
4. Convert Host/Sessions (they already have internal sub-layers that
   become pushes; `host_preview`/`host_prompting`/`session_info_detail`
   booleans dissolve into the stack).
5. Ship the quick switcher (it can land earlier as a thin MRU list over
   `Modal` values and be re-pointed at `ViewId`s in phase 3; shipping it
   last keeps its semantics stable).

## References

- `crates/neenee-tui/src/modal.rs` — the current discriminant + `Recess`
- `crates/neenee-tui/src/app.rs:419-1477` — flattened modal state,
  `modal_scroll_field`, `caret_owner`
- `crates/neenee-tui/src/event_loop/actions/modals.rs:258-375` — the
  deepest-first close chain
- `crates/neenee-tui/src/event_loop/actions/mouse.rs:93-183` — the
  hand-mirrored outside-click twin
- `crates/neenee-tui/src/keymap.rs` — `GLOBAL_BINDINGS` registry
- ADR-0096 (unified session daemon — the detach/attach mental model this
  ADR extends to views)
- ADR-0126 (queue affordances — the Ctrl-row precedent and the F-row
  portability argument)
- `docs/reference/tui/modals.md`, `docs/reference/tui/layout.md` — current
  view/modal documentation (both need updating once this lands)
