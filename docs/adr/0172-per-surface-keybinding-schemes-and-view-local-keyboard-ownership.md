# 0172. Per-Surface Keybinding Schemes and View-Local Keyboard Ownership

- **Status:** Accepted
- **Date:** 2026-09-02
- **Builds on:** ADR-0170 (composer-first & `CommandSpec` action registry), ADR-0133 (view surfaces & quick switch), ADR-0139 (surface router & lifecycle), ADR-0141 (view means fullscreen, modal means modal)
- **Supersedes:** The implicit central-match enforcement of ADR-0170's routing cascade. ADR-0170's *principles* (SSOT, composer-first, six canonical globals) remain; what changes is who owns and enforces a surface's keys.

## Context

ADR-0170 declared a `CommandSpec` SSOT and a six-stage input cascade, but left the **enforcement** inside a single monolithic `match` in `input/mod.rs` (≈1300 lines, `process_event`). That match re-derives which surface owns a key by hand from a 30+-field `InputContext` mirror, and view-local behavior is interleaved with modal and global behavior in the same arms.

The cost is structural and has produced real bugs:

1. **Dead / drifted shortcuts.** Keys advertised by hints and docs but unbound (top-level `Ctrl+P` queue toggle, `Ctrl+O`/`Ctrl+N` model-bar keycaps, `Ctrl+S`, `F3`), because hints come from four+ independent hard-coded sites while handling lives in one match no one cross-checks.
2. **No view layer exists.** `Scope::{Global,Session,Composer,Transcript,BrowsePanel,BlockingDialog}` is closer to a focus-region taxonomy than to the surface model (ADR-0141). The "Session view" does not own a single line of its keyboard behavior; it is a set of `active_modal == None` branches scattered across the `Esc`/`Enter`/`Tab`/arrow/printable arms.
3. **Multi-mode session logic is global code.** The Session view's own state machine (composer vs. transcript-focus vs. focused-target vs. running vs. completion vs. history) is expressed as guard clauses in the central match, so it cannot be tested, extended, or customized in isolation.
4. **Implicit view conflation.** The `active_modal == None` arms apply to every full-screen view (Session, Runner, Side, Dashboard, Settings) with no view discriminator, so "no modal" silently means "chat surface" for all views.

## Decision

### 1. Per-surface keybinding schemes

Each surface (a full-screen view or a modal) that owns keys declares a **keybinding scheme** — a self-contained module that maps chords to that surface's semantic actions and resolves them against that surface's own state. The scheme is the surface's single semantic origin (ADR-0170 §1.4): hints, Help, palette, and dispatch all derive from it.

The **Session view** is the first concrete owner: `session` (`src/session.rs`) owns the chat-surface keys — its two focus planes (composer / transcript), focused-target steps, completion menu, run state, and history — and resolves them with its own multi-mode state machine. The Runner and Side views remain folded into the same chat-surface module for behavioral parity (they are zooms/siblings of the session chat), split out only if their schemes later diverge.

### 2. A thin layered router, not a decision engine

`process_event` stops being the decision engine and becomes a router with four layers, in order:

1. **Global interceptor** — unchanged hard chord set (`F1`, `Ctrl+P`/`Ctrl+L`, `Esc`, `Ctrl+C`, `Ctrl+Q`, `Ctrl+Shift+C`/`Cmd+C`, `Ctrl+O`, `Ctrl+N`), plus blocking-dialog preemption. These are the escape hatches and are never view-owned.
2. **Surface dispatch** — the active surface's own scheme resolves the key. This is where a view/modal's keys are decided, by the surface, from its own state.
3. **Shared affordance library** — cross-surface verbs implemented once by the framework and reused (readline editing family, caret motion, paste, generic list scroll/paging, modal list navigation). The router provides these; surfaces declare which they adopt.
4. **Text insertion** — printable fallthrough.

The router routes; it does not decide view semantics.

### 3. View discriminator is explicit

`InputContext` gains `current_view: View`. Surface dispatch is gated on the *actual* surface (view + modal), so "no modal" never silently means "session" for Dashboard/Settings.

### 4. Discovery stays derived

Palette, Help, footer hints, and the model/composer keycaps keep deriving from the `COMMAND_REGISTRY` + per-surface schemes, and a consistency test asserts that **every advertised chord resolves to a handler** and that no global chord maps to two commands. Adding a binding without a resolver is a compile-time-adjacent test failure, not a silent drift.

## Alternatives considered

