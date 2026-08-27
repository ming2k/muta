# Panel top/bottom padding

User messages pad their top and bottom edges with a **full row of solid
panel background** — no characters, no transition glyphs. (The input box no
longer pads: it is drawn as a rounded line frame — see
[input box](input-box.md) — whose border rows are the padding.)

```text
  ┃                             ← top: full panel-bg padding row
  ┃ text content                ← full height
  ┃                             ← bottom: full panel-bg padding row
```

## Why not half-block characters?

An earlier iteration used the Unicode half-block pair `▄` (U+2584, lower half)
and `▀` (U+2580, upper half) so the panel edge occupied only half a row:

| Character | Unicode | Name | Half filled |
|-----------|---------|------|-------------|
| `┃` | U+2503 | BOX DRAWINGS HEAVY VERTICAL | Full height |
| `╻` | U+257B | BOX DRAWINGS HEAVY DOWN | Bottom half only |
| `╹` | U+2579 | BOX DRAWINGS HEAVY UP | Top half only |
| `▀` | U+2580 | UPPER HALF BLOCK | Top half = fg color |
| `▄` | U+2584 | LOWER HALF BLOCK | Bottom half = fg color |

That produced a compact half-row inset, but it depends on the terminal font
rasterizing `▄`/`▀` exactly to the cell's half height. In practice glyph
hinting, line-height scaling, and font substitution make the seam land a pixel
or two off in some terminals, so the panel edge looked different depending on
where it ran.

A terminal cell can only carry **one** background color — there is no way to
paint the top half of a cell one color and the bottom half another without a
glyph. So the only background-only option is a full row. Trading the half-row
inset for a full padding row keeps the edge pixel-identical across every
terminal, which is the consistency the design optimizes for.
