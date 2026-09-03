# 0174. Arrow Edge Hand-off, Transcript Browse Focus, and Channel-Separated Interaction Color

- **Status:** Proposed
- **Date:** 2026-09-04
- **Builds on:** ADR-0173 (unbounded session keyboard — revises its arrow table and conditionally revives one rejected alternative), ADR-0172 (per-surface schemes), ADR-0170 (bounce-to-composer), ADR-0008 (step state machine channels)

## Context and Problem Statement

Three usability findings against the ADR-0173 session surface, each traceable to the same underlying shape — a single channel or decision carrying two meanings:

1. **↑/↓ at the draft's edges are inert.** ADR-0173's plane-less table restricts bare ↑/↓ to "completion walk → caret line motion"; at the first/last line of the draft they return `None` (the comment reads "history recall is Alt+P / Ctrl+R"). Every mainstream readline/chat surface (bash, zsh, Claude Code, Codex CLI) hands the edge to history recall. The discoverability cost lands on the most common gesture in the product: recalling the last prompt.
2. **Clicking blank transcript space never dims the composer.** "Composer inactive" is defined as exactly `focused_target.is_some()` — there is no third state for "attention is on the transcript, but no specific step". A click on a gap row or below the last message classifies as `ContentGap`/`Dead` and *clears* focus — semantically a bounce-to-composer. The user's mental model is "I clicked into the transcript, the transcript has my attention"; the model cannot express that, and the one state that could (a region-focus state) was explicitly rejected in ADR-0173's alternatives.
3. **The hover affordance loses every salience contest by construction.** The disclosure state machine encoded both "pointer/keyboard present" and "body open" on one luminance ladder (`muted < hover < fg`), with a monotonicity invariant pinning expanded at `fg`. The active state is therefore *structurally brighter* than the affordance — the cue that exists to attract the eye is the dimmer of the two by decree.

## Decision Drivers

