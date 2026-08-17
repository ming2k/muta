# 0111. Transcript entry unification and concurrent rendering

- **Status:** Accepted
- **Date:** 2026-08-18
- **Revises:** [ADR-0106](0106-command-row-interaction-and-projection.md)
  (revises disclosure/inline layout into flat entry bodies),
  [ADR-0108](0108-one-command-component-input-output-lifecycle.md) (revises
  lifecycle representation into first-class entry granularity), and
  [ADR-0109](0109-command-card-and-triangle-disclosure.md) (revises
  disclosure marker requirement for command results)

## Context

Previous iterations treated slash commands and shell passthroughs as special-cased
cards with folding/disclosure behavior (`▸`/`▾` triangle markers in ADR-0109)
or inline joins (ADR-0106/0108). This introduced several friction points:

1. **Granularity mismatch**: A command is fundamentally a user-initiated input
   action, carrying the exact same structural weight and granularity as a
   conversational dialogue turn. Forcing command results into folding cards
   treated them as subordinate secondary artifacts rather than primary stream
   events.
2. **Reading friction from collapsible folding**: Collapsing multi-line command
   replies behind disclosure markers (`▸`/`▾`) broke natural terminal stream
   reading flow, requiring extra manual clicks or key presses to inspect
   deterministic command output.
3. **Multi-entry concurrency**: When a conversational turn (Entry A) is actively
   streaming tokens or executing tool steps, users need to be able to dispatch
   commands (Entry B) without blocking, modal deadlocks, or layout collisions.
   Both entries must be capable of rendering and updating concurrently on screen,
   with the downstream Entry B naturally respecting the dynamic vertical height
   expansion of Entry A.

## Decision

### 1. Unified `Entry` mental model

The transcript is formalized as an ordered stream of **Entries** (`TranscriptEntry`):

- **Turn Entry**: Initiated by a user prompt (or system turn). Contains a
  distinct Header (`> user prompt · HH:MM` or `> turn N · model · HH:MM`)
  followed by the conversational/reasoning body (thinking traces, tool steps,
  assistant text).
- **Command Entry**: Initiated by a slash command or shell passthrough. Contains
  a generic Header (`⌘ command · HH:MM` or `❯ command · HH:MM`) where both glyph
  and label share the indicator color, followed by its body containing the
  concrete invocation (`/autopilot on`, `!cargo test`) and unfolded result content.

Every Entry shares the universal structure:
```text
┌─────────────────────────────────────────────────────────────┐
│ Header: [Glyph] [Category/Role] · [HH:MM]                   │  ← Boundary & Indicator (e.g. ⌘ command · 00:41)
├─────────────────────────────────────────────────────────────┤
│ (1 blank row gap - universal entry design constraint)       │  ← 1-Row Gap (TURN_HEADER_BODY_GAP_ROWS)
├─────────────────────────────────────────────────────────────┤
│ Body:   [Concrete Invocation / Direct Result / Stream]      │  ← Unfolded Content
└─────────────────────────────────────────────────────────────┘
```

### 2. Overturn folding: clean header and direct body rendering

Command entries discard both the card identity bar (`┃`) and the collapsible
folding mechanism (`▸`/`▾`):
- **Header**: Renders as `⌘ command · HH:MM` (or `❯ command · HH:MM`)
  where `⌘ command` shares the bold indicator tone (`info` for slash, `ok` for shell).
- **Universal Header-Body Gap**: Exactly 1 blank row (`TURN_HEADER_BODY_GAP_ROWS = 1`)
  separates the Entry header and its body content, preserving the universal
  transcript typography rhythm.
- **Body**: Displays the concrete command invocation (e.g. `/autopilot on`),
  followed by the result blocks when completed.
- When **Pending** (`CommandPhase::Pending`), the Entry renders its Header,
  the 1-row gap, and the invocation in muted running style.
- When **Completed** (`CommandPhase::Completed`), the Entry renders its Header,
  the 1-row gap, the invocation in active style, and the result body blocks.

### 3. Concurrent updates and multi-entry layout mechanics

The runtime and TUI layout engine support concurrent rendering across different
entry types:
- While a Turn Entry (Entry A) is in flight (`is_responding` / streaming),
  dispatching a Command Entry (Entry B) is permitted.
- Entry B is appended below Entry A in the transcript.
- As Entry A receives additional streamed tokens or tool events and its height
  grows, the layout engine stages heights dynamically:
  $$\text{Y}(\text{Entry B}) = \text{Y}(\text{Entry A}) + \text{Height}(\text{Entry A}) + \text{Gap}$$
- Both entries update in real time without layout collision or lock contention.

### 4. Projection rebuilding and caching

Incremental layout caches (`HeightCache`, `VirtualLayoutIndex`) operate at Entry
boundaries. Streaming updates dirty only the active entry, leaving settled
entries cached for $O(\text{visible})$ rendering performance.

## Alternatives considered

- **Retain collapsible `▸`/`▾` cards for commands**: Rejected. Added visual
  noise and interaction friction without benefit in terminal environments.
- **Block command dispatch during active turns**: Rejected. Users must be able
  to inspect status, query context, or run independent tools while long-running
  agent turns progress.
- **Name the abstraction `Interaction`**: Rejected. Most transcript volume
  consists of unidirectional streaming outputs and tool traces rather than
  two-way conversational interactions. `Entry` accurately models the ledger
  nature of the transcript.

## Consequences

**Positive:**
- Complete symmetry between dialogue turns and command invocations.
- Clean terminal reading flow with zero unnecessary disclosure clicks.
- Smooth concurrent execution and rendering while agent rounds are in flight.
- Clean projection and height cache boundaries.

**Neutral:**
- Command card disclosure tests update to assert direct unfolded rendering.

## References

- [ADR-0106](0106-command-row-interaction-and-projection.md) — command row layouts.
- [ADR-0108](0108-one-command-component-input-output-lifecycle.md) — command component lifecycle.
- [ADR-0109](0109-command-card-and-triangle-disclosure.md) — command card appearance.
- [ADR-0110](0110-commands-do-not-trigger-the-activity-bar.md) — command activity decoupling.
