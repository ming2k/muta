# 0153. Shell watchdog budgets and honest termination notes

- **Status:** Accepted
- **Date:** 2026-08-27

## Context

Two defects surfaced together in one incident: `cargo nextest run -p
aegis-command-panel 2>&1 | tail -20` died at "failed 1m 0s".

1. **The idle watchdog killed a healthy build.** `tail` emits nothing until
   its stdin closes, so the entire compile looked silent. The idle budget
   was `timeout/3` clamped to [5s, 60s]; the default 300s wall budget
   therefore granted only 60s of silence — and the run was killed at
   exactly 60s. The clamp existed to bound "waiting for stdin" latency,
   but it equally punished legitimate quiet work (compiles, network waits,
   pipe-buffered output).
2. **The model was told nothing.** `ToolOutput::to_text()` dropped the
   `termination` field entirely, so the model saw a bare `Exit -1` — no
   distinction between "your command failed" and "the harness killed a
   still-working command". Meanwhile the TUI footer carried a remedy
   hint (`--passphrase-file / SUDO_ASKPASS / \`y | …\``) written for the
   stdin case only, which read as noise when the actual cause was a
   compiling build. The explanation was in the wrong place, and wrong.

## Decision

1. **Idle budget: `timeout/3` clamped to [5s, 480s].** Eight minutes of
   silence is tolerated at the default budget. Prompt-blocking is still
   surfaced fast for short-explicit-timeout calls (the 5s floor), while
   long-budget callers are no longer killed at an arbitrary mark.
2. **Default wall timeout: 300s → 1800s (30 minutes).** A harness guard
   should bound runaway work, not race a legitimate full test suite.
3. **`to_text()` appends a termination note** (`[killed by harness: …]`)
   for `IdleBlocked` / `Timeout` / `Cancelled` / `InteractiveBlocked`,
   stating the fact (interrupted, not failed), the likely cause, and the
   remedy. The model is the actor that can act on this; the note is one
   block, no prose beyond it.
4. **TUI footers state the fact, sized to a human.** "killed by harness:
   no output within the idle-guard window — likely compiling,
   pipe-buffered output, or a stdin prompt." No flag recipes in the
   footer; the model note carries the retry guidance.
5. The legacy text-path marker classifier recognizes the new
   `[killed by harness` / `[not executed` prefixes so restored sessions
   highlight them like other structural markers.

## Consequences

- Quiet-but-working commands survive to completion at default settings.
- The model can distinguish harness kills from real failures and self-
  corrects (bigger `timeout`, non-interactive flags, or splitting work)
  instead of re-running the same doomed call.
- `Exit -1` in a transcript now implies "check the trailing note" rather
  than being the only signal.
- A genuinely stdin-blocked command takes up to 8 minutes to surface at
  default settings; callers who want fast failure can pass a small
  explicit `timeout` (which also shrinks the idle budget).
