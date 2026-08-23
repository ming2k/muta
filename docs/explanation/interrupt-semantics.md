# Interrupt semantics

A round is not one indivisible operation — it is a pipeline with three
distinct phases, and an interrupt (`Esc` / `AgentOp::Interrupt`) means
something different in each. This page is the single reference for *what an
interrupt actually does* at each phase, *what survives in the conversation
context*, and *what it costs*. It is the design rationale behind two
intertwined decisions: neenee always uses streaming, and an interrupt is
treated as a three-phase, billing-aware event rather than a blunt kill.

For the round lifecycle that the phases below carve up, see
[Rounds and turns](agent-design/rounds-and-turns.md). For how token counts
are normally booked on a *completed* round, see
[Token accounting](agent-design/token-accounting.md).

## Why streaming (and why it matters here)

Every model request neenee makes is a streaming request (`stream: true`,
SSE). This is not a cosmetic choice for a nicer typing animation; it is the
foundation that makes interrupt semantics tractable. The relevant property
of streaming is this:

> Closing the client side of a streaming connection is a *signal the server
> acts on*. The provider detects the disconnect and stops generating
> within a few tokens. Closing a **non-streaming** request does no such
> thing — the server keeps generating to completion, you just are not there
> to receive it.

This distinction is why the three-phase model below is meaningful at all.
Under a non-streaming transport, "the request was sent" and "the server is
generating" collapse into one phase with no clean boundary, and an early
cancel saves no output tokens because the server finishes the whole
generation regardless. Under streaming, the client's read loop is also the
server's backpressure channel: dropping the stream is a genuine "stop"
instruction, acknowledged (after a small lag) on the server side.

