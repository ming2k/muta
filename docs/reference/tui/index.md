# TUI reference

The muta terminal UI is split into three layers — see
[architecture.md](architecture.md) for the full picture. In short: the in-house
[mutx-engine](../../../apps/tui/crates/mutx-engine/src/lib.rs) engine (ADR-0038) is a
retained cell grid with write-marks-dirty tracking, a back/front diff, and a
crossterm backend; the **view layer**
(the view modules of the `mutx` crate — formerly the `mutx-view`
crate, re-merged by ADR-0079 and re-extracted with the shell by ADR-0098) holds the widget
tree (entry point `view.rs`) and the semantic document model, rendering
*into* the engine's grid via `Frame::render_widget`; and the **app shell**
(`apps/tui/crates/mutx/src`) owns `App` state, the event loop, and input
mapping, driving the view layer through the borrowed `TranscriptView` seam.

## Frame layout

```text
┌──────────────────────────────────────────────────────────┐
│  Transcript viewport                            (Min 0)  │
│   (messages, expandable steps, sticky pinned summaries)  │
├──────────────────────────────────────────────────────────┤
│  Activity bar                                 (0 or 1 row)  │
│  Todo bar                                      (0 or 1 row)  │
│  Queue bar                                     (0 or 2 rows) │
│  Input box                         (2 + wrapped lines)  │
│  Hint bar                                       (1 row)  │
└──────────────────────────────────────────────────────────┘
```

See [layout.md](layout.md) for the footer stack, the envoy zoom view,
the modal overlay mode, chrome hiding, and the full measurements table.

## Transcript focus

There are no modal "zones" and no zone-toggle key. Keyboard navigation
rests on a single optional state — the **focused step**
(`App::focused_target`):

| State | Owns keys | How to enter | How to leave |
|-------|-----------|--------------|--------------|
| **Prompt** (default) | Input box — typing inserts into the prompt | (default) | `Ctrl+↑` / `Ctrl+↓` |
| **Focused step** | One transcript step is reverse-highlighted | `Ctrl+↑` / `Ctrl+↓` (nearest step first) | `Esc`, or any printable character falls through to the prompt |

While a step is focused, `↑`/`↓` cycle steps, `Enter` opens it, and the
composer panel drops to its dimmer palette to signal "keys act on the
step." Typing still lands in the prompt. `Tab` is completion-only (commits
the highlighted slash/path suggestion when a menu is open, re-opens one
that `Esc` dismissed); it is not a focus toggle.

## Components

| Component | Description |
|-----------|-------------|
| [User message](user-message.md) | Sent prompts on a dimmer panel with `┃` bar |
| [Input box](input-box.md) | Live editable prompt on a brighter panel |
| [Assistant text](assistant-text.md) | Regular markdown text, 4-space indent |
| [Code block](code-block.md) | Borderless code with `┃` bar + line-number gutter |
| [Expandable step](expandable-step.md) | Shared shape for collapsible transcript entries |
| [Command card](command-card.md) | One-row card (`┃` bar + band) owning a command's input and output, with a pending→completed lifecycle |
| [Tool step](tool-step.md) | Expandable step for tool calls |
| [Thinking step](thinking-step.md) | Expandable step for reasoning text |
| [Step state machine](step-state.md) | The three orthogonal axes (Lifecycle × Disclosure × Interaction) and the accent/weight color channels |
| [Envoy view](envoy-view.md) | Inline envoy step + zoomed-in child stream + navigation bar + focus stack |
| [Activity bar](activity-bar.md) | Breathing-dot liveness anchor + live status label + elapsed; clickable to open the Activity modal |
| [Todo bar](todo-bar.md) | One-row task-list summary: `TODOS` tag · done/total progress · current item; click to open the Activity modal on the Todos tab |
| [Hint bar](hint-line.md) | Next-Enter action sentence + model/reasoning/context cluster |
| [Head row](status-bar.md) | Ambient session state at the top of every view: session identity + workspace (left) + mode flags (right) |
| [Modals](modals.md) | Models, Model editor, Sessions, Session, History, Question, Permission, Tool-step detail, Help, Toasts |
| [Render components](components.md) | Reusable view-layer components: modal pages, lists, scroll bodies, footers, toasts, notices, and option rows |
| [Visual language](visual-language.md) | The join ladder: how ` · `, whitespace, and ` › ` encode relationship strength between adjacent tokens |

## Other reference

- [Architecture](architecture.md) — the engine / view / shell layers, the
  `TranscriptView` seam, and the component reuse tiers
