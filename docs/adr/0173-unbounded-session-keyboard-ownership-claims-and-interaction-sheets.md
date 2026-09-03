# 0173. Unbounded Session Keyboard, Per-Surface Ownership Claims, and Interaction Sheets

- **Status:** Proposed
- **Date:** 2026-09-03
- **Builds on:** ADR-0172 (per-surface keybinding schemes), ADR-0170 (composer-first & bounce-to-composer), ADR-0141 (view means fullscreen, modal means modal), ADR-0139 (surface router & lifecycle)
- **Amends:** ADR-0172's Session chat scheme (the dual focus-plane grammar is replaced by a plane-less one); ADR-0169's dual-mode confinement is retired rather than confined.

## Context

ADR-0172 gave the Session view its own scheme (`src/session.rs`), but the scheme still encodes a **dual focus-plane model** (composer plane vs. transcript plane) inherited from ADR-0169. Four forces make that model untenable:

1. **The mode pays a permanent indication tax.** A persistent state must be announced: the `step_focus_bar` row (`chrome/step_focus_bar.rs`), a declared `SessionFocusRegion` enum (`app/mod.rs:58`), and dual hint sets exist only to answer "which mode am I in". The declared layer already drifts from the actual layer (`focused_target`): the mouse path and `retain_visible_focused_target` mutate one without the other, so `SessionFocusRegion`-derived help filtering can disagree with reality.
2. **Tab is a three-job chord.** `resolve_tab` (`session.rs:296`) juggles completion commit, completion reopen, and mode toggle; BackTab mirrors part of it. Shell convention is unambiguous — Tab means completion — and the mode-toggle job is the one that created force 1.
3. **`docs/explanation/tui.md` ("No modes: a single optional focused step") states the opposite stance.** The dual plane is a late add-on that contradicts the view's own design record.
4. **AI-initiated interactions are misfiled as floating modals.** `Modal::Permission` already renders *in place* — it extends the composer slot over the input box and hint bar (`event_loop/render.rs:396-414`) — yet it is routed as a transient modal, carries `Recess::None`, and appears in three per-modal policy special-cases (`modal.rs:204,232,258`). `Modal::Question` is routed the same way. The pattern that actually fits — an inline replacement of the composer slot — was discovered by accident and never named. Tools can also raise multiple concurrent questions; the single `pending_permission` slot and transient-modal routing cannot queue them.

Separately, keyboard ownership across surfaces is enforced by scattered `active_modal != None` gates, while the visual side already has a declaration pattern: `Modal::recess()` (`modal.rs:180`) is the single source of truth that keeps layout and paint from disagreeing. Keyboard behavior never got the same treatment — and the counterexamples exist (the most keyboard-dominant modals, Permission/Question, are visually the *lightest*: `Recess::None`, no dimming).

## Decision

### 1. The Session keyboard is plane-less ("unbounded")

There are no focus planes and no mode to enter or leave. Every chord has **one** meaning in the Session/Runner/Side chat surface; the focused step is a transient selection (a cursor floating over steps), not a place the keyboard lives.

