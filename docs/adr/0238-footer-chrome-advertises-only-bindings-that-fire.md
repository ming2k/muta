# 0238. Footer chrome advertises only bindings that fire

- **Status:** Accepted
- **Date:** 2026-09-11
- **Scope:** `mutx` (footer bars: `chrome/queue_bar.rs`, `chrome/model_bar.rs`,
  `view_header.rs`; the chord registry `keymap.rs`; the dispatcher
  `input/router.rs`)
- **Supersedes:** ADR-0126 §3 (the non-destructive **queue pointer**, whose
  `↑`/`↓` edge-hand-off ADR-0174 already gave to inline history recall — §1's
  `Ctrl+Q` expand chord is *restored* here, not reversed)
- **Related:** ADR-0205 (chrome never advertises what it cannot honour)

## Context

A sweep of the footer chrome against the code that drives it turned up four
places where a surface and its own truth had parted company. They shared one
shape: **something on screen promised an affordance that no code honoured.**

1. **The queue bar advertised a chord that resolved to nothing.** Its legend
   rendered `Ctrl-q expand` from a *literal* (`Key::CTRL_Q`), while the command
   registry — the single source of truth for chords since ADR-0172 — declared
   `OpenQueue` with `bindings: &[]`, and the dispatcher had no `Ctrl+Q` arm at
   all. The chord was reachable from nowhere: not the bar's keycap, not the
   palette, not config. (The prose above the bar had already been corrected to
   say so — a doc fixing a broken promise is a symptom, not a fix.)
2. **`GlobalOverrides::effective_binding` returned a placeholder.** For a
   command with no canonical chord it returned `Key::ESC`, so a bar asking "what
   should I draw?" was handed a chord that would fire something else entirely.
3. **A whole aside-view legend was dead code.** `draw_view_header_hints`
   short-circuits on a breadcrumb, and the aside page is *identified* by its
   crumb (`Main › Aside`, set by `event_loop::render` whenever it sets
   `ViewKind::Btw`) — so the hand-written `ViewKind::Btw` legend (`Esc back`,
   `F5 asides`, `Ctrl+C interrupt`) could never render. Its unit test pinned
   that unreachable state, which is worse than no test: it certified the legend
   as covered while the reachable row showed only `Esc back`.
4. **A dead dispatcher action.** `InputAction::RecallQueued` (pop the newest
   outbox item into the composer) had a handler and an enum variant but no
   producer — ADR-0174 had already handed `↑`/`↓` to history recall, leaving the
   outbox with only the panel's explicit `Enter`
   (`RecallQueuedSelected`/`recall_queued_at`, both live). Three reference docs
   still described the removed pointer as the primary `↑` behaviour.

The `has_content()` doc-comment contradicted its own first branch (the
`Subagent => false` arm claimed "never", while the breadcrumb check above it
returned `true` for exactly that page), which is what let (3) hide.

## Decision

**A keycap is a promise. Chrome renders the chord the dispatcher will actually
resolve, or nothing.**

1. **Keycaps come from the registry.** `GlobalOverrides::effective_binding`
   returns `Option<Key>` — the user's override, else the canonical chord, else
   `None`. The placeholder fallback is deleted; a command with no chord has no
   keycap. `draw_queue_bar` takes the resolved chord on its props
   (`QueueBarProps::expand_key`) resolved by the shell, exactly as the model bar
   already asked the registry for its two keycaps.
