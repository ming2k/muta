# 0169. Session View Dual-Mode Confinement, Device-Agnostic Input State Machine, and Unified `Ctrl+X` Leader Architecture

- **Status:** Superseded by ADR-0170
- **Date:** 2026-09-02
- **Builds on:** ADR-0133 (view surfaces & quick switch), ADR-0139 (surface router & lifecycle), ADR-0141 (view means fullscreen, modal means modal)
- **Supersedes:** Any overlapping legacy multi-leader chord proposals (`Ctrl+C` leader) and ambient dual-mode diffusion.

## Context

Prior to this architecture decision, the TUI interaction model suffered from mode ambiguity, dead chord bindings, and lack of visual alignment:

1. **Deadlock & Shadows in Leader Chords**:
   - `Ctrl+C` was defined as an Emacs-style leader chord in `which_key.rs` and input handling, but was shadowed and consumed globally by `keymap::Registry::resolve` (`Action::CopyOrClear`), rendering the entire `Ctrl+C (Agent / Mode)` branch unreachable dead code.
2. **Dual-Mode Scope Ambiguity**:
   - The boundary between "Composer editing" and "Transcript step inspection" was implicit. Printable characters typed while a step was focused in the transcript would bleed into the composer input buffer, causing input corruption and jarring UX.
3. **UI Hint & Discoverability Disconnect**:
   - The Help modal and footer hints did not surface the two-stroke leader architecture.
   - Composer hints were cluttered with symbol separators (`·`) and unhelpful character counters, failing to clearly separate navigation escape paths from action submission.

## Decision

### 1. Strictly Confine Dual-Mode to Session View

Dual-mode (`focused_target: Option<InteractiveTarget>`) is strictly confined to the **Session View (Chat root)**:
- **Session View** has two persistent, co-existing physical focal planes: the scrolling **Transcript** stream and the bottom **Composer** input box.
- All auxiliary views and modals (`/settings`, `/models`, `/tools`, `/mcp`, `/telemetry`, etc.) are **single focal plane** surfaces (a single list, form, or document) and must never inherit dual-mode complexity.

```
 ┌────────────────────────────────────────────────────────────────────────┐
 │                      1. Composer Edit Mode (Default)                   │
 │  • app.focused_target == None                                          │
 │  • Typing enters prompt, Readline keys edit buffer                     │
 └───────────────────────────────────┬────────────────────────────────────┘
                                     │
           [Enter Step Focus]        │                 [Exit Step Focus]
           • Keyboard: Ctrl+X o      │                 • Keyboard: Esc / Ctrl+X o
           • Mouse: Click step       │                 • Mouse: Click Composer
                                     ▼
 ┌────────────────────────────────────────────────────────────────────────┐
 │                      2. Transcript Step Focus Mode (Inspector)         │
 │  • app.focused_target == Some(target)                                  │
 │  • Composer dimmed, input strictly isolated (typing swallowed)         │
 │  • ↑/↓ navigate steps, Enter toggles/activates target                   │
 │  • StepFocusBar active directly above Composer                         │
 └────────────────────────────────────────────────────────────────────────┘
```

### 2. Device-Agnostic State Machine Isomorphism

Keyboard and mouse inputs are equal **input probes** driving the exact same underlying state transitions:
- **Focus Step**: `Ctrl+X o` / `Ctrl+↑↓` or clicking an interactive step card sets `focused_target = Some(target)`.
- **Dismiss Focus**: `Esc` / `Ctrl+X o` or clicking the Composer panel sets `focused_target = None`.
- **Toggle Step**: `Enter` on focused step or clicking a summary toggles expansion.

### 3. Unified `Ctrl+X` Leader Chord as SSOT

- **`Ctrl+C`** is released to pure immediate control (copy selection / running interrupt / idle double-press quit).
- **`Ctrl+X`** is the single, canonical two-stroke leader namespace, registered directly in `GLOBAL_BINDINGS`:
  - `b`: View switcher / buffer
  - `k`: Close view / modal
  - `o`: Toggle focus between Composer and Transcript
  - `t`: Todos task list
  - `q`: Queue outbox
  - `p`: Pause / resume queue
  - `m`: Models picker
  - `n`: Connection detail
  - `d`: Session telemetry & performance report
  - `a`: `/btw` asides
  - `s`: Settings view
  - `?`: Help modal
  - `Ctrl+C`: Quit application
  - `Ctrl+G` / `Esc`: Cancel leader chord

### 4. Clean Whitespace Layout (Left Navigation, Right Execution)

- **Eliminated all noise symbols (`·`) and character counters**.
- **Composer hint row**:
  - **Left**: Navigation & escape actions (`[Ctrl+X o] focus   [Ctrl+X] actions`).
  - **Right**: Execution & submission (`[Enter] send prompt`).
  - **Inactive state**: Muted `(Composer inactive)` when a transcript step is focused.
- **Transcript step focus bar (`StepFocusBar`)**:
  - Proximity inspector bar dynamically rendered at the bottom of the transcript viewport:
    `◈ STEP FOCUS   [↑↓] step   [Esc / Ctrl+X o] input     [Enter] expand/toggle`
