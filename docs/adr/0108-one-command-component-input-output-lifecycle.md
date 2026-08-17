# 0108. One command component: input and output in one row, with a lifecycle

- **Status:** Accepted
- **Date:** 2026-08-17
- **Revises:** [ADR-0106](0106-command-row-interaction-and-projection.md) §2
  (placement and per-row view state are unchanged; the row's *identity* and
  live lifecycle are revised), and the live-echo part of
  [ADR-0091](0091-command-ledger-and-typed-results.md) D4

## Context

ADR-0106 fixed the marker and made the transcript a projection, but a command
still arrived on the surface as **two unrelated rows**:

1. The invocation was echoed optimistically as a normal **user message**
   (origin `Slash`) — a full `▌ cmd · 21:39` header plus a user panel
   containing the literal `/autopilot on`.
2. The reply arrived later as a **`CommandResult` row** (`Role::Tool`), the
   compact `⌘`/`❯` line.

Reading a transcript with a command in it therefore required joining two
visually unrelated rows across a seam, and the pair carried the same
information twice (the invocation appeared in both). Worse, the Disclose
layout routed its header through the *reasoning-trace* renderer, so a
multi-line result such as `/autopilot on`'s long ack rendered as a naked

```text
+ /autopilot · 21:39
```

— no `⌘` glyph (unlike the Plain/Inline layouts), and a bold unmuted
timestamp welded into the summary. Three shapes for one concept, two rows per
command, no running state: a command in flight was invisible until it
finished, because `CommandResult` was atomic.

The user-visible defect this ADR answers: *把输入和输出做成一个组件，因此这个
组件存在运行和完成两种状态，同时一个组件能展示完整的效果，防止割裂或者弄乱
transcript 的阅读体验的上下文。*

## Decision

### 1. One component owns both halves

The command **component** is a single transcript row that owns the invocation
(the input) and the result (the output). The invocation is never echoed as a
user message on the live path: dispatching pushes one optimistic
`MessageKind::CommandResult` row whose `raw` is the invocation, and the reply
settles **that same row in place** (same message id, so scroll position,
focus, and selection survive). One command, one row, live and after resume.

The span grammar is one grammar everywhere:

```text
[marker] ⌘ /name args · 21:39 [· inline reply]
```

- `⌘ ` (slash, info tone) or `❯ ` (shell, ok tone) — the component's lead.
- The marker `+`/`-` leads **only** when a body exists to disclose
  (ADR-0106's truthfulness rule, unchanged) — and now also *phase-gated*:
  a pending row shows no marker, because there is no output yet.
- `· HH:MM` stays muted in every state; the row's disclosure × interaction
  tone ladder is carried by the invocation span alone. (Previously the
  Disclose layout fused the time into the bold summary, and the marker came
  from the reasoning renderer with no glyph at all.)

`Disclose` no longer reuses `draw_reasoning_summary`; command rows build
their own line (`command_summary_line`) and keep their own `BlockRegion`
registration, as they already did for Plain/Inline.

### 2. Two states: `Pending` → `Completed` (or `Cancelled`)

`MessageKind::CommandResult` gains a `phase: CommandPhase`:

| Phase | When | Render |
|-------|------|--------|
| **Pending** | Dispatched, no result yet | `⌘ /autopilot on` in the muted running tone — no marker, no reply |
| **Completed** | The typed result arrived (or is known not to exist) | `⌘ /new · Started new session: a1b2c3` inline, or `+`/`-` disclosure for long/multi-line replies |
| **Cancelled** | The dispatch cycle ended with no reply (modal/picker/side-view commands emit no `RoundEvent::CommandResult`) | like a result-less completed row — settled, not promising |

Settle is one-shot and idempotent: `settle_command_result` only transitions
`Pending → Completed`, re-deriving the parsed body blocks from the typed
result. If a reply arrives for a row that is not pending (or for a command
this view never dispatched — a transcript rebuild, or a daemon reply racing a
fresh attach), the handler pushes a fresh completed row instead, so a reply is
never dropped.

Commands are synchronous control-plane operations, so the lifecycle stops
there — no permission-denied or interrupted states to represent, unlike a
tool step.

### 3. Projection parity on resume

The restore path folds a durable slash/shell **echo** (`Message::command_echo`
provenance, or the legacy `display_content`-is-the-literal-`/cmd` shape) out
of the dialogue *before* `merge_command_rows` interleaves the ledger rows —
exactly one row per command after resume, the same row the live path
produced. Classification in `transcript_message_from_core` is unchanged (the
Activity modal still needs to know a user row was non-driving); only the
list-level projection drops the echo.

`merge_command_rows` (ADR-0106 §2) is unchanged: ledger rows already carry
`record.timestamp` and land at their turn seams.

## Alternatives considered

- **Keep two rows, restyle them.** Rejected: no styling fixes the core defect
  — the invocation is duplicated across two rows joined by nothing, and a
  slow command still has no input-side surface until it finishes.
- **Defer the whole row until the result arrives.** Rejected: the optimistic
  input half is exactly what makes a slow `/search` feel dispatched; deferring
  reintroduces the invisible-in-flight gap.
- **Model the lifecycle in the ledger (`CommandRecord.status`).** Rejected:
  `Pending` is *view* state that exists for the ~milliseconds-to-seconds
  between dispatch and reply; the ledger's `CommandStatus` is the durable
  terminal record and stays untouched.

## Consequences

**Positive.**

- One row per command, live and after resume; the input is never duplicated.
- A pending command is visible and clearly in-flight (muted, markerless).
- The Disclose layout finally looks like the other command layouts (glyph,
  muted time) — no more naked `+ /cmd`.
- No schema change: `CommandRecord`/`CommandResult` untouched; the phase is
  projection-only view state, like `expanded`.

**Negative.**

- The live settle path matches a pending row by its invocation text; two
  identical commands dispatched in quick succession pair last-writer-first
  (mitigated by the one-shot settle and the fresh-row fallback).
- Resume relies on the fold classification; a legacy echo whose shape matches
  neither signal still renders as a bubble (pre-existing limitation, now
  rarer).

**Neutral.**

- `CommandPhase::Cancelled` rows render identically to `Plain` rows; the
  distinction is kept in the model for future affordances.

## Verification points

- Dispatching `/autopilot on` pushes one pending row (`⌘ /autopilot on`,
  no marker) which settles in place to `⌘ /autopilot on · Autopilot ON…`.
- A `/permissions` reply keeps `+`/`-` **with** the `⌘` glyph and a muted
  timestamp: `+ ⌘ /permissions · 21:39`.
- A modal command (`/models`) leaves a cancelled row, not a pending promise.
- Resuming a session with echoes + ledger renders one row per command, at the
  correct seams, and no `▌ cmd` bubbles.

## References

- [ADR-0091](0091-command-ledger-and-typed-results.md) — the ledger; D4's
  live rendering contract revised here.
- [ADR-0106](0106-command-row-interaction-and-projection.md) — the
  shape-driven layouts and the projection model this builds on.
- [ADR-0088](0088-command-acknowledgment-toast-notices.md) — ack toasts,
  unchanged (the ledger twin renders as the component's inline reply).
