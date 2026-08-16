# 0104. Demand-driven header legend and the empty-state help carousel

- **Status:** Accepted
- **Date:** 2026-08-16

## Context

Two chrome surfaces were saying things they did not need to say.

**1. The head band's row 2 was unconditional.** ADR-0103 §3 fixed the
two-row head: row 1 identity/status, row 2 the view-affordance legend. The
implementation reserved row 2 on every frame of every page and always
painted at least one pair — because a global `F1 help` pair was appended
unconditionally. In the common case (a main-view session with no asides,
agent idle) that row showed *only* `F1 help`, a global affordance that every
modal footer already carries (`? help` collapses in narrow footers) and that
the Help modal exists to enumerate. One terminal row of vertical space was
spent on a duplicate.

The duplication was worse on the pages with a second legend surface:

- **Envoy**: row 2 rendered `Esc back  F1 help` while the Envoy page's
  permanent three-row footer rendered `Esc back  [ prev  ] next  F1 help`
  on the *same screen*. Two copies of the same keycaps one screen apart.
  That footer copy was itself half global: `F1 help` is not an Envoy
  affordance any more than it is a row-2 one.
- **Main while a round runs**: row 2 offered `Esc interrupt` while the
  activity bar — also on screen — rendered `Esc Esc interrupt`. Not just a
  duplicate but a *contradiction*: interrupting actually requires the
  double-Esc arming (the second Esc inside the arm window), which the
  activity bar spells correctly and row 2 misstated.

**2. The empty state was a dead end.** ADR-0033 made the empty transcript a
centered logo hero; ADR-0057 specified contextual guidance variants
(`NeedsProvider`, `Onboarding`) — but the shell never wired any variant
(`guidance: Default::default()` at the render call site, both non-default
variants `#[allow(dead_code)]`). Every user, forever, saw one static
tagline. The surface where a new user *looks first* taught nothing.

## Decision

### 1. Row 2 is demand-driven

`PageHints::has_content()` decides whether row 2 exists at all; the layout
split reserves the row only when it returns `true` (`PAGE_HEADER_ROWS = 2`
remains the recorded ceiling, not a constant height):

- **Main**: only while at least one aside is live — the aside chip
  (`btw 2 · 1 running`) and `F5 asides` are exactly the affordances the row
  exists for (ADR-0103 §3). No aside, no row.
- **Btw**: always — `Ctrl-C back` is the view's single exit and exists on
  no other surface.
- **Envoy**: never — the page's permanent footer carries the legend; row 2
  would duplicate it.

Within the row, global affordances are gone: no `F1 help` pair anywhere
(modal footers own that discovery), and no `Esc interrupt` pair on the main
view (the activity bar's `Esc Esc interrupt` is the authoritative — and
correct — copy). `/btw` keeps `Esc interrupt aside` because its activity
bar may be describing the parent, not the aside. The same rule reaches the
one *other* persistent legend, the Envoy footer: it keeps only its own
navigation (`Esc back`, `[ prev`, `] next`) — the `F1 help` pair is gone
from every persistent surface, not just the head band. Help discovery
lives where the user is stuck: the mandatory `? help` chip on every modal
footer, the Help modal itself, and the empty-state tour's `F1`/`?` page.

### 2. The empty state carries a rotating help carousel

Beneath the logo, one help page at a time rotates every
`CAROUSEL_SLIDE_SECS = 8` s — a single line, no position indicator:

- The index derives from **wall-clock** elapsed time (`carousel_epoch`, the
  same pattern as `spinner_epoch`), so the cadence is independent of draw
  frequency; the loop's `animating` flag includes "empty state showing" so
  the idle heartbeat advances slides.
- No dot/position indicator: the rotating copy is self-explaining (each
  page carries its own affordance), and an indicator row would spend a
  second row of chrome restating "this line changes" — information the
  user cannot act on. The hero stays a minimal landing strip.
