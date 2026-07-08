# 0049. `MetaStrip` render component for transcript metadata headers

- **Status:** Accepted
- **Date:** 2026-07-09

## Context

Two unrelated renderers each paint a one-line "metadata header" row with the
same two-tone shape — an info-tone bold anchor followed by muted ` · `-joined
details — but neither shared the code:

| Header | Where | Form |
|--------|-------|------|
| Assistant round header | `render/layout/layout_default.rs::draw_round_header` | `◆ round N · model · HH:MM` |
| Sent user-message header | `render/message_body.rs` (`sent_header_anchor` / `sent_header_meta`) | `turn N · HH:MM` (or `⏸ Queued`) |

Both hand-built a `Vec<Span>` with an info-tone-bold `Style` for the anchor and
a muted `Style` for every trailing segment, interleaving ` · ` separators by
hand. The two copies had already drifted in small ways: the round header used
`format!(" · {}", name)` per segment; the user header pre-joined anchor + meta
and measured widths with `.width()`. Adding a third such header (e.g. a
tool-call-count or token-cost detail) would copy the pattern a third time.

A second, separate concern surfaced during the refactor: the round header had
no gutter affordance at all, while the user-message header was indented by
`USER_MESSAGE_OUTER_GUTTER_COLS + USER_MESSAGE_TEXT_GAP_COLS` of plain spaces.
Because a user **turn** is the larger, user-perceived scope (one submitted
message → one reply, per ADR-0047), it deserves a *stronger* visual anchor
than the in-round tool band — but the old all-spaces indent gave it none.

## Decision

Extract a single reusable component, `MetaStrip`, into
`render/components/meta_strip.rs`, and route both headers through it.

### The component

`MetaStrip` is a one-row rail assembled from replaceable `MetaChip`s:

```rust
MetaStrip::new()
    .left_pad(cols)            // optional plain left padding
    .lead("◆ ", MetaTone::Accent)   // a leading icon/prefix chip
    .anchor(format!("round {}", n)) // the info-tone bold anchor
    .detail(model)             // muted " · detail" chips
    .detail(time)
    .fill_tail(theme.surface())     // optional bg tail fill
    .render(frame, rect, theme)
```

- **`MetaChip`** — one piece of metadata: text + `MetaTone` + a `separated`
  flag (whether a leading ` · ` is inserted before it).
- **`MetaTone`** — the semantic visual tone: `Accent` (info + bold),
  `Muted` (grey), `WarningItalic` (warn + italic). It maps to a `Style` via
  the `Theme`, so the palette stays the single owner of color.
- **`MetaStrip`** — owns chip order, left padding, the ` · ` separator, and an
  optional tail background fill. Its `render` paints one row; empty chips are
  dropped so detail-only strips degrade cleanly (`Sent`, not ` · Sent`).

The separator is emitted only when a previous visible chip exists and the
current chip is marked `separated`, which is what lets the same `.detail()`
call produce `turn N · HH:MM` (with separator) or a bare fallback label
(without).

### Both headers now compose a strip

- `draw_round_header` builds `lead("◆ ", Accent) · anchor("round N") ·
  detail(model) · detail(time)`.
- The sent user-message header builds `lead(turn_gutter, Accent) ·
  anchor("turn N") · detail(time)`, or for a queued message `lead(turn_gutter,
  Accent) · status("⏸ Queued", WarningItalic)`.

### Turn gutter rail

The user-message header gains a `▌` (left-half block) glyph as a lead chip,
drawn in the accent tone, occupying the first column of the old
`USER_MESSAGE_TEXT_GAP_COLS`. The remaining gap columns become spaces, so the
`turn N` text stays aligned with the message body below. This gives the larger
user-visible scope a deliberate visual anchor the round header does not have,
without changing any column width.

## Alternatives considered

- **Leave the two copies duplicated.** Rejected: the two-tone "anchor ·
  detail" treatment is now a shared design language across the transcript, and
  a third caller was plausible. ADR-0045 already established
  `render/components/` as the home for exactly this kind of shared chrome.

- **Generalize into a full `Badge` / pill system** (rounded boxes, per-chip
  backgrounds, icons as first-class types). Rejected as over-engineering: the
  current callers need one row of inline spans with two tones and a ` · `
  separator, nothing more. `MetaTone` covers the styling; a richer chip model
  can grow later without changing the call sites.

- **Use a box-drawing bar (`┃`) for the turn gutter.** Rejected on visual
  review: it read as a panel border, not a guide. The filled left-half block
  `▌` reads as a weighty marker that distinguishes the turn without implying a
  bordered container.

- **Give the round header a matching gutter.** Rejected: the round is the
  *smaller*, in-ReAct-loop unit. Reserving the gutter marker for the larger
  user turn keeps the visual hierarchy — turn > round — legible at a glance.

## Consequences

**Positive.**

- One owner for the two-tone metadata treatment. A future header (token cost,
  tool-call count, pursuit iteration) is a `.detail()` call, not a new
  `Vec<Span>` builder.
- Palette discipline is centralized: `MetaTone::style` is the only place that
  maps tone → `Theme` token, so retuning the palette still happens in one file.
- The user turn now has a deliberate visual anchor (`▌`) that reflects its
  role as the larger scope, closing the hierarchy gap the round header had
  opened.

**Negative.**

- One more module in `render/components/` to keep in the component catalog and
  architecture summary. (Catalogued below.)
- The turn gutter glyph is a hardcoded `▌` in `message_body.rs` rather than a
  `design` token. Acceptable while it has a single caller; promote to
  `design.rs` if a second caller appears.

**Neutral.**

- No behavior, persistence, wire-protocol, or config change. `MetaStrip` and
  `MetaTone` are `pub(in crate::render)` — crate-internal, like every other
  component. The rendered output of both headers is byte-identical to before,
  except for the added `▌` glyph on the user-message header.
- The existing `queued_user_message_renders_badge_and_dimmer_bg` test and the
  full `neenee-tui-view` suite (198 tests) pass unchanged, confirming the
  refactor is behaviour-preserving.

## References

- [ADR-0045](0045-extract-neenee-tui-view.md) — established
  `render/components/` as the home for reusable composed render chrome; this
  component follows that seam.
- [ADR-0047](0047-round-contains-turn-vocabulary.md) — fixed the round/turn
  vocabulary so **round** is the user-perceived unit; the visual hierarchy
  here (turn gutter > round anchor) builds on that distinction.
- [TUI render components](../reference/tui/components.md) — the component
  catalog this decision is reflected in.
