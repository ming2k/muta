# 0109. Command rows are cards; the disclosure glyph is `▸`/`▾`

- **Status:** Accepted
- **Date:** 2026-08-18
- **Revises:** [ADR-0108](0108-one-command-component-input-output-lifecycle.md)
  §1 (the row's *identity* becomes a card; the component model, lifecycle,
  and projection are unchanged), and the marker decision of
  [ADR-0106](0106-command-row-interaction-and-projection.md) §1

## Context

ADR-0108 made a command one component with a lifecycle, but the component
still rendered *flat on the page background*: a muted bold invocation led by
`⌘`/`❯`, padded with `Style::default()`. Two user-reported defects followed:

1. **混淆 (blending).** The transcript already has two flat, text-like
   component families — assistant prose and tool/reasoning steps — that are
   deliberately un-carded (`design.rs`: "a blank row would be a panel/card
   affordance"). Dropping commands into the same flat treatment means a
   command row differs from a sentence only by a leading glyph and tone.
   At a glance — especially scrolled, or in a busy transcript — a
   `/schedule` row reads as just another line of text.
2. **`+` 概念混淆 (marker collision).** The disclosure marker `+`/`-` was
   already overloaded: diff rows carry bold `+`/`-` sign columns on colored
   bands, and edit summaries carry `+1 -1` counts. An expanded edit step
   therefore begins with `- Edit a.rs +1 -1` — *four* sign characters, three
   of them diff semantics, one of them disclosure. The same glyph denoting
   two unrelated concepts is exactly the kind of ambiguity a visual grammar
   exists to prevent.

The card grammar was already in the codebase, spoken by three other
component families: the user-message panel (`user_panel_bg` band + `▌`
gutter rail), the code block (`code_bg` band + `┃` accent bar), and the
notice card (severity band, full-width). Commands were the only *object*
in the transcript without it.

## Decision

### 1. The command component renders as a card

One row, full-width band, thick `┃` identity bar in the family tone
(`info` for slash `⌘`, `ok` for shell `❯`):

```text
┃ ▾ ⌘ /permissions · 21:39          ← collapsed Disclose (band + bar)
┃   ⌘ /new · 21:39 · Started new session: a1b2c3   ← Inline
```

- **Band:** `Theme::command_surface` (new token, derived per scheme: one
  step above the page), lifting to `command_surface_hover` under
  pointer/focus — the same idle→hover ladder the notice card uses, so "an
  interactive card lights up" reads identically everywhere. A theme test
  pins both bands a visible margin off the page background in *every*
  scheme.
- **Bar:** `┃` in the family tone, BOLD — the card's identity, matching
  the code block's accent bar. Color alone was not enough; the *shape*
  separates a command from prose before color is even read.
- **Fixed marker slot:** the `▸`/`▾` column is *reserved* even when empty
  (Pending rows render blanks). Settling a reply never shifts the row
  horizontally — the optimistic row and its completed form occupy the same
  columns (ADR-0108's "settles in place", extended to the x-axis).
- The classifier (`command_row_layout`) now budgets the card chrome
  (`COMMAND_ROW_CHROME_COLS`) and the trailing `· HH:MM` before deciding
  Inline vs Disclose, so a reply that joins inline genuinely fits inside
  the card.

The card is one row tall. Commands are operations, not documents; a
multi-row card would over-state their weight and re-create the panel
noise `design.rs` deliberately removed from tool steps.

### 2. The disclosure glyph is `▸`/`▾`; `+`/`-` is reserved for diffs

Every disclosure site — tool steps, reasoning traces, provider retries,
command cards, sticky pins — migrates to the triangle pair:

```text
▸ Read a.rs · 12ms        ▾ Edit a.rs +1 -1
                          @@ -1 +1 @@
                          - let x = 1;
                          + let x = 2;
```

`+`/`-` now means exactly one thing: a diff sign (or a diff count in a
summary). The triangle is directional (collapsed → expanded), unclaimed by
any other transcript glyph (surveyed: `▌` rails, `┃` bars, `↳` tree
nesting, `◆` turn anchors, `·` attribute joins, `›` breadcrumbs, `⋯`
folds), and matches the web panel's existing `▸`/`▾` chevrons — the two
front ends now share one disclosure vocabulary.

The truthfulness rule (ADR-0106) is unchanged and re-stated for the new
glyph: the marker appears only when a body exists to disclose; Pending and
Cancelled rows show an empty slot.

### 3. Tool steps and reasoning stay flat

The card is *not* promoted to tool steps or reasoning traces. Those are
log-like content the model produced continuously; their grouping is
carried by indent (deliberate, documented in `design.rs`), and carding
every step would bury the page in bands. Commands are different in kind:
discrete, user-initiated, control-plane operations that interleave with
conversation. Carding exactly one family — commands — makes the card
itself the signal: *this object is an operation, not a message.*

## Alternatives considered

- **Restyle only (louder glyph/tone, stay flat).** Rejected: tone was
  already distinct and the row still read as text. The defect is
  *categorical* (operation vs conversation), and only shape carries
  categories at a glance.
- **Card tool steps too (uniform).** Rejected: destroys the deliberate
  flat log language, visually shouts every tool call, and removes the
  contrast that makes command cards meaningful.
- **Different disclosure glyphs per family.** Rejected: one affordance,
  one glyph. A per-family marker set re-introduces the `⚙`-style noise
  ADR-0106 retired.
- **`>`/`v` or `chevron-right`/`chevron-down` ASCII.** Rejected: `>` is
  the composer prompt and `v` reads as text; the transcript already uses
  box-drawing glyphs (`┃`, `▌`, `◆`), so the triangle fits the existing
  register.

## Consequences

**Positive.**

- A command row is identifiable by shape alone — no color vision or tone
  reading required — in every scheme (theme test enforces the band step).
- `+`/`-` is unambiguous: it always means diff content.
- TUI and web panel share one disclosure glyph vocabulary.
- Settling a pending command never shifts the row's columns.

**Negative.**

- Every disclosure site and ~16 insta snapshots churn (one-time).
- The card costs 4 fixed columns (bar + marker slot + glyph) of reply
  width; the classifier budgets them, so rows that used to join inline at
  exactly-full width now disclose instead — slightly more disclosure in
  narrow terminals, in exchange for never truncating a joined reply.
- Users who learned `+`/`-` re-learn one glyph pair.

**Neutral.**

- `COMMAND_CARD_LEAD_COLS` joins the design tokens; the marker slot's
  fixed reservation is the same trick tool steps use for stability.

## Verification points

- `┃ ▾ ⌘ /permissions` (collapsed: `┃ ▸ ⌘ …`) renders with the bar on the
  band; the band brightens on hover/focus.
- A pending row renders `┃   ⌘ /autopilot on` — empty marker slot, no
  marker — and settling the reply keeps every prior column in place.
- `▾ Edit a.rs +1 -1` expands to a body whose `+`/`-` signs are diff
  semantics; the summary's counts never collide with the marker.
- `command_card_bands_stay_visible_in_every_scheme` passes for all six
  schemes.
- Tool steps and reasoning traces render identically to before except the
  lead glyph.

## References

- [ADR-0108](0108-one-command-component-input-output-lifecycle.md) — the
  one-component lifecycle this card is the surface of.
- [ADR-0106](0106-command-row-interaction-and-projection.md) — the
  shape-driven layouts and truthfulness rule, retained verbatim.
- `docs/reference/tui/visual-language.md` — the join ladder the card's
  `·` meta still follows.
