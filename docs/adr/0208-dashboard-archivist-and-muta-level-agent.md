# 0208. Dashboard Archivist: a muta-level conversational agent embedded in the dashboard, cross-project session retrieval, and the `--dashboard` entry

- **Status:** Proposed
- **Date:** 2026-06-13 (updated by code survey)
- **Builds on:** ADR-0167 (worker-station model — station placement without new archetypes), ADR-0183 (homogeneous agent kernel), ADR-0096/0097 (unified session daemon; dashboard addressing grammar), ADR-0163/0168/0187 (unified SQLite ledger, FTS substrate), ADR-0193 (Round-EOL session digest — the semantic metadata source), ADR-0146/0147 (tool hazard model; orthogonal workspace-security planes)

## Context

The user-facing goal: `mutx --dashboard` opens the dashboard directly (no fresh
workspace session), and the dashboard is rebuilt around an embedded **muta-level
agent** — an agent bound to the daemon instance, not to any workspace — that
knows where sessions live and can answer "find me that conversation where I was
debugging the retry loop, I only remember roughly what we said."

The codebase survey (this round) establishes the ground truth:

1. **The muta-level station already exists.** `Hypervisor`
   (`crates/muta-runtime/src/hypervisor.rs`) is the daemon-singleton Root agent
   with no workspace binding, already equipped with
   `hypervisor_list_sessions` / `inspect` / `instruct` / `coordinate_debug` and
   mesh send/peers. ADR-0167 deliberately killed the "Steward" persona because
   persona-wrapping *stateless internal tools* caused conceptual clutter — it
   does **not** forbid a user-conversational daemon-level agent, which is a
   legitimate station placement under the worker-station model.

2. **The FTS substrate was unwired at every consumer above the engine.** The
   persistence engine already carries a complete `fts_entries` FTS5 virtual
   table with triggers (`crates/muta-persistence/src/db.rs`, migrations v5+
   rebuilt after the ADR-0186 entries swap) and a `search_history` BM25 query
   method — but with **zero callers** outside its own definition, and no
   `AgentRequest`/`AgentResponse` wire variants to reach it from a frontend or
   an agent tool. This is exactly the "unwired harness promise" class ADR-0190
   names, one layer higher than it first appeared: not the schema (that is
   alive and trigger-fed), but the query path. §4 wires it end-to-end.

3. **Semantic metadata exists but is invisible to retrieval.** ADR-0193's
   Round-EOL digest produces `{title, intent, history[]}` per session and
   `MonitoredSession.digest` carries it on the wire
   (`crates/muta-contracts/src/monitor.rs:255-258`) — but no query path uses it
   for search. The dashboard's "Cognitive Dossier" pane references a digest
   pipeline whose retired Steward naming lingers in comments while the data
   field is very much alive.

4. **The dashboard entry surface is split.** `mutx dashboard` (subcommand,
   `Mode::Dashboard`) and `MUTX_STARTUP_VIEW=dashboard` exist; a long option
   does not. The mode already runs the dashboard over a carrier without opening
   a fresh session (`main.rs:99-101`), so the flag is pure parse-layer work.

5. **The tool-boundary tension.** "muta-level ⇒ no workspace ⇒ all tools" reads
   naturally but collides with the confinement posture (ADR-0146/0147): file
   and shell tools derive their roots from a workspace the daemon-level agent
   does not have. Granting it unrestricted host access by default would turn
   `--no-confinement` into a permanent implicit default.

## Decision

### 1. `--dashboard` entry (already implemented this round)

`mutx --dashboard` is accepted as a long option and normalizes to the existing
`Mode::Dashboard` — identical semantics to `mutx dashboard`: open the
full-screen dashboard directly over the daemon's monitor stream, no fresh
workspace session. Help text gains the flag. No new mode, no carrier changes.

### 2. Dashboard Archivist: a conversational daemon-level agent, not a new station

Keep the worker-station taxonomy untouched: no new `MeshStation` variant, no
new `AgentKind`. The Hypervisor station gains a **conversational archetype
instance** — the **Archivist** (`MeshAddress::hypervisor("archivist")`) —
staffed by a Root agent with its own `AgentIdentity`:

> *"The muta instance's Archivist: it knows every session the daemon has ever
> hosted, where they live, and how to find them back."*

Rationale for co-stationing on the Hypervisor rather than minting a fourth
station: the routing lawfulness of ADR-0167 (top-down Instruction, bottom-up
Report) already admits Hypervisor→Session and Session→Hypervisor traffic; a
sibling conversational identity on the same station reuses the whole mesh
contract for free, while a new station would fork `MeshStation`, the routing
table, the wire protocol, and the tracker for zero behavioral gain.

### 3. Tool boundary: daemon-read authority + delegated execution (confinement-preserving)

The Archivist's toolset is **not** the session master's pool. It gets:

- **Retrieval plane** (implemented this round, `crates/muta-runtime/src/archivist.rs`):
  `archivist_search_history` (cross-project BM25 over `fts_entries`),
  `archivist_list_sessions` (metadata/digest survey over `sessions` across all
  project buckets), `archivist_read_session` (transcript tail by id or hex
  prefix, served through the field-private `SessionTranscriptView` /
  `read_session_transcript` persistence API so `SessionData`'s internals stay
  encapsulated), plus mesh send for delegation.
- **Instance plane**: the live-session plane remains the operator
  Hypervisor's (`hypervisor_list_sessions` / `inspect` / `instruct`);
  **the Hypervisor station is now materialized on the daemon** —
  `SessionRegistry` carries the instance's `MeshTracker` (ADR-0167's mesh,
  previously test-only), every hosted session's master joins at
  `session/<id>` on assemble (mailbox RAII-unregisters on kill / suspend /
  drain / crash eviction), the station registers its own address with a
  mailbox held for its lifetime, and the station is lazily constructed on
  the first assemble. Routing lawfulness is enforced live:
  station→session `Instruction` is deliverable, and the same send after
  teardown is refused (fail-closed). The Archivist reads durable state from
  the shared store rather than the in-memory registry, so its answers cover
  suspended sessions the registry no longer holds.
- **Execution by delegation, not by hand (landed)**: the Archivist's one
  write path is `archivist_instruct_session` — resolve the target id (full
  id or hex prefix, against the durable store, so delegation fails honestly
  instead of mis-addressing), then send a station→session `Instruction` over
  the shared mesh. Deliberately narrower than the generic `MeshSendTool`: no
  other station, no Report/PeerNote — a retrieval agent that could broadcast
  would be a liability. Workspace-restricted tools are **not granted**;
  execution happens inside a confined workspace, driven by a session master,
  and the workspace-security posture (ADR-0146/0147) is untouched. An
  explicit config opt-in (`[archivist] tools = …`) may widen it later.

This preserves "no workspace, no workspace limits" where it is true — the
**retrieval plane is inherently cross-project** (all project buckets, one
`muta.db`) — without a cross-project escape hatch for host mutation.

### 4. Cross-project session retrieval: wire the dead FTS, then layer semantics

Three layers, in build order:

1. **Layer 1 — FTS5 match (wired this round).** The engine's `search_history`
   is now reachable end-to-end:
   - *Persistence* (`muta-persistence::db`): the query is sanitized — every
     whitespace-separated word becomes a quoted phrase token joined with AND
     (`sanitize_fts_query`), so raw user input can never inject FTS5 column
     filters or boolean grammar (the live failure that motivated it:
     `no such column: needle` for the bare query `nonexistent-needle-xyz`);
     hits JOIN `sessions` for title + workspace and rank by `bm25`.
   - *Wire* (`muta-contracts::events`): `AgentRequest::SearchHistory` →
     `AgentResponse::HistorySearch(Vec<HistorySearchHit>)` — the daemon stays
     the sole DB authority (ADR-0197); the TUI never opens `muta.db`.
   - *Dispatch* (`muta-runtime::handlers_history::search_history`): fail-open
     — any engine error degrades to an empty hit list.
   - *Agent tool*: `archivist_search_history` (below). Triggers already keep
     the index fresh (ADR-0187); zero write-path changes.
2. **Layer 2 — metadata filters.** Plain SQL over `sessions` (title LIKE,
   time window, project_root, fork_kind) combinable with Layer 1.
3. **Layer 3 — gist recall, deterministic leg (landed).** The "I only
   remember roughly" fallback ships as a **two-stage recall** in both query
   paths (the wire handler and the Archivist tool): the strict AND-form
   query runs first; an empty strict result re-queries the same sanitized
   words OR-joined (`search_history_relaxed`), BM25-ranked so multi-word
   matches float up. The tool's result carries a `recall` field
   (`strict`/`relaxed`) so the agent can frame its confidence honestly.
   The LLM gist-rewrite (rephrasing a gist into candidate keyword sets) is
   **deferred**, not dropped: with sanitizer + two-stage recall covering the
   dominant failure modes (column-name collisions, non-co-occurring words),
   the remaining gap is true semantic paraphrase — worth measuring against
   real usage before spending a provider hop on it. Embedding vectors
   remain deferred per the original decision.

Tool results carry `session_id` + anchors the dashboard can act on: selecting a
hit opens the existing session preview modal, and `a`/attach flows reuse
ADR-0097 addressing unchanged.

### 5. Dashboard rebuild: Archivist pane as first-class console peer

The dashboard gains a third surface alongside the console and sessions dock:
an Archivist address in the console grammar — `? <question>` (or `/ask`) —
whose answer lands as a console receipt. Unlike `@N` (session addresses) the
Archivist needs no target and no dock selection: it is the muta-level agent.
The wire shape is a synchronous control round:

- `ControlRequest::AskArchivist { text }` (ADR-0208); the answer travels in
  the `ControlReply` free-string channel with `ok = true` (the control
  grammar has no dedicated result slot — the field is historically named
  `error`; the client's `control_with_reply` surfaces it on success).
- The daemon's `SessionRegistry` owns an `ArchivistService`
  (`crates/muta-runtime/src/archivist_service.rs`): one long-lived Archivist
  agent, a bounded rolling scratch context (24 messages) so follow-ups stay
  coherent, and per-round **borrowed provider binding** — the round rides
  whichever hosted session's live channel the ask arrives with (all sessions
  share the post-switch channel), so `/models` switches propagate for free
  and the `NoProvider` sentinel degrades to the same honest refusal the chat
  path uses.
- Round mechanics are the agent kernel's own streaming loop over a scratch
  message list — no workspace session store, no transcript persistence (the
  Archivist's answers are ephemeral cockpit dialogue), an 8 000-character
  answer budget, unattended posture inherited from the tool-free toolset.
- The dead "Cognitive Dossier" comments are renamed to the digest pipeline
  that actually feeds them (`SessionDigest`, ADR-0193). Session
  cards/actions (attach, kill, suspend, prompt) are unchanged — this ADR adds
  a conversant, it does not re-mother the control verbs.

## Alternatives considered

- **Fourth `MeshStation::Archive` station.** Rejected: new archetype×station
  cell buys identity separation the Archivist does not need (it *is* daemon
  governance in conversation), while forcing routing/wire/tracker changes and
  a station-depth rethink (`depth()` ordering, `may_command` lattice).
- **Archivist with full unrestricted tools ("muta-level ⇒ everything").**
  Rejected as a default: it would contradict ADR-0146/0147's confinement
  posture and silently make every dashboard launch a `--no-confinement`
  equivalent. Delegation achieves the capability with the boundary intact;
  opt-in widening remains available.
- **Fresh embedding index over session digests.** Deferred: real semantic
  recall value, but premature before FTS+rewrite recall quality is measured;
  the deferred choice does not foreclose adding vectors as a later layer.
- **Separate `mutx archivist` / standalone chat binary.** Rejected: the user
  explicitly wants the dashboard to host the agent; a second entry point
  fragments the cockpit (ADR-0097's console grammar) for no new capability.
- **Keep `mutx dashboard` only, no long option.** Rejected: flag entries
  compose in scripts and shell aliases; parity costs one parse arm and was
  implemented this round.

## Consequences

- **Positive.** The dashboard becomes an intelligence surface, not only a
  control surface; "find my old conversation" stops being a manual
  `ls`-through-buckets chore; the dead FTS5 substrate earns its keep (closing
  the ADR-0190-class gap §Context.2 names); the Hypervisor reuses its engine
  wholesale; confinement posture survives the feature.
- **Neutral.** `--dashboard` is pure parse sugar; digest plumbing, monitor
  stream, and addressing grammar are consumers, not targets.
- **Negative.** The Archivist consumes provider tokens on its own turns
  (budgeted like any Master); Layer-3 rewrite adds one provider hop in the
  fallback path (fail-open bounds it); first release must resist scope creep
  toward host mutation — delegation only.
- **Migration.** Ordered implementation, all landed except where noted:
  (a) §1 flag (landed); (b) §4 Layer 1 wire-up + Archivist identity & tools
  (landed); (c) §5 conversational surface (landed); (d) mesh + station
  materialization (landed); (e) delegation + recall widening (landed:
  `archivist_instruct_session` with durable-store id resolution, the
  Archivist's own mesh endpoint parented to the station,
  `search_history_relaxed` two-stage recall with a `recall` confidence field
  in tool results); **deferred**: the LLM gist-rewrite (`CognitiveTask`)
  pending real-usage recall measurement, an `[archivist]` config opt-in for
  wider tools, and a Hypervisor conversational plane. Each stage ships
  independently; no schema change (the FTS table and triggers already exist
  and are trigger-fed).

## References

- ADR-0167 — worker-station model; Hypervisor placement; why the Steward
  persona died (and why this agent is not its resurrection).
- ADR-0183 — homogeneous agent kernel; `Agent::new(provider, tools, identity)`
  reuse.
- ADR-0096 / 0097 / 0093 — unified daemon, dashboard addressing grammar,
  monitor protocol the dashboard already folds.
- ADR-0163 / 0168 / 0187 — unified SQLite ledger; FTS5 table + triggers
  (`crates/muta-persistence/src/db.rs:136-159`); single-writer actor.
- ADR-0193 — Round-EOL digest; `SessionDigest` on `MonitoredSession` as the
  semantic metadata source.
- ADR-0146 / 0147 — tool hazard model; workspace-security planes; the
  boundary §3 preserves.
- ADR-0190 — the unwired-harness-promise discipline this ADR applies to the
  dead FTS substrate.
- Code sites: `crates/muta-runtime/src/hypervisor.rs` (station),
  `crates/muta-persistence/src/db.rs` (FTS substrate),
  `crates/muta-contracts/src/monitor.rs:206-287` (row shape),
  `apps/tui/crates/mutx/src/cli.rs` (`Mode::Dashboard`, `--dashboard`),
  `apps/tui/crates/mutx/src/overlays/dashboard.rs` (rebuild target).