- **The static tagline is retired.** The carousel's first page already
  answers "how do I start" ("Send a message, or `/` command — try
  `/help`"), so a separate "Type a message below to begin." line beneath
  the logo restated page 0 verbatim in spirit. One hint slot, one hint.
- Copy teaches only **durable** capabilities (send/`/`, queue-on-Enter,
  `/btw`, `F1`/`?`, `Ctrl-R`, `Ctrl-M`/`/models`, `!` shell, `@` mentions),
  never transient state. Every page fits the minimum terminal width
  (40 cols) on one line, asserted by test, because the height accounting is
  wrap-independent by contract.
- The shell finally consumes ADR-0057's variants: no keyed provider ⇒ the
  carousel is replaced by the pinned `/connections` blocker (`NeedsProvider`)
  — a blocker does not rotate; a tour page would scroll it away. Otherwise
  the tour runs (`Tour`). The provider check reads `provider_picker` rows
  (a configured row with a ready key ⇒ keyed), so the blocker clears the
  moment the user actually fixes it; an *empty* snapshot counts as keyed
  ("not synced yet") so an already-configured user never sees a false
  blocker flash before the first listener sync.
- `EmptyStateGuidance::Onboarding` is folded away: its one-time
  slash-command nudge is subsumed by the tour pages, which every empty
  conversation now shows.

The carousel is built as a reusable slot (`carousel_pages()` returns the
page list; a future caller — a pause screen, a session-switch preview — can
host a different page set in the same chrome).

## Alternatives considered

- **Keep row 2 always, drop only the duplicates.** Rejected: without the
  global `F1` pair and the main-view interrupt pair, the idle main view's
  row 2 would render *nothing* — an always-reserved blank row is worse than
  a conditional row, and the geometry work is identical.
- **Move the interrupt hint into row 2 and drop the activity-bar copy.**
  Rejected: the activity bar sits where the user looks while waiting
  (directly above the composer, with the elapsed timer), and it already
  names the correct double-Esc gesture. Row 2 is identity chrome, not a
  waiting surface.
- **Static multi-line hints on the empty state** (list every capability at
  once). Rejected: a wall of keycaps is unreadable and competes with the
  logo hero; one rotating hint at a time respects the calm-landing-strip
  intent while still covering the full set over ~1 min.
- **Keyboard-driven tour** (arrows to page). Rejected for v1: the empty
  state is transient by nature (the first message replaces it), so the
  passive rotation plus the persistent `F1` reference surface is enough;
  interactivity can be added to the same slot later without layout changes.

## Consequences

- The idle main view and the Envoy page reclaim one terminal row for the
  transcript; the band grows back only when it has something to say.
- The Envoy footer's legend is purely navigational (`Esc back`, `[ prev`,
  `] next`); with the global pair gone, the drop ladder only ever sheds the
  sibling pair, and the exit pair survives at any width.
- `docs/reference/tui/status-bar.md` and `layout.md` describe the
  demand-driven band; the height table cites the ceiling, not a constant.
- The carousel adds one `Instant` (`App::carousel_epoch`) and keeps the
  loop at its 1 s idle heartbeat while an empty state is showing (the draw
  is a cheap fixed-size hero).
- Tests pin all three behaviors: `hints_presence_is_demand_driven_per_page_kind`,
  `main_view_without_asides_renders_a_single_row_head_band`,
  `envoy_view_omits_row2_entirely`, `carousel_rotates_on_a_wall_clock_cadence`,
  `carousel_pages_fit_the_minimum_terminal_width`,
  `tour_carousel_renders_only_the_current_page_line`,
  `empty_state_tour_renders_the_current_carousel_page` — and
  `envoy_footer_drops_affordances_as_the_row_narrows` now also asserts the
  legend never carries the global `F1` pair.

## References

- ADR-0033 (empty-state replacement), ADR-0057 (contextual guidance),
  ADR-0103 §3 (the two-row head this refines), ADR-0017/0103 (aside keys).
- Prior art: Claude Code's hint carousel / Codex CLI status hints —
  rotating one-line affordances under a hero, never a wall of keycaps.
