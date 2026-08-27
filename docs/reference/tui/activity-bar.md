# Activity bar

Transient activity indicator shown in the footer stack, directly above the
input box (below the ambient [todo bar](todo-bar.md) and queue bar). It
unifies the live status label and the breathing-dot liveness anchor into one
click-to-open bar. Long-lived session-state flags (`DELEGATED` and friends)
are deliberately absent — they live on the dedicated [head row](status-bar.md)
below the model bar — and the task-list summary lives on its own todo bar above.

## Appearance

```text
 ● waiting for model [23s] · retry 2/8 next in 4s    Esc Esc interrupt
 └─ master label     └─ elapsed  └─ transport clause  └─ fixed hint
```

The bar surfaces what the user most wants to know mid-round: the **master
label** (the typed phase — steady brand italic) and the **elapsed** timer.
During a provider backoff a **muted transport clause** appears beside
(never instead of) the master label, counting down live: the workflow story
("waiting for model") and the transport setback ("retry 2/8 next in 4s")
are separate channels and never overwrite each other. Under width pressure
segments die in a fixed order — full clause → compact clause (`· 2/8`) →
clause gone → elapsed → label truncation → interrupt words — so the master
label keeps its column budget intact.

## The dot's three liveness regimes

Exactly one mechanism drives the dot each frame (`classify_liveness` is a
pure function; gate wins over everything, byte presence outranks the
clock). All three quote real facts — motion here is always paid for by an
event, never by the frame clock:

| Regime | When | Dot | Meter cells |
|--------|------|-----|-------------|
| `Flowing` | A stream phase (`thinking` / `answering`) with at least one delta arrived | Byte-driven luminance: each delta injects energy that decays exponentially (fast ≈0.4s, slow ≈1.6s); a dark-ember floor (~28% brand mix) keeps inter-chunk quiet from reading as death | `▏..█` two-cell histogram of the same two channels — chunk pressure readable at a glance |
| `Holding` | No stream armed (waiting for model, running a tool) | Classic slow breath (`breathing_color`) — seconds ticking are the only honest change to quote | hidden |
| `Gated` | Permission / ask_user pending | Static amber, no motion — paused for a human; animating would lie about who is working | hidden |

```text
 ● answering █▆ 31s · 2m07s            Esc Esc interrupt   ← flowing
 ● making edits 12s · 2m07s            Esc Esc interrupt   ← holding
 ● awaiting permission                 Esc Esc interrupt   ← gated
```

The meter cells vanish outside stream phases on purpose: a tool execution
has no stream to quote, and empty cells would imply a measurement where
none exists.

## Silence clause

Once deltas have flowed in the current turn and none has arrived for ≥8s
(`pulse::SILENT_AFTER`, long enough to spare thinking models' natural
inter-chunk pauses), the annotation slot shows `· silent Ns`. It is gated
to stream phases only — silence means "this stream stopped producing", not
"nothing is happening". The slot itself is exclusive by construction:
transport retries live in `awaiting model`, silence in streaming phases,
so the two clauses can never compete.

The structural counters — `round N · turn M · <model>` — no longer live on
the bar. They take space and change rarely, so they moved into the
**Activity modal** that this bar opens on click. The whole bar is a click
target (and `Tab`/`Enter` opens the modal): one glance answers "what's
happening, how long?", one click shows the full breakdown (Activity tab:
current prompt, round/turn/model/elapsed; Todos tab: the task list).

| Attribute | Value |
|-----------|-------|
| Location | 1 row directly above the input box |
| Height | `ACTIVITY_BAR_ROWS = 1` while a round is active, 0 when idle |
| Glyph | `●` (`spinner_glyph`), BOLD |
| Glyph color | regime-dependent — see the three liveness regimes above; `Holding` is `breathing_color(phase, theme.brand(), theme.surface())` |
| Master label color | `theme.brand()` + ITALIC |
| Transport clause color | `theme.muted()` — an annotation, not a headline |
| Elapsed | `theme.muted()` |
| Indent | 1 space |

The dot is the TUI's single liveness anchor — every other
running indicator (tool step, thinking marker) holds a steady
accent so this dot is the only thing in the user's peripheral vision that
moves. The master label carries no resident animation on purpose: an early
shimmer was paid for by the frame clock rather than by any work event, and
the typed phase's own word changes are the honest freshness signal. See
[ADR-0008](../../adr/0008-single-breathing-anchor.md).

## Visibility

| Condition | Visible? |
|-----------|----------|
| Idle | No — the row returns to the transcript (the task list lives on the [todo bar](todo-bar.md)) |
| Streaming assistant text ("responding") | Yes — the bar stays up across the whole round lifecycle, sustaining the breathing-dot liveness anchor (ADR-0008) through the longest phase |
| Running tool / queued / waiting | Yes |
| Slash command dispatched (harness idle) | No — a command is a synchronous control-plane operation outside the round state machine, so it never arms the bar; its in-flight state is the pending command row in the transcript ([ADR-0110](../../adr/0110-commands-do-not-trigger-the-activity-bar.md)) |
| Overlay modal open | No |

The bar persists from round start (user submits) through every phase —
`queued`, `responding`, tool work, `finalizing response` — and only
disappears when the harness returns to idle. This keeps the breathing dot
in peripheral vision for the entire active round and avoids a layout shift
at the streaming boundary.

## Round and turn

The bar no longer shows the round/turn counters; they live in the Activity
modal (click the bar) as a detail line `round N · turn M · <model> ·
<elapsed>`. See [Rounds and turns](../../explanation/agent-design/rounds-and-turns.md)
for the full concept; in short:

| Counter | Meaning |
|---------|---------|
| `round N` | The user-perceived round number (1-indexed). Bumped once per submitted message. |
| `turn M` | The model-request index within the current round (1-indexed). A turn spans one model request plus the tool work that follows. |

The turn number resets each round; the round number resets only on a new
session.

## Activity labels

Labels are folded once — in `mutx::phase::Phase::classify` — from the wire's
free-form `Activity` strings into a typed phase enum; the bar, modal, and
per-session chrome all render from that enum, never from re-parsed text. A
test (`phase::tests::vocabulary_closure`) pins every backend label to a
named variant, so adding a label on the agent side fails the TUI test first
by design and unknown labels degrade to a verbatim passthrough instead of
going blank.

| Phase | Label |
|-------|-------|
| Queued (a chat round admitted, not yet running) | `queued` |
| Request assembly | `preparing context` |
| Waiting for provider (first byte or retry in flight) | `waiting for model` |
| Reasoning stream producing deltas | `thinking` |
| Answer stream producing deltas | `answering` |
| Tool execution | `exploring` / `searching codebase` / `making edits` / `running command` / `updating tasks` / `running runner` / `using MCP` |
| Human gate (permission / ask_user) | `awaiting permission` |
| Finalizing stream | `finalizing response` |

Transport setbacks own **no label**: a provider backoff renders as the
muted clause `· retry 2/8 next in 4s` beside whatever master label is live,
and its details stay in the Activity modal. The runner-side peek row
likewise shows bare `waiting to retry …` rather than `running waiting to
retry`, because a backoff is a pause, not progress.

## Source

`draw_activity_bar` in `render/chrome.rs`. Glyph from `spinner_glyph`;
luminance sweep from `breathing_color` in the same module. Spinner phase
driven by `app.spinner_tick` incremented once per frame. Round and turn values
are mirrored from the round-admission and turn-start events by the response
listener.
