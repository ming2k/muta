# 0091. Command ledger and typed command results

- **Status:** Accepted
- **Date:** 2026-02-18
- **Revises:** [ADR-0050](0050-non-driving-command-echoes.md) (storage/rendering
  mechanism; the durability goal is retained)

## Context

Every `/` command produces two artifacts, and both are currently awkward:

1. **The invocation** is recorded as a `CommandEcho` *message* in the message
   stream (`handlers_slash.rs::dispatch` pushes `Message::command_echo(&cmd)`
   into `model_window` via `mutate_messages`), visible on resume/export but
   never sent to the model (ADR-0050).
2. **The reply** is either an ephemeral `RoundEvent::Text(String)` (content
   commands: `/search`, `/review`, `/session status`, `/permissions`,
   `/schedule`, …) or an ephemeral toast (`RoundEvent::Notice(CommandAck)`,
   ADR-0088). **No reply is ever persisted.**

Three problems follow, all rooted in the same conflation — *commands are
operations on the session, but they are stored and rendered as if they were
conversation*:

- **P1 — Transcript purity.** The message stream blends dialogue (user prompt →
  model turn → tool steps) with command invocations. On resume an echo renders
  as a `Role::User` bubble (`tui/transcript.rs` classifies it
  `UserMessageOrigin::Slash`), so the transcript reads as if the user typed
  `/compact` as a message.
- **P2 — Reply impersonation + loss.** Content replies are emitted as
  `RoundEvent::Text`, which the TUI renders as `Role::Assistant` + `MessageKind::Text`
  — visually identical to model prose — and they vanish on restart. Resume
  shows the invocation and not the answer.
- **P3 — No structure.** Replies are free-form `String`s. Nothing re-renders,
  queries, diffs, or filters them; `/export` cannot tell a command block from a
  model turn; a future richer frontend would have to string-sniff.

The in-tree precedent for the fix already exists: `ToolOutput`
(`neenee-core/src/tool_output.rs`, ADR-0001) replaced string-sniffed tool
results with a closed, serde-able enum of typed results, each owning its text
rendering, migrated incrementally (Strangler) from a `Text(String)` fallback.
Commands are the last stringly place on the harness side.

## Decision

Split the two artifacts: move command **records** out of the message stream
into a first-class **command ledger** on the session, and give every command a
**typed, per-command result** (`CommandResult`) that is the single contract for
persistence, live rendering, resume, and export. The message stream becomes
pure dialogue again.

### 1. `CommandRecord` + `CommandResult` in `neenee-core`

New module `neenee-core/src/command.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus { Success, Error, UserCancelled }

/// One durable slash-command invocation with its structured result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRecord {
    pub name: String,                    // "search"
    pub args: String,                    // raw remainder after the command word
    pub status: CommandStatus,
    /// None ⇒ invocation recorded but result unknown (pre-ledger legacy echo).
    pub result: Option<CommandResult>,
    pub timestamp: u64,                  // unix ms
    pub duration_ms: Option<u64>,
}

/// Typed result of a slash command. Each variant is the *schema* for one
/// command family's reply; each owns its text rendering via `to_text()`;
/// each serde-round-trips so the ledger is the durable record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CommandResult {
    /// Plain text / markdown — back-compat variant for unmigrated handlers
    /// (the ADR-0001 Strangler bridge).
    Text(String),
    Error { message: String, detail: Option<String> },
    /// Mirrors the ADR-0088 `CommandAck` toast; recorded durably even though
    /// the live surface is an ephemeral bubble.
    Ack { title: String },
    /// `/permissions`
    PermissionList { allowed: Vec<String> },
    /// `/search`
    Search { query: String, hits: Vec<SearchHit> },
    /// `/session status`
    SessionStatus {
        id: String, parent_id: Option<String>,
        message_count: usize, archived_count: usize,
        last_projection: Option<String>,
    },
    /// `/review`
    Review { verdicts: Vec<String>, turns: u64 },
    /// `/schedule`, `/repeat`
    Scheduled { kind: String, id: String, next_fire: String },
    // Remaining variants grow with real callers, not speculatively
    // (ADR-0001's "grows with real callers" rule).
}

impl CommandResult {
    /// The text scheme: how this result renders for live display / export.
    pub fn to_text(&self) -> String { /* per-variant renderer */ }
}
```

### 2. Session store carries the ledger

`SessionStore` gains `commands: Vec<CommandRecord>` persisted in `session.json`,
written through a new atomic `mutate_commands(f)` primitive that mirrors
`mutate_messages` (single write path, ADR-0048). `#[serde(default)]` keeps
legacy session files loadable with `commands: []`.

### 3. `dispatch` returns results instead of pushing echoes

