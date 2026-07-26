# ADR-0069: Pursuit budgets and runtime stats

- Status: Superseded by ADR-0083
- Date: 2026-07-17

## Context

`/pursue` (ADR-0015) drove an autonomous loop with a marker-based stop-gate:
the model emits `[NEENEE_PURSUIT_COMPLETE]` and the harness keeps the turn going
until it does (or a hard 50-iteration cap). ADR-0010 had *deliberately removed*
pursuit-level token/time/turn accounting and the status machine, keeping only
`objective` + `is_complete`.

The comparison against `kimi-code`'s `/goal` showed the cost: neenee's pursuit
had **no budget, no stats, no convergence guidance, and no terminal reason**.
A runaway pursuit could burn unbounded tokens with no steering and no
accountability, and the user could not bound it ("stop after 20 turns"). This
is the single biggest behaviour gap versus the tool-driven `/goal`.

## Decision

Re-introduce budgets and stats **without** abandoning neenee's marker-based
simplicity (no LLM judge, no model-facing pursuit tools — ADR-0031 stands).

1. **`PursuitBudget`** (optional, opt-in) — `max_turns` / `max_tokens` /
   `max_wall_clock_ms`. Added to the durable `Pursuit` with `#[serde(default)]`
   so legacy snapshots still load. Set via the new `/pursue budget turns=N
   tokens=N time=Ms` subcommand (any subset; empty clears). Fuzzy expressions
   ("soon") never set a budget — explicit integers only.

2. **`PursuitStats`** (session-scoped, not persisted) — `turns` / `tokens` /
   `wall_clock_ms`, zeroed on arm and accumulated each continuation round via a
   new `Agent::book_pursuit_turn` called at both turn-exit gates. Surfaced in
   `/pursue status` and the stop summary.

3. **Budget enforcement** — `PursuitState::continuation` consults the budget
   before forcing another round; when any axis is exceeded it stamps a
   `terminal_reason` on the pursuit ("turn budget reached (20/20)"), disarms,
   and stops. The `Ok(false)` arm surfaces the reason + a usage summary.

4. **Convergence reminder** — when a budget is ≥75% consumed, an authoritative
   `<system-reminder>` (ADR-0068) steers the model to finish in-flight work and
   emit the marker rather than starting new optional work — mirroring
   `kimi-code`'s budget-band guidance.

5. **`terminal_reason`** — a free-form `Option<String>` on `Pursuit` recording
   why a non-completion stop happened, surfaced in `format_pursuit_status`.

## Consequences

- A pursuit is now bounded and accountable: `/pursue budget turns=10` caps it,
  the stop summary names the cause, and `/pursue status` shows live usage.
- The marker-based completion model is preserved — the model still self-audits
  and emits the marker; the budget is a backstop, not a judge.
- Stats are session-scoped (rebuilt on resume from `iterations`), so no schema
  migration is needed; only the optional `budget`/`terminal_reason` join the
  durable record, both defaulted for backward compatibility.
- The convergence nudge reuses the ADR-0068 reminder channel, demonstrating the
  two-tier trust model in production.
