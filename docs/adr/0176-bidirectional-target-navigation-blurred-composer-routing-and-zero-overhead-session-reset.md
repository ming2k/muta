# 0176. Bidirectional Target Navigation, Blurred-Composer Routing, and Zero-Overhead Session Reset

- **Status:** Accepted
- **Date:** 2026-09-06
- **Builds on:** ADR-0174 (arrow edge hand-off & transcript browse focus), ADR-0173 (unbounded session keyboard), ADR-0172 (per-surface keybinding schemes), ADR-0170 (bounce-to-composer)
- **Amends:** ADR-0173's canonical binding table (replaces the asymmetric `ClearFocusedTarget` on `Alt+↓` with `FocusNextTarget`), ADR-0174's browse-focus keyboard contract (prevents Up/Down from accidentally disarming browse focus and corrupting composer history).

## Context and Problem Statement

Three interconnected design flaws existed in the session lifecycle and keyboard event routing:

1. **Slow `/new` Session Creation**:
   Invoking `/new` to start a fresh session was unexpectedly sluggish. In `start_fresh_session`, `reapply_session_selection` unconditionally triggered a full provider `activate(...)` cycle: reloading all connections from disk, checking/refreshing OAuth credentials over async locks, re-instantiating HTTP client transports via `catalog::build_provider_for_model`, persisting model usage telemetry to disk, and broadcasting redundant provider status events — even when the active provider and model remained completely unchanged.
2. **Asymmetric Navigation on `Alt+↑` / `Alt+↓`**:
   `Alt+↑` was bound to `SurfaceVerb::FocusPrevTarget`, invoking `app.focus_interactive_target(-1)` to step backward into transcript targets. However, `Alt+↓` was arbitrarily bound to `SurfaceVerb::ClearFocusedTarget`, which immediately discarded focus (`app.focused_target = None`), causing the selection to collapse straight back to the composer instead of stepping forward to the next target below.
3. **Leaked Keyboard Events during Composer Blur & Target/Browse Focus**:
   When a transcript target held focus (`has_focused_target = true`) or when the user clicked into the transcript (`transcript_focused = true`), the composer was visually dimmed and the caret hidden. Yet pressing bare `↑`/`↓` was unconditionally intercepted by `resolve_up`/`resolve_down` to perform readline history recall (`HistoryPrev`/`HistoryNext`) or caret line motion in the dimmed composer. Furthermore, `event_rearms_composer_follow` treated bare `↑`/`↓` as composer editing intent, instantly disarming transcript browse focus and mutating the hidden composer draft with recalled command history.

## Decision

### 1. Zero-Overhead Session Reset on `/new`
In `crate::handlers_provider::reapply_session_selection`, check whether the resolved session selection matches the currently active provider and model (`agent.provider.provider_id() == provider_id && agent.provider.model() == model`). When they match (the default case for `/new`), return immediately. This eliminates redundant OAuth credential resolution, connection disk reloads, HTTP client re-allocations, telemetry disk writes, and provider state broadcasts, reducing `/new` session reset overhead to sub-millisecond memory-only state swaps.

### 2. Symmetric Bidirectional Target Navigation
- Introduce `SurfaceVerb::FocusNextTarget` with canonical chord `Key::ALT_DOWN` (config name `"focus_next"`).
- Rebind `SurfaceVerb::ClearFocusedTarget` canonical chord to `Key::ESC` (config name `"clear_focus"`).
- In `app.focus_interactive_target(direction: i8)`:
  - `direction < 0` (`Alt+↑` or `↑` when focused) walks backward toward older targets.
  - `direction > 0` (`Alt+↓` or `↓` when focused) walks forward toward newer targets.
  - Stepping downward past the newest visible target at the bottom of the transcript gracefully exits focus back to the composer.

### 3. Intent-Driven Routing for Blurred Composer
- **Target Focused (`has_focused_target = true`)**:
  - Bare `↑` routes to `InputAction::FocusPrevTarget`.
  - Bare `↓` routes to `InputAction::FocusNextTarget`.
  - `Enter` activates the focused target (expand/collapse).
  - `Esc` clears focus back to the composer (`InputAction::ClearFocusedTarget`).
  - Printable characters bounce to the composer: insert character into the draft and clear target focus.
- **Transcript Browse Focused (`transcript_focused = true`)**:
  - Bare `↑` routes to `InputAction::ScrollUp` (line-by-line scrolling).
  - Bare `↓` routes to `InputAction::ScrollDown` (line-by-line scrolling).
  - `Esc` clears browse focus back to the composer.
  - Printable characters bounce to the composer.
- **Disarm Protection in Event Loop**:
  - In `event_loop/mod.rs`, bare `↑`/`↓` keys do NOT disarm `transcript_focused` or rearm composer caret following when transcript browse focus or target focus is active.

## Consequences

- Starting a new session via `/new` is instant (<1ms) without blocking on disk or network checks.
- `Alt+↑` and `Alt+↓` provide fully symmetric, predictable directional component navigation.
- Blurred composer states (transcript browse focus and component step focus) respect component autonomy: navigation arrows navigate/scroll rather than unexpectedly corrupting composer text with history recall.
- Readline history recall is preserved exclusively when the composer is active and at the top/bottom boundary of the input field.