`handlers_slash.rs::dispatch` stops pushing `Message::command_echo` into the
message stream. Each handler arm produces a `CommandResult`; a small helper
(`record_command`) records the `CommandRecord` in the ledger and emits the
reply event. Content replies travel to the TUI as a new typed event carrying
the invocation identity (so the live block can label itself without the TUI
string-sniffing):

```rust
RoundEvent::CommandResult {
    name: String,   // command word without the leading slash
    args: String,   // raw argument remainder
    result: CommandResult,   // TUI renders result.to_text()
}
```

Ack replies stay `RoundEvent::Notice(CommandAck)` toasts (ADR-0088 unchanged);
the same `Ack` result is recorded in the ledger (via `record_ack`) so the
confirmation is durable. Special-response commands (`/sessions`, `/btw`,
`/compact` checkpoints, `ConversationCleared`/`Replaced`, exit) record the bare
invocation via `record_invocation` (`result: None`); errors record
`CommandStatus::Error` via `record_error` and keep their `AgentResponse::Error`
surface. `ConversationReplaced` gains a `commands: Vec<CommandRecord>` field so
`/session open` / `/resume` rebuild command rows alongside the dialogue.

### 4. TUI renders commands as a projection of the ledger

- **Restore:** rebuild command rows from the ledger as compact, dimmed,
  non-conversational lines (`⚙ /search foo`), expandable to the full persisted
  `to_text()` result via the shared disclosure machinery (own
  `InteractiveTargetKind::CommandResult`; Enter/click toggles the body). The
  `UserMessageOrigin::Slash` classification in `tui/transcript.rs` remains only
  for legacy echo messages.
- **Live:** `RoundEvent::CommandResult` renders through the same dimmed row +
  result block; it no longer impersonates the assistant.
- **Export:** commands render as a distinct blockquote block, not a `## User`
  heading, so shared markdown keeps the dialogue pure.

### 5. Migration (zero-read, one-time-upgrade-write)

- **Read:** legacy `CommandEcho` messages (`is_command_echo()`) keep loading
  unchanged; `InjectionKind::CommandEcho` and `is_command_echo()` stay for
  legacy reads.
- **Upgrade:** on load (schema migration v9→v10, folded in
  `migrate_session_data`), every echo message in `model_window` +
  `archived_transcript` is folded into the ledger as
  `CommandRecord { result: None, … }` and dropped from the window. New writes
  never produce echo messages.
- The pre-wire echo filter (`neenee-agent/src/model_request/mod.rs:50`) and the
  compaction exclusions (`neenee-persistence/src/session/mod.rs:1961, 2018`)
  become safety-nets for legacy windows only; they are removed once folded
  sessions are the norm.

## Decisions (settled)

The following four points were left open during review and are now decided;
they are binding parts of this ADR.

### D1 — Transcript purity: ledger is truth, transcript is a projection

The message stream is **pure dialogue**. Command records live in the
`SessionStore.commands` ledger as the single source of truth; the transcript
never carries a command *record*, only a rendered **projection**:

- **Live:** `RoundEvent::CommandResult { name, args, result }` replaces every
  command reply that used to be `RoundEvent::Text`. The TUI renders it as a
  compact, dimmed, non-conversational command row with an expandable result
  block — it never impersonates the assistant. Toasts (`CommandAck`,
  ADR-0088) are unchanged and additionally recorded in the ledger as
  `CommandResult::Ack`.
- **Resume:** command rows are rebuilt from the ledger, not from messages.
  Legacy sessions with echo messages continue to render them as before until
  the upgrade fold, then render from the ledger.

The `D-hybrid` option (ledger as record + projected transcript row) is chosen
over both the status quo (echoes in the stream) and a pure ledger-with-no-TUI
presence (which would lose the resume narrative).

### D2 — First migration wave

Rich `CommandResult` variants land for the content-bearing commands:
`/permissions` (`PermissionList`), `/session status` (`SessionStatus`),
`/search` (`Search`), `/review` (`Review`), `/schedule` + `/repeat`
(`Scheduled`), `/autopilot` confirmations (`Ack`). Every other command —
`/principal`, `/resume`, `/session list|fork|open|resume|new`, `/init`,
`/reload`, `/trust`, `/untrust`, `/export`, `/debug`, custom project commands —
goes through the `Text`/`Error` Strangler bridge: its reply is recorded as
`CommandResult::Text(reply)` (or `Error`) and rendered as a command block, so
**no command reply is ever left unstructured or lost**. The bridge is the
fallback for all unmigrated handlers, exactly as `ToolOutput::Text` is
(ADR-0001); remaining rich variants grow with real callers.

### D3 — Shell passthrough joins the ledger

