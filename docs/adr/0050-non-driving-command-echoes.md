# 0050. Non-driving command echoes in the durable transcript

- **Status:** Accepted
- **Date:** 2026-07-09

## Context

Every user input reaches the TUI transcript one way or another, but only some
reach the **durable** transcript. After the ADR-0050-preceding consistency
fix (interactive `/cmd`s now echo into the TUI transcript + input history like
`/pursue`), the remaining gap is durability: which invocations survive a
restart.

| Input | TUI transcript (live) | **Durable transcript** |
|-------|-----------------------|------------------------|
| Chat prompt | ✅ | ✅ driving |
| Slash command `/pursue …` (notification-style) | ✅ echo | ❌ only its `RoundEvent::Text` reply is durable, the `/cmd` text is not |
| `!command` shell passthrough | ✅ echo | ❌ ephemeral — neither command nor result is persisted |
| Slash command `/provider …` (modal-style) | ✅ echo (ADR-0050-preceding fix) | ❌ |

Resume rebuilds the scrollback from `session.full_transcript()` at
`neenee-code/src/main.rs:329`. Because slash/shell invocations are never
written there, **every `/cmd` and `!cmd` you typed before restarting simply
vanishes** — the resumed conversation has no record you ever opened the
provider picker, ran `/pursue`, or shell-passthrough'd `!ls`. This is the
outstanding asymmetry; it makes `/export` and audit faithfulness impossible.

### The `hidden` trap

The obvious lever — `Message::hidden` (`neenee-core/src/message.rs:159`) — is
the **wrong axis** and using it would be a bug. A first-hand audit of the wire
path shows `hidden` means "hidden from the **TUI display, markdown export,
session-title derivation, and review**" but **still sent to the model**:

- `agent.rs:1789` — `self.provider.stream_chat_events(messages.clone())`
  passes the full list with no `hidden` filter.
- `prompt.rs:503` (`prepare_turn_messages`) only drops empty assistant tails,
  rebuilds the head system message, and auto-loads skills — no `hidden` filter.
- `openai/request.rs:62` (and the per-provider analogues) filter only vision
  images and orphan tool results — no `hidden` filter.
- Every `.hidden` read in the tree is in display/export/review/title/
  compaction-selection code (`transcript.rs:19`, `export.rs:65`,
  `session_review.rs:199`, `session_title.rs:110`, `prompt.rs:470/515/525`,
  `session/mod.rs:1790`), **never** in the provider wire path.

So `hidden` currently encodes the *opposite* half of what "non-driving" needs:
hidden ⇒ invisible to the UI, visible to the model. Persisting a slash echo as
`Message::hidden(...)` would make it **invisible on resume** (defeating the
goal) while **still sending it to the model** (exactly what must not happen).

### No "durable-but-not-model-visible" bucket exists

`SessionStore` (`session/mod.rs`) has two message vectors:

- `model_window` (`:113`) — the live authoritative window; cloned at
  `execute_round` (`orchestration.rs:642`) and sent to the model verbatim.
  `model_window()` (`:731`) returns it raw with **no filter predicate**.
- `archived_transcript` (`:116`) — messages evicted by compaction; returned by
  `full_transcript()` (`:739`) on resume/export, but only ever populated by a
  `ContextProjectionCommitted` event, never by direct append.

There is no third category for "persisted, visible on resume, never sent to the
model." A non-driving echo must live in `model_window` (so it survives and
shows on resume) but be **projected out before the wire** — and that projection
does not exist today.

## Decision

Introduce a **provenance-based, non-driving message category** keyed on the
existing `origin` axis, projected out at the single pre-wire funnel. Do **not**
touch the `hidden` axis (its semantics stay exactly as they are).

### 1. New provenance variant

Add `CommandEcho` to the closed `InjectionKind` classifier
(`message.rs:60`). The doc-comment on that enum already frames exhaustiveness
as "the design lever that forces every injection to be traceable," so a new
source site belongs here. Construct echoes via:

```rust
Message::injected(Role::User, text,
    InjectionOrigin::new(InjectionKind::CommandEcho))
```

— but with `hidden = false` (overriding `Message::injected`'s hidden default),
because the echo **must be visible** in the TUI/export/resume. The variant's
presence on `origin` is the "non-driving" signal, not the `hidden` flag.

Add the new variant to the `every_injection_kind_serialises_distinctly` test
(`message.rs:621`) so provenance stays discriminable on disk.

### 2. Project non-driving messages out before the wire

Filter at `prepare_turn_messages` (`prompt.rs:503`) — the single pre-wire
funnel that both the streaming (`agent.rs:1769`) and non-streaming turn paths
call, immediately before `stream_chat_events` (`agent.rs:1789`):

```rust
pub(crate) fn prepare_turn_messages(&self, messages: &mut Vec<Message>) {
    crate::agent::remove_empty_assistant_messages(messages);
    // Project out non-driving echoes so they never reach the provider,
    // while remaining durable + visible on resume/export.
    messages.retain(|m| !is_command_echo(m));
    self.ensure_system_prompt(messages);
    self.inject_implicit_skills(messages);
}
```

This is the **one** place every provider's request body funnels through, so a
single filter covers all backends. `to_wire()` cannot serve this role — it is a
per-message field projection that resets `hidden=false` and strips `origin`
without dropping messages; the filter must be list-level and happen *before*
the per-message projection.

### 3. Insertion seams (where echoes are written durably)

- **Slash** — `handlers_slash.rs::dispatch` (`:60`), which already receives
  `session: &Arc<SessionStore>`. Append the literal `/cmd` as a non-driving
  echo via the atomic `session.mutate_messages(|w| w.push(echo))`
  (`session/mod.rs:1013`) at the top of `dispatch`, before the per-command
  match. This records **every** slash command's invocation uniformly.
- **Shell** — `shell.rs::run_shell_command` (`:26`). Today it receives
  `agent` but not `session`; thread `session: Arc<SessionStore>` through from
  `handlers_chat.rs::shell` (`:62`, where it is already available) and append
  the `!command` echo as a non-driving `Message`. The shell tool *result*
  stays ephemeral (it already surfaces live via `RoundEvent::ToolResult` and
  mirroring it durably would duplicate the model-driven `bash` path); only the
  command text is echoed, matching the TUI's live `!command` display.

### 4. Make resume reconstruct the origin correctly

Today `transcript_message_from_core` (`transcript.rs:18`) infers
`UserMessageOrigin` purely from text shape — `display_content` presence for
slash, `!` prefix for shell (`:65-80`), with a code comment admitting the
heuristic is "exact for the shapes the harness produces." A durable echo whose
`content` is the literal `/cmd` but carries **no** `display_content` (the
natural shape for an echo) would fall through to `Chat` and be mis-shown as the
turn's driving prompt in the Activity modal.

Consult the stored origin **first**, falling back to the shape heuristic only
when `origin` is `None` (preserving legacy-session fidelity and the existing
shape-inference tests):

```rust
if message.role == Role::User {
    if is_command_echo(&message) {
        msg.origin = UserMessageOrigin::Slash; // durable echo ⇒ non-driving
    } else if /* existing shape heuristic */ { ... }
}
```

`origin` is `#[serde(default)]` (`message.rs:188`), so pre-ADR session files
load as `origin: None` and fall through the unchanged heuristic — **zero
migration**.

### 5. Keep non-driving messages out of compaction turn-counting

`select_compaction` (`session/mod.rs:1767`) counts turn boundaries as
`role == User && !content.starts_with("[Conversation checkpoint]")`. A
`CommandEcho` is `Role::User`, so it would inflate the turn count and skew
which turns compaction preserves. Exclude echoes there by origin.

### 6. `/export` visibility

`/export` (`export.rs:65`) skips `hidden`/`System` and renders every remaining
`Role::User` as `## User`. Command echoes should be **exported** (the whole
point is faithful audit), so no change is needed at the `hidden` filter —
echoes are `hidden: false` and will export naturally as a `## User` block. The
export path's separate `model_window()`-vs-`full_transcript()` discrepancy
(notably, export snapshots only the live window today) is a pre-existing
faithfulness gap **out of scope** for this ADR.

## Alternatives considered

- **Reuse `hidden` for "non-driving".** Rejected — first-hand audit shows
  `hidden` is the *opposite* axis (hidden from UI, still sent to model). It
  would both hide echoes on resume and leak them to the model.
- **A new `non_driving: bool` field on `Message`.** Rejected in favour of
  reusing `origin`. `origin` already exists, is the documented provenance axis,
  and carries `#[serde(default)]` (zero migration). A parallel boolean would
  duplicate the classification and risk drift from `InjectionKind`.
- **Filter at each provider's request builder.** Rejected — duplicated across
  OpenAI/Anthropic/Gemini and bypassed by any future backend.
  `prepare_turn_messages` is the single funnel.
- **A third `SessionStore` vector ("echoes")**. Rejected — adds a fourth
  representation of message truth (contradicting ADR-0048) and requires a new
  `full_transcript` composition rule. Keeping echoes in `model_window` with a
  wire-side projection is strictly simpler and keeps resume/export free.
- **Persist shell results durably too.** Rejected for now — the `!cmd` result
  already surfaces live via `RoundEvent::ToolResult`, and duplicating it would
  double-record the model-driven `bash` tool path. Only the command text is
  echoed, matching the live TUI. Revisit if audit needs the result body.

## Consequences

**Positive.**

- Resume, `/export`, and audit become faithful to everything the user invoked,
  not just chat prompts — closing the last command-family asymmetry.
- "Non-driving" becomes a first-class, traceable category (provenance-stamped)
  rather than an ad-hoc flag, extending the ADR-0017/0022 origin contract.
- Zero migration: legacy sessions load unchanged and fall back to the existing
  shape heuristic.

**Negative.**

- A new list-level retain runs on every round in `prepare_turn_messages`. It is
  O(window) over a small constant predicate — negligible next to the existing
  token estimation already run there.
- `append_turn`'s prefix-divergence guard (`session/mod.rs:1075`) compares
  `role`/`content`/`tool_call_id`; an echo appended mid-turn via
  `mutate_messages` outside `turn_history` could trip it. Echoes are appended
  from the server handlers (outside `execute_round`), so they land between
  turns and `execute_round`'s next `session.model_window().await` clone picks
  them up — no divergence, since the scratch is rebuilt from the session each
  turn.

**Neutral.**

- The live TUI continues to show the echo via the `SendSlash`/modal-echo path
  (ADR-0050-preceding fix); the durable echo is the *same* logical record,
  persisted, so resume renders it identically. The transient live push and the
  durable record are not de-duplicated on resume — the live push is gone after
  restart, and the durable record is the only source. (On a *non*-restart
  session the live push already rendered; the durable write does not re-render
  because the server-side append emits no `RoundEvent`.)

## Verification points

- `InjectionKind::CommandEcho` round-trips and serialises distinctly (extend
  `every_injection_kind_serialises_distinctly`).
- A `CommandEcho` message is **retained** out by `prepare_turn_messages` so the
  provider never sees it (unit test over the funnel).
- `select_compaction` no longer counts `CommandEcho` user messages as turn
  boundaries.
- A session with slash/shell echoes, after a simulated restart, reconstructs
  those echoes in the TUI with the correct `UserMessageOrigin` and they are
  **absent** from `to_wire` of the resumed turn.

## References

- ADR-0017 / ADR-0022 — the `origin`/`#[serde(default)]` zero-migration
  provenance contract this extends.
- ADR-0035 — mid-turn save point (`append_turn`), preserved unchanged.
- ADR-0040 — `model_window` / `archived_transcript` / context-projection
  vocabulary.
- ADR-0048 — `mutate_messages` atomic primitive and session-as-single-source;
  the insertion seam this ADR builds on.
