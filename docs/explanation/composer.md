# Composer and Input Architecture

mutx uses a unified input surface — the **Composer** — at the bottom of
the terminal screen. This page explains *why* the composer is designed as
a multi-modal yet non-modal control plane, how its intent state machine
operates, how two-tier completion eliminates render flicker, and how
attachments, history pointers, and caret ownership are coordinated across
the view and shell layers.

For the visual layout, color tokens, and keymap specifications, see the
[Input box reference](../reference/tui/input-box.md). For the underlying
rendering engine and terminal lifecycle, see [Terminal UI](tui.md).

## Why a unified composer

In a conversational agent interface, user input serves several distinct
operational purposes:

- Sending a new prompt to start an agent round.
- Invoking a local slash command (e.g. `/plan`, `/reset`, `/models`).
- Mid-round steering to interrupt or redirect a running turn.
- Mid-round follow-up staging into the outbox queue.
- In-place editing of already-queued messages.
- Staging large pasted code blocks or images behind compact identifiers.
- Supplying fuzzy queries for modal filters (history, models, connections).

A naive design might split these into separate input fields, modal prompts,
or `vi`-style modal zones (insert mode vs command mode). mutx rejects modal
zones in favor of a **single unified surface with an intent-driven state
machine**. The user always types in the same visual location; the system
derives the consequence of `Enter` dynamically and communicates it
unambiguously through a single hint sentence.

## The intent-driven state machine

Every frame, the composer derives its active **compose target** from the
current input buffer, session execution state, and outbox pointer:

| Compose Target | Condition | Consequence of `Enter` | Hint Row Verb |
|----------------|-----------|------------------------|---------------|
| `Prompt` | Session idle, plain prose | Starts a new agent round | `Enter send prompt` |
| `Command` | Buffer starts with recognized `/` | Executes local slash command | `Enter send command` (Brand bold) |
| `Steer` | Session busy, steer mode armed | Interrupts round at safe boundary | `Enter send steer` (Amber warning) |
| `FollowUp` | Session busy, follow-up armed | Appends message to outbox queue | `Enter send follow-up` (Info blue) |
| `QueueEdit` | Armed queue pointer on item `#N` | Updates queued item in place | `Enter update follow-ups[N]` |
| `Completion` | Open completion popup | Commits selected candidate | `Tab / Enter select` |
| `HistorySearch`| Ctrl+R panel active | Inserts matched history entry | `Tab / Enter insert` |

### The `Tab` delivery toggle

While an agent round is running, the user can compose text without waiting
for the model to finish. Pressing `Tab` toggles between two delivery modes:

1. **Steer mode** (Amber accent): `Enter` delivers an interrupt-and-steer
   payload at the next turn boundary.
2. **Follow-up mode** (Info blue accent): `Enter` pushes the message into
   the queued outbox, executed sequentially after the active round finishes.

The hint row always displays the action of `Enter` and uses its trailing
clause to show the alternative mode (`Enter send steer  Tab follow-up mode`).
The verb hue strictly echoes the delivery consequence so the user cannot
accidentally steer when intending to queue.

### Intent state invariant

A critical invariant of the composer state machine is that **intent is
bound to the domain prefix, not transient cache availability**.

When an input buffer starts with `/`, the composer is strictly classified
in the command domain (`Completion` or `Command`). It is forbidden for a
`/`-prefixed buffer to transiently collapse to `Prompt` while background
completion data is fetching. This invariant ensures that hint bars and
palettes never flash or jitter during typing.

## Zero-latency two-tier completion

Autocomplete in the composer covers two distinct candidate domains:

1. **Harness & Slash Commands**: Fixed catalog of commands, subcommands,
   and descriptions.
2. **Filesystem Mentions (`@path`)**: Dynamic project directory scans and
   file paths.

To eliminate frame drops and transient flicker when typing commands, mutx
implements a **Two-Tier Completion Pipeline** with Stale-While-Revalidate
(SWR) cache retention ([ADR-0162](../adr/0162-zero-latency-two-tier-completion-and-flicker-free-composer.md)):

