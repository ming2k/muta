# Tool step

An [expandable step](expandable-step.md) for a tool call (`read_text`, `execute_command`,
`edit_text`, …). It renders flat on the app background — no band, no section
labels — like a [thinking step](thinking-step.md). The header alone summarizes
the call (tool + key arguments + duration); expanding reveals the tool-specific
content directly. Results are typed
[`ToolOutput`](../../adr/0001-tool-rendering-redesign.md) (Shell/Code/Listing/
Matches/Patch), so each tool renders from structured data instead of a sniffed
string.

## Collapsed

The default state. Header only — no preview, no body. The whole point of
collapsing is to keep noisy tool I/O out of the transcript until you ask for it.

```text
  + Read crates/main.rs · 0ms
```

| Attribute | Value |
|-----------|-------|
| Background | `app_bg` (flat — no band), inset 2 cols (`TRANSCRIPT_H_INSET`) |
| Marker | `+` (collapsed) / `-` (expanded), BOLD |
| Status indicator | Conveyed by header color only — no glyph. Resolved through the shared [step state machine](step-state.md): `Ok` falls through to the disclosure × interaction weight ladder; `Running` / `Failed` / `Denied` / `Cancelled` each supply a steady accent (`info` / `error_fg` / `warn` / `text_muted`) that wins outright |
| Header text | Human-readable description + duration, BOLD |

## Expanded

Flat on `app_bg`: the tool-specific content begins directly below the header,
indented 2 cols to align with the header text. There is no component-local
bottom row; the transcript layout resolves the boundary to the next component.
There are **no** `Tool` / `Arguments` / `Result` labels and no surrounding
`menu_bg` band. Only the content block carries a `code_bg` so it reads as a
distinct panel against the app background.

```text
  - Read crates/main.rs · 0ms
    1  fn main() {
    2      ...
```

### Content rendering (per tool)

Dispatch is by `result_kind`, so structured output gets a purpose-built
renderer instead of a generic code block. `execute_command` additionally prefixes its
block with a `$ command` line, so an expanded execute_command step reads like a terminal
session.

