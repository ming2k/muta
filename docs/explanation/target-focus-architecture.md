# Target Focus and Elastic Intent Architecture

This page documents the **Target Focus and Elastic Intent Architecture** of the muta TUI (`mutx`). It explains the interaction model, focus state transitions, key event routing invariants, and the separation between session data lifecycle and host infrastructure.

For the TUI layout and component specifications, see [Terminal UI Reference](../reference/tui/index.md). For keyboard mapping details, see [ADR-0176](../adr/0176-bidirectional-target-navigation-blurred-composer-routing-and-zero-overhead-session-reset.md).

---

## 1. The Core Interaction Paradigm

Terminal AI chat interfaces face an inherent design tension:
- On one side: a fast, fluid, readline-style conversational prompt where the user spends 95% of their time typing.
- On the other side: a rich, addressable transcript populated with interactive tool invocations, expandable thinking traces, execution diffs, and sub-agent runners.

Traditional approaches either:
1. **Flatten output into dumb terminal stream**: Output becomes dead characters; inspectability is lost.
2. **Impose modal splits (e.g. Vim Normal vs Insert mode)**: Forces explicit mode-toggle chords (`Esc`, `i`, `Tab`), adding continuous friction to conversational flow.
3. **Pseudo-modal hacks with event leakage**: Visually dimming the input box while allowing background keys to accidentally mutate draft text or trigger command history recall.

mutx resolves this with **Modeless Elastic Focus**:
- There is **no modal penalty**: no "Normal mode" to enter or exit.
- Every chord has an unambiguous, context-respecting meaning governed by **Intent Orthogonality**.

---

## 2. The Three Focus Domains

At any instant on the chat surface, user attention occupies one of three states:

```text
               ┌────────────────────────┐
               │    Composer (Prompt)   │◄───────────┐
               └───────────┬────────────┘            │
     Alt+↑ (from bottom)   │                         │
     or Mouse Click Target │   Bounce on Printable   │ Esc / Downward
                           ▼   or Enter (Submit)     │ Edge Egress
               ┌────────────────────────┐            │
               │   Interactive Target   ├────────────┤
               │   (Component Focus)    │            │
               └───────────┬────────────┘            │
               Up / Down   │                         │
       (Walk Sibling /     │                         │
        Component Action)  ▼                         │
               ┌────────────────────────┐            │
               │    Browse Focus        ├────────────┘
               │  (Viewport Scroll)     │
               └────────────────────────┘
```

| Focus Domain | Visual Indication | Key Ownership | Primary Gestures |
|---|---|---|---|
| **Composer (Prompt)** | Composer bright, active cursor blinking, metadata visible | Composer input line | Printable text inserts; `↑`/`↓` move caret or recall history at boundaries; `Home`/`End` move caret to line-start/end; `Enter` sends |
| **Interactive Target** | Focused card highlighted with reverse accent; Composer dimmed; caret hidden | Focused component (`ToolStep`, `Thinking`, `CommandResult`, `Notice`) | `Alt+↑`/`Alt+↓` (and bare `↑`/`↓`) walk targets; `Enter` activates/toggles; `y`/`c` copies content; `Esc` clears focus; Printable text bounces to prompt |
| **Browse Focus** | Composer dimmed; caret hidden; viewport border/recess adjusted | Transcript viewport | Bare `↑`/`↓` line-scroll; `PgUp`/`PgDn` page; `Home`/`End` jump to top/bottom; `Esc` or clicking composer clears browse focus |

---

## 3. Four Core Architectural Invariants

### Invariant 1: Visual-Action Parity (VAP)
Visual rendering and keyboard event routing MUST be a lossless, bidirectional reflection of the exact same state machine.
- If the Composer is visually dimmed and its cursor hidden, it is **structurally impossible** for any keypress to mutate the Composer draft or trigger history recall.
- A visual promise made to the user's eye ("attention is on this card") is strictly honored by the input dispatcher.

### Invariant 2: Intent Orthogonality (Lexical vs. Spatial)
Keys are classified strictly by their semantic intent:
- **Lexical / Generative Intent** (printable characters, paste): Always belongs to the draft. If pressed while a target or the transcript is focused, the TUI immediately bounces attention to the composer and inserts the content.
- **Spatial / Navigational Intent** (`↑`/`↓`, `Alt+↑`/`Alt+↓`, `Home`/`End`, `PgUp`/`PgDn`, `Esc`): Belongs strictly to the active focus container. Navigation keys never leak into the inactive composer.

### Invariant 3: Symmetric Bidirectional Navigation & Downward Edge Egress
Target navigation chords are strictly symmetric vectors:
- `Alt+↑` walks to the previous (older, upward) interactive target.
- `Alt+↓` walks to the next (newer, downward) interactive target.
- Stepping downward past the newest visible target at the bottom of the transcript naturally exits focus back to the Composer, mirroring physical spatial geometry.

### Invariant 4: Component Autonomy Protocol
When a component holds target focus, it possesses autonomous control over its localized verbs:
- `Enter`: Activates disclosure (expand/collapse) or enters zoomed sub-view (e.g. Runner task).
- `y` / `c`: Copies the component's representative content (tool output, thinking trace, command result) to the clipboard with an immediate toast notification, without modifying the composer draft.
- `Esc`: Yields control back to the composer.

---

## 4. Zero-Overhead Lifecycle Separation

A corollary of the Target Focus architecture is the strict decoupling of **Session State** from **Host Infrastructure**:

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Host Infrastructure (Long-lived)                │
│   Tool Pool · Provider Pool · Connection Storage · MCP Runtime · PAL   │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   │
                   Multiplexes across sessions
                                   │
       ┌───────────────────────────┼───────────────────────────┐
       ▼                           ▼                           ▼
┌──────────────┐            ┌──────────────┐            ┌──────────────┐
│  Session A   │            │  Session B   │            │  Session C   │
│ Ledger+Store │            │ Ledger+Store │            │ Ledger+Store │
└──────────────┘            └──────────────┘            └──────────────┘
```

- A session is merely an append-only event ledger, a round counter, and an active turn lifecycle.
- Operations like `/new`, `/fork`, and opening sessions (`/sessions <id>`) MUST NOT re-initialize host tools, reload connection configurations from disk, re-probe OAuth credentials, or re-instantiate HTTP clients.
- If the resolved model and provider match the currently active instance, activation is a **sub-millisecond, memory-only state swap**.