2. **The advertised chord is made real.** `CommandId::OpenQueue` gains its
   canonical `Ctrl+Q` (ADR-0126 §1's chord), a `canonical_global_key` arm, and a
   top-level-only dispatcher arm (`dispatch.overlay.is_none()`, matching the
   telemetry/connection chords: the bar is session chrome and is not on screen
   behind a modal).
3. **The invariant is tested, not asserted in prose.** A registry test walks
   every `Global` command and fails unless each declared binding resolves back
   to that command *and* the display path agrees with the dispatch path; the
   queue bar is tested in both directions (a remap shows through; no binding
   renders no keycap).
4. **Dead code is deleted, not documented.** `InputAction::RecallQueued` and its
   handler are gone; the unreachable aside legend is gone, replaced by a
   `debug_assert!` and a test pinning the reachable contract (crumb + `Esc
   back`); `has_content()` states the truth, and the doc-comments that
   contradicted it are corrected.
5. **The removed pointer is recorded, not just deleted.** ADR-0126 §3's
   `↑`/`↓` queue walk is hereby superseded by ADR-0174's arrow edge hand-off;
   the outbox is recalled by an explicit panel action. The three reference docs
   that still sold the pointer are corrected to the live model.

## Alternatives considered

- **Delete the keycap instead of binding the chord** (`bindings: &[]` stays).
  Rejected: the bar's click target is its only affordance, so removing the
  keycap leaves the panel reachable only by `/queue` or the palette while the
  legend still claims an expand action. ADR-0126 chose the Ctrl row for these
  gestures deliberately, and ADR-0156 already established that raw mode clears
  `IXON`, so `Ctrl+Q` reaches the app on both the direct and multiplexer paths.
- **Keep the literal in the renderer and bind the chord in the registry.**
  Rejected: two sources for one fact is exactly how this drifted. A future
  remap (or a retired chord) must not need a renderer edit to stay honest.
- **Keep `effective_binding`'s `Key::ESC` fallback.** Rejected: it is the
  same defect wearing a default. `ESC` on a footer bar reads as "press Esc",
  which is a *different* real action.
- **Keep the dead aside legend as harmless documentation.** Rejected: it was
  not harmless — it was pinned by a passing test, so it advertised coverage of
  a row that renders something else. Dead branches that look live are how the
  `has_content` contradiction survived.
- **Keep `InputAction::RecallQueued` for a future re-wire.** Rejected: the
  gesture it encoded (destructive newest-pop) contradicts the panel's
  non-destructive selected-item recall; a resurrected gesture should come with
  its own decision, not a spare variant.
- **Write the fixes without an ADR** (treat them as four independent bugs).
  Rejected: the durable part is the *rule* — chrome may only advertise what
  fires — plus an explicit supersession of ADR-0126 §3, which is an Accepted
  record that the code no longer implements.

## Invariants & Behavioral Boundaries

- **INV-1 — Advertised bindings resolve.** Every chord a `Global` command
  declares in the registry resolves back to that command, and the chord chrome
  draws for it is the same chord (enforced by
  `keymap::tests::every_advertised_global_binding_resolves_to_its_command`).
- **INV-2 — No placeholder chords.** `GlobalOverrides::effective_binding`
  returns `None` for a chordless command; chrome must render no keycap. No
  surface may substitute a default chord.
- **INV-3 — Renderers do not hardcode chords.** A bar obtains its keycap from
  the registry (via props), so a remap or a retirement needs no renderer change.
- **INV-4 — Breadcrumb-identified pages carry their crumb.** The aside and
  subagent pages always arrive with `breadcrumbs: Some(..)`; a crumb-less hint
  set for those kinds is a caller bug (`debug_assert!`) and renders nothing.
- **INV-5 — The composer's arrows address draft and history only.** The outbox
  is recalled by an explicit panel action; `↑`/`↓` never consume, reorder or
  rewrite queued work.

## Consequences

**Positive.**

- The footer cannot advertise an affordance that does not fire, in the specific
  cases found *or* the class behind them (registry ↔ display agreement is a
  test, not a review habit).
- `Ctrl+Q` opens the queue panel from the chat surface, restoring ADR-0126's
  Ctrl row for the one gesture that survived it.
- A user remap of the expand chord now shows through on the bar; an unbind
  removes the keycap rather than showing a stale one.
- The aside page's hint row has a test that pins what it actually renders; the
  `has_content` contradiction is gone.
- Three reference docs and two explanation docs stop describing a removed
  pointer as live behaviour.

**Negative / neutral.**

- `QueueBarProps` gains a field and `effective_binding` changes signature —
  a small ripple through the renderer call sites and their test fixtures.
- A malformed (crumb-less) aside/subagent hint set now trips a debug assertion
  instead of silently rendering nothing; release builds render nothing, as
  before.
- `App::recall_queued` (newest-pop) keeps its tests but has no production
  caller; it is public crate API whose removal would delete the coverage of the
  outbox bookkeeping it exercises, so it is left in place deliberately.

## Verification

- `keymap::tests::every_advertised_global_binding_resolves_to_its_command` —
  the registry/display/dispatch agreement, for every `Global` command.
- `chrome::tests::queue_bar_legend_renders_the_resolved_chord_and_nothing_when_unbound`
  — a remap shows through, `None` renders no keycap, the canonical chord is
  `Ctrl-q expand`.
- `input::tests::modals::ctrl_q_opens_the_queue_panel_at_the_top_level_only` —
  the advertised chord dispatches, and does not steal the chord behind a modal.
- `view_header::tests::aside_page_legend_is_its_breadcrumb_plus_esc_back` and
  `crumb_less_aside_hints_render_nothing` — the reachable aside row, and the
  malformed-set behaviour.
- `cargo nextest run -p mutx` green; `cargo clippy -p mutx --all-targets` clean.

## References

- [ADR-0126](0126-queue-affordances-ctrl-row-transcript-inserts-queue-pointer.md)
  — the Ctrl row this restores and the queue pointer it supersedes (§3).
- [ADR-0174](0174-arrow-edge-hand-off-transcript-browse-focus-and-channel-separated-interaction-color.md)
  — the arrow edge hand-off that displaced the pointer.
- [ADR-0172](0172-per-surface-keybinding-schemes-and-view-local-keyboard-ownership.md)
  — the registry as the single chord origin (INV-3's precedent).
- [ADR-0192](0192-inline-history-recall-pointer-badge-and-escapable-recall-state.md)
  — the recall badge the composer's arrows now serve.
- [ADR-0205](0205-unified-tui-surface-architecture-and-spatial-modality-taxonomy.md)
  — "chrome never advertises what it cannot honour", the principle this ADR
  encodes as a test.
- [ADR-0156](0156-ctrl-s-performance-report-bounded-xon-xoff-exposure.md) — the
  `IXON` analysis that makes a `Ctrl+Q` binding deliverable.
