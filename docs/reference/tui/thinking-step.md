# Thinking step

An [expandable step](expandable-step.md) for model reasoning / chain-of-thought
text. It renders flat on the app background — no band, like a
[tool step](tool-step.md) — so reasoning reads as quiet prose rather than a
panel.

## Collapsed

```text
  + Thinking · 148 tokens
  + Thinking · 140 tokens · 1.2s
```

First line: while the trace streams (no duration yet). Second line:
after the trace finishes.

| Attribute | Value |
|-----------|-------|
| Background | `app_bg` (flat — no band), inset 2 cols (`TRANSCRIPT_H_INSET`) |
| Marker | `+` (collapsed) / `-` (expanded), BOLD — same disclosure marker as a tool step; the streaming state is conveyed by the summary text (duration omitted while streaming) and the steady `info` hue, never by the marker |
| Header text column | 4 from transcript edge (after the `+ ` prefix) |

The summary color is the pure weight channel from the
[state machine](step-state.md) — reasoning never carries a text accent, so
the lifecycle is conveyed by the summary text (duration appears once the
trace finishes) and the steady `info` hue. The marker is always `+`/`-`;
with the activity bar as the single breathing anchor
([ADR-0008](../../adr/0008-single-breathing-anchor.md)), nothing about the
marker needs to change between streaming and finished.

## Header format

| State | Format |
|-------|--------|
| Streaming | `Thinking · {tokens} tokens` (duration omitted) |
| Completed | `Thinking · {tokens} tokens · {duration}` |

While the trace streams the line counts tokens up as they arrive — the number
climbs like a filling meter rather than reading as an estimate. Past 100 tokens
the streamed count is floored to a multiple of 25 instead of reported exactly:
the streaming summary repaints on every render heartbeat, and a per-token count
would dirty the row for nearly every delta. The floor keeps label changes
O(n ÷ 25) over a trace while the number still grows monotonically. A finished
trace reports the exact count and appends the duration.

## Expanded

```text
  - Thinking · 140 chars · 1.2s
    reasoning text in text_muted...
```

The first reasoning line sits directly below the header. Consecutive text
blocks are blank-separated; paragraph breaks inside a single block are already
preserved as empty rows by `wrap_text`.

| Attribute | Value |
|-----------|-------|
| Background | `app_bg` (flat) |
| Body indent | `TRANSCRIPT_BODY_PREFIX_COLS` (transcript column 4, left-aligned with the header text) |
| Header-to-body gap | 0 rows (`REASONING_TRACE_BODY_TOP_GAP_ROWS`) |
| Block gap | 1 row (`REASONING_TRACE_BLOCK_GAP_ROWS`) |
| Body color | `text_muted` |
| Body style | Plain wrapped text (no code gutter) |

## Interaction

See [expandable step](expandable-step.md#behavior) for the shared toggle,
sticky-pin, and narrow-fallback behavior.

Thinking steps participate in the same keyboard focus order as tool steps.
Use `Alt+↑` / `Alt+↓` to select a step, then `Alt+↑` / `Alt+↓` to walk
steps. `Enter` / `Space` opens or closes the focused thinking step.

## Source

`draw_reasoning_trace` (and `draw_reasoning_trace_header`) in
`apps/tui/crates/mutx/src/disclosure/renderers.rs`. Header data from
`thinking_header()` in `document.rs`.
