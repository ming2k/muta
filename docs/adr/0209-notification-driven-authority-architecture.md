# 0209. Canonical Notification-Driven Daemon Authority Architecture (The Death of Standalone-Client Mindset)

- **Status:** Accepted
- **Date:** 2026-09-09

> **Implementation Roadmap & Tracking (2026-09-09):**
>
> - **M1 completed** (`Turn-boundary Token Report Push`): `RoundCompleted` and round finish hooks in `muta-agent/src/orchestration.rs` proactively emit `AgentResponse::TokenUsageReport` containing live session ledger snapshots. Open Session Stats / Telemetry overlays render turn-by-turn live token accrual without client-side polling.
> - **M2 completed** (`Cross-Session Picker Fan-out`): `SessionRegistry` in `muta-runtime/src/registry.rs` acts as global fan-out authority for `ProviderPicker`. Any session activating a model, adjusting favorites, or refreshing discovery propagates the updated picker to all hosted sessions' event buses and updates daemon-level attach-sync buffers.
> - **M3 completed** (`Cross-Client Model Synchronization on /new`): `reapply_session_selection` in `muta-runtime/src/handlers_provider.rs` synchronizes with daemon-wide authoritative state on pin-less sessions, ensuring `/new` and session switches inherit cross-client active models and recency ranking.
> - **M4 completed** (`Shared In-Memory Daemon Authority`): `SessionDriver`'s owned `config: Config` and `provider_usage: ConnectionUsage` have been replaced by shared `SharedConfig` (`Arc<tokio::sync::RwLock<Config>>`) and `SharedConnectionUsage` (`Arc<tokio::sync::RwLock<ConnectionUsage>>`) handles owned authoritatively by `SessionRegistry` and bound to all sessions via `BootstrapParams`. Split-brain memory divergence and disk-as-IPC re-reads are permanently eradicated.
> - **M5 completed** (`Full Elimination of Destructive View Wiping & Polling`): Eradicated destructive `app.token_report = None` and `app.usage_stats = None` on dialog entry in `surfaces.rs` and `actions.rs`; frontends act as pure functional views of the reactive store.

---

## 1. Context & Problem Statement

Muta has evolved from an in-process interactive CLI tool into a persistent, multi-session daemon architecture (ADR-0096). However, the implementation remained crippled by architectural schizophrenia: the daemon hosts multiple sessions and clients, yet the internal data and communication models retained the legacy **standalone-client mindset** (单机思维).

An architectural audit revealed five critical structural defects:

### 1.1 On-Demand Snapshot Queries for Dynamic Runtime State
Auxiliary overlays in the TUI (Session Stats / Telemetry modal, `/usage` cross-session analytics modal, Tools, Mcp, Skills) relied on `AgentRequest::Query*` round-trips fired strictly when the dialog was first opened (`enter_panel`).
- When an LLM turn streamed tokens or completed new rounds, an open Session Stats modal remained completely frozen at the initial snapshot.
- To make numbers move, frontends were forced to invent client-side polling loops (tick timers waking every 1–2 seconds to re-query the backend). This converted an event-driven system into a polling architecture, adding spurious wakeups, thread contention, and protocol noise.

### 1.2 Per-Session Response Sinks & Cross-Session Blindness
The response transmission channel (`resp_tx` / `events`) was treated as a private point-to-point pipe belonging exclusively to a single session driver.
- When Client A in Session 1 activated a new model (e.g., `gpt-5.6-sol` or `claude-3.7-sonnet`), the underlying SQLite database (`state:connection_usage`) updated recency rankings. However, the driver pushed the resulting `AgentResponse::ProviderPicker` *only* to Session 1's local channel.
- Client B connected to Session 2 on the same daemon received nothing. If Client B opened the model list, models remained sorted by old usage.
- Models and Connections modals lacked any refresh request upon being opened, cementing stale local snapshots indefinitely until a reconnect.

