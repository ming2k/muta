# 0194. First-Class Execution Telemetry: Eradication of Retry-Resolution Projection and Restoration of Single-Transcript Purity

- **Status:** Proposed
- **Date:** 2026-10-01
- **Builds on:** ADR-0186 (single-transcript projection directives — the persistence substrate), ADR-0187 (persistence v2 — incremental append and blob reference ledger), ADR-0188 (runtime hot-path degradation elimination), ADR-0189 (streaming prefix virtualization)
- **Supersedes:** `sessions.retry_resolutions` column from ADR-0186/migration 9, `RoundEvent::RetryResolved` wire event, and `SessionStore::retry_resolutions` sidecar ledger

## Context

When a provider request fails due to a transient transport error (e.g. TCP connect timeout, HTTP 502/503/504, or rate limiting), the agent harness executes an exponential backoff retry loop (`crates/muta-agent/src/orchestration.rs`). If a retry attempt succeeds, the round continues normally and generates assistant messages, tool invocations, and tool results.

However, upon round completion, the user is presented with a jarring terminal banner:
```text
 ℹ recovered                                                            23:46

    Recovered after 1 provider retry (round 1)
    OpenAI transport error: error sending request for url (https://opencode.ai
    /zen/go/v1/chat/completions) (client error (Connect): operation timed out)
```

This behavior stems from a series of architectural compromises that violate core design principles:

1. **Temporal Inversion and Post-Round Noise:**
   In `crates/muta-agent/src/orchestration.rs`, retry faults are buffered during the loop. Only at round completion—after the assistant has already responded or tools have completed—is `RoundEvent::RetryResolved` emitted. Clients (`mutx` TUI, Web UI) fold this into a permanent `Notice`-severity row appended to the end of the user's transcript. The user is confronted with a raw transport error banner *after* the work has already succeeded, causing false panic ("did my run fail?").

2. **Violation of the Single-Transcript Model (ADR-0186):**
   ADR-0186 firmly established:
   > *"The durable session stores facts and decisions, never views. There is exactly one transcript — an append-only list of immutable entries plus the projection decision history — and every consumer-facing window is a pure derivation from it."*

   Yet `retry_resolutions` was introduced as a sidecar column in SQLite (`sessions.retry_resolutions TEXT NOT NULL DEFAULT '[]'`) and maintained via ad-hoc APIs (`SessionStore::retry_resolutions`, `SessionStore::record_retry_resolution`). Comments in `history.rs` openly admitted:
   `"Pure projection state — never part of the transcript."`
   This is an architectural anti-pattern: inventing a side-table state because the transcript was deemed "too pure" for transport telemetry, while simultaneously having frontends fabricate pseudo-transcript messages from that very state to display in the main chat log.

3. **Conflation of Ephemeral State, Domain Facts, and Observability:**
   - **Ephemeral State (During Retry):** While waiting on backoff, the user needs immediate live feedback explaining the delay (e.g. Activity spinner: `⏳ Upstream timed out, retrying (1/3)...`). Once the next attempt connects, this transient state must evaporate completely.
   - **Domain Facts (The Dialogue):** In the primary conversation stream, self-healed transport failures are implementation details of the execution engine. They have no semantic meaning to the conversation and do not belong in the main message stream.
   - **Observability (Provenance & Telemetry):** The record of attempts, latency, TTFT, and transient transport errors belongs strictly to the metadata/telemetry of the materialized turn/entry, discoverable on-demand via an Inspector or audit log, not forced into the primary dialogue viewport.

## Decision

We break cleanly with all historical compromises regarding retry resolution:

1. **Eradicate Sidecar Persistence:**
   - Remove the `retry_resolutions` column from `sessions`.
   - Remove `SessionHistory::retry_resolutions` and `SessionStore::record_retry_resolution`.
   - Remove `RoundEvent::RetryResolved` from the wire protocol.
   - Stop fabricating synthetic `TranscriptMessage::notice` entries for recovered attempts.

2. **First-Class Execution Telemetry in Single Transcript:**
   - Promote request execution telemetry to a first-class field on `MessagePayload` (and transitively `TranscriptEntry`):
     ```rust
     #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
     pub struct ExecutionTelemetry {
         pub total_duration_ms: u64,
         #[serde(default, skip_serializing_if = "Option::is_none")]
         pub ttft_ms: Option<u64>,
         pub attempts: Vec<ProviderAttemptRecord>,
     }

     #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
     pub struct ProviderAttemptRecord {
         pub attempt: u32,
         pub endpoint: String,
         pub duration_ms: u64,
         pub outcome: AttemptOutcome,
     }

     #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
     #[serde(tag = "status", content = "detail")]
     pub enum AttemptOutcome {
         Success,
         Failed {
             error_type: String,
             message: String,
             status_code: Option<u16>,
             backoff_ms: u64,
         },
     }
     ```
   - When an assistant turn completes (or when a streaming round finishes), the harness attaches this structured `ExecutionTelemetry` directly into the turn's `MessagePayload`.
   - This telemetry is committed once as part of the immutable `TranscriptEntry`.

3. **Ephemeral In-Flight Signaling:**
   - During retry backoff, the harness emits `RoundEvent::Activity("Retrying provider request (attempt 2/3)...")`.
   - The UI displays this transient status in the Activity Bar / Status area.
   - As soon as tokens resume streaming, the Activity phase transitions or clears naturally. No terminal notice is ever appended to the transcript.

4. **On-Demand Inspection (Zero Main-View Noise):**
   - The main transcript presentation renders only genuine conversation messages, tool executions, and true unrecoverable errors.
   - Users or developers inspecting an entry (via `/entry <seq>`, `/inspect`, or TUI/Web detail modals) can inspect full attempt waterfalls, latencies, and underlying transport fault traces.

## Invariants

1. **Transcript Singularity:** No sidecar tables or session columns may store execution or retry history outside the append-only `entries` log.
2. **Dialogue Sanctity:** A self-healed transient fault shall never produce a persistent system notice or message entry in the primary dialogue view.
3. **Auditability:** Every provider attempt (failed or successful) that culminated in an entry is permanently recorded inside that entry's `telemetry` structure.
4. **Transient Ephemerality:** Live retry indicators exist only during the in-flight retry window and leave zero visual residue upon convergence.