- [Color palette](theme.md) — all `Theme` tokens with RGB values
- [Transcript spacing](transcript-spacing.md) — spacing ownership rules for transcript layouts and components
- [Key measurements](layout.md#key-measurements) — indents, margins, scroll steps
- [Panel padding](half-block-chars.md) — why the top/bottom edges use full panel-bg rows, not `╻╹▀▄┃` glyphs

## Source files

See [architecture.md](architecture.md) for how these three groups depend on each
other. View and shell files both live under `apps/tui/crates/mutx/src/` since
ADR-0079; paths below are relative to that directory.

### View layer — `apps/tui/crates/mutx/src/` (view modules)

| File | Responsibility |
|------|---------------|
| `view.rs` | Draw orchestration: `draw_transcript`, `TranscriptView`, `TranscriptRender`, `transcript_band_rect`, `TRANSCRIPT_H_INSET` |
| `design.rs` | Non-color design tokens: spacing, gutters, fixed row counts, text measurement limits |
| `theme.rs` | `Theme` (all color tokens) |
| `primitives.rs` | `viewport_rect`, `centered_rect`, `panel_block`, `recess_backdrop`, `modal_area`, color helpers |
| `components/` | Reusable composed render components: modal pages, selectable lists, scroll bodies, footer hints, toasts, transcript notices, question option rows, and one-line metadata strips (`MetaStrip`) |
| `text_layout.rs` | `wrap_text`, `WrappedLine`, `line_spans`, `code_gutter_line` |
| `message_body.rs` | `draw_message_body` (markdown text, user panels, code blocks) |
| `disclosure/mod.rs` | Disclosure module: draw orchestration, shared header rendering, sticky-pin tracking |
| `disclosure/renderers.rs` | Tool-step, thinking (`draw_reasoning_trace`), and envoy step renderers |
| `disclosure/state.rs` | Step state machine: `Disclosure`, `Interaction`, summary color/weight computation |
| `layout/` | Transcript arrangement strategies: `turn_band` (selected by `[tui] transcript_layout`) |
| `tools/` | Per-tool-step presenters (`bash`, `edit`, `read`, `search`, `web`, `ask_user`, `read_image`, `diff`, `meta`, `fallback`) |
| `composer.rs` | `draw_composer` (live input box), `INPUT_MSG_IDX` |
| `chrome.rs` | `draw_activity_bar` (breathing dot + status + elapsed), `draw_todo_bar` (task-list summary), `draw_queue_bar` (outbox summary), `draw_hint_bar` / `HintBarView`, `draw_completion_menu` |
| `page_header.rs` | `draw_page_header` / `PageHeader` / `SessionHead` / `PageHints::has_content` — the unified head band at the top of every view (demand-driven row 2) |
| `overlays/` | Modal subsystem (dir): one renderer per modal — `permission`, `provider`, `history`, `help`, `session`, `permissions_manager`, `activity`, `config`, `config_theme`, `config_theme_custom`, `mcp`, `skills`, `tools`, `token_report`, `toast` — backed by shared render components where possible |
| `empty_state.rs` | Empty-transcript placeholder view: logo hero, rotating help carousel (`carousel_pages`), `parse_logo` |
| `notice.rs` | Transcript notice entry point; delegates glyph/color/wrapping to `components/notice.rs` |
| `markdown_table.rs` | `build_table_render`, `shrink_column_widths` |
| `model/document.rs` | Document model: `TranscriptMessage`, `Block` enum, `MessageKind`, markdown parsing, `parse_arguments_kv` |
| `model/layout.rs` | `LayoutMap`, `BlockRegion`, `SemanticCursor`, hit-testing |
| `model/selection.rs` | `SelectionState`, `get_selected_text`, character-boundary snapping |
| `fuzzy.rs` | Fuzzy matcher for history / provider search |
| `providers.rs` | Provider/model picker ranking (`RankedProvider`, `RankedModel`, …) |
| `modal.rs` | Shared discriminants: `Modal`, `Recess`, `ActivityTab` |
| `completion.rs` | Completion-menu data types: `Completion`, `CompletionKind` (matching logic stays in the shell) |

### App shell — `apps/tui/crates/mutx/src/` (shell modules)

| File | Responsibility |
|------|---------------|
| `mod.rs` | Entry point `run_tui`; declares the merged view and shell modules |
| `app.rs` | Application state: `App`, `CaretOwner`, scroll/zoom snapshots |
| `event_loop.rs` | App loop: state sync, draw orchestration, action handling, `extract_selection_text` |
| `input/` | Event-to-action mapping (dir): `mod.rs` keyboard/mouse dispatch, `InputAction` enum, `tests.rs` |
| `terminal.rs` | Terminal lifecycle: raw-mode/alt-screen setup-teardown, render-loop wiring |
| `step_interaction.rs` | Transcript-step focus, toggle, and keyboard interaction |
| `clipboard.rs` / `clipboard_ops.rs` | OSC52 + system clipboard integration; async copy/spawned-ops |
| `completion.rs` | Slash-command / `@path` completion **logic** (`impl App`); reuses the view layer's data types |
| `question_model.rs` | Question-modal state machine |