### 1.3 Split-Brain In-Memory State & Disk-as-Message-Bus Antipattern
Every session assembled via `bootstrap::assemble` independently executed `Config::load()` and `ConnectionUsage::load()`, moving owned struct values (`pub config: Config`, `pub provider_usage: ConnectionUsage`) into each `SessionDriver`.
- When Session 1 switched models or modified favorites, it mutated its private `Config` and `ConnectionUsage` and saved them to disk.
- Session 2's driver held distinct in-memory copies that remained completely unaware of Session 1's mutations.
- When Session 2 executed `/new`, its unpinned fallback read its own stale in-memory `config.default_model` and `provider_usage.last_models`.
- To paper over this split-brain bug, lifecycle operations were forced to re-read files from disk (`Config::load()`). **Using the filesystem as an uncoordinated inter-session message bus** created invisible read/write races and violated the daemon's role as the single authority.

### 1.4 Unsynchronized Attach Hydration
When a client attached or reconnected to an existing session, `AttachSyncBuffer` provided startup snapshots. However, the buffer was strictly session-local. If another session had modified provider keys, favorites, or model recency after this session started, a new attacher received obsolete model state.

### 1.5 Summary of Architectural Violations
The standalone-client mindset treats the backend as a passive, pull-based database and frontends as independent state islands. In an authoritative daemon architecture, **state transitions are authoritative, live in shared daemon memory, and proactively stream to all interested view projections.**

---

## 2. Inviolable Tenets

To eradicate the standalone-client mindset permanently, this ADR establishes three non-negotiable architectural tenets:

### Tenet I: The Daemon is the Sole Authoritative State Machine
1. **Frontends are pure reactive projections (View = f(State))**: Frontends (TUI `mutx`, Web, Headless, CLI) SHALL NOT own business state, shall not cache un-invalidated state, and shall not decide when to refresh data.
2. **Zero Client-Side Polling**: Frontends SHALL NOT run periodic timers or ticks to query backend status or telemetry. All telemetry, usage, and catalog changes must arrive via proactive server-pushed notifications.
3. **Unified In-Memory Daemon Authority**: Authoritative runtime configuration (`Config`), connection telemetry (`ConnectionUsage`), token accounting (`TokenSourceLedger`), and usage ledgers (`UsageStatsStore`) exist as **single synchronized instances in daemon memory**. Session drivers hold shared handles (`Arc<RwLock<T>>`), never diverging clones.

### Tenet II: Pure Notification-Driven State Propagation
1. **Query Deprecation**: Static pull requests for dynamic runtime state (`QueryTokenUsage`, `QueryUsageStats`, `QueryProviderPicker`) are deprecated.
2. **State Mutation Guarantees Event Emission**: Any state change—token consumption, round completion, model switch, favorite toggle, credential modification—MUST immediately emit an event notification over the active event fabric. Frontends re-render reactively upon receiving events.

### Tenet III: Two-Tier Decoupled Event Fabric
Communication is strictly separated into two distinct scopes:
1. **Session Scope (`Wire::Response` / `AgentResponse::Round`)**: Conversation-private streaming (turn deltas, tool executions, approval dialogs, transcript mutations).
2. **Global Fabric Scope (`Wire::Broadcast` / `GlobalEvent`)**: Daemon-wide facts affecting all sessions and clients (model catalog updates, cross-session usage aggregates, configuration changes, daemon task updates). All active connections multiplex the global fabric.

---

## 3. Decision Specifications

### D1. Unified In-Memory Daemon Authority

The daemon maintains single instances of mutable state. `SessionDriver` stops owning isolated copies of `Config` and `ConnectionUsage`.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                                MUTA DAEMON                                  │
│                                                                             │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                    Authoritative State Handles                        │  │
│  │   SharedConfig = Arc<RwLock<Config>>                                  │  │
│  │   SharedConnectionUsage = Arc<RwLock<ConnectionUsage>>                │  │
│  │   SharedTokenLedger = Arc<TokenSourceLedger>                          │  │
│  │   SharedUsageStats = Arc<UsageStatsStore>                             │  │
│  └───────────────────────────────────┬───────────────────────────────────┘  │
│                                      │ Mutate & Notify                      │
│                                      ▼                                      │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                       Global Event Fabric                             │  │
│  │  - GlobalEvent::PickerUpdated(ProviderPickerSnapshot)                 │  │
│  │  - GlobalEvent::UsageStatsUpdated(UsageStatsReport)                   │  │
│  │  - GlobalEvent::ConfigUpdated(ConfigDelta)                            │  │
│  └───────────────────────────────────┬───────────────────────────────────┘  │
│                                      │ Multi-sink Fan-out                   │
│          ┌───────────────────────────┴───────────────────────────┐          │
│          ▼                                                       ▼          │
│  ┌───────────────────────────────┐               ┌───────────────────────┐  │
│  │     SessionDriver [S-101]     │               │  SessionDriver [S-102]│  │
│  │  Holds SharedConfig handles   │               │Holds SharedConfig hdl │  │
│  └───────────────┬───────────────┘               └───────────┬───────────┘  │
└──────────────────┼───────────────────────────────────────────┼──────────────┘
                   │ Wire Protocol                             │ Wire Protocol
                   ▼                                           ▼