- Muscle-memory compatibility with every readline-style surface users already know.
- Zero mode-indication tax (ADR-0173's core win) must be preserved.
- One visual channel should carry one meaning; two meanings on one channel force an arbitrary priority that some state always loses.

## Considered Options

- Option 1: Keep ADR-0173 verbatim (arrows inert at edges; blank clicks bounce to composer; single luminance ladder).
- Option 2: Full region-focus state revival (the rejected ADR-0173 alternative, with focus bar and hint rows).
- Option 3: Minimal channel separation — arrow edge hand-off in the existing scheme; a pointer-derived `transcript_focused` flag reusing the existing composer-dim indicator; interaction as a separate hue channel in the disclosure state machine.

## Decision Outcome

Chosen option: **Option 3**, because it resolves all three findings without reintroducing any of the machinery (focus bar, declared region enum, dual hint sets) whose cost motivated ADR-0173.

### 1. Arrow edge hand-off (revises ADR-0173's ↑/↓ row)

`resolve_up` / `resolve_down` in `src/session.rs` gain a terminal arm: completion walk first, then caret line motion, then — at the true first/last line — `HistoryPrev` / `HistoryNext`. The `history_index` cursor, draft stash, and attachment restore built for `Alt+P`/`Alt+N` are reused unchanged. `Alt+P`/`Alt+N`/`Ctrl+R` remain valid and equivalent. The one-chord-one-verb rule is not broken: ↑/↓ have one meaning — *completion → caret → history, in priority order* — which is precisely how readline surfaces define the arrow.

### 2. Transcript browse focus (conditionally revives the rejected region-focus alternative, at minimum weight)

`App` gains `transcript_focused: bool` — a pointer-derived transient, not a declared keyboard plane:

- Set by any click resolving to `StepSummary`, `Content`, or `ContentGap` (the whole transcript viewport is the hit surface: `transcript_content_rect` now spans the full area, gutters and below-content blank space included).
- Cleared by a composer click and by any keypress matching the existing composer-intent predicate (`event_rearms_composer_follow`) — the bounce-to-composer grammar (ADR-0170) is the exit, unchanged.
- Indicated solely by the already-existing indicator pair (composer dim + caret hide): `render.rs` composes `focused_target.is_some() || transcript_focused` into the single `step_focused` signal, and `caret_owner` treats it identically. No bar, no region enum, no extra hint row — the indication tax stays zero.

This differs from the rejected ADR-0173 alternative by having no keyboard grammar of its own (no arrows-pick-steps, no region-clearing scroll semantics) and no dedicated announcement surface: it is a *pointer-attention* bit feeding the existing dim state machine.

### 3. Channel-separated interaction color (amends ADR-0008's two-channel presentation)

The disclosure state machine's presentation becomes three channels composed in `summary_text_color`:

| Channel | Source | Token | Role |
|---|---|---|---|
| Luminance | Disclosure | expanded → `fg`, collapsed → `muted` | "is it open" |
| Lifecycle accent (hue) | Lifecycle (unchanged) | `ToolStatus::color` | "is it running/failed/denied" |
| **Affordance (hue, new)** | Interaction | `theme.affordance()` (derived: muted tinted toward the accent) | "is it interactive — hover/focus" |

Hover/focus no longer move the luminance rung at all; they tint toward the affordance hue (`INTERACTION_HOVER_BLEND = 0.65`), composed last so the cue reads identically over plain, accented, and open summaries. Expanded summaries carry no transient cue (the `-` marker, the body, and the sticky pin already announce them) — the old model's lesson was that re-decorating the active state only muddies the disclosure signal. The `text_hover` token and accessor are retained for other uses; the step scheme stops consulting them.

### Positive Consequences

- ↑/↓ recall works where every user's muscle memory expects it, with no new state.
- Clicking anywhere in the transcript — blank space included — now visibly parks attention on the transcript, matching the user's mental model; clicking the composer or typing visibly hands it back.
- A hover cue that is *hued* rather than *brighter* can never be structurally out-shone by the active state; luminance ordering becomes purely a disclosure fact.
- All three changes are additive at the seams (resolver arms, a composed boolean, a composed color) — no keyboard plane, no deleted indicator, no new hint row.

### Negative Consequences

- ↑/↓ no longer "mean one thing" in the strictest reading (they are a three-stage priority chain). Mitigation: the chain is the universal readline convention, and the stage order is fixed and testable.
- `transcript_focused` is a second writer of the composer-dim state (alongside `focused_target`). Mitigation: it is a single composed boolean at render; both writers are covered by the same test suite.
- One new theme token (`affordance_fg`) and one new blend constant to keep distinct across all schemes; guarded by the existing scheme-consistency tests extended to assert hue distinctness from `muted`/`fg`.
- Users who learned ADR-0173's "arrows never recall" will find the behavior changed; `Alt+P`/`Alt+N`/`Ctrl+R` remain, so nothing is lost, only added.

## Pros and Cons of the Options

### Option 1 — keep ADR-0173 verbatim

- Good, because zero implementation churn.
- Bad, because the ↑/↓ recall gap, the dead blank-click region, and the muted-by-design hover cue are all live usability findings, not hypotheticals.

### Option 2 — full region-focus revival

- Good, because a real region state enables future transcript-keyed grammar.
- Bad, because it re-pays the full indication tax (focus bar, third key-grammar row, region-clearing semantics on scroll) to solve what a pointer-derived bit solves, and it contradicts the modeless stance of `docs/explanation/tui.md`.

## Links

- ADR-0173 (the arrow table revised here; the region-focus alternative conditionally revived), ADR-0172 (the session scheme owning `resolve_up`/`resolve_down`), ADR-0170 (bounce-to-composer — the browse-focus exit), ADR-0008 (the step state machine whose presentation channels gain the affordance axis).
- `apps/tui/crates/mutx/src/session.rs` (arrow arms), `src/app/mod.rs` + `event_loop/mod.rs` + `event_loop/actions/mouse.rs` + `event_loop/render.rs` (browse focus), `src/view/mod.rs` (full-viewport content rect), `src/disclosure/state.rs` + `src/theme.rs` (affordance channel).