| Tool | Renderer | Notes |
|------|----------|-------|
| `execute_command` | `draw_execute_command_content` | A `$ command` prompt line, then the captured lines in **arrival order** — stdout and stderr interleaved exactly as the process wrote them, each coloured by source stream (stderr in `error_fg`) — then an `exit N` / `[output truncated]` footer, all one `code_bg` block. The `exit N` row is always painted when the code is known — `exit 0` is included, dimmed, so an expanded step closes with a diagnostic fact even on success. Carriage returns are collapsed (only the text after the last `\r` on a line survives). The ordered view comes from the structured `Shell::lines` field (available while streaming); legacy/restored payloads with only flat `stdout`/`stderr` fall back to the all-stdout-then-all-stderr bands. Command comes from the structured `Shell` payload, falling back to the parsed arguments. Long output is **middle-folded** (see [Long output folding](#long-output-folding)). |
| `find_files`, `list_dir` | `draw_listing_content` | One entry per row, no gutter, on `code_bg`. Directories (entries ending in `/`) in `info`, files in `code_fg`. |
| `search_text` | `draw_matches_content` | Matches grouped under a bold `heading_fg` file-path header; each match shown as `{lineno}  {content}` with the line-number column aligned and dimmed. |
| `edit_text`, `write_file` | `draw_diff_content` | A real `similar`-based unified diff: line-number gutter, `+`/`-` sign column, and intra-line word highlight on the changed spans, on `code_bg`. |
| `read_text`, others | `draw_code_content` | Code block with line-number gutter on `code_bg` (the fallback for unrecognized tools). |

Unknown / MCP tools (`arg_layout = KeyValue`) print their arguments as plain
`key: value` rows on `app_bg` before the result block, since the header only
carries the primary argument. The key names are self-describing, so no label is
needed; the result block's `code_bg` keeps the two visually distinct.

### Status colors

Status is conveyed by the header text color (there is no status glyph).
The full hue / luminance resolution is centralized in the
[step state machine](step-state.md); the tool-step-specific suffix on the
summary line is:

| State | Header suffix |
|-------|---------------|
| Completed | ` · 0ms` |
| Failed | ` · failed 0ms` |
| Running | (no suffix) |
| Cancelled | (no suffix) |

(The child-step accents and sticky-pin color use the raw
[`ToolStatus::color`](step-state.md#lifecycle-accent) palette directly. Per
[ADR-0008](../../adr/0008-single-breathing-anchor.md), the activity bar is the
single breathing anchor, so the parent summary carries a steady accent while
running — no luminance sweep.)

### Long output folding

An expanded `execute_command` step can emit hundreds of stdout/stderr lines, which would
bury the trailing "events" — the `exit N` line, the `[output truncated]`
marker, and the themed termination footer (timeout / blocked / cancelled) —
far below the fold. To keep those events visible, the structured `Shell`
output is **middle-folded** when it exceeds `BASH_FOLD_HEAD_ROWS +
BASH_FOLD_TAIL_ROWS + 1` logical lines (default 7):

- a head of the first 3 output lines (full, selectable),
- one dim `⋯ N lines hidden` summary row (not selectable),
- a tail of the last 3 output lines (full, selectable), then
- the `exit N` / `[output truncated]` / termination footers, always visible.

Short output (≤ 7 lines) renders verbatim, so folding only kicks in when it
actually saves a row. This is a pure rendering convenience: it does **not** add
a third disclosure state (the binary Collapsed/Expanded model is unchanged), so
tool-batch spacing and the `user_pinned` invariant are unaffected. Selection
still works on the visible head and tail — `byte_offset` advances past the
hidden middle so the tail rows anchor at their true source positions, exactly
as the unfolded path would; hidden rows are neither painted nor selectable.
Only the live structured `Shell` path folds; the legacy flat-`content` fallback
(used by restored sessions without a structured payload) renders verbatim,
since it inlines its markers (`Exit N`, `STDOUT:`, …) at arbitrary positions.

## Inline disclosure

Activating a focused tool step — `Enter`, a click on its summary, or a
right-click — toggles its inline disclosure, expanding the body in place to
show the full structured payload (not the transcript-truncated view). For
`Shell` the expanded body renders `$ command`, the captured lines in
**arrival order** (stdout and stderr interleaved as written, stderr in
`error_fg`), and the exit/truncation footer directly from the
`ToolOutput::Shell` fields. Envoy `envoy` steps navigate into the
child session on `Enter`/click instead of expanding. The bulk `Ctrl+T`
toggle expands or collapses every step at once. See
[ADR-0001](../../adr/0001-tool-rendering-redesign.md).

## Interaction

See [expandable step](expandable-step.md#behavior) for the shared toggle,
sticky-pin, and narrow-fallback behavior. Tool-step specifics:

- `Enter` on a focused tool step toggles its inline disclosure — the same
  effect as clicking its summary or right-clicking it.
- `↑` / `↓` while a step is focused includes visible tool steps in the keyboard focus order.

## Envoy children

Nested sub-task tool calls render as indented child steps inside the parent's
expanded body (6-space indent), flat on `app_bg`. Each child shows a compact
one-line header (the summary, colored by run state) with no marker glyph.

## Source

`draw_tool_step` in `apps/tui/crates/mutx/src/disclosure/renderers.rs`. Shared
header via `draw_expandable_step_header` (from
`apps/tui/crates/mutx/src/disclosure/mod.rs`). Expanded content dispatched by
`draw_tool_result` to `draw_listing_content`, `draw_matches_content`,
`draw_bash_content` (which renders the `$ command` line + the structured
`Shell` payload), `draw_diff_content`, or `draw_code_content`. The execute_command command
is resolved by `bash_command_for`. Presenters (summary / `result_kind` /
`arg_layout`) live in `apps/tui/crates/mutx/src/tools/`. The structured
payload comes from `ToolOutput`
([ADR-0001](../../adr/0001-tool-rendering-redesign.md)); header data from
`tool_step_header()` and `parse_arguments_kv()` in `document.rs`.