┌──────────────────────────────────────┐   ┌──────────────────────────────────┐
│              TUI Client 1            │   │           TUI Client 2           │
│     Reactive Store: View Projection  │   │  Reactive Store: View Projection │
│       ZERO POLLING / ZERO GUESSING   │   │    ZERO POLLING / ZERO GUESSING  │
└──────────────────────────────────────┘   └──────────────────────────────────┘
```

- `SessionDriver` fields change from owned structs to shared handles:
  ```rust
  pub struct SessionDriver {
      ...
      pub config: Arc<RwLock<Config>>,
      pub provider_usage: Arc<RwLock<ConnectionUsage>>,
      ...
  }
  ```
- When a user changes a model via `/model` or the Models picker, the mutation updates the shared `Arc<RwLock<Config>>` and `Arc<RwLock<ConnectionUsage>>` in place, commits asynchronously to disk via the single-writer database pipeline, and immediately broadcasts the change to the global event fabric.
- Session switches (`reapply_session_selection`) and `/new` (`start_fresh_session`) read directly from the live shared memory state. The "read-from-disk-to-sync" workaround is completely deleted.

### D2. Turn-Boundary Proactive Telemetry Push

Dynamic session accounting flows proactively from the execution engine to the wire:
- In `muta-agent/src/orchestration.rs`, whenever a turn reaches a terminal state (`RoundCompleted`, `RoundInterrupted`, or round finish), the engine snapshots the session's ledger from `agent.token_ledger()` and immediately pushes `AgentResponse::TokenUsageReport`.
- Attached frontends receive the fresh report on the session event stream. If the Telemetry / Session Stats modal is open, the mutation applier (`AppMutation::TokenReport`) updates `app.token_report` and triggers an immediate frame redraw.
- Frontends never poll. The numbers increment naturally as turns execute and complete.

### D3. Daemon-Wide Cross-Session Picker Fan-Out

The model picker reflects global truth across all active sessions:
- `SessionRegistry` maintains `latest_picker: Arc<Mutex<Option<AgentResponse>>>`.
- Each hosted session's broadcast tap observes outgoing responses. When any session emits `AgentResponse::ProviderPicker(snapshot)`, the tap intercepts the event, records it in `latest_picker`, and calls `SessionRegistry::broadcast_picker_to_other_sessions(origin, response)`.
- Every other hosted session receives the fresh picker on its broadcast stream. Open Models and Connections modals re-sort instantly.
- When a new client attaches or re-synchronizes after a channel lag, `snapshot_attach_sync` splices the daemon-wide `latest_picker` over the session-local buffer, ensuring fresh attachers hydrate the true global model recency ordering.

### D4. Frontends as Pure Reactive Stores (Eradication of Polling)

In accordance with ADR-0197, the frontend is a strict client of protocol events:
- All polling timers, interval constants (`TOKEN_REPORT_REFRESH_PERIOD`, `USAGE_STATS_REFRESH_PERIOD`), and periodic refresh methods (`refresh_snapshot_dialogs`) are permanently removed from `mutx`.
- Frontends maintain view state purely as a reduction over inbound mutations:
  - `AgentResponse::TokenUsageReport` → `M::TokenReport` → updates `app.token_report`.
  - `AgentResponse::UsageStatsReport` → `M::UsageStats` → updates `app.usage_stats`.
  - `AgentResponse::ProviderPicker` → `M::ProviderPicker` → updates `app.provider_picker`.
- Modals become pure functional views of `App` state. If opened while idle, they render the current buffered snapshot. If open while turns execute, they update reactively in real time.

### D5. Elimination of Disk I/O as IPC

- The filesystem (`config.toml`, SQLite `state:connection_usage`) is an append-only/write-behind durability medium, never a synchronization channel between runtime threads.
- Direct disk reads in `reapply_session_selection` and startup paths are eliminated in favor of reading the synchronized `Arc<RwLock>` state.

---

## 4. What Gets Deleted

| Component | Target for Deletion | Replacement |
|---|---|---|
| `apps/tui/crates/mutx` | `refresh_snapshot_dialogs` | Proactive backend push (`TokenUsageReport`, `UsageStatsReport`) |
| `apps/tui/crates/mutx` | `token_report_refresh_at`, `usage_stats_refresh_at` | Reactive `AppMutation` updates |
| `apps/tui/crates/mutx` | `TOKEN_REPORT_REFRESH_PERIOD`, `USAGE_STATS_REFRESH_PERIOD` | Event-driven turn boundary hooks |
| `muta-runtime` | Cloned `Config` and `ConnectionUsage` per `SessionDriver` | Shared `Arc<RwLock<Config>>` and `Arc<RwLock<ConnectionUsage>>` |
| `muta-runtime` | `Config::load()` and `ConnectionUsage::load()` in `reapply_session_selection` | Direct read from shared daemon memory handles |
| `muta-contracts` | Reliance on `QueryTokenUsage` / `QueryUsageStats` for live UI | Proactive push events on round boundaries |

---

## 5. Invariant Proofs

### Invariant 1: Cross-Session Recency Consistency
*Theorem:* If Client A in Session 1 activates model $M$ at time $t_0$, any client opening the Models modal in Session 2 at $t_1 > t_0$ must see $M$ sorted according to its activation recency $t_0$.
*Proof:* Activation in Session 1 updates the shared `ConnectionUsage` authority and emits `ProviderPicker`. The broadcast tap forwards this snapshot to `SessionRegistry::broadcast_picker_to_other_sessions`, placing the frame into Session 2's event bus. Session 2's client translates the frame into `M::ProviderPicker`, updating `app.provider_picker`. Subsequent rendering passes calculate rankings from this updated snapshot. Q.E.D.

### Invariant 2: Zero Idle Wakeup Invariant
*Theorem:* A frontend displaying an open Session Stats modal in an idle session shall incur zero network round-trips and zero CPU wakeups outside terminal animation frames.
*Proof:* All timer-driven re-query mechanisms (`refresh_snapshot_dialogs`) are eradicated. The event loop's sleep timeout remains at its idle duration (1000ms) without dialog-specific timeout clamps. When no model turn is running, no `TokenUsageReport` is emitted. CPU utilization remains at zero. Q.E.D.

---

## 6. Migration Milestones

### Milestone 1: Turn-Boundary Token Report Push (Landed)
- Hooked `agent.token_ledger()` snapshot push into `RoundCompleted` and round finish/interrupt handlers in `muta-agent/src/orchestration.rs`.
- Proactively emits `AgentResponse::TokenUsageReport` on the live session stream.

### Milestone 2: Cross-Session Picker Fan-out (Landed)
- Implemented `SessionRegistry::broadcast_picker_to_other_sessions` and `latest_picker` cache in `muta-runtime/src/registry.rs`.
- Updated `serve.rs` attach-sync replay to splice the daemon-wide picker over per-session buffers.

### Milestone 3: Cross-Client Model Synchronization on /new (Landed)
- Updated `reapply_session_selection` in `muta-runtime/src/handlers_provider.rs` to synchronize with daemon defaults.

### Milestone 4: Shared In-Memory Daemon Authority (In Progress)
- Refactor `SessionDriver` to consume `Arc<RwLock<Config>>` and `Arc<RwLock<ConnectionUsage>>`.
- Unify session bootstrap to bind drivers to shared daemon handles.

### Milestone 5: Deprecation Sweep & Client Cleanliness (In Progress)
- Remove `QueryTokenUsage` and `QueryUsageStats` calls from TUI `enter_panel`.
- Deprecate corresponding query request variants in `muta-contracts`.
- Run compiler boundary tests verifying no frontend components initiate polling loops.

---

## 7. References

- ADR-0096: Persistent Multi-Session Daemon Architecture
- ADR-0122: Durable Cross-Session Usage Aggregation
- ADR-0134: Protocol Wire Envelope and Versioning Discipline
- ADR-0190: Agent as Actor — Unified Task Fabric and Loop Concurrency
- ADR-0197: One Frontend Truth — The Shell as a Client of the Engine and Protocol
