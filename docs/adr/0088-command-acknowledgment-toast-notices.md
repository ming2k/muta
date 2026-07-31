# 0088. Command acknowledgments as ephemeral toast notices

- **Status:** Accepted
- **Date:** 2026-08-04
- **Related:** [ADR-0050](0050-non-driving-command-echoes.md) (command
  *invocations* stay durable; this ADR governs the *reply*)

## Context

A slash command such as `/autopilot on` produces two artifacts in the
transcript:

1. the **invocation** `/autopilot on` — the literal text the user typed, and
2. a **reply** from the harness, e.g.
   `Autopilot ON: the agent will run without human intervention — …`.

Until now the *reply* was emitted as a `RoundEvent::Text(String)`, which the
TUI turns into a `Role::Assistant` + `MessageKind::Text` transcript message —
**visually identical to the model's own prose**: same panel, same color, same
provider/model attribution. Two problems follow:

- **No salience.** A status confirmation reads as if the model answered. The
  user cannot tell "the model produced this sentence" from "the harness
  acknowledged a config change." `/autopilot`, `/permissions`, `/session
  status`, `/resume`, … all collapse into the assistant voice.
- **Transcript pollution.** The reply is appended to the same scrollback the
  user is reading/writing. It carries no conversational content — it is not
  something the model will ever need to reference — yet it permanently occupies
  a row and interrupts the visual continuity between a user prompt and the
  model's response.

The interruption is *not* a streaming hazard: slash dispatch is a separate
`match` arm from the chat round (`session_driver.rs`), and a chat round runs in
its own spawned task, so a slash reply never tears an in-flight assistant
stream. The damage is purely visual/cognitive.

### What already existed, unused

The codebase already had the *bones* for a better answer:

- `AgentNotice` (`neenee-core/src/events.rs`) carries `kind`, `severity`, and
  a `surface: NoticeSurface { Inline, Toast, Banner }`.
- `NoticeSurface::Toast` was declared but **never wired** —
  `push_core_notice` read `notice.surface` into `_surface` and discarded it;
  every notice degraded to an inline transcript row.
- A complete toast-bubble component (`components/toast.rs`) existed but served
  only clipboard-copy and armed-keypress feedback — no notice was routed to it.

So the design lever was *connection*, not invention.

### The ADR-0050 boundary

ADR-0050 made slash **invocations** durable (`Message::command_echo`, projected
out before the provider wire) so resume/export/audit are faithful. That
contract concerns the *invocation*, not the *reply*: the reply was always
ephemeral (a `RoundEvent::Text` that is never persisted). Making the reply a
transient toast therefore **does not break ADR-0050** — audit still records
"the user ran `/autopilot on`"; it merely no longer records which exact
confirmation sentence the harness printed back, which was never the point of
the audit.

## Decision

Route command **acknowledgment replies** through the existing-but-dormant
`NoticeSurface::Toast` path instead of `RoundEvent::Text`, so they surface as
a transient top-right bubble that fades on its own and **never enters the
transcript**.

### 1. A new notice kind for command acknowledgments

Add `NoticeKind::CommandAck` to the closed classifier. A convenience
constructor stamps the uniform shape:

```rust
impl AgentNotice {
    /// Ephemeral, toast-surfaced acknowledgment of a slash command / config
    /// change (the *reply*, not the invocation). Info severity, Toast surface,
    /// Harness source.
    pub fn command_ack(title: impl Into<String>) -> Self {
        Self::new(NoticeKind::CommandAck, NoticeSeverity::Info, title,
                  NoticeSource::Harness)
            .with_surface(NoticeSurface::Toast)
    }
}
```

`Toast` surface + `CommandAck` kind together are the signal frontends branch
on, so a future reconnect/replay path can choose to suppress re-surfacing
without sniffing text.

### 2. Harness: emit a `Notice`, not `Text`, for acknowledgments

Migrate the acknowledgment replies that are **one-or-two-line status
confirmations**:

- `/autopilot on|off` → `RoundEvent::Notice(AgentNotice::command_ack(...))`
  (the `AutopilotChanged(bool)` badge event is unchanged).
- `--autopilot` startup notice (bootstrap) → same.

**Not migrated:** commands whose reply is a genuine *query result* with real
content — `/search`, `/review`, `/session status`, `/sessions`. Those stay
`RoundEvent::Text` because the user asked for the content and will reference
it; a toast would discard it. The discriminator is *content vs. confirmation*:
a status line is an acknowledgment; a report is a result.

### 3. TUI: wire the dormant Toast surface to the existing bubble

The response listener now branches on `notice.surface`:

```rust
RoundEvent::Notice(notice) => {
    if notice.kind == ProviderRetry { /* RetryScheduled owns it */ }
    else if notice.surface == Toast {
        // Forward as a transient bubble — never appended to the transcript.
        *notice_toast_signal.lock() = Some(NoticeToastSignal { … });
    } else {
        push_core_notice(…);  // Inline (existing behavior)
    }
}
```

The event loop drains the signal into `App` toast state
(`notice_toast_until`/`_message`/`_severity`, mirroring the copy-toast slot),
expires it on a wall-clock deadline, and renders it through a new
`draw_notice_toast` overlay that reuses the existing `ToastBubble` component
with a severity-derived accent color. It shares the copy-toast's screen slot
(copy toast takes priority), and the `animating` flag includes the notice
toast so it advances/expires at the existing ~10fps cadence.

### 4. Visibility of the underlying state change

Where a command changes *persistent* state (autopilot), the badge is the
long-lived signal: `RoundEvent::AutopilotChanged(bool)` refreshes the
activity-bar badge, which stays visible long after the toast fades. The toast
is the "just happened" confirmation; the badge is the "still in effect"
indicator. Commands that change no persistent state (pure acknowledgments)
need only the toast.

## Alternatives considered

- **Inline `Notice` (`ℹ` icon), still appended to transcript.** Solves
  salience (different color + glyph) but not pollution. Rejected because the
  user's stated requirement was that command feedback not be appended to the
  transcript at all.
- **Make *all* command replies toasts.** Rejected — `/search` and `/review`
  return real query results the user will read and scroll back to; a
  self-dismissing bubble would throw that away. Content vs. confirmation is
  the line.
- **Persist the reply durably too (new "ephemeral reply" bucket).** Rejected —
  diverges from ADR-0050's actual scope (which is about invocations) and adds
  a fourth representation of message truth (contra ADR-0048) for no audit
  value.
- **A brand-new command-feedback component.** Rejected — the `Notice` type +
  `Toast` surface + `ToastBubble` component already form exactly this; building
  parallel machinery would duplicate the severity→color map and the toast
  lifecycle.

## Consequences

**Positive.**

- Command acknowledgments are now visually distinct from model output
  (severity-colored transient bubble) and no longer pollute the transcript.
- The previously-dead `NoticeSurface::Toast` is connected, so any future
  notice (not just command acks) can opt into the toast surface by setting
  `.with_surface(Toast)`.
- ADR-0050's durability contract is untouched.

**Negative.**

- One more cross-task signal (`notice_toast_signal`) crosses the
  listener→loop boundary, mirroring the existing `unsent_input_signal` /
  `outbox_signals` pattern.
- The toast auto-dismisses (~2.6s); a user who looks away may miss a
  confirmation. Mitigated for state-changing commands by the persistent badge
  (`AutopilotChanged`).

## Verification points

- `AgentNotice::command_ack` stamps `CommandAck` / `Info` / `Toast` /
  `Harness` uniformly (unit test).
- `NoticeKind::CommandAck` serialises to `"command_ack"` and round-trips
  (unit test) — frontends and persisted/forwarded notices cannot confuse it
  with other kinds.
- `/autopilot` and `--autopilot` emit a `Notice(CommandAck)` toast; the
  `AutopilotChanged` badge event is unchanged.
- Inline notices (`surface: Inline`) are unaffected — only `Toast` is
  rerouted.

## References

- [ADR-0050](0050-non-driving-command-echoes.md) — durable command
  *invocations*; this ADR governs the ephemeral *reply*.
- `neenee-core/src/events.rs` — `AgentNotice`, `NoticeKind`, `NoticeSurface`.
- `crates/neenee-cli/src/tui/components/toast.rs` — the pre-existing bubble.
