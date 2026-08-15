# TUI architecture

The neenee terminal UI is split into **three layers** with dependencies
pointing strictly downward. The engine is its own crate; the view and shell
layers are module trees inside the `neenee-tui` library crate (extracted
from the `neenee-cli` binary by ADR-0098; the view had earlier been the
separate `neenee-tui-view` crate until ADR-0079 re-merged it — so the
one-way seam remains a documented convention rather than
compiler-enforced). The split exists so the rendering engine, the widgets,
and the application wiring can be
reasoned about (and tested) in isolation, and so the widget layer can never
secretly reach into application state.

```text
┌──────────────────────────────────────────────────────────────────────┐
│  neenee-tui-engine  ·  ENGINE                                  (ADR-0038)     │
│  Retained cell grid · write-marks-dirty tracking · back/front diff ·   │
│  crossterm backend · Frame / Rect / Layout / Span primitives.         │
│  Knows nothing about neenee — pure terminal drawing.                  │
└──────────────────────────────────────────────────────────────────────┘
                          ▲  widgets render *into* the grid
                          │  (Frame::render_widget)
┌──────────────────────────────────────────────────────────────────────┐
│  neenee-tui view modules  ·  VIEW (widgets + document model)         │
│  render/ widget tree · document model · layout/hit-testing ·          │
│  selection · fuzzy · provider ranking · shared modal discriminants.   │
│  Renders neenee_contracts domain types → depends on neenee-contracts.           │
│  NEVER depends on the shell modules.                                  │
└──────────────────────────────────────────────────────────────────────┘
                          ▲  the shell fills in a borrowed
                          │  TranscriptView<'a> each frame
┌──────────────────────────────────────────────────────────────────────┐
│  neenee-tui shell modules  ·  APP SHELL (neenee-tui crate)           │
│  App state · event loop · input→action mapping · terminal lifecycle · │
│  completion logic · clipboard · session wiring.                       │
│  Owns the data; drives the view modules in the same crate.            │
└──────────────────────────────────────────────────────────────────────┘
```

## The three layers

### Engine — `crates/neenee-tui-engine`

The in-house grid engine (ADR-0038). A retained 2-D cell grid with
write-marks-dirty tracking, a back/front buffer diff, and a crossterm backend.
It exposes `Frame`, `Rect`, `Layout`, `Span`, `Style`, `Grid`, `TestTerminal`,
and friends. It has **no neenee dependencies** — it is a general terminal
drawing engine that the view layer paints into.

### View — `crates/neenee-tui/src` (view modules)

The widget layer and the semantic document model. Everything here is a pure
function of borrowed data: it reads `neenee_contracts` domain types and a `Theme`
and writes cells into the engine's grid. The view modules depend on
`neenee-tui-engine` (to draw),
`neenee-contracts` (the domain types they render), and `neenee-providers` (the model
catalog the picker ranks). They **do not** depend on the shell modules — since
ADR-0079 re-merged the view crate into the binary, the one-way boundary is a
documented convention rather than compiler-enforced.

The view modules live flat under `crates/neenee-tui/src/`, grouped by concern:

| Module | Responsibility |
|--------|----------------|
| `view.rs` | The transcript-area renderer: `draw_transcript`, `TranscriptView`, `HeightCache`; re-exports the drawing surface (chrome, composer, overlays, theme, …) the shell consumes. |
| `components/` | Reusable composed components: modal pages, selectable lists, scroll bodies, footer hints, toasts, notices, option rows, and one-line metadata strips (`MetaStrip`). |
| `overlays/` | One renderer per modal (provider, session, help, activity, config, permission, …). |
| `tools/` | Per-tool-step renderers (bash, edit, read, grep, web, ask_user, diff, …). |
| `disclosure/` | Expandable-step disclosure: state machine, sticky-pin tracking, step renderers. |
| `layout/` | Transcript arrangement strategies (`default` / `legacy`). |
| `theme.rs` / `design.rs` | Color scheme + non-color design tokens (spacing, gutters, row counts). |
| `chrome.rs` / `composer.rs` / `primitives.rs` / `text_layout.rs` / … | Drawing leaves: activity/state/hint bars, input composer, rect helpers, text wrapping. |
| `model/` | Semantic data model: `document` (`TranscriptMessage`, `Block`, markdown parsing), `layout` (`LayoutMap`, `BlockRegion`, `SemanticCursor`, hit-testing), `selection` (`SelectionState`). |
| `fuzzy` / `providers` / `modal` / `completion` | Helpers shared with the shell. |

### App shell — `crates/neenee-tui/src`

The application: `App` state, the event loop, input→action mapping, terminal
lifecycle, completion logic, clipboard, and session wiring. It owns all the
mutable state and drives the view layer once per frame. Shell and view share
the `neenee-tui` crate since ADR-0098; the shell addresses the view as
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
(`HintBarView`, `ActivityModalView`, `CustomEditorView`, …) the same way.
The shell calls the view and never the reverse.

## Shared discriminants — why `modal` lives in the view layer

`Modal`, `Recess`, and `ActivityTab` are fieldless enums that *name* things
without owning state:

- `Modal` — which overlay is open. The view layer needs it for modal geometry
  (`modal_area`) and per-modal rendering; the shell needs it as state.
- `Recess` — how the live surface recedes behind a modal (float / dim /
  takeover). The view layer's recess pass and the shell's footer-collapse
  decision both key off it.
- `ActivityTab` — which section the Activity modal shows.

Because both layers share them and dependencies point downward, they live in
the lower layer (`tui::modal`) and the shell re-exports them. Same
reasoning for `completion::{Completion, CompletionKind}`: the render code draws
them, so the *types* live in the view layer while the *matching logic* stays in
the shell as an `impl App`.

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
  lists, scroll bodies, width-aware modal footers, toasts, transcript notices,
  and question option rows. These are still pure render helpers; event
  handling and action logic stay in the app shell.
- **`disclosure/`** — the collapsible-step state machine (`Disclosure`,
  `Interaction`) and shared header rendering, reused by every `tools/*` renderer.
- **`layout/`** — transcript arrangement strategies (`default`,
  `legacy`) selected by `[tui] transcript_layout`.

The leaves (`tools/*`, the per-modal overlays) are intentionally thin: they
compose the mid-tier and base helpers rather than re-implementing wrapping,
panels, or color logic.

## See also

- [ADR-0038](../../adr/0038-in-house-grid-diff-rendering-engine.md) — the engine.
- [index.md](index.md) — component reference and the full source-file map.
- [components.md](components.md) — reusable render-component lookup table.
- [layout.md](layout.md) — frame measurements, footer stack, modal modes.
- [step-state.md](step-state.md) — the disclosure/interaction state machine.
