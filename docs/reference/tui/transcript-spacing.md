# Transcript spacing

Transcript spacing is intentionally split by responsibility. The goal is to keep
message groups, user panels, tool steps, reasoning traces, and markdown blocks
aligned without every renderer inventing its own `+ 2` or blank row.

## Rule of thumb

> Outer spacing belongs to the layout. Inner padding belongs to the component.
> Concrete numbers live in `render/design.rs`.

In practice:

| Layer | Owner | Do | Avoid |
|-------|-------|----|-------|
| Transcript horizontal gutter | `view.rs::transcript_band_rect` | Apply `TRANSCRIPT_H_INSET` once before the stream is rendered | Re-applying the same gutter inside message components |
| Inter-message / group spacing | `render/layout/*` through `Stream::gap` / `Stream::message_gap` | Add blank rows through the shared stream helpers so scroll height and clipping stay correct | Drawing ad-hoc blank rows in a component to separate it from the next message |
| Component interior | The component renderer, using `render/design.rs` tokens | Use named tokens such as `USER_MESSAGE_TEXT_GAP_COLS`, `TOOL_STEP_BODY_TOP_GAP_ROWS`, `REASONING_TRACE_BLOCK_GAP_ROWS` | Hard-coding `2`, `1`, `repeat(2)`, or local `h_inset` values for visual spacing |
| Shared one-line metadata | `components/meta_strip.rs` | Compose `round N · time` / `round N · model · time` with `MetaStrip` | Rebuilding separator and tone spacing by hand |

## Horizontal gutter contract

`draw_transcript` carves the visible transcript stream into an already-inset
`band` by calling `transcript_band_rect`. That is the single owner of the outer
left/right transcript gutter:

```text
terminal / viewport
└─ transcript band = area minus TRANSCRIPT_H_INSET on both sides
   └─ message body receives this band and only adds its own interior padding
```

A downstream renderer should trust the `Rect` it receives. If a body needs a
prose indent or a code-band inset, that is a separate interior measurement and
should use a named design token.

## Vertical spacing contract

Layout strategies own the space between transcript items:

- `Stream::message_gap()` inserts the standard `MESSAGE_GAP_ROWS` blank row.
- `Stream::gap(n)` is the escape hatch for non-standard layout chrome.
- These helpers update `content_lines`, `skip_rows`, and `current_y` together.
- The default layout resolves each component boundary once using the matrix
  below. Components never add an outer trailing row themselves.

| Relationship | Blank rows | Token / rule |
|--------------|------------|--------------|
| Turn header → first component | 1 | `TURN_HEADER_BODY_GAP_ROWS` |
| Thinking header → first expanded reasoning line | 0 | `REASONING_TRACE_BODY_TOP_GAP_ROWS` |
| Thinking ↔ tool batch / assistant text | 1 | `MESSAGE_GAP_ROWS` |
| Tool-like → tool-like in the same known turn | 0 | Same-turn batch rule |
| Tool-like → assistant text | 1 | `MESSAGE_GAP_ROWS` |
| Different turns, user messages, or notices | 1 | `MESSAGE_GAP_ROWS` |
| Adjacent collapsed legacy tools with no turn stamp | 0 | Compatibility fallback |

Do not add a component-local trailing blank row just to separate from the next
message. That usually double-counts spacing and can also make scroll accounting
wrong.

## Component-local spacing

Component renderers may add internal padding when it belongs to the component's
own shape:

- user message panel text padding: `USER_MESSAGE_TEXT_GAP_COLS`,
  `USER_MESSAGE_RIGHT_PAD_COLS`;
- reasoning body/block padding: `REASONING_TRACE_BODY_TOP_GAP_ROWS`,
  `REASONING_TRACE_BLOCK_GAP_ROWS`;
- tool-step body indent: `TOOL_STEP_BODY_INDENT_COLS`
  (`TOOL_STEP_BODY_TOP_GAP_ROWS` exists but is pinned to 0 — see the tool-step
  special case below);
- block-level code/math band geometry: `BLOCK_SURFACE_H_INSET`,
  `CODE_BAND_*`, `MATH_MARKER_GAP_COLS`.

If a new renderer needs spacing, add a descriptive token to `render/design.rs`
first. A named token explains intent; a bare `2usize` does not.

## Tool-step special case

Tool steps deliberately support a compact log shape:

- tool steps in the same known turn stack flush regardless of whether they
  are collapsed or expanded;
- old restored steps without a turn stamp retain the collapsed-adjacency
  fallback;
- an expanded body sits directly under its header — grouping is carried by the
  body indent (`TOOL_STEP_BODY_INDENT_COLS`) alone, with **no** top blank row
  (`TOOL_STEP_BODY_TOP_GAP_ROWS` is pinned to 0). A blank row would be a
  panel/card affordance that competes with the indent (the row says "two
  blocks", the indent says "one block's content"); the flat log shape wants the
  indent to own the grouping;
- there is no dedicated tool-step bottom-gap token. The layout resolves the
  following component boundary from the matrix above.

This prevents expanded steps from gaining double bottom spacing while keeping a
parallel tool batch visually tight.

## Tests

Spacing is a visual behavior, so it should be locked with snapshot or grid
assertions when changed. Existing examples include collapsed tool steps stacking
flush and expanded tool bodies padding themselves. Add similar coverage for new
spacing rules, especially when changing user panels, reasoning traces, turn
headers, or code/tool-result bands.
