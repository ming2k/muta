# 0139. Unified TUI surface router and complete view lifecycle

- **Status:** Accepted
- **Date:** 2026-08-25

## Context

ADR-0133 established the desired buffer-like behavior for TUI views, but its
implementation still used `Modal` as both presentation and identity. That is
not lossless: Activity and Todos are different places while both render as
`Modal::Activity`. Navigation was also split between `active_modal`, a
quick-switcher return field, a `ViewRegistry` editor stack, and request-sheet
assignments. Consequently, some switch paths skipped exit hooks, request
sheets overwrote their parent, hidden views disappeared from MRU, and the
Tree/Sessions quick-switch paths could open without a backend data path.

The architecture also lacked a testable boundary between a durable view, a
transient sheet/editor, an internal sub-layer, and the modal discriminant used
by rendering.

This decision supersedes ADR-0133. It retains the buffer-like product model but
replaces the incomplete identity, routing, lifecycle, and data-refresh
mechanisms.

## Decision

### 1. Classify surfaces by lifecycle, not appearance

A **view** is a stable place where the user can stand and later return. It
must have an exact `ViewId`, be directly focusable (including through the
global switcher), own retained navigation state, and support the complete
create/show/hide/switch/close lifecycle.

The TUI views are:

- Help, Activity, Todos;
- Tools, MCP, Skills, Permissions;
- Usage statistics, Context report, Asides, Settings;
- Models, Connections, History, Queue;
- Session dashboard, Sessions, Session tree.

Activity and Todos remain separate view identities even though both use the
Activity modal renderer.

The following are not views:

- chat is the root surface, not a retained auxiliary destination;
- Permission, Question, and Input Injection are request-driven sheets;
- View Switcher is a transient chooser;
- Model Editor, Provider Template, Custom Provider, and OAuth Pending are
  transient workflow steps;
- Host preview/prompt, Sessions info, Context-report detail, Settings detail,
  and similar drill-ins are sub-layers owned by their parent view;
- toasts, completion menus, confirmations, and keymap pages are adornments or
  sub-layers, not navigation destinations.

`Modal` identifies rendering/input presentation only. It must never be used to
reconstruct view identity.

### 2. Make `SurfaceRouter` the single navigation authority

`App` owns one `SurfaceRouter` containing the active `Surface` (`Chat`,
`View(ViewId)`, or `Transient(Modal)`) and a bounded transient return stack.
There is no independent `active_modal` state field. Rendering reads the
router's modal projection; lifecycle code reads its exact `ViewId`.

Editors, request sheets, and the quick switcher use the same push/pop
mechanism. A pop restores the exact parent, including `Todos` rather than an
ambiguous Activity default. Transactional child surfaces and open sub-layers
block the quick switcher unless their state can be suspended safely.

### 3. Give every view the complete lifecycle

`ViewRegistry` owns lazy-created `ViewState` records and a most-recently-used
order. The verbs are:

- **create**: the first show lazily allocates state and runs one-time UI
  initialization;
- **show**: focus the view, restore retained position/query, run enter hooks,
  and refresh authoritative data according to policy;
- **hide**: leave for chat while retaining state and MRU membership;
- **switch**: run the origin's exit hook, retain it, then show the target;
- **close**: run any active exit hook, remove the registry state and MRU entry,
  and clear the view-owned volatile UI payload;
- **close all**: on viewed-session change, clear the router, all retained
  states, session-scoped payloads, drafts, and armed hooks.

Hiding does not remove a view from MRU; only explicit close does. `Del` in the
quick switcher closes the highlighted view. This destroys TUI view state only;
it never deletes a session, provider, aside, or other backend resource.

Composer-borrowing views store two separate values: the user's parked chat
draft and the view's own filter query. Switching away restores the chat draft;
returning restores the filter. No switch can confuse one for the other.

Queue auto-block/resume, draft parking, and volatile sub-layer cleanup are
enter/exit hooks and therefore run for every path, including quick switching,
shortcuts, mouse entry, Esc, Ctrl+C, outside click, and backend open signals.

### 4. Separate data refresh from presentation signals

All TUI entry paths call one `enter_view` transaction. First-show UI defaults
and refresh-on-show effects are declared there rather than copied into input
arms.

Backend snapshot responses update data and do not navigate. In particular,
`QuerySessionsOverview` and `QuerySessionTree` return `SessionsOverview` and
`SessionTreeSnapshot`. Bare slash commands that are intended to navigate emit
separate `OpenSessionsPanel` or `OpenTreePanel` presentation signals. This
lets the quick switcher refresh a view without causing a second navigation
event and makes Tree a real end-to-end view rather than a placeholder.

Request-driven sheets may preempt a view. They push over the current surface,
own input until settled, and pop back to the exact parent. Concurrent requests
remain queued; settling one sheet hands off to the next without losing the
underlying view.

## Alternatives considered

- **Keep `active_modal` plus an `active_view` side field.** Rejected because
  two writable sources can drift and preserve the Activity/Todos bug class.
- **Continue inferring identity with `TryFrom<Modal>`.** Rejected because the
  mapping is mathematically non-injective and cannot be made correct.
- **Treat every modal as a view.** Rejected because request sheets and
  transactional editors have queue/parent lifecycles, should not appear in
  MRU, and cannot be switched away from safely.
- **Refresh only on first open.** Rejected because backend-owned lists and
  reports become stale after out-of-band changes. Position is retained;
  authoritative data is refreshed on show.
- **Let snapshot responses open UI.** Rejected because data transport then
  has surprising presentation side effects and cannot serve background
  refresh or multiple frontends cleanly.

## Consequences

- View identity, presentation, retained state, and transient navigation have
  distinct owners and can be tested independently.
- Every entry path gets the same initialization, refresh, and enter/exit
  effects; switching no longer bypasses Queue or draft hooks.
- Request sheets return to the interrupted view instead of dropping to chat.
- The switcher is a true MRU/discovery/close surface and identifies
  Activity/Todos correctly.
- Additive attach-protocol variants are required for session-list/tree queries
  and their explicit open signals.
- View render payloads remain fields on `App` where renderers consume them,
  but their lifetime is governed centrally by `enter_view`, `deactivate_view`,
  `close_view`, and `reset_view_payload`; direct navigation mutation is not
  permitted.

## References

- [ADR-0133](0133-view-surfaces-buffer-like-lifecycle-and-quick-switch.md)
- [TUI modals and surface lifecycle](../reference/tui/modals.md)
- [TUI architecture](../reference/tui/architecture.md)
- `apps/tui/crates/mutx/src/views.rs`
- `apps/tui/crates/mutx/src/app.rs`
- `apps/tui/crates/mutx/src/event_loop/actions.rs`
