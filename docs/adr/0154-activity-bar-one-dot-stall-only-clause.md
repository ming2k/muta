# 0154. Activity-bar semantics: one breathing dot, stall-only silence clause

- **Status:** Accepted
- **Date:** 2026-08-30

## Context

Two complaints landed together on the activity bar (the transient row above
the input box, `apps/tui/crates/mutx/src/chrome.rs`):

1. **Two indicator glyphs.** `● ██ ` — the breathing dot was followed by a
   two-cell block-density "micro-meter" (`meter_cells`), a decaying
   histogram of recent `StreamDelta` pressure. Two glyphs telling one story
   is noise; the meter's density steps read as visual clutter, not data.
2. **Italic chrome.** The master status label (and the Activity modal's
   live status) used `Modifier::ITALIC`. In a monospace grid oblique type
   reads as content emphasis — quotes, math, aside text — the wrong
   register for chrome, and it fights the upright status vocabulary
   everywhere else in the footer.

Underneath the styling, the `BytePulse` module's *purpose* had drifted. It
maintained two decay channels (fast ~0.4s / slow ~1.6s) to drive dot
luminance and the micro-meter — i.e. "is output *continuous*" — while the
signal a user actually needs, "the HTTP request is open, the connection is
held, but no token has arrived for a long time", was a side effect
(`silent_secs`, armed only after deltas had flowed, threshold 8s, and only
in `Thinking`/`Answering`). The genuinely suspicious long-quiet case — a
fresh request whose first byte is overdue — was deliberately excluded.

## Decision

1. **One glyph.** The row's only indicator is the breathing dot (`●`,
   `spinner_glyph`). The micro-meter, `meter_cells`, `METER_STEPS`, the
   `EMBER_FLOOR` luminance blend, and `Liveness::Flowing` are retired.
   `Liveness` collapses to `Breathing | Gated` (gate = pending human
   decision, static amber).
2. **No italic chrome.** The master label and the Activity modal status
   render upright. Italic remains reserved for content (quoted text, math
   spans), never chrome.
3. **`BytePulse` → `TokenWatch`, semantics inverted to stall-only.** The
   module now answers exactly one question — *has the held connection gone
   quiet too long?* — with a single last-activity stamp and an armed flag:
   - `arm(now)` on every new model-request cycle (`TurnStarted`);
   - `note_token(now)` on every delta;
   - `stalled_secs(now) -> Option<u64>` past the applicable threshold.
4. **Two regimes, two thresholds.**
   - First byte of a fresh request (`saw_token == false`):
     `FIRST_BYTE_AFTER = 45s`. TTFT is routinely slow on reasoning models;
     the client's request timeout remains the real backstop, so this is a
     courtesy heads-up, rendered as `· no tokens 52s`.
   - A stream that flowed and went quiet (`saw_token == true`):
     `STREAM_SILENT_AFTER = 8s` (unchanged). Any token proves the
     connection is live, so a gap here is the exact held-connection case;
     rendered as `· silent 9s`.
5. **Gating follows the regime.** The clause is computed while a model
   request is open — `AwaitingModel | Thinking | Answering` — not only in
   the streaming phases. Tool execution has no HTTP stream that could go
   silent; its liveness story is the step clock, so it stays excluded.
   Transport retries still own the annotation slot first when counting
   down.

## Alternatives considered

- **Keep the micro-meter, drop the dot.** Rejected: block density is a
  poorer liveness signal than motion, and dot color already carried the
  same decay data.
- **Byte-driven dot luminance only (no meter).** Rejected halfway through:
  luminance deltas at terminal frame rates still read as shimmer, and it
  kept the "continuous output" semantics alive that the product question
  retired.
- **One threshold for both regimes.** Rejected: 8s on first byte would cry
  wolf on every reasoning model; 45s on a mid-stream gap would hide the
  case users actually flag.
- **Wire-level stall detection (per-request watchdog in the client).**
  Out of scope: the client already has `CHAT_REQUEST_TIMEOUT`; the bar's
  clause is presentation, and the phase-gated stamp is sufficient input.

## Consequences

- The bar reads `● thinking · no tokens 52s [1:12]  Esc Esc interrupt` —
  one glyph, upright text, and the one annotation that matters.
- `pulse_levels` leaves `ActivityBarView` and `TranscriptView`; the dot's
  appearance is a pure function of gate state and the animation clock.
- Continuous-output feedback (tokens/sec shimmer) is gone by design. Rate
  remains available where it is measured, not inferred: the model bar's
  stream-rate segment and the Performance report (ADR-0151).
- Snapshot expectations for the meter cells were never pinned (all
  constructors passed `pulse_levels: None`), so no snapshot churn.

## References

- ADR-0008 (single breathing anchor) — this ADR narrows the anchor to one
  glyph and re-points its meaning.
- ADR-0151 (request performance telemetry) — where stream-rate truth lives.
- ADR-0153 (shell watchdog budgets) — the sibling server-side decision
  about how long quiet work deserves before anyone calls it stuck.
