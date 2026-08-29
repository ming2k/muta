# TUI architecture

The muta terminal UI is split into **three layers** with dependencies
pointing strictly downward. The engine is its own crate; the view and shell
layers are module trees inside the `mutx` library crate (extracted
from the `mutx` binary by ADR-0098; the view had earlier been the
separate `mutx-view` crate until ADR-0079 re-merged it — so the
one-way seam remains a documented convention rather than
compiler-enforced). The split exists so the rendering engine, the widgets,
and the application wiring can be
reasoned about (and tested) in isolation, and so the widget layer can never
secretly reach into application state.

```text
┌──────────────────────────────────────────────────────────────────────┐
│  mutx-engine  ·  ENGINE                                  (ADR-0038)     │
│  Retained cell grid · write-marks-dirty tracking · back/front diff ·   │
│  crossterm backend · Frame / Rect / Layout / Span primitives.         │
│  Knows nothing about muta — pure terminal drawing.                  │
└──────────────────────────────────────────────────────────────────────┘
                          ▲  widgets render *into* the grid
                          │  (Frame::render_widget)
┌──────────────────────────────────────────────────────────────────────┐
│  mutx view modules  ·  VIEW (widgets + document model)         │
│  view/ widget tree · document model · layout/hit-testing ·          │
│  selection · fuzzy · provider ranking · shared modal discriminants.   │
│  Renders muta_contracts domain types → depends on muta-contracts.           │
│  NEVER depends on the shell modules.                                  │
└──────────────────────────────────────────────────────────────────────┘
                          ▲  the shell fills in a borrowed
                          │  TranscriptView<'a> each frame
┌──────────────────────────────────────────────────────────────────────┐
│  mutx shell modules  ·  APP SHELL (mutx crate)           │
│  App state · event loop · input→action mapping · terminal lifecycle · │
│  completion logic · clipboard · session wiring.                       │
│  Owns the data; drives the view modules in the same crate.            │
└──────────────────────────────────────────────────────────────────────┘
```

## The three layers

### Engine — `apps/tui/crates/mutx-engine`

The in-house grid engine (ADR-0038). A retained 2-D cell grid with
write-marks-dirty tracking, a back/front buffer diff, and a crossterm backend.
It exposes `Frame`, `Rect`, `Layout`, `Span`, `Style`, `Grid`, `TestTerminal`,
and friends. It has **no muta dependencies** — it is a general terminal
drawing engine that the view layer paints into.

### View — `apps/tui/crates/mutx/src` (view modules)

The widget layer and the semantic document model. Everything here is a pure
function of borrowed data: it reads `muta_contracts` domain types and a `Theme`
and writes cells into the engine's grid. The view modules depend on
`mutx-engine` (to draw),
`muta-contracts` (the domain types they render), and `muta-providers` (the model
catalog the picker ranks). They **do not** depend on the shell modules — since
ADR-0079 re-merged the view crate into the binary, the one-way boundary is a
documented convention rather than compiler-enforced.

The view modules live flat under `apps/tui/crates/mutx/src/`, grouped by concern:

| Module | Responsibility |
|--------|----------------|
| `view/mod.rs` | The transcript-area renderer: `draw_transcript`, `TranscriptView`, `HeightCache`; re-exports the drawing surface (chrome, composer, overlays, theme, …) the shell consumes. |
| `components/` | Reusable composed components: modal pages, selectable lists, scroll bodies, selectable document bodies (`selectable_body`), footer hints, toasts, notices, option rows, and one-line metadata strips (`MetaStrip`). |
| `overlays/` | One renderer per modal (provider, session, help, activity, config, permission, …). |
| `tools/` | Per-tool-step presenters (execute_command, edit, read, search, web, ask_user, diff, …). |
| `disclosure/` | Expandable-step disclosure: state machine, sticky-pin tracking, step renderers. |
| `layout/` | Transcript arrangement strategies (`default` / `legacy`). |
| `theme.rs` / `design.rs` | Color scheme + non-color design tokens (spacing, gutters, row counts). |
| `chrome.rs` / `composer.rs` / `primitives.rs` / `text_layout.rs` / … | Drawing leaves: activity/state/model bars, input composer, rect helpers, text wrapping. |
| `model/` | Semantic data model: `document` (`TranscriptMessage`, `Block`, markdown parsing), `layout` (`LayoutMap`, `BlockRegion`, `SemanticCursor`, hit-testing), `selection` (`SelectionState`). |
| `fuzzy` / `providers` / `modal` / `completion` | Helpers shared with the shell. |

### App shell — `apps/tui/crates/mutx/src`

The application: `App` state, the event loop, input→action mapping, terminal
lifecycle, completion logic, clipboard, and session wiring. It owns all the
mutable state and drives the view layer once per frame. Shell and view share
the `mutx` crate since ADR-0098; the shell addresses the view as
`crate::{view, components, …}`.

| Module | Responsibility |
|--------|----------------|
| `app.rs` | `App` state, `CaretOwner`, scroll/zoom snapshots. |
| `event_loop.rs` | App loop: state sync, draw orchestration, action handling. |
| `input/` | Event→`InputAction` keyboard/mouse dispatch. |
| `terminal.rs` | Raw-mode / alt-screen setup-teardown, render-loop wiring. |
| `completion.rs` | Slash-command / `@path` completion **logic** (`impl App`); the data types live in the view layer. |
| `step_interaction.rs` | Transcript-step focus, toggle, keyboard interaction. |
| `clipboard.rs` / `clipboard_ops.rs` | OSC52 + system clipboard, async copy. |
| `question_model.rs` | Question-modal state machine. |

