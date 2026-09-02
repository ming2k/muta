# 0170. Composer-First Architecture, Single Source of Truth Action Registry, and Unified `Ctrl+L` Command Palette

- **Status:** Accepted
- **Date:** 2026-09-03
- **Builds on:** ADR-0133 (view surfaces & quick switch), ADR-0139 (surface router & lifecycle), ADR-0141 (view means fullscreen, modal means modal)
- **Supersedes:** ADR-0169 (Session View dual-mode & `Ctrl+X` leader keyboard architecture)

## Context

Prior iterations of the TUI keyboard interaction architecture (including ADR-0169's two-stroke `Ctrl+X` leader chord and which-key popup) created recurring friction across the interaction model:

1. **Input Loss and Hidden Modality**:
   - Isolating transcript navigation by creating an explicit "inactive composer" state caused user keystrokes in transcript focus to be silently swallowed or discarded.
   - Tab was overloaded to toggle submission modes (`Steer` vs `FollowUp`) without an unambiguous visual contract.
2. **Keybinding Inflation & Shadowing**:
   - Introducing leader chords (`Ctrl+X`, which-key overlays, `Alt+X` switcher, scattered single-letter commands like `i`/`e`/`a`/`r`/`d`/`D`) increased cognitive overhead, collided with terminal multiplexers, and burdened users with memorization.
3. **Fragmented SSOT & Help Drift**:
   - `GLOBAL_BINDINGS` was a loose static vector decoupled from contextual modal keys, readline bindings, and Help modal text. Help modal and footer hint representations frequently drifted from runtime dispatch.
   - Nested in-modal keymap sub-pages added multi-layer modal complexity without solving discoverability.

## Decision

### 1. Ten Foundational Design Principles

1. **Composer-first, zero input modality**: The application is typing-centric; the composer is never locked out.
2. **Single input owner**: Exactly one region, panel, or dialog owns keyboard focus at any instant.
3. **Visible, predictable, recoverable focus**: Overlays trap focus exclusively; closing restores source focus.
4. **Single semantic origin**: Every action has one authoritative definition.
5. **Unified Action Registry (True SSOT)**: Keybindings, F1 Help, Footers, and Command Palette are strictly derived from `CommandSpec`.
6. **Search over memorization**: Rare and administrative actions are discovered via `Ctrl+L` Command Palette.
7. **Zero background leakage**: Active modals and dialogs completely isolate keyboard and mouse events from underlying views.
8. **Zero loss of printable characters**: Printable keystrokes arriving in the transcript automatically switch focus to the composer and insert the characters.
9. **ANSI / ANSI-xterm independence**: Core workflows never require the Kitty enhanced keyboard protocol.
10. **Clean break from legacy baggage**: Complete retirement of leader chords, which-key overlays, and nested keymap sub-pages.

### 2. Session View Focus Regions

Session View is a composite view with two explicit focus regions (not modes):

```
Session View
├── Transcript region
└── Composer region (default focus)
```

- **Focus Switching**:
  - `Tab`: Composer → Transcript.
  - `Shift+Tab` / `Esc` / `Tab`: Transcript → Composer.
  - Mouse click on a region focuses that region.
- **Transcript Focus Navigation**:
  - `↑` / `↓`: Move step / interactive item selection.
  - `Enter`: Open / expand / drill down into focused step.
  - `PageUp` / `PageDown`: Scroll transcript viewport.
  - `Home` / `End`: Jump to top / bottom of transcript.
  - `Esc`: Return focus to Composer.
  - Printable characters: Immediately return focus to Composer and insert the character(s).
  - Focus bar renders: `TRANSCRIPT   ↑↓ move   Enter open   Esc compose`.
- **Composer Focus Actions**:
  - `Enter`: Send prompt when idle; enqueue follow-up when running.
  - `Alt+S`: Immediate steering intervention when running.
  - `Alt+Enter` / `Ctrl+J`: Insert literal newline.
  - `Ctrl+R`: History search panel.
  - `PageUp` / `PageDown`: Scroll transcript viewport without stealing focus.
  - `/`: Slash command completion menu.
  - Footer when running: `Tab transcript   Alt+S steer now                     Enter queue follow-up`.
  - Footer when idle: `Tab transcript                                      Enter send`.

### 3. Exactly Six Canonical Global Shortcuts

1. **`F1`**: Dynamic contextual Help.
2. **`Ctrl+L`**: Unified Command Palette & Surface Navigator.
3. **`Esc`**: Dismiss overlay, clear focus, or step back one level.
4. **`Ctrl+C`**: Interrupt running task (with two-press confirmation).
5. **`Ctrl+Q`**: Quit application.
6. **`Ctrl+Shift+C` / `Cmd+C`**: Copy selection to clipboard.

Prohibited legacy behaviors:
- `Ctrl+C` never clears composer text (use standard `Ctrl+U`).
- `Ctrl+C` never closes modals (use `Esc`).
- `Ctrl+C` never quits on idle double-press (use `Ctrl+Q`).

### 4. Authoritative Command Registry (`CommandSpec`)

All commands are declared in `COMMAND_REGISTRY`:

```rust
pub struct CommandSpec {
    pub id: CommandId,
    pub label: &'static str,
    pub hint: &'static str,
    pub category: CommandCategory,
    pub scope: Scope,
    pub bindings: &'static [Key],
    pub slash: Option<&'static str>,
    pub availability: fn(&AppContext) -> Availability,
    pub disclosure: DisclosurePriority,
    pub danger: DangerLevel,
    pub description: &'static str,
}
```

The unified 6-stage input routing cascade:
1. Active confirmation / blocking dialog (`Permission`, `Question`, `ModelEditor`, `CustomProvider`)
2. Active panel / popover (`CommandPalette`, `Help`, `Models`, `Connections`, `HistorySearch`, `Todos`, `Queue`, `Telemetry`, etc.)
3. Focused widget (`Composer` vs `Transcript`)
4. Current view (`Session`, `Dashboard`, `Settings`, `Runner`, `Side`)
5. Global hard-bound shortcuts (`F1`, `Ctrl+L`, `Esc`, `Ctrl+C`, `Ctrl+Q`, `Ctrl+Shift+C`/`Cmd+C`)
6. Text insertion / readline editing

### 5. Unified Command Palette (`Ctrl+L`)

`Ctrl+L` opens the application-wide command center, combining quick switcher, actions menu, surface navigation, settings, and rare administrative commands:
- Live fuzzy search across labels, hints, slash triggers, and descriptions.
- Availability evaluation: unavailable commands displayed with rationale.
- Danger badges (`[DANGER]`, `[CAUTION]`) for destructive actions.
- MRU ordering for recently executed commands.
- Direct execution via semantic `CommandId` without synthesized keyboard events.

## Consequences

- **Zero Keystroke Loss**: Users typing while inspecting transcript steps never lose draft text.
- **Predictable Muscle Memory**: 6 universal shortcuts; all other capabilities discovered via `Ctrl+L` search.
- **Strict SSOT**: Help modal, command palette, footer hints, and key dispatch are 100% synchronized by design.
- **Clean Architecture**: Complete elimination of `LeaderChord`, `which_key.rs`, `modal_keymap_open`, and nested keymap pages.
