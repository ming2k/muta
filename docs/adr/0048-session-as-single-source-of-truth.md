# 0048. Session as the single source of truth

- **Status:** Accepted
- **Date:** 2026-07-08

## Context

The session store was designed as a **persistence container**, not the live
source of truth. Today the conversation a turn operates on lives in a separate
in-memory vector that is seeded from the session at startup and reconciled back
by hand at turn boundaries. Three representations of the same message list
coexist and are kept in sync by explicit clone-and-replace calls rather than by
shared ownership.

### Three copies of message truth

| Copy | Where | Lifetime | Authoritative? |
|------|-------|----------|----------------|
| `model_window` | `SessionData` (`neenee-store/src/session/mod.rs:97`) | Durable | Persistence only today |
| `history` | `RoundContext.history: Arc<Mutex<Vec<Message>>>` (`neenee-agent/src/orchestration.rs:486`) | Process | **Live working copy** |
| `turn_history` | local `Vec<Message>` in `execute_round` (`orchestration.rs:643`) | One turn | Scratch |

`Agent` itself **holds no messages** (`agent.rs:102` has no message field). Every
turn method receives `&mut Vec<Message>` as a parameter — `run_streaming_with_events`
(`agent.rs:1508`), `record_tool_result` (`agent.rs:2109`). The live history is
owned by the orchestration layer.

The reconciliation is manual, in two places per turn:

- **Turn start:** `turn_history` is cloned out of `history` + the new user
  message (`orchestration.rs:643-669`), then `session.replace_messages(turn_history)`
  is called once (`:670`) so a mid-turn crash is recoverable.
- **Turn end:** both copies are written back independently
  (`orchestration.rs:895-896`):
  ```
  *history.lock().await = turn_history.clone();      // write to working copy
  session.replace_messages(turn_history).await?;     // write to session
  ```
  Mid-turn, only the session is delta-appended via the `round_persist` closure
  (`orchestration.rs:679-685`); `history` is updated only at turn end.

### Scattered turn-forming state

A turn's context is assembled from at least four sources, merged at the single
choke point `build_prompt_context` + `prepare_turn_messages`:

- `SessionStore` — messages, pursuit, todos, provider pin.
- `Agent` (~24 fields) — `disabled_tools`, `turn_counter`, `pursuit_state`
  (armed flag + iteration counter), `prompt_registry`, hooks, skills,
  permissions, identity.
- `Config` — compaction policy, nudge thresholds.
- `Provider` — model guidance, context window.

The `Agent` fields that are genuinely **session-scoped** but **not persisted**
today — `disabled_tools`, `turn_counter`, the pursuit `armed`/`iterations` flags
— are silently lost on resume. The net effect: two resumes of the same session
file can yield different next-turn contexts because state diverged across the
restart, not because the environment changed.

### Why this matters now

This split makes three downstream guarantees impossible:

1. **Provenance治理 (context-injection provenance).** Tool results, envoy
   summaries, web fetches, and bash output all enter context as opaque
   `Role::Tool` blobs with `origin: None` (`agent.rs:2109-2172`). There is no
   single choke point that can stamp trust/provenance/evictability because the
   write target (`history`) is a raw `Vec`, not a governed object.
2. **Network serialization as a pure projection.** The wire body is built from
   `turn_history`, a transient copy, not from the authoritative state
   (`neenee-ai-sdk-openai/src/openai/request.rs:54`). Serialization cannot be a
   pure function of the session until the session *is* the live state.
3. **Resume fidelity.** Session-scoped runtime state resets on restart, so the
   same session file is not a faithful replay.

## Decision

Make `SessionStore` the **single source of truth** for message truth and all
session-scoped state. Network serialization, the `Agent`, and every tool-result
write become **pure projections or atomic mutations of the session** — never
independent copies.

This is staged in three phases so each is independently verifiable.

### Phase 1 — Converge message truth onto the session

Eliminate the `history: Arc<Mutex<Vec<Message>>>` working copy. The session's
`model_window` becomes the live authoritative state the turn operates on, not a
persistence mirror.

To do this without re-introducing the divergence risk, add **one new primitive**
to `SessionStore`:

```rust
/// Apply `f` to the live message window under the session lock, append the
/// resulting event, and persist. The single atomic mutation point that
/// replaces the clone-out / mutate-locally / swap-back trio.
pub async fn mutate_messages<F>(&self, f: F) -> Result<(), String>
where
    F: FnOnce(&mut Vec<Message>);
```

This is necessary because the existing API exposes only `model_window() -> Vec`
(clone) and `replace_messages(Vec)` (swap), which carry exactly the divergence
risk this ADR removes. `mutate_messages` takes the lock once, applies the
closure in place, appends one event, and persists — atomic, no window for
concurrent clobbering.

`turn_history` survives but is demoted to a **pure scratch value**: cloned out
of the session at turn start (`session.model_window().await`), mutated during
the turn, and committed back through `mutate_messages` (or `replace_messages`
for the wholesale turn-end commit). The mid-turn `round_persist` closure keeps
calling `append_turn` for the delta — unchanged.

**Removed:** `RoundContext.history` and `InteractiveRoundContext.history`
fields; the `*history.lock().await` sites in `orchestration.rs:644,895` and the
~11 sites in `neenee-server/src/handlers_slash.rs` + `session_view.rs:24` +
`side.rs:60`. Server handlers read/write the session directly instead of
mirroring into a parallel `Arc<Mutex<Vec>>`.

### Phase 2 — Move session-scoped runtime state into the session

Classify every `Agent` field as **session semantics** (persist) or **runtime
environment** (leave on `Agent`, re-derive on resume).

| Field | Classification | Rationale |
|-------|---------------|-----------|
| `disabled_tools` | **Session** | A user toggled a tool off mid-session; that intent must survive restart. |
| `turn_counter` | **Session** | A monotonic session watermark consumed by the todo stale-detector; resetting to 0 corrupts staleness comparisons. |
| `pursuit_state.armed` / `.iterations` | **Session** | An armed stop-gate mid-iteration must not silently disarm on resume. The `Pursuit` objective already persists; only the runtime view was missing. |
| `toolset` / `resolved_tools` / `mcp_tools` | Runtime env | Depends on installed tools + MCP connections, which are environment-derived. Rebuilt at startup. |
| `permissions` | Runtime env | Persisted separately to `permissions.json`; re-loaded, not duplicated. |
| `hooks` / `skills_registry` | Runtime env | Built from `config.toml` + filesystem; environment-derived. |
| `identity` | Runtime env | Supplied by the embedding (code vs quant). |
| `prompt_registry` | Runtime env | `default_prompt_registry()` is pure composition; rebuilt. |
| `context_prune_threshold_tokens` | Runtime env | Derived from the resolved model's window. |
| `token_ledger` | Runtime env | Accounting, not semantics; resets per process. |

Add three fields to `SessionData` (all `#[serde(default)]` for zero-migration
backward compat, matching the ADR-0017/0022 contract): `disabled_tools:
HashSet<String>`, `turn_counter: u64`, and extend the pursuit persistence to
carry `armed` + `iterations` (either as a sub-struct on the existing `pursuit`
field or a sibling field — see Alternatives). Each gets a `SessionEvent`
variant so the event log records the change (`DisabledToolsSet`,
`TurnCounterSet`, folded into `PursuitSet`).

`Agent` fields that mirror these become **read-through projections**: the getter
reads from the session (held by reference), the setter writes through to the
session and updates the in-memory cache. The hand-mirror in
`orchestration.rs:968` (todos) is the anti-pattern this removes.

### Phase 3 — Serialization as a pure projection

`request::body` and the per-provider `message_obj` builders cease to take a
transient `Vec<Message>`. Instead the orchestration reads
`session.model_window().await` once and passes that as the projection input.
`Message::to_wire()` is the deterministic projection step (stripping
`children`, `envoy_meta`, `origin`) — unchanged in content, but now guaranteed
to run over the single authoritative copy.

This is what makes **provenance治理 (ADR follow-on)** tractable: every message
that reaches the wire passed through one governed write path
(`mutate_messages` / `record_tool_result`), so trust/provenance stamps applied
at write time are guaranteed present at projection time. No copy can bypass the
governance.