## The seam — `TranscriptView<'a>`

The shell and the view layer communicate through one borrowed struct,
`view::TranscriptView<'a>`, that the event loop fills in each frame. It
carries **only borrowed data** — `&[TranscriptMessage]`, `&SelectionState`,
`&Theme`, scroll/activity/todo snapshots — and crucially **no
reference to `App`**. This is what keeps the view layer a pure rendering
function: there is no back-channel into application state, so a widget can
only draw what the shell chose to hand it.

`draw_transcript(frame, &mut LayoutMap, view)` is the single entry point
for the transcript; the per-modal overlays (`draw_models_modal`,
`draw_permission_sheet`, …) take their own small borrowed view structs
(`ActivityModalView`, `CustomEditorView`, …) the same way.
The shell calls the view and never the reverse.

## Surface routing and shared presentation discriminants

`Modal`, `Recess`, and `ActivityTab` are fieldless enums that *name* things
without owning state:

- `Modal` — which overlay presentation to draw and which modal input map to
  use. It is a projection, never durable navigation identity: Activity and
  Todos intentionally project to the same modal.
- `Recess` — how the live surface recedes behind a modal (float / dim /
  takeover). The view layer's recess pass and the shell's footer-collapse
  decision both key off it.
- `ActivityTab` — which section the Activity modal shows.

The shell owns exact navigation separately in `surfaces.rs`. `SurfaceRouter`
is the sole authority for the active `Surface` (`View`, `Panel(PanelId)`, or
`Transient(Modal)`) — the base full-screen view plus the transient return
stack; `PanelRegistry` owns lazy panel state and MRU order. Render code
receives only the router's `Modal` projection. Lifecycle code operates on
`PanelId`/`View`, so it never attempts the lossy inverse mapping from a
modal back to a surface.

Under ADR-0141 a **view** is an independent full-screen destination
(`Session`, `Dashboard`, `Settings`, `Envoy`, `Side`) and a **panel** is a
retained modal — one of the browse overlays (help, activity, todos, tools,
…) floating over the active view. Envoy zoom and the aside view route
through the router as views (`App::focus_stack` / `side_session_id` remain
frame data), so `in_envoy_view()` / `in_side_view()` derive from the router
instead of scattered booleans and stack emptiness.

All entry paths converge on the event loop's `enter_panel` / `enter_view`
transactions. They run first-create initialization, refresh-on-show backend
queries, and enter/exit hooks consistently for shortcuts, mouse actions,
switcher actions, and backend presentation signals. Snapshot responses
update data only; separate open signals navigate. Request sheets and
workflow editors push/pop through the router, while drill-ins remain state
owned by their parent surface. See
[ADR-0139](../../adr/0139-unified-tui-surface-router-and-view-lifecycle.md)
and [ADR-0141](../../adr/0141-view-means-fullscreen-and-modal-means-modal.md).

Because presentation types are shared by both layers and dependencies point
downward, they live in the lower layer (`tui::modal`) and the shell re-exports
them. The same reasoning applies to
`completion::{Completion, CompletionKind}`: render code draws them, while
matching logic remains in the shell as an `impl App`.

## Component reuse inside the view layer

Within the view modules, components stack into reuse tiers — lower tiers know
nothing about higher ones:

```text
  leaves    tools/*  ·  overlays/{help,session,provider,…}
              │ build on
  mid-tier  components/  ·  disclosure/  ·  composer  ·  chrome
              │ build on
  base      primitives  ·  text_layout  ·  markdown_table
              │ tokens
  tokens    theme (colors)  ·  design (spacing/gutters/row counts)
```

- **`primitives`** — `viewport_rect`, `centered_rect`, `panel_block`,
  `recess_backdrop`, `modal_area`, color helpers. The shared rect/panel/color
  vocabulary everything else is built from.
- **`text_layout`** — `wrap_text`, `WrappedLine`, `line_spans`, the
  gutter/wrapping core reused by message bodies, code blocks, and tools.
- **`theme` / `design`** — the only places colors and fixed measurements are
  defined; every component reads tokens from here instead of hard-coding.
- **`components/`** — composed, reusable view pieces: modal pages, selectable
  lists, scroll bodies, the selectable document body
  (`selectable_body::render_selectable_body` — the single copy-by-default
  path modal documentary text goes through; it wraps in the application
  layer and registers every visual row as a `MODAL_DOC` selection region),
  width-aware modal footers, toasts, transcript notices, and question option
  rows. These are still pure render helpers; event handling and action logic
  stay in the app shell.
- **`disclosure/`** — the collapsible-step state machine (`Disclosure`,
  `Interaction`) and shared header rendering, reused by every `tools/*` renderer.
- **`layout/`** — transcript arrangement strategy (`turn_band`)
  selected by `[tui] transcript_layout`.

The leaves (`tools/*`, the per-modal overlays) are intentionally thin: they
compose the mid-tier and base helpers rather than re-implementing wrapping,
panels, or color logic.

## See also

- [ADR-0038](../../adr/0038-in-house-grid-diff-rendering-engine.md) — the engine.
- [index.md](index.md) — component reference and the full source-file map.
- [components.md](components.md) — reusable render-component lookup table.
- [layout.md](layout.md) — frame measurements, footer stack, modal modes.
- [step-state.md](step-state.md) — the disclosure/interaction state machine.