`!command` invocations (currently `CommandEcho` via `shell.rs:41`) are folded
into the ledger as `CommandRecord { name: "shell", args: <command>, … }`, with
the same result: the invocation is durable. The shell **result** stays
ephemeral (ADR-0050's boundary is retained: it already surfaces live as a tool
step and persisting it would duplicate the model-driven `bash` path). Legacy
shell echoes fold to `result: None` like every other echo.

### D4 — Live rendering contract

`RoundEvent::CommandResult` is the only channel for command replies. The TUI:
(i) renders it as a distinct command block (dimmed header + expandable result),
(ii) never renders it as `Role::Assistant` prose, (iii) keeps the local input
echo row (`UserMessageOrigin::Slash`) exactly as today — it shows what the user
typed, the command block below shows the result. `/export` renders commands as
a distinct block style (blockquote), not a `## User` heading.

## Alternatives considered

- **Status quo (echoes in stream + ephemeral replies).** Rejected — P1/P2/P3 all
  stand; this ADR exists because the conflation is the problem.
- **A third message vector inside the store ("echoes").** Rejected as in
  ADR-0050 — still message-shaped, cannot carry a structured result, and
  complicates `full_transcript` composition. The ledger is *not* a third
  representation of message truth; it is a different record type for a
  different concept (operations vs. dialogue).
- **Keep the echo message, only re-render it (cosmetic purity).** Partial fix —
  solves perception, not P3 (structure) or P2 (reply loss). Kept as a fallback
  if we want a zero-schema change, but it leaves the ledger's query/export wins
  on the table.
- **Persist replies as additional assistant messages.** Rejected — pollutes the
  model-visible stream or needs another hidden category; still unstructured.
- **`CommandResult` as `serde_json::Value` (schema-less).** Rejected — the whole
  point ("返回结果约束") is a closed enum with compile-time exhaustiveness and a
  shared `to_text()`; a free-form Value gives neither.

## Consequences

**Positive.**

- Transcript is pure dialogue again (P1); commands are first-class, structured,
  and persist their actual results (P3) — visible on resume and export (P2).
- Single write path preserved (ADR-0048); durability goal of ADR-0050 retained,
  mechanism upgraded.
- Reuses the proven ADR-0001 `ToolOutput` pattern in-tree.
- Per-command constraints enforced by exhaustiveness: a new command must pick a
  `CommandResult` variant; a new variant must render (`to_text`) and persist.

**Negative.**

- New session-schema field (back-compat via `#[serde(default)]`; one-time
  migration fold on upgrade write).
- The largest handler (`handlers_slash.rs::dispatch`) changes shape: each arm
  returns a result instead of only sending text. Incremental (Strangler): every
  arm can start as `CommandResult::Text(reply)` and migrate to a rich variant
  later.
- TUI gains a command-row rendering path and an expandable result block.

**Neutral.**

- `!command` shell passthroughs share the same `CommandEcho` bucket today
  (ADR-0050); folding them into the ledger under a `shell` name is a follow-up,
  out of scope here.
- `InjectionKind::CommandEcho` remains in the closed classifier (legacy reads);
  it is simply no longer produced.

## Verification points

- `CommandResult` / `CommandRecord` round-trip through `session.json`;
  `commands` field loads as empty on legacy files (`#[serde(default)]`).
- A legacy session with `CommandEcho` messages folds them into the ledger
  (`result: None`) at schema v10 migration and drops them from the window.
- The ledger survives a full event-log replay (log compaction / legacy
  import): `snapshot_to_events` emits `CommandsReplaced` and `apply_events`
  restores it.
- `/search`, `/session status`, `/review`, `/permissions`, `/schedule` produce
  typed results; resume reconstructs the full result text from the ledger
  without string-sniffing.
- `!cmd` passthroughs record under the `"shell"` name (invocation durable,
  result ephemeral per ADR-0050's boundary).
- New sessions write no `CommandEcho` messages — the pre-wire echo filter and
  the compaction echo exclusions become safety nets for legacy windows only.
- The TUI renders `RoundEvent::CommandResult` as a dimmed command block
  (expandable via the disclosure machinery, own interactive target), never as
  assistant prose; command rows restore from the ledger with no round/turn
  position.
- `/export` renders commands as distinct blockquotes, not `## User` headings.

## References

- [ADR-0050](0050-non-driving-command-echoes.md) — durability goal retained;
  storage/rendering mechanism revised by this ADR.
- [ADR-0088](0088-command-acknowledgment-toast-notices.md) — reply surface
  (toast vs. text); unchanged.
- ADR-0001 (`ToolOutput`) — the typed-result + Strangler precedent this follows.
- ADR-0048 — session-as-single-source-of-truth; `mutate_commands` extends the
  single-write-path primitives.