## Alternatives considered

- **Keep `history` and just make the session a better mirror.** Rejected. The
  three-copy split *is* the defect; syncing harder preserves the divergence
  window and keeps serialization pinned to a transient copy. The only fix that
  removes the class of bug is to not have a second authoritative copy.
- **Give `SessionStore` a `RwLock` instead of `Mutex` for read concurrency.**
  Rejected for now. The current `tokio::Mutex` has no second lock to deadlock
  against (documented at `mod.rs:536-540`); a reader/writer split adds
  write-starvation reasoning for negligible gain, since message reads are
  O(window) clones that don't block the runtime's other tasks. Revisit only if
  profiling shows contention.
- **Persist pursuit `armed`/`iterations` inside `Pursuit` vs a sibling field.**
  Preferred: a sibling `pursuit_runtime: Option<PursuitRuntime>` so the
  `Pursuit` core type stays the clean objective + is_complete record and the
  stop-gate view is a separate session-scoped field. Embedding it in `Pursuit`
  pollutes a type that ADR-0032 deliberately slimmed.
- **Batch the per-tool-result event-log fsync.** Out of scope. The current
  `append_turn` is O(delta) bytes and one fsync per call — acceptable for a CLI
  agent's cadence. If throughput ever matters, add a write-coalescing layer;
  this ADR's `mutate_messages` primitive is the right place to hang it, but it
  is not required for correctness.

## Consequences

**Positive.**

- One message list, one write path. The "which copy won?" class of bug is
  eliminated by construction.
- Resume restores session-scoped runtime state; the same session file produces
  the same next-turn context regardless of restart (modulo environment-derived
  state, which is allowed to differ).
- Serialization is a pure function of the session, unblocking per-message
  provenance governance (trust/provenance/evictability stamps applied at the
  single write choke point).
- `Agent` holds the session by reference; its session-scoped getters/setters
  become thin projections, removing the orchestration-level mirror dances
  (todos, pursuit).

**Negative.**

- `execute_round` now holds an `Arc<SessionStore>` and awaits it on every
  message read/write. The session lock is the new serialization point for
  message access; all the `history.lock()` sites become `session` method calls.
- Phase 1 changes ~13 `history.lock()` sites across `orchestration.rs` and
  `neenee-server`. Server handlers (`handlers_slash.rs`) that today mirror
  `history` must be rewritten to read/write the session directly.
- The new `mutate_messages` primitive carries the same synchronous event-log
  fsync as `replace_messages`; per-tool-round writes remain fsync-bound. Acceptable
  but not free.

**Neutral.**

- `turn_history` survives as scratch — the ReAct loop still mutates a local
  vector mid-turn for the streaming/tool-dispatch path; only the *commit* is
  unified through the session.
- Blob offloading and rehydration are unchanged: write-side offload, load-side
  rehydrate, `model_window()` stays a pure in-memory clone.

### Verification points

- **Phase 1:** `RoundContext` / `InteractiveRoundContext` no longer carry a
  `history` field; `grep -r "history.lock"` returns zero hits outside tests;
  the `append_turn`/`replace_messages` round-trip tests still pass; a crashed
  mid-turn still resumes with the tool results intact.
- **Phase 2:** `/tool disable` survives a restart; the todo stale-detector
  fires correctly across a resumed session; an armed pursuit survives restart.
- **Phase 3:** the wire body is provably `to_wire(session.model_window())` —
  no transient-copy divergence is possible.

## References

- ADR-0017 — backward-compat contract for session fields (`#[serde(default)]`).
- ADR-0032 — fold pursuit into the session store (the prior precedent for
  moving runtime state into `SessionData`).
- ADR-0035 — mid-turn save point (`append_turn`), preserved by this ADR.
- ADR-0039 — unified prompt registry; the prompt is already a function of
  scattered state assembled at one choke point — this ADR converges the *data*
  that feeds it.
- ADR-0040 — model-context projection vocabulary; serialization-as-projection
  extends the same term to the wire body.
- ADR-0042 — principal/envoy role vocabulary.
