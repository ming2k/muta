# Session Persistence

A muta session is a recoverable work site, not a transcript-shaped cache
for the next model request. The local session has to preserve enough state to
resume the work exactly where it stopped: what the user saw, what the model saw,
which tool calls ran, which results returned, which context was projected out of
the model window, and which loop-level obligations were still active.

This page explains that durable model (ADR-0186). For the request-scoped view
sent to a provider, see [Model context](model-context.md). For the storage
location model, see [Platform-native persistence categories](../persistence.md).

## Single Transcript

The durable session stores **facts and decisions, never views**. There is
exactly one transcript — an append-only list of immutable entries plus the
projection decision history — and every consumer-facing window is a pure
derivation from it:

- **Entries** are the facts: user, assistant, and tool messages; harness
  injections; compaction checkpoints. Each carries a coarse provenance
  (`origin`: genuine / harness / checkpoint) and a visibility flag; per-message
  protocol-private state (e.g. a provider's thinking signature) rides inside
  the entry's `provider_meta` and is interpreted only by the protocol adapter
  that produced it. Each assistant message is stamped with its producing
  provider, so a session that mixed models stays attributable.
- **Projection directives** are the decisions: a prune (tool-result bodies
  replaced by placeholders in views), a compaction (a history range replaced
  by its checkpoint entry), or a freeze (an entry's provider-visible shape is
  byte-pinned for KV-cache stability). Appending a directive is the only
  effect a projection has on storage.
- **Working state** — the todo list (derived from `state` entries), title,
  digest, provider pin, round counter, interrupt records, the retry point, and
  the command ledger — lives on the session row in SQLite.

The model window is `derive(entries, directives)`. So is the presentation the
TUI renders. Nothing derived is persisted, so nothing derived can disagree
with the facts.

## Event Log and Snapshot

The event log is the durable history. Each meaningful session mutation is
recorded as an event, so replay can rebuild the session even if the process
stopped between turns. A snapshot exists as an acceleration layer: it gives the
loader a compact current image, but it must describe the same state the event
history would produce.

This matters for context projection. Pruning and compaction are not ordinary
message edits. They archive the original messages and replace the model window
in one atomic session mutation. On replay, that mutation must not be expanded
into "archive these messages" plus "replace the model window" plus another
projection record, because that would duplicate the archive. The event and the
snapshot describe one operation: original context was retained durably, while a
smaller projection became model-visible.

## Admission and Commit

Durability starts before the provider call. When a user submits a round, the
session admits the user message first. Only after that does the agent call the
provider, run tools, or ask for permissions. If the process stops after
admission, resume can see that the user request existed and continue from a
well-defined point rather than losing the prompt.

During a round, tool turns are also committed back to the session. Assistant
tool-call messages and matching tool-result messages enter the model window
after the tool work completes. A late commit is rejected if the user switched
sessions while the round was running, so work from an older branch cannot land in
the newly selected one.

The invariant is simple: the durable session should never require guessing
which side effects already happened. If a tool result is present, the tool call
has completed. If it is absent, the resumed round can reason from the persisted
state instead of replaying an unsafe half-known action.

## Context Projection

Context pressure changes what the model sees, but it must not erase the
recoverable scene. muta uses **model-context projection** for that boundary:

- pruning appends a directive that replaces stale tool-result bodies with
  informative placeholders in views (originals stay recoverable),
- compaction appends a checkpoint entry plus a directive that replaces the
  older complete rounds with it,
- tool-output shaping appends a freeze directive that byte-pins an entry's
  provider-visible form (KV-cache prefix stability),
- all originals remain in the transcript.

When the agent-computed projection result is exactly reproducible as
directives, the store appends them; when it is not, the transcript rebuilds
from the caller's window — correctness over cleverness. A resumed session
re-derives the exact prior view without re-projection.

For the two projection layers, see [Context pruning](context-pruning.md) and
[Context compaction](context-compaction.md). For the naming decision, see
[ADR-0040](../../adr/0040-session-state-and-context-projection.md); for the
single-transcript model, see [ADR-0186](../../adr/0186-single-transcript-projection-directives-persistence.md).

## Resume

Resume starts from the durable session, not from provider memory. Providers are
stateless; they do not remember previous requests. muta therefore restores the
local session first, then derives the projected view and sends it on the next
provider request.

The resume path restores the transcript (entries + directives), the working
state — round-interrupt records (one durable record per round that stopped
before completing, each carrying the reason and the timestamp), the todo
mirror, the title, the digest, the provider pin, the retry point, and the
command ledger — plus any blobs still referenced by entries. It also restores
the session's **connection pin**: if the session had switched providers or
models, resume lands back on that choice rather than the global default, so a
reopened session talks to the same connection it was using. For the dual-write
selection decision, see
[ADR-0066](../../adr/0066-dual-write-provider-selection.md).

Projected-out originals are not blindly reinserted into the provider request —
that would undo pruning or compaction and push the session back into the same
pressure state. They remain available for recovery, audit, summarization, and
future tooling, while the model reads the exact projection committed before
the restart.

## Correct Recovery Contract

A session is correctly resumable when these conditions hold:

- the projected view after resume is byte-identical to the one before exit
  (guaranteed: the derive is a pure function of the persisted facts and
  directives),
- every entry is intact — nothing projected-out was lost, and tool results
  still pair with their calls,
- harness-injected messages keep their provenance,
- working state (todos, title, digest, pin, interrupts, retry point, command
  ledger) is restored before the next round.

If those conditions hold, a resumed session does not need to re-prune or
re-compact just to rediscover the state it already had. It may project again
later if new messages create new pressure, but the prior projection remains a
durable fact.
