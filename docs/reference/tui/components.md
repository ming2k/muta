# TUI render components

`apps/tui/crates/mutx/src/components/` contains reusable view-layer
components built above low-level drawing primitives and below per-feature
renderers. Components are pure render helpers: callers pass borrowed state,
theme tokens, body lines, scroll cursors, and selection indices; components
draw into a `Frame` and do not own application behavior.

## Layer Position

```text
feature renderers  overlays/* · notice.rs · tools/*
        │
components          modal · list · scroll · footer · toast · notice · options · meta_strip
        │
low-level helpers   primitives · text_layout · markdown_table
        │
tokens              theme · design
```

`components` is the preferred place for reusable composed UI. `primitives`
stays focused on geometry, panels, color helpers, and raw body rendering.
`theme` owns color tokens; `design` owns fixed spacing and measurement tokens.

## Component Files

| File | Public shape inside `render` | Reused by | Responsibility |
|------|------------------------------|-----------|----------------|
| `components/modal.rs` | `ModalPage`, `ModalHeader`, `ModalPageSize`, `draw_modal_page`, `modal_body_width` | Help, Config, Config appearance, Config layout, Tools, MCP | Complete centered modal shell: geometry, panel chrome, header, scrollable body, footer |
| `components/list.rs` | `SelectableListPage`, `draw_selectable_list_page`, `row_style` | Config, Tools, MCP | Selectable list modal composition and selected-row palette |
| `components/scroll.rs` | `ScrollBody` | Modal and list components | Scrollable body wrapper around `render_body`, including follow-row and edge-margin behavior |
| `components/selectable_body.rs` | `SelectableRow`, `RowSegment`, `render_selectable_body` | Help, Usage Statistics, Context Usage drill-in, Activity, Sessions info sub-view, History preview, Permission sheet body, OAuth sheet, every `?` keymap sub-page | The single selectable-document path for modal bodies: wraps in the application layer, paints decoration prefixes outside the copy text, and registers every visual row as a `MODAL_DOC` selection region (drag-select + copy). Any new documentary modal body uses this instead of hand-rolled region registration |
| `components/footer.rs` | `FooterHint`, `render_modal_footer`, `modal_footer_text` | Modal component, legacy primitive re-export, direct modal renderers | One-line modal command strip with width-aware degradation |
| `components/toast.rs` | `ToastBubble`, `ToastKind`, `draw_toast` | Toast overlay | Transient top-right notification bubble |
| `components/notice.rs` | `NoticeView`, `draw_notice_view` | Transcript notice renderer | Transcript notice card (header with expand/collapse micro-affordance, severity palette, wrapping, and expandable formatted detail payloads) |
| `components/meta_strip.rs` | `MetaStrip`, `MetaChip`, `MetaTone` | Turn header (`turn_band`), sent/queued user-message header (`message_body`) | One-row two-tone metadata strip (accent anchor · muted details, R1 attribute joins per the [join ladder](visual-language.md)), with optional left padding and background tail fill |
| `components/options.rs` | `QuestionOptionRow` | Permission/question overlay | Wrapped option rows for single- and multi-select question surfaces |

The symbols are crate-internal. They are intentionally exposed only inside the
view layer so the app shell cannot depend on component internals.

## Styling Ownership

| Styling concern | Owner |
|-----------------|-------|
| Palette and semantic color mapping | `render/theme.rs` |
| Fixed spacing, gutters, row counts, width constants | `render/design.rs` |
| Modal/footer/row/option composition styles | `components/` |
| Raw rect carving, panel fills, contrast, backdrop, body scrolling | `primitives.rs` |
| Text wrapping, code gutters, source-line spans | `text_layout.rs` |

Component styles should read from `Theme` and `design` tokens rather than
hard-coding colors or dimensions. If a style is shared by several composed
surfaces, it belongs in `components`; if it is a one-off for one feature, keep
it near that feature's renderer.

## Interaction Boundary

Components may encode visual interaction state such as selected, highlighted,
disabled, expanded, or followed-row presentation. They do not perform side
effects, mutate application state beyond caller-owned scroll cursors, or map
keyboard/mouse input to actions.

Interaction logic remains in the app shell or in shell-owned state machines:

| Behavior | Owner |
|----------|-------|
| Keyboard and mouse event dispatch | `apps/tui/crates/mutx/src/input/` |
| Modal open/close and action handling | `apps/tui/crates/mutx/src/event_loop.rs` |
| Question-modal state machine | `apps/tui/crates/mutx/src/question_model.rs` |
| Transcript-step focus and toggles | `apps/tui/crates/mutx/src/step_interaction.rs` |
| Hit-region storage and lookup | `LayoutMap` / `ModalHitMap`, owned by the shell and filled by renderers |

This keeps the view layer React-like in composition, but not React-like in
state ownership: components are reusable render functions, while event and
operation logic stay above the view-layer boundary.

## When to Add a Component

Add a new component when at least one condition is true:

- Two or more renderers repeat the same modal/list/row/footer/body pattern.
- A renderer mixes feature-specific data shaping with generic chrome or
  wrapping behavior.
- A style decision needs a single owner so future overlays stay consistent.
- A low-level primitive starts accumulating feature-specific presentation
  rules.

Keep code local to a feature when the style or behavior is unique to that
feature and is unlikely to be reused.

## Compatibility Re-exports

Some modal footer symbols are still re-exported through
`primitives.rs` while older overlay renderers are migrated. New code
should import modal footer helpers from `components::footer` directly.
