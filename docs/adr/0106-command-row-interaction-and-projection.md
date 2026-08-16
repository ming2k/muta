# 0106. Command rows: interaction by shape, and the transcript projection

- **Status:** Accepted
- **Date:** 2026-08-16
- **Revises:** [ADR-0091](0091-command-ledger-and-typed-results.md) (D4 live
  rendering contract — the ledger model is unchanged; only the row's
  presentation and interaction surface are revised)

## Context

ADR-0091 made the command ledger the source of truth and rendered every
command as one shape: a disclosure block whose collapsed header read

```text
+ ⚙ /new
```

Two defects surfaced in use:

1. **A false affordance.** The `+` promised an expansion, but for most
   commands (`/new`, `/permissions` with one rule, `/schedule`, acks) the
   body was a single short line — expanding bought the user at most one
   row they could have read inline. The `⚙` glyph restated the same
   non-information a second way (and meant nothing distinct: it labeled a
   `/search` the same as an `!ls`). A disclosure marker is a promise; it
   must only appear when there is a second view worth opening.
2. **Interaction debt.** Because only the header row was clickable, the
   *content* of a command reply — the thing the user actually typed the
   command to get — was unreachable by pointer and invisible until
   expanded.

The deeper question these expose: ADR-0091 settled *where command records
live* (a ledger; the message stream is pure dialogue) but left the live
surface looking like a message. That conflation is what produced both
symptoms.

## Decision

### 1. Interaction follows shape (`CommandRowLayout`)

Command rows classify at render time into one of three layouts, and the
marker/glyph question dissolves — no row shows `⚙`, and `+`/`-` appears only
when it is true:

| Layout | When | Render | Pointer on row |
|--------|------|--------|----------------|
| **Plain** | No result (shell passthroughs, legacy folds) | `!ls -la` dimmed | copy (nothing to expand) |
| **Inline** | Single-line reply that fits beside the invocation | `/new · Started new session: a1b2` | **expand to full reply** |
| **Disclose** | Multi-line or over-long reply | `+ /search foo` → body | **expand/collapse** |

The classifier (`command_row_layout` in `neenee-tui/src/model/document.rs`)
is width-aware: a reply joins inline only when `invocation · reply` fits the
band without truncation, so an inline reply is never a fragment. The join is
the R1 attribute dot (` · `) from the visual-language ladder — the reply is
the outcome of the invocation, its attribute. `Plain` and `Inline` rows push
no sticky summary (a one-line body has nothing to pin).

An `Inline` row's toggle expands to the full reply body (the same body a
`Disclose` row shows); if the reply is exactly the row's text the expansion
is a no-op height-wise, which is correct — the affordance never lies, it is
just redundant when the terminal is very narrow.

### 2. The transcript is a projection, and commands participate in it

The ledger remains the only durable record (ADR-0091 D1 untouched). But the
live transcript accepts command rows as first-class projection rows:

- **Placement.** A command row renders *in sequence, at the moment it
  happened* — after the user's `/cmd` echo row, before whatever the user
  did next — not appended to the tail of the dialogue.
- **Disclosure state is per-row view state.** `expanded`/`user_pinned` live
  on the `TranscriptMessage` projection (as ADR-0091 already placed them),
  never in the ledger: two windows on one session may disagree.
- **Model visibility.** Command rows are never sent to the model. The
  message stream stays pure dialogue; the model never sees `/compact` or
  its result (unchanged from ADR-0050/0091).
- **Export.** `/export` keeps the ADR-0091 blockquote form; `to_text()`
  remains the single text scheme.

This resolves the "追加还是不追加" dilemma by refusing the binary: append
*nothing* to the durable dialogue (no pollution, no compaction interactions,
no model-window coupling), but render *everything* in the projection (the
narrative survives resume — command rows rebuild from the ledger and
interleave by their `timestamp`).

## Alternatives considered

- **Drop commands from the transcript entirely (pure ledger).** Rejected —
  a resume that cannot answer "what did I run and what did it say" forces
  the user to re-run commands; the ledger would need its own dedicated
  surface before this could be safe, and ADR-0091 already chose the hybrid
  for exactly this reason.
- **Echo commands into the message stream (pre-ADR-0091).** Rejected — that
  is P1, the original transcript-purity problem; every conflation defect
  (compaction must skip them, wire filters must strip them, they render as
  user bubbles) returns.
- **Keep one shape, always `+`.** Rejected — a marker that sometimes lies is
  worse than no marker; users stop trusting the affordance everywhere.
- **Keep one shape, never `+`.** Rejected — a `/search` body (numbered hits,
  scores, quoted history) genuinely needs collapsing; inline-by-default with
  no way to fold would bury dialogue under command output.

## Consequences

**Positive.**

- The marker is truthful everywhere it appears; `⚙` stops double-encoding
  "this is a command" (the `/` already says so).
- Short replies are readable at a glance — the common case now costs zero
  interaction.
- The whole row is a click target, not a one-line header.
- No schema change: `CommandRecord`/`CommandResult` are untouched; the
  layout is derived, not stored.

**Negative.**

- Row height depends on terminal width (a `/new` confirmation inlines at 80
  cols, discloses at 40); snapshot baselines pin one width.
- Two code paths where there was one (disclose vs plain/inline), though they
  share the tone ladder and hit-testing.

**Neutral.**

- `CommandResult::Ack` records surface as `Inline` rows rather than
  duplicating their toast; toasts remain the ephemeral live surface
  (ADR-0088 unchanged).

## Verification points

- A `/new` result renders as `+ /new` collapsed → `Started new session: …`
  inline at conversational widths — no `⚙`, no false `+`.
- An `!ls -la` passthrough renders as a plain dimmed row.
- A `/search` result keeps the `+`/`-` disclosure and its body renders
  through the shared block renderer.
- Clicking/Enter on an `Inline` row expands the full reply body.
- Resume rebuilds command rows from the ledger with no round/turn position.

## References

- [ADR-0091](0091-command-ledger-and-typed-results.md) — the ledger this
  renders a projection of; D4 revised here.
- [ADR-0088](0088-command-acknowledgment-toast-notices.md) — toast surface
  for acks, unchanged.
- Visual-language ladder (`docs/reference/tui/visual-language.md`) — the R1
  ` · ` attribute join used by the `Inline` layout.