| Chord | Semantics | Change from ADR-0172 |
|---|---|---|
| PgUp / PgDn | Transcript scrolls one page, **unconditionally** | composer-typing history-walk branch removed |
| Home / End | Transcript scrolls to top / bottom, **unconditionally** | `has_focused_target` gate removed; readline line-start/end moves to Ctrl+A/E (the canonical emacs bindings) |
| Alt+↑ / Alt+↓ | Walk step selection / clear it (`SurfaceVerb::FocusPrevTarget` / `ClearFocusedTarget`, user-remappable) | canonical chords unchanged |
| ↑ / ↓ | Completion walk → caret line motion | step-walk branch removed (composer-only) |
| Tab | Completion commit / reopen **only** | mode-toggle branches removed; BackTab becomes inert |
| Esc | Dismiss completion → clear step selection → interrupt running round | unchanged, minus nothing |
| Printable | Insert into draft; a selected step bounces to composer (ADR-0170's bounce grammar, retained verbatim) | unchanged |
| Mouse | Click acts on what it hits (step click selects/toggles, content click arms drag-selection and clears selection) | no region-focus state exists to switch |

Rationale for Alt over Ctrl on the step-walk chords: `Ctrl+↑/↓` is already claimed by modal-body scrolling (`input/mod.rs:148`) and ADR-0172's one-chord-one-verb discipline forbids a second meaning; Ctrl+arrows depend on `modifyOtherKeys`/kitty-protocol support and collide with macOS Mission Control, while Alt is the universally reliable ESC-prefix encoding; and all session verbs already live on the Alt plane (`Alt+S` steer, `Alt+P`/`Alt+N` history).

Deleted outright: `SessionFocusRegion`, `saved_focus`, the `FocusTranscript` / `FocusComposer` palette commands, `FooterRowId::StepFocus` and `chrome/step_focus_bar.rs`, and the `Tab transcript` hint. Selection indication remains what already exists (step highlight, composer dim, caret hiding); **no new indicator is introduced** — with no mode there is no mode to announce.

### 2. Keyboard ownership is declared, not inferred — the `Recess` pattern extended to keys

Every surface (each `Modal` variant, each full-screen view) declares its keyboard claims next to its existing visual declaration:

```rust
impl Modal {
    pub fn recess(self) -> Recess { … }              // existing: visual recedence
    pub fn keyboard_claims(self) -> Claims { … }     // new: All | Partial(…) | None
}
```

- **All** — the surface consumes the keyboard (`Question`-class decisions, text-editor forms). Default for modals.
- **Partial** — named key families are owned; everything else falls through (completion popup: nav keys only, typing reaches the draft; sheets: submit/edit keys owned, transcript scroll passes through).
- **None** — visually present, keyboard-inert (toasts, hover chrome).

The input router, `Scope`/help availability gates, and the hint derivation all consume this one declaration; the hand-written `ctx.active_modal != None` conditionals in `keymap.rs:1793-1814` and the central match are deleted. The governing principle: **visual stacking predicts keyboard ownership by default, and every deviation is a one-line declaration at the surface's policy block, never a scattered gate.** A consistency test reconciles the two columns: every modal that renders as a visual takeover must claim `All`; a `None`-claim surface must not appear in `App::caret_owner`.

The shared affordance library (readline editing, caret motion, paste, list paging) stays a pure-function library that text-bearing surfaces delegate to — it is not a stack layer and owns nothing.

### 3. AI-initiated interactions are Sheets, not modals

A **sheet** is a replacement state of the chat surface's composer slot — never a floating layer:

- Same slot, same bottom edge; height computed from content; grows upward; the transcript behind it stays live and scrollable.
- The composer slot is a state machine: `Draft | Sheet(Permission) | Sheet(Question) | Sheet(InputInjection) | …`. While a sheet occupies the slot the draft stash is frozen (the existing "draft saved" discipline), and Esc never discards a sheet.
- Each sheet kind owns its own scheme (chord family + derived hint row) inside the session surface module, following ADR-0172's scheme pattern.

**Initiator taxonomy** — what makes an interaction a sheet:

| Initiator | Form | Instances |
|---|---|---|
| AI → user interaction request | **Sheet** (composer slot, inline) | Permission, Question, InputInjection |
| User-invoked tool | Modal overlay | Models, Connections, Tools, Skills, … |
| User-invoked space | Full-screen view | Dashboard, Settings, Runner zoom |

When the AI interrupts, the interaction point must appear where the user's attention already is — the slot they type in. A sheet appearing in the Runner or Side view renders in that chat surface's own composer slot; the rule is host-agnostic.

**Interaction queue.** Tools may ask multiple questions and raise permissions concurrently; the single `pending_permission` slot becomes a FIFO `pending_interactions: VecDeque<SheetRequest>`. The front sheet occupies the slot; a `n/N` queue badge and a pair of queue-walk session verbs (`session.prev_sheet` / `session.next_sheet`, canonical chords finalized at implementation, override-first per ADR-0172 §9) reach the rest. Sheet-local keys (approve/deny, question options, free-text "Other") are unchanged and are advertised by the sheet's own hint row.

## Alternatives considered

- **Promote `SessionFocusRegion` to a real region-focus state** ("transcript focused, no step"; click-anywhere-into-transcript; arrows pick the first step). Rejected: it pays the full indication tax — focus bar, a third key-grammar row, region-clearing semantics on scroll — to save one modifier key, and it institutionalizes the state that `docs/explanation/tui.md` argues should not exist. The unbounded grammar provides the same reach (Alt+↑/↓ from anywhere) with zero states to announce.
- **Keep the dual mode and improve its indication** (head-band segment, gutter accent column, flash overlay on switch). Rejected for the same reason: all are payments on a state whose value was never established; the correct count of mode indicators for a modeless design is zero.
- **Ctrl+↑/↓ for step walking.** Rejected: double-books a chord the modal-body scroll already claims, depends on non-universal terminal encoding, and collides with a macOS system shortcut.
- **Keep Permission/Question as transient modals with better chrome.** Rejected: they are not spatially "above" the session — they replace its input affordance — so every modal policy (recess, dismissal, caret) needs a special case. Naming the sheet removes the special cases instead of styling them.
- **A dynamic runtime surface stack collected per frame.** Rejected as over-machinery: the layering is a fixed four-slot shape (shared affordances → view → modal → blocking), the dynamic part is only which `Option` slots are filled. Ownership belongs in per-surface declarations, not in a runtime-assembled structure.

## Consequences

Positive:

- The mode-indication tax goes to zero: no bar, no declared region enum, no dual hints, and a whole class of declared-vs-actual drift bugs becomes unrepresentable.
- One chord, one verb, restored: Tab belongs to completion; PgUp/PgDn/Home/End belong to the transcript everywhere; Ctrl stays text-level.
- Keyboard ownership becomes declarative and testable at the same site as visual recedence; Scope/help derivation and the router can no longer disagree with each other or with what is on screen.
- Concurrent AI interactions queue honestly instead of overwriting a single slot; sheets put the interaction at the user's attention point in every chat surface.

Negative / neutral:

- Readline loses Home/End (Ctrl+A/E remain) and the composer loses PgUp/PgDn history recall (Alt+P/Alt+N and Ctrl+R remain). Both are degradations of convenience, not capability.
- Every mouse/keyboard test that asserted plane-switching behavior needs rewriting; the diff is broad and mechanical.
- Sheet extraction moves three variants out of `modal.rs` and the transient-surface router; the `pending_permission` single-slot contract changes shape.

Migration steps:

1. Delete the mode apparatus: `SessionFocusRegion`, `saved_focus`, focus palette commands, `step_focus_bar` / `FooterRowId::StepFocus`, `Tab transcript` hint (done).
2. Rewire the chat scheme to the plane-less table above; un-gate `ScrollTop`/`ScrollBottom`; PgUp/PgDn page the transcript unconditionally (the composer history-walk branch proved to be stale documentation only — actual wiring already scrolled the transcript); strip Tab/BackTab and bare-arrow mode branches; update `live_chat_hints` and the keybinding reference docs (done).
3. Introduce `keyboard_claims()` alongside `recess()`; derive the router's family predicates (`edits_input_field`, `scrolls_own_body`) from it; add the visual-ownership reconciliation tests (done).
4. Extract the sheet module (`src/sheet.rs`: `SheetKind` + per-sheet claims + verb scheme): the three variants leave `Modal` entirely, and — per §3's slot model — sheet state lives on `App::active_sheet` as **composer-slot state, not router foreground identity**: the slot mounts either the draft editor or one sheet, sibling components. The router never sees sheets; the question sheet re-anchors from a centered fixed-geometry modal to the slot (bottom-edge anchored, body scrolls within it). `InputContext` gains `active_sheet` and the input router grows explicit sheet arms (verb dispatch ahead of modal dispatch — the sheet blocks the agent, the modal is a browsing aid — plus Esc/Enter/arrow semantics and claims-driven scroll routing). A coexisting modal (opened via a global chord while a sheet is pending) renders beneath the sheet and yields non-global keys to it (done).
5. Interaction queue: the runtime already FIFO-queues concurrent interactions (`pending_permission` / `pending_question` / `pending_input` are `VecDeque`s); the TUI now mirrors the queue depth and renders the `N queued` sheet badge (done). Queue-walk verbs (`session.prev_sheet` / `session.next_sheet`) remain to be designed against the harness's decision-addressing guarantees (pending).
6. Update `docs/explanation/tui.md` (the "No modes" section is now literally true — record that ADR-0169's dual plane is retired), `docs/explanation/composer.md`, and the keybinding reference (done).

## References

- ADR-0172 (per-surface schemes — the machinery this ADR re-scopes), ADR-0170 (bounce-to-composer, action registry), ADR-0169 (dual-mode origin, hereby retired), ADR-0141/0139/0133 (surface model).
- `apps/tui/crates/mutx/src/session.rs` (chat scheme), `src/modal.rs` (`Recess` — the declaration pattern extended to keys), `src/input/mod.rs` (router), `src/chrome/step_focus_bar.rs` (deleted), `event_loop/render.rs:396` (the emergent Permission sheet).