```text
Keystroke Event
      │
      ├─► Tier 1: Synchronous Pure Domain (0ms, Frame 1)
      │     └─ Matches CommandCatalog in-memory
      │     └─ Authoritative candidates painted on current frame
      │
      └─► Tier 2: Asynchronous Daemon IPC (Background)
            └─ Scans filesystem for @path mentions
            └─ Retains previous completions + optimistic client-side narrowing
            └─ Atomically swaps candidate list on arrival (monotonic generation)
```

- **Tier 1 (Synchronous Fast-Path)**: Slash command matching is pure
  in-memory string prefix computation. It runs synchronously within the
  input event handler, guaranteeing 0ms latency and instant candidate
  rendering on Frame 1 without waiting for daemon IPC.
- **Tier 2 (Asynchronous Slow-Path)**: Dynamic workspace path scanning
  runs in the background service to keep the 60/120 FPS event loop free of
  disk I/O.
- **Cache Retention**: Keystrokes never wipe the completion cache. In-flight
  requests retain the prior candidates and apply client-side prefix
  filtering until the newer daemon response atomically updates the state.

## Multi-slot history and outbox pointer model

The `↑` and `↓` arrow keys treat the composer as a pointer over three
distinct slot tiers:

```text
┌────────────────────────────────────────────────────────┐
│ 1. Draft (Unsent Input Slot)                           │  Newest
│    • Current live typing                               │
│    • Preserved across ↑ / ↓ navigation excursions      │
│    • Restored automatically on pre-response interrupt  │
├────────────────────────────────────────────────────────┤
│ 2. Queue Outbox Slots (Editable Projections)           │
│    • Steer / follow-up items staged for delivery       │
│    • Enter commits modifications in place              │
├────────────────────────────────────────────────────────┤
│ 3. History Snapshot Slots (Read-only Records)          │  Oldest
│    • Session prompts backfilled from transcript file   │
│    • Persisted global history across sessions          │
└────────────────────────────────────────────────────────┘
```

### In-place queue editing

When the outbox queue contains staged items, pressing `↑` enters the
**queue pointer** rather than jumping directly to historical sessions.
The composer loads the queued item's text, and the top breathing row
displays a contextual badge (e.g. `[edit: follow-up #1 · draft saved]`).

Pressing `Enter` commits the modified text **in place** within that item's
existing queue slot. It never duplicates or re-orders the queue. Pressing
`Esc` or navigating down past the newest queue item restores the stashed
live draft untouched.

### Pre-response unsend draft recovery

When an agent round is interrupted before the model has produced any
output tokens (Phase-1 interrupt), the prompt was never answered. The
harness rolls back the turn and returns the prompt text to the frontend.
The composer adopts this text back into the draft slot, provided the user
is not actively typing a new draft (`DraftAdoption::OnlyIfIdle`).

### Session scoping and transcript backfill

History navigation via `↑`/`↓` is strictly scoped to the active session:

- When attaching to or resuming an existing session, mutx backfills prompt
  history directly from the session's durable transcript file. Prompts
  typed on other terminal clients or prior daemon connections are recallable
  immediately.
- Global cross-session history is reserved for `Ctrl+R` (`HistorySearch`),
  which fuzzy-searches across all sessions and workspaces.

## Caret ownership and semantic selection

The terminal caret has a single authoritative owner determined each frame
by the `caret_owner()` state machine:

- `CaretOwner::Composer`: Normal chat drafting or when floating panels
  (like History search) borrow the composer line.