This is also why neenee's [Token accounting](agent-design/token-accounting.md)
can under-report on an interrupted round — the `usage` chunk that carries the
authoritative count is the *last* SSE event, emitted only after generation
completes, and an interrupt never reaches it. See [Interrupted turns and the
token ledger](#interrupted-turns-and-the-token-ledger) below.

## The three phases

A round flows through three phases in strict order. An interrupt is
interpreted according to **which phase the round is in at the instant `Esc`
fires**. The table below is the executive summary; each phase is then
explained in detail.

| Phase | What is happening | What an interrupt does | Context effect | Billing |
|-------|-------------------|------------------------|----------------|---------|
| **1. In-flight, pre-response** | Request sent; no bytes back yet | Cancel the request, **unsend** the user message | None — message returns to the input for re-editing; context reverted to pre-send | Input tokens of the *cancelled* request may still bill (request already left); but no assistant message, no output tokens |
| **2. Local, pre-remote** | Response streaming in; TUI rendering deltas | Drop the stream, discard the partial text | Partial assistant text is **dropped** (never pushed, never persisted); no marker inserted | Input tokens bill; output tokens generated so far bill; generation stops within a few tokens |
| **3. Remote / tool** | Assistant message committed; tools executing | Cancel tools, emit `ToolCancelled`, do not append results | Committed assistant message **stays**; tool results are **not** appended; turn is not persisted | Input + committed output tokens bill; cancelled tools' side effects are best-effort stopped |

The naming is deliberate. **Local** vs **remote** refers to *where the
interruptible work has moved to*: in Phase 2 the work is the remote model
generation being rendered locally; in Phase 3 the work has moved fully onto
the server / tool side (a tool call is "remote" work driven by a committed
assistant decision). Phase 1 is the window where nothing remote has happened
*that we have evidence of* yet, so it is reversible.

### Phase 1 — In-flight, pre-response

This is the window between `provider.stream_chat_events(messages)` being
called and the first SSE event arriving. The harness races the request
against the cancel token:

```rust
let mut stream = tokio::select! {
    biased;
    _ = cancel.cancelled() => return Err(HarnessError::Interrupted),
    result = tokio::time::timeout(
        STREAM_IDLE_TIMEOUT,
        self.provider.stream_chat_events(messages.clone()),
    ) => match result { /* ... */ },
};
```

If `Esc` lands in this window the round aborts *before the stream object
exists*, so nothing has been rendered and the cancellation is clean. This
window exists by construction: the `select!` is `biased` so the cancel arm
wins ties, and `STREAM_IDLE_TIMEOUT` bounds how long the harness will wait
for the provider to even open the connection.

**Design (implemented unsend):** because no evidence of a response has
reached the client, this phase is the only one where the round is *truly
reversible* at the conversation layer. An interrupt here is treated as an
**unsend** rather than an abort.

The mechanism (`execute_round` in `orchestration.rs`): on the error path, the
`is_phase1_unsend` guard holds when the result is `Err(Interrupted)` **and**
the round's `streamed_text` flag is still `false` **and** no tool has run
(`tool_activity` still `false`). The harness then pops the user message back
out of `round_history`, reverts the session store with `replace_messages`,
emits a `RoundEvent::UnsentInput { prompt, images }`, and returns `Ok(())`
instead of propagating the error. The `streamed_text` / `tool_activity`
guards are what distinguish Phase 1 from Phases 2/3: they are the
cross-thread aggregates of "did any model output or tool execution happen
this round", and they remain `false` exactly through the Phase-1 window.

The TUI's response listener pops the matching user message from the
transcript and forwards the prompt via a one-shot signal
(`unsent_input_signal`) to the event loop, which restores it into the input
box — text and pasted images — so the user can re-edit and re-submit. The
restore is guarded: it only adopts an **idle** composer, so a draft the user
was mid-typing while the round ran is never clobbered by the asynchronous
event; in that case a toast says the prompt is in the input history
(`Ctrl+R` / `↑`) instead. The conversation context ends up identical to the
pre-send state either way. Hidden control prompts (hook output, compaction
checkpoints) are not unsendable: they are harness-internal and are never
surfaced as editable user input.

This is clean at the conversation layer: no assistant message enters history,
so no future round re-sends phantom output. The request ledger retains an
`interrupted` attempt with an estimated prompt because the HTTP request was
already on the wire and the provider may still charge its input. Phase 1
unsend saves a bad conversation round and future output tokens, but it cannot
un-send the network packet. See
[Billing reality](#billing-reality).

#### Why "first content delta" is the boundary — not the first response packet, not "request never left"

Two adjacent boundaries are tempting and both are wrong:

- **"Any response packet counts."** The first network response bytes — the
  HTTP status line, SSE keep-alives, role-only deltas — arrive almost
  immediately after the provider accepts the request, long before any model
  output exists. Anchoring the unsend window to them would close it before
  the user could ever use it, and would key a *conversation-layer* decision
  to transport noise. The sentinel is therefore the first **content** delta
  (`streamed_text`, flipped by `AgentEvent::AssistantDelta` /
  `ReasoningDelta`): the earliest observable evidence that the model has
  committed to a reply.
- **"Only if the request never left the machine."** Whether the request is
  on the wire is not observable at the harness layer (the socket belongs to
  the provider implementation), and it is the wrong question anyway: billing
  control is lost the moment the request is handed to the provider, but
  *conversation reversibility* is not. The unsend is a statement about the
  local context ("nothing of this round exists"), which stays true well past
  the moment the packet leaves.

What about "any time before the first turn completes"? Reversibility ends
before that: the first turn may commit an assistant message with tool calls
into history, and tools may have executed real side effects (a bash command
ran, a file was written). Unsending the user message past that point would
have to discard committed history and disavow real-world effects — the
conversation would no longer be self-consistent. The `streamed_text` /
`tool_activity` sentinels are monotonic across the whole round, so the guard
that holds for the first request holds until the first content delta or tool
call — whichever comes first — and never re-opens.

The residual token cost of an unsend is exactly the interrupted round's
input tokens (already on the wire, always billed, see
[Billing reality](#billing-reality)); generation stops within a few tokens
of the disconnect because dropping the SSE stream is itself the stop signal.
There is no scenario where the provider happily finishes a full response
after an unsend — that is the property streaming-only is designed to buy.

### Phase 2 — Local (stream rendering)

The stream is open and `ProviderStreamEvent` deltas are arriving. The
rendering loop tracks two sentinels — `emitted_text` and
`emitted_reasoning` — that record whether *anything* has been shown to the
UI yet:

```rust
loop {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(HarnessError::Interrupted),
        event = tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next()) => {
            // accumulate into `content` / `reasoning_content` / `calls`,
            // set emitted_text = true on first TextDelta, etc.
        }
    }
}
// ...only reachable if the loop completed normally:
self.book_turn_usage(&mut state, &response, streamed_usage.take());
messages.push(response.clone());
```

If `Esc` lands here, the `biased` cancel arm fires `return Err(Interrupted)`
**before** `messages.push(response)` and **before** `book_turn_usage`. The
consequences are precise and important:

- The accumulated `content`, `reasoning_content`, and `tool_calls` strings
  are **dropped on the floor**. They live only in local stack variables and
  are never converted into a `Message`.
- No assistant message enters `messages`, so it never enters `round_history`,
  so it is never persisted and never sent in any future request's context.
- No terminal provider usage is normally available. The request attempt is
  retained as `interrupted`, using the pre-wire prompt estimate plus observed
  output as an explicitly estimated total.
- On the wire, the SSE connection is dropped (the `stream` binding goes out
  of scope), which the provider treats as a stop signal.

**No marker is inserted.** neenee does *not* inject a "the previous response
was interrupted" note, system reminder, or `[ANSWER NO LONGER NEEDED]`
placeholder into the context. From the next round's point of view, the model
never replied — the conversation simply ends with the user's prompt. The
only places an "interrupted" string appears are the ephemeral TUI render
signal (`RoundEvent::Text`), which is never persisted, and the durable
**projection** record described in
[The durable interrupt record](#the-durable-interrupt-record) below, which is
user-visible only and never enters the model's context. This is a deliberate
choice: a context marker would be an extra injected user/system round that
costs tokens and can itself confuse the model, and the absence of a reply
already conveys "cut short" well enough.

### Phase 3 — Remote / tool execution

The assistant message was fully received and has been pushed to `messages`
at `:1696`. The harness is now in `dispatch_tool_calls`, running one or more
tools. This is "remote" in the sense that the work is now server/tool-side
and driven by a *committed* assistant decision that is already in history.

An interrupt here cannot undo the committed assistant message — it is
already in `messages` and (via the mid-round save point) may already be on
disk. Instead the harness:

1. Emits a terminal `AgentEvent::ToolCancelled` for each in-flight tool, so
   the UI shows the cancellation rather than leaving tools hanging.
2. Does **not** append the `Role::Tool` result messages for the cancelled
   tools.
3. Does not persist the turn via `append_turn`.

The committed assistant message stays. The next round therefore sees an
assistant round that issued tool calls whose results never came back — which
is exactly the state provider sanitizers are built to clean up at
serialization time (see [Request flow](request-flow.md)): OpenAI-compatible
endpoints strip unanswered `tool_calls` (and drop the assistant message if
it becomes empty); Anthropic strips unanswered `tool_use` blocks. So the
history is self-healing across a Phase 3 interrupt: the committed message
may look incomplete in the local `Vec<Message>`, but it is reshaped into a
wire-valid form before any provider sees it.

## The durable interrupt record

Every stop path also writes a **`RoundInterrupt` record** to the session
store — `{ reason, at_ms, round }` — and emits its live twin,
`RoundEvent::RoundInterrupted`. The reason is a closed classifier:

| Reason | Stop site | What the user sees |
|--------|-----------|--------------------|
| `user` | `AgentRequest::Interrupt` (double-Esc), `InterruptSide` | `Interrupted · Esc Esc` |
| `superseded` | a newer round's `begin()` (new message, `!cmd`), a session switch (`/resume`, `/session open|fork|new`), an aside close | `Interrupted · new message` |
| `terminated` | daemon kill paths (signal, control verb, `KillSession`, shutdown drain) + load-time inference from crash residue | `Interrupted · process exited` |

Mechanically, the reason is *parked, not threaded*: each stop site calls
`RoundLifecycle::record_interrupt(reason)` at the moment it requests the
cancellation, and the unwinding round task reads it back with
`take_interrupt()` in `start_interactive_round`'s tail — one write, one
read, no changes to `HarnessError::Interrupted` (which stays a unit
variant). The tail then persists the record and emits the event on **every**
genuinely-stopped path: the visible `[Interrupted]` arm, the
generation-suppressed supersede arm that previously left no trace at all,
and the Phase-1 unsend (which returns `Ok(RoundCompletion::Unsent)` after
`UnsentInput`).

A parked reason alone, however, does **not** make a round interrupted. Two
guards keep the record honest:

- **`RoundLifecycle::begin()` clears the slot.** Stop sites park
  unconditionally — even when no round is live (Esc Esc on an idle session,
  `/resume` after the round already finished) — so a reason parked while
  idle can never leak into the next round's tail and mislabel a successful
  round.
- **The tail checks the outcome.** Only an actually-stopped round keeps its
  record: `Err(_)` (the unwind arms) and the Phase-1 unsend
  (`Ok(RoundCompletion::Unsent)`) record; a natural completion
  (`Ok(RoundCompletion::Completed)`) and a hook-denied prompt
  (`Ok(RoundCompletion::NotStarted)`) are successes and drop the parked
  reason instead. This covers the late-Esc case: a stop request that lands
  after the round passed its last cancellation checkpoint — the model
  already converged, the history already committed — changes nothing and
  must not fabricate an `▲ interrupted` marker over a successful round.

Two termination paths cannot run *any* code, so they are covered by
inference instead:

- **Registry kill paths** (daemon shutdown, `KillSession`, `EndSession`)
  record `terminated` into the store *before* dropping the driver future —
  the round task never runs its tail.
- **Hard kills** (SIGKILL, panic, power loss) leave a persisted request
  still `in_flight`; on the next load `TokenLedger::restore_session` flips
  it to `abandoned`, and the driver synthesizes one `terminated` record per
  abandoned round.

The record is **projection state, not conversation state** — the same
distinction as the command ledger (ADR-0091). It never enters
`model_window` or `archived_transcript`, never reaches the provider, and
costs zero context tokens. On resume it is re-projected into the transcript
at its timestamp seam (`▲ interrupted · HH:MM` + `round N · <reason>` in the
TUI, an equivalent row in the web panel), so a restored session answers
"this round stopped, here is why and when — continue?" at a glance. This
does not contradict [the no-marker decision](#why-no-interrupted-marker-in-context):
that decision is about the *model-visible* context; this record is
user-visible only.

## Interrupted turns and the token ledger

Every provider request enters the ledger before it goes on the wire. An
interrupt changes that same attempt from `in_flight` to `interrupted`.

If a provider usage event arrived before cancellation, the attempt retains the
reported counts. Otherwise it uses the pre-wire prompt estimate plus observed
output and labels the result estimated. The exact provider invoice remains
unrecoverable without a terminal usage event, but the attempt no longer
disappears or masquerades as a precise zero.

After cleanup, current context is recomputed from the final committed history.
An unsend therefore removes the user message from current context immediately;
discarded partial output never contributes to it.

## Billing reality

An interrupt optimizes the *conversation* and the *local accounting*, not
the *invoice*. Three layers, three different truths:

**1. neenee's local ledger** — records an interrupted attempt. It uses reported
usage when available and otherwise shows an explicit estimate. The estimate is
diagnostic, not a provider invoice.

**2. The provider's real invoice** — is computed server-side from what the
model actually processed and produced, independent of whether the client
read the result. Three rules govern it:

- **Input (prompt) tokens always bill.** The request left your machine; the
  provider parsed and embedded the entire prompt. Escaping cannot recall it.
  This is the dominant cost for long-context turns and is unaffected by how
  early you interrupt.
- **Output tokens generated up to the disconnect bill.** Generation is
  pipelined: the model produces tokens into a server-side buffer slightly
  ahead of what is on the wire. The tokens generated during this "detection
  lag" — a handful, typically — are produced and billed even though the
  client never rendered them.
- **Tokens never generated do not bill.** This is the whole point of
  streaming for interrupts: once the provider registers the dropped
  connection it halts generation within a few tokens, so the large body of
  output that *would* have been produced is never generated and never
  charged.

**3. Prompt caching** — adds a wrinkle. On Anthropic (and OpenAI's
automatic caching), the input tokens billed on the interrupted round may
include cache-write (`cache_creation_input_tokens`) cost; the *next* request
with the same prefix then hits cache-read pricing (cheaper). So an early
interrupt on a fresh large context is disproportionately expensive relative
to its (zero) local output, but it primes the cache for the retried round.
See [Token accounting](agent-design/token-accounting.md) for how neenee
tracks cache tokens when they *are* reported.

The unavoidable conclusion: **Escaping saves output tokens (the bigger the
would-be response, the more it saves) but cannot save input tokens, and the
savings ratio is worst exactly when input dominates** — long context, short
desired answer. Cost control therefore has two levers, and the interrupt is
only the second one:

- **Primary (input):** keep the context small — pruning, compaction,
  disabling unused tools, a shorter system prompt. This is where the money
  is on long sessions.
- **Secondary (output):** interrupt early when a response is clearly going
  wrong. Under streaming this genuinely stops generation and saves the
  un-generated output; under a non-streaming transport it would save
  nothing, which is the concrete reason neenee is streaming-only.

## Why no "interrupted" marker in context

A natural alternative to the current design is to record the fact of an
interrupt in the context so the model "knows" its previous round was cut
short — e.g. append a system or user message like `"[The previous response
was interrupted by the user.]"`. neenee does not do this, for three
reasons:

1. **It costs tokens every time.** Every interrupt would inject a permanent
   message into the rolling context, inflating input cost on every
   subsequent round — the exact cost the interrupt was meant to avoid.
2. **The omission is already informative.** A conversation that ends with a
   user prompt and no assistant reply reads, to the model, exactly like the
   start of a fresh reply to that prompt. There is no ambiguity to resolve.
3. **Markers can steer the model in unwanted ways.** A "you were
   interrupted" note invites the model to apologize, resume, or
   second-guess, which is rarely the desired behavior when the user
   interrupted because the answer was bad.

The trade-off is accepted consciously: if a future use case needs the model
to be aware of a partial answer (for example, to explicitly resume it), the
clean insertion point is in `execute_round` (`orchestration.rs`) just after
the Phase-1 unsend check, where a marker message could be pushed into
`round_history` before the write-back. The current design leaves that hook
unused — what *is* recorded ([The durable interrupt
record](#the-durable-interrupt-record)) is user-visible projection state
only.

## Summary

- neenee is streaming-only because streaming is what makes an interrupt a
  real "stop" signal to the provider rather than a dropped result.
- An interrupt is interpreted by phase: **Phase 1** (pre-response) is
  reversible at the conversation layer and **unsends** the user message back
  to the input box (gated on `streamed_text` + `tool_activity` both false);
  **Phase 2** (local rendering) drops the partial assistant text with no
  marker; **Phase 3** (tool execution) keeps the committed assistant message
  but drops the tool results, and provider serialization self-heals the
  dangling tool calls.
- Every stop path additionally records a durable `RoundInterrupt`
  (`user` / `superseded` / `terminated` + timestamp) in the session store
  and emits `RoundEvent::RoundInterrupted`. The record is projection state —
  it re-projects into the transcript on resume so the user can decide
  whether to continue, and it never reaches the model.
- Interrupted turns remain visible in the local request ledger. Without a
  terminal usage event their totals are estimated, while the real invoice may
  differ: input usually bills, some generated output may bill, and the bulk of
  un-generated output does not.
- No "interrupted" marker is injected into context — omission is cheaper,
  clearer, and avoids steering the model.
