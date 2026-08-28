# 0156. Ctrl+S claim for the performance report: bounded XON/XOFF exposure

- **Status:** Accepted
- **Date:** 2026-08-28

## Context

ADR-0126 moved the queue family onto the Ctrl row and, in its
*Alternatives considered*, explicitly rejected `Ctrl+S` as a candidate:

> `Ctrl+S` collides with XON/XOFF flow control in terminals that re-enable it.

Later, the model-bar drill-downs (ADR-0151's Performance report) claimed
`Ctrl+S` anyway: `Ctrl+O` opens the context usage report and `Ctrl+S` the
latest-turn performance report (`crates` path:
`apps/tui/crates/mutx/src/keymap.rs`, the `GLOBAL_BINDINGS` registry). The
binding shipped with only an in-code comment arguing the chord is safe
because nothing in this TUI uses it as "save" — leaving the registry in
unresolved tension with ADR-0126's recorded rejection.

This ADR resolves that tension by recording the actual risk analysis and the
decision to keep the chord.

## Decision

Keep `Ctrl+S` bound to *open the latest-turn performance report*
(`Action::OpenPerformanceReport`, gated `Gate::NoModal`), with the following
risk finding: the XON/XOFF exposure ADR-0126 cited is real but **bounded to
environments that re-enable flow control on the app's own TTY**, and the
failure mode is a silent no-op on a read-only drill-down.

The mechanics, in order of the byte's path:

1. crossterm's raw mode is `cfmakeraw`-based, which clears `IXON` on the
   app's controlling TTY. In the default configuration the kernel line
   discipline does not interpret `0x13` as XOFF; the byte is delivered to
   the app as `KeyCode::Char('s')` + `CONTROL`.
2. Under tmux/screen, the multiplexer runs the inner program on a pty it
   also configures raw, and the outer terminal is likewise in raw mode for
   the multiplexer's own use — the byte crosses both layers as data.
3. The residual exposure is therefore limited to setups that re-enable
   `IXON` around the app (a wrapper that restores cooked mode, an emulator
   with local flow-control interception, `script`-style wrappers). There the
   chord never arrives — same class of environmental loss as the F-row
   dispatch ADR-0126 moved the queue family away from.

What makes the asymmetry acceptable where ADR-0126's queue bindings were
not: **cost of loss vs. cost of the gesture**. ADR-0126's family includes
`Ctrl+P` block/resume — a time-sensitive control the user may depend on
*while a turn is running*; a silently eaten chord there fails invisibly at
exactly the moment it matters. The performance report is a passive,
read-only drill-down of the last completed turn: nothing is lost but the
shortcut, the model bar's rate-gauge click target remains the fully
portable twin, and `?`-Help keeps advertising the chord with no correctness
dependency on it firing.

## Alternatives considered

- **Honor ADR-0126's rejection and move the binding.** Every candidate has a
  worse cost: the free Ctrl letters are readline-claimed (`Ctrl+D`
  delete-char/EOF, `Ctrl+N` next-history, `Ctrl+Y` yank — the composer keeps
  the readline family per `keymap.rs`'s scope note), `Ctrl+G` is
  byte-collided with readline abort without the Kitty protocol (the reason
  `/btw` stayed on `F5`), `Ctrl+I`/`Ctrl+M` are Tab/Enter in legacy
  protocols, and the F-row is exactly the environmental loss ADR-0126
  rejected. `Ctrl+S` is the only mnemonic slot ("s" for "speed") whose
  worst case is a no-op on a non-destructive surface.
- **Bind it but hide it from Help until Kitty/XON safety is probed.** No API
  exists to probe `IXON` at runtime; hiding a working chord on modern
  terminals to protect legacy ones inverts the audience. Rejected.
- **Drop the keyboard twin entirely, keep the click target.** Rejected: the
  model bar's gauges are width-pressured (first to drop under narrow
  terminals), so the keyboard path must exist.

## Consequences

- The registry keeps `Ctrl+S → OpenPerformanceReport`; Help and the model
  bar render it from the one registry, so the vocabulary cannot drift.
- ADR-0126's alternatives table remains historically accurate (it rejected
  `Ctrl+S` *for the queue family*, on time-sensitivity grounds this ADR
  does not contradict but scopes); no supersession is needed.
- If a future binding wants `Ctrl+S` for a stateful or time-sensitive
  action, it must displace the performance report *and* re-litigate this
  ADR — the loss-cost analysis above is what would have to be answered.
- `docs/reference/tui/model-bar.md` keeps documenting the chord and the
  click twin; no user-facing copy changes.

## References

- ADR-0126 (the queue-family Ctrl-row move and the original XON/XOFF
  rejection), ADR-0151 (the Performance report surface the chord opens)
- `docs/reference/tui/model-bar.md` (gauge click twins, keycap hints)
- `apps/tui/crates/mutx/src/keymap.rs` (`GLOBAL_BINDINGS`), `apps/tui/crates/mutx/src/lib.rs`
  (Kitty `DISAMBIGUATE_ESCAPE_CODES` push, raw-mode setup)
