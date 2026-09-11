# Activity bar

Transient activity indicator shown in the footer stack, directly above the
input box (below the [queue bar](queue-bar.md) and the tasks bar). It unifies
the live status label and the breathing-dot liveness anchor into one row.
Long-lived session-state flags (`DELEGATED` and friends) are deliberately
absent — they live on the dedicated [head row](status-bar.md) below the
model bar.

The row is a **status surface, not a control**: it has no click target and no
dedicated modal (a press on it is inert). The structural counters it used to
show live in the Session Stats modal (`Ctrl+O`) — see
[Round and turn](#round-and-turn).

## Appearance

```text
 ● waiting for model [23s]  retry 1/7 (next in 4s)       Esc Esc interrupt
 └─ master label     └─ elapsed  └─ transport clause      └─ fixed hint
```

The bar surfaces what the user most wants to know mid-round: the **master
label** (the typed phase) and the **elapsed** timer. During a provider backoff
a **warning-tinted transport clause** appears beside (never instead of) the
master label, counting down live: the workflow story ("waiting for model") and
the transport setback (`retry 1/7 (next in 4s)`) are separate channels and
never overwrite each other.

Segments are separated by plain whitespace (two columns before a clause, one
bracket-delimited block for the elapsed timer) — never a `·`, which the
[join ladder](visual-language.md) reserves for peers of equal rank. The
interrupt hint is right-aligned and separated from the content by at least two
columns.

| Attribute | Value |
|-----------|-------|
| Location | 1 row directly above the input box |
| Height | `ACTIVITY_BAR_ROWS = 1` while a round is active, 0 when idle |
| Glyph | `●` (the typography set's dot, `theme.glyphs.dot`), BOLD — the row's only indicator glyph (ADR-0154) |
| Glyph color | regime-dependent — see [the two liveness regimes](#the-dots-two-liveness-regimes) |
| Master label color | `theme.fg()`, BOLD, upright; `theme.warning` while a human decision is pending (italic was retired by ADR-0154 — oblique type is a content register, not chrome) |
| Elapsed format | `[23s]` / `[2m 07s]` / `[1h 05m]`, `theme.muted()` |
| Transport clause | two-space gap, `theme.warning` — `retry R/M (next in 4s)` counting down, then `retry R/M (running for 3s)` past the deadline |
| Indent | 1 column |

`R` counts **spent** provider attempts and `M` the retries the round is
allowed, both as the harness reports them (`attempt − 1` / `max_attempts − 1`),
so `retry 1/7` is the first retry of a round that may make seven.

### Width pressure

Two ladders run independently, and the master label is the last thing to give:

1. **Interrupt hint** — `Esc Esc interrupt` collapses to bare `Esc Esc`
   keycaps, then disappears (below ~22 columns for the words, ~12 for the
   keys). The label keeps a minimum 4 columns while the words fit.
2. **Content** — full clause → compact clause (`retry 1/7 (next in 4s)` →
   `(1/7)`, dropping the countdown tail) → clause gone → elapsed gone → label
   truncated with `…` (minimum 1 column).

## The clause's lifetime

The clause annotates one phase — `waiting for model` — and ends with it
(ADR-0235). It is published when the harness schedules a retry
(`RetryScheduled`) and retired by the **first phase write that says anything
else**, which is what makes it structurally impossible to outlive the attempt
it describes:

| Phase write | Clause |
|-------------|--------|
| `waiting for model` (the attempt in flight, backing off or retrying) | Live — `retry 1/7 (next in 4s)`, then `retry 1/7 (running for 3s)` |
| A fresh `RetryScheduled` while still waiting | Replaced in place (a later attempt, same slot) |
| `thinking` / `answering` / a tool verb / `finalizing response` / `awaiting permission` / `queued` / idle (`None`) | Gone — the request it described has landed, or the round ended |

Nothing else retires it: the response translator publishes it and has no clear
to forget, because a hand-maintained "events that mean the retry is over" list
is what once left a countdown beside `answering` for the whole of a streamed
answer. A retrying `/btw` aside owns its own clause (session-scoped chrome), so
it never appears on the primary view's bar.

## The dot's two liveness regimes

Exactly one mechanism drives the dot each frame (`classify_liveness` is a
pure function; gate wins over everything). The dot is the row's only
indicator glyph — the former two-cell block-density micro-meter and the
byte-luminance channel were retired by ADR-0154: continuous-output feedback
is noise on chrome, and the honest stream rate lives on the
[model bar](model-bar.md).

| Regime | When | Dot |
|--------|------|-----|
| `Breathing` | Default while a round runs (waiting for model, streaming, running a tool) | Classic slow breath (`breathing_color`) — seconds ticking are the only honest change to quote |
| `Gated` | Permission / ask_user pending | Static amber, no motion — paused for a human; animating would lie about who is working |

```text
 ● answering [2m 07s]                    Esc Esc interrupt   ← breathing
 ● making edits [12s]                    Esc Esc interrupt   ← breathing
 ● awaiting permission                   Esc Esc interrupt   ← gated
```

There is **no stall clause**. ADR-0154 also specified one (`pulse::TokenWatch`,
rendering `· no tokens 52s` / `· silent 9s` while a held connection went quiet);
that machinery was removed and nothing derives it today. The row's only
annotation is the transport clause above, so the annotation slot needs no
priority rule — there is nothing to compete with.

## Visibility

| Condition | Visible? |
|-----------|----------|
| Idle | No — the row returns to the transcript |
| Streaming assistant text ("responding") | Yes — the bar stays up across the whole round lifecycle, sustaining the breathing-dot liveness anchor (ADR-0008) through the longest phase |
| Running tool / queued / waiting | Yes |
| Permission / `ask_user` pending | Yes — forced on even if the loop reports idle, so the bar stays the visible anchor above the permission sheet, tinted in the warning hue |
| Slash command dispatched (harness idle) | No — a command is a synchronous control-plane operation outside the round state machine, so it never arms the bar; its in-flight state is the pending command row in the transcript ([ADR-0110](../../adr/0110-commands-do-not-trigger-the-activity-bar.md)) |
| Overlay modal open (Sessions, Models, Queue, Help, …) | Yes — the overlay floats over the conversation (recessing or dimming it); only a full-screen scene or the subagent detail page takes the whole footer away |
| Full-screen scene (Dashboard, Settings) | No — these own the terminal |
| Subagent detail page (zoomed into a task) | No — that page is read-only and its only chrome is its page header |

The bar persists from round start (user submits) through every phase —
`queued`, `responding`, tool work, `finalizing response` — and only
disappears when the harness returns to idle. This keeps the breathing dot
in peripheral vision for the entire active round and avoids a layout shift
at the streaming boundary.

## Round and turn

The bar does not show the round/turn counters. The round the user perceives is
anchored in the **transcript** — each user message's header reads `round N`
plus its send time — and the counters are also what the **Session Stats** modal
(`Ctrl+O`, or the model bar's context/rate gauge) reports: its **Activity** tab
lists the recorded rounds with tokens in/out, cache hit rate, stream TPS,
duration and turn count, drilling into a round's turns and from there into an
attempt's latency timeline. See
[Rounds and turns](../../explanation/agent-design/rounds-and-turns.md) for the
full concept; in short:

| Counter | Meaning |
|---------|---------|
| `round N` | The user-perceived round number (1-indexed). Bumped once per submitted message. |
| `turn M` | The model-request index within the current round (1-indexed). A turn spans one model request plus the tool work that follows. |

The turn number resets each round; the round number resets only on a new
session.

## Activity labels

Labels are folded once — in `mutx::phase::Phase::classify` — from the wire's
free-form `Activity` strings into a typed phase enum; the bar, the transcript's
stamped entries, and per-session chrome all render from that enum, never from
re-parsed text. A test (`phase::tests::vocabulary_closure`) pins every backend
label to a named variant, so adding a label on the agent side fails the TUI
test first by design and unknown labels degrade to a verbatim passthrough
instead of going blank.

| Phase | Label |
|-------|-------|
| Queued (a chat round admitted, not yet running) | `queued` |
| Request assembly | `preparing context` |
| Waiting for provider (first byte or retry in flight) | `waiting for model` |
| Reasoning stream producing deltas | `thinking` |
| Answer stream producing deltas | `answering` |
| Tool execution | `exploring` / `searching codebase` / `making edits` / `running command` / `updating tasks` / `running subagent` / `using MCP` |
| Human gate (permission / ask_user) | `awaiting permission` |
| Finalizing stream | `finalizing response` |

Transport setbacks own **no label**: a provider backoff renders as the
warning-tinted clause `retry 1/7 (next in 4s)` beside whatever master label is
live, and the clause ends the moment the phase leaves `waiting for model` — see
[the clause's lifetime](#the-clauses-lifetime). The subagent-side peek row
likewise shows bare `waiting to retry …` rather than `running waiting to
retry`, because a backoff is a pause, not progress.

## Source

`draw_activity_bar` in `chrome/activity_bar.rs`; props assembled in
`event_loop/render.rs`, helpers (`classify_liveness`, `dot_color`,
`breathing_color`, `format_elapsed`, `truncate_for_bar`) in `chrome/common.rs`.
Spinner phase is sampled from `app.spinner_epoch` once per frame. Round and
turn values are mirrored from the round-admission and turn-start events by the
response listener.

The transport clause's value is `SessionChrome::transport_setback` of the
*viewed* session (`App::provider_retry` is the primary's slot), rendered
through `ProviderRetryState::summary`; its lifetime is
`phase::ends_transport_setback`, applied by `SessionChrome::set_phase` /
`App::set_phase` — the only writers, so no other code retires it (ADR-0235).