- `CaretOwner::Modal`: Dedicated text inputs inside modals (e.g. API key
  entry, Question modal's custom "Other" text row).
- `CaretOwner::None`: When transcript steps have focus, when browsing
  read-only modal lists, or when viewing runner tasks.

### Single source of truth: `cursor_screen_pos`

Caret placement on the terminal screen is computed once per frame in
`cursor_screen_pos`. Both the visual highlight and the physical terminal
cursor position (required for host IME window alignment) derive from this
same function, preventing cursor drift across wrap boundaries.

### Selection relay

When a user selects text within the composer using mouse drag or triple-click,
the selection is registered under `INPUT_MSG_IDX` in the semantic layout map.
While a selection is active, the block caret is visually hidden.

The next direction key (`←`, `→`, `Home`, `End`) adopts the caret at the
selection's **head** (the release point) or **tail** (the anchor point) and
clears the selection in a single gesture, preventing lost cursor state.

## Attachment staging and chip lifecycle

When pasting large text snippets (≥4 lines or ≥500 bytes) or image files
from the clipboard, mutx stages the payload behind an inline **chip**
rather than flooding the text box:

- Large text pastes: `[Pasted text #1 +42 lines · 12.5 KB]` (Info blue)
- Image pastes: `[Image #1 · 24.1 KB]` (Warning amber)

### Payload-backed integrity

Chips are rendered as colored pills only when backed by an actual staged
payload in memory (`#N <= pending_count`). An orphan chip label typed
manually or left over after undo renders as plain text and is stripped
before dispatch. This ensures the model context never receives invalid or
fabricated attachment references.

A single `Backspace` or `Del` landing on a chip deletes the entire chip and
its trailing delimiter in one stroke.

## Zero-jitter breathing chrome and overflows

The composer panel is designed with a fixed 4-row baseline for single-line
inputs:

```text
┌ row 1 ─ Top breathing row (Mode badge or ↑ N lines overflow) ──┐
│ › prompt text...                                               │ ← row 2: text
├ row 3 ─ Bottom gap row (↓ N lines overflow) ───────────────────┤
│   Enter send prompt                               1–3/12 lines │ ← row 4: hint & position
└────────────────────────────────────────────────────────────────┘
```

To prevent UI jitter when multiline drafts scroll beyond the viewport:

- Rows 1 and 3 double as **quantified overflow indicators** (`↑ 4 lines` /
  `↓ 12 lines`) when content is clipped above or below.
- Reusing existing padding rows means the overall panel height does not
  change when text crosses the scroll boundary.
- The composer height dynamically expands with wrapped text up to a maximum
  cap of 50% of the terminal height, ensuring the transcript history is
  never fully obscured.

## Surface borrowing and draft parking

Overlay pickers (such as the Model selector and Connection manager) borrow
the live composer line as their active fuzzy search field.

To ensure zero draft loss:

1. When opening a picker, `park_draft_into(panel_id)` stashes the current
   composer text and attachments into that panel's state slot.
2. Keystrokes filter the picker's list in real time.
3. When closing the picker or completing the selection,
   `restore_draft_from(panel_id)` returns the stashed draft to the composer,
   allowing the user to resume typing seamlessly.

## Where the code lives

| Subsystem | Source Location | Responsibility |
|-----------|-----------------|----------------|
| Composer View | `apps/tui/crates/mutx/src/composer.rs` | Retained panel drawing, text wrapping, chip styling, layout map recording |
| App Composer State | `apps/tui/crates/mutx/src/app/composer.rs` | Caret ownership, selection adoption, draft parking, Esc/Ctrl+C timers |
| Hint Row Component | `apps/tui/crates/mutx/src/components/composer_hints.rs` | `ComposeTarget` derivation, hint row sentence construction, consequence coloring |
| Attachment Chips | `apps/tui/crates/mutx/src/composer_attachments.rs` | Chip parsing, payload staging, label formatting, atomic deletion |
| Two-Tier Completion | `apps/tui/crates/mutx/src/completion.rs` | Synchronous Tier 1 fast-path, SWR cache retention, daemon IPC integration |

## References

- [ADR-0038: In-house grid + diff rendering engine](../adr/0038-in-house-grid-diff-rendering-engine.md)
- [ADR-0126: Unified outbox and delivery queues](../adr/0126-unified-outbox-and-delivery-queues.md)
- [ADR-0162: Zero-latency two-tier composer completion and flicker-free rendering](../adr/0162-zero-latency-two-tier-completion-and-flicker-free-composer.md)
- [Input Box Reference](../reference/tui/input-box.md)
- [Terminal UI Explanation](tui.md)