- **Keep the central match and only fix individual dead keys.** Rejected: treats symptoms, leaves the structural fragility that produces the next drift.
- **Drive enforcement purely from a data table of `(Key, CommandId)`.** Rejected: the chat surface's multi-mode reactions are state machines, not flat bindings; a flat table cannot express "Enter means send when idle, queue when running, activate when a step is focused, commit when a completion is highlighted" without an imperative resolver anyway.
- **Give every modal its own scheme in the same change.** Deferred to later stages: modals are mostly single-surface lists/forms that already behave well through the shared affordance layer; migrating them now maximizes churn for minimal structural gain.

## Consequences

Positive:

- **Cohesion.** The Session view's keyboard behavior (including its multi-mode state machine) lives in one module, testable in isolation and independently customizable.
- **Drift-proof.** The composer hint row now renders from the chat surface's own scheme (`session::live_chat_hints`), so the Session view's advertised hints and its dispatch share a single semantic origin; the consistency test blocks future dead shortcuts.
- **Correct scope.** `current_view` removes the "no modal == session" conflation.
- **Extensible.** A future per-view or per-user custom keybinding scheme plugs in by replacing a surface's scheme, not by editing the central match.

Negative / neutral:

- The central match still owns the shared affordances (list navigation, Enter select, Esc close, Tab focus, readline editing, paste) and the palette filter — by design, those are cross-surface verbs, not per-surface.
- The input test suite remains the behavioral guard during the extraction; the diff is large and mechanical.

Migration steps:

1. Extract `session` scheme (done): resolver + `live_chat_hints` discovery origin + consistency tests.
2. Rewire `process_event` to consult the session scheme before the modal/global match and strip the chat-surface `active_modal == None` branches from the central arms (done).
3. Derive the composer hint row from the session scheme (done).
4. Give the Runner and Side views their own schemes, routed per-view via `resolve_view_key` (done).
5. Extract every modal's single-letter verb keys into per-modal schemes (`src/modal_keys.rs`, `resolve_modal_key`) and strip them from the central printable arm (done).
6. Migrate the history modal's full key family (Esc/Enter/Tab/↑/↓) into its own scheme and derive its hint row from it (done). The generic `HintSide`/`LiveHint` hint vocabulary now lives in `keymap.rs`, shared by every surface scheme. The scheme migration surfaced two history-modal verbs whose key wiring had silently rotted away — `HistoryTogglePreview` (Tab flip to full-text preview) and `HistoryClearAll` (Ctrl+X arming the clear confirmation) were no longer produced by any key; rather than resurrect bindings nobody could reach, their actions, the `history_preview` / `history_clear_confirm` state, the preview renderer branch, and the clear-input-history path were removed outright (done).
7. Migrate the command palette's filter family (printable query, Backspace, Delete, Enter) into its own scheme; list walking and Esc-close stay in the shared affordance layer by design (done). With every modal's specific keys now scheme-owned, the remaining central match holds only cross-modal shared verbs (list navigation, Enter-select for generic lists, Esc-close, readline editing, paste, scrolling, text insertion).
8. User-overridable global chords: a `[keybindings]` table in `mutx/config.toml` remaps the canonical global chords (`parse_key` chord syntax, `GlobalOverrides` override-first resolution, `effective_binding` for dispatch *and* the visible keycaps so a remapped chord advertises exactly what fires) (done). Surface-chord remapping (session / modal verbs) remains a future extension — the per-surface resolvers are the seam.
9. User-overridable surface verbs: a `[keybindings.session]` sub-table remaps the full-screen views' single-purpose chords (`SurfaceVerb` / `SurfaceOverrides`, override-first like the global layer). The Session/Runner/Side resolvers consult the effective binding — the assigned chord fires, the canonical chord goes inactive — and the composer hint row renders the `steer` verb's effective keycap (done). The multi-mode interaction grammar (Enter send/queue/activate/commit, Tab commit/focus, Esc dismiss/focus/interrupt, ↑/↓ walk, printable text) and the modal single-letter verbs are deliberately **not** remappable: they are the surfaces' language, not shortcuts, mirroring how `Esc`/Back stay non-remappable globally.

## References

- ADR-0170 (composer-first & action registry), ADR-0169 (superseded leader architecture), ADR-0126 (Ctrl-row claims), ADR-0103 (asides), ADR-0133/0139/0141 (surface model).
- `apps/tui/crates/mutx/src/input/mod.rs` (pre-change central match), `src/keymap.rs` (`COMMAND_REGISTRY`, `Scope`), `src/session.rs` (new chat-surface scheme).