# 0163. Unified SQLite Event-Sourced Ledger, CAS Blob Storage, and Single-Writer Multi-Reader Architecture

- **Status:** Accepted
- **Date:** 2026-09-01
- **Builds on:** ADR-0014 (XDG persistence), ADR-0018 (multi-instance concurrency), ADR-0048 (session as single source of truth), ADR-0091 (command ledger), ADR-0096 (unified session daemon), ADR-0158 (native framed transport), ADR-0159 (protocol orthogonalization)
- **Supersedes:** ADR-0024 (legacy sqlite removal supersession note) and flat-file directory scanning for session catalogs and history search.

## Context

As Muta evolved from a local single-turn CLI into a multi-session daemon (`muta-runtime`), supporting interactive TUI (`mutx`), gRPC/IPC services (`proto/muta/v1/muta.proto`), and full-text history search across workspaces, the previous flat-file persistence model reached fundamental physical limits:

1. **Scalability Breakdown on Directory Scans**:
   - In the flat-file architecture, `ListSessions`, `SearchHistory`, and session metadata lookups required scanning hundreds of `.json` snapshot files and `.jsonl` event logs on disk ($O(N)$ disk I/O and JSON deserialization).
   - This introduced perceptible latency during daemon startup, workspace switching, and TUI picker rendering.

2. **Split-Brain and Dual-Writing Hazards**:
   - Partial transitions where some state lived in ad-hoc JSON files (`usage.json`, `route_settings.json`), some in `.jsonl` session files, and some in emerging database tables caused ambiguous "source-of-truth" semantics.
   - Ad-hoc file locking (`.lock` via `flock`) across distributed files increased edge-case failure modes under abnormal process termination.

3. **Absence of Native Full-Text Indexing (FTS)**:
   - Full-text history search (`SearchHistoryRequest` in `muta.proto`) was either unindexed or forced to do brute-force memory regex scans over all past session transcripts.

4. **Async-to-Sync Database Impedance Mismatch**:
   - Naively executing blocking SQLite calls inside Tokio asynchronous tasks blocks the runtime worker threads. Scattered `spawn_blocking` calls without structured batching cause thread thrashing and write lock contention.

## Decision

We execute a **clean-break, uncompromising unification of Muta's persistence layer**:

```
┌──────────────────────────────────────────────────────────────────────────┐
│                   Muta Runtime & Daemon (Tokio Engine)                   │
├──────────────────────────────────────────────────────────────────────────┤
│  Read Queries (Concurrent)               Write Events (Channel mpsc)     │
│  - ListSessions, SearchHistory           - AppendEvent, RecordCommand    │
│  - GetSessionDetail                      - UpdateKV, SetTitle            │
└──────────────┬───────────────────────────────────────────┬───────────────┘
               │                                           │
               ▼ (Read Pool)                               ▼ (Dedicated OS Thread)
┌──────────────────────────────┐            ┌──────────────────────────────┐
│  SQLite Reader Connections   │            │   SQLite Persistence Actor   │
│  - PRAGMA query_only = ON    │            │   - Micro-Batch Transactions │
│  - Direct In-Memory B-Tree   │            │   - WAL Commit Log           │
└──────────────┬───────────────┘            └──────────────┬───────────────┘
               │                                           │
               └───────────────────┬───────────────────────┘
                                   ▼
┌──────────────────────────────────────────────────────────────────────────┐
│                             Storage Layer                                │
├─────────────────────────────────────────────┬────────────────────────────┤
│  Single SQLite Database (muta.db)           │  CAS Blob Store (blobs/)   │
│  ├── sessions (Tree & Lineage)              │  ├── <sha256_hash_1>       │
│  ├── session_events (Monotonic Event Log)   │  ├── <sha256_hash_2>       │
│  ├── messages (Materialized Projection)     │  └── (Diffs, tool logs,    │
│  ├── commands (Command Ledger)              │       images > 4KB)        │
│  ├── fts_messages (FTS5 BM25 Search)        │                            │
│  └── kv_store (Configuration & KV Caches)   │                            │
└─────────────────────────────────────────────┴────────────────────────────┘
```

### 1. Authoritative SQLite Storage Engine (`muta.db`)

All persistent state is unified into a single embedded SQLite database (`$XDG_DATA_HOME/muta/muta.db` or configured state directory) managed by `muta-persistence::db`.

- **Engine PRAGMAs**:
  - `PRAGMA journal_mode = WAL;` (Write-Ahead Logging for high-throughput non-blocking concurrency)
  - `PRAGMA synchronous = NORMAL;` (Maximum durability without unnecessary synchronous disk stalls)
  - `PRAGMA foreign_keys = ON;` (Referential integrity enforcement across session trees and cascades)
  - `PRAGMA busy_timeout = 5000;` (Automatic retry backoff on write transactions)

- **Core Relational Schema & Event Ledger**:
  1. `sessions`: Relational session identity, lineage (`parent_id`, `fork_kind`), workspace root (`project_root`), title, and temporal timestamps.
  2. `session_events`: Strict monotonic append-only event ledger `(session_id, seq, event_type, payload, created_at_ms)`. Replaces standalone `.jsonl` files while preserving event-sourcing invariants.
  3. `messages`: Materialized message log with direct mapping to `muta.proto::Message`, role, provider, model, and token attribution.
  4. `commands`: Structured command execution audit ledger (ADR-0091) tracking commands, arguments, results, and execution statuses.
  5. `fts_messages` & `fts_commands`: Native **SQLite FTS5** virtual tables indexing prompts, assistant outputs, reasoning text, and command arguments with BM25 ranking for sub-millisecond global search.
  6. `kv_store`: Key-value registry for session-scoped or global settings, provider usage caches, and dynamic states.

### 2. Content-Addressable Storage (CAS) Threshold Isolation

To prevent database bloat and maintain cache locality:
- Payloads exceeding **4 KB** (such as large compilation outputs, extensive git diffs, file snapshots, and multimodal images) are persisted to the content-addressed blob store (`$XDG_DATA_HOME/muta/blobs/<sha256>`).
- Database rows store the SHA-256 hash reference (`content_blob_hash`) and payload size.
- A mark-and-sweep garbage collection routine cleans orphaned blobs by scanning active database references.

### 3. Single-Writer Actor & Multi-Reader Connection Pool

To eliminate the async-to-sync impedance mismatch:
- **Write Path (Persistence Actor)**:
  - Writes are dispatched through a dedicated asynchronous channel (`mpsc::Sender<PersistenceCommand>`) to a dedicated persistence OS worker thread.
  - The worker processes events with micro-batching (flushing within 5ms or 32 events), executing batch inserts inside a single transaction.
  - Zero lock contention; Tokio worker threads are never blocked on I/O.
- **Read Path (Reader Pool)**:
  - Read queries (`ListSessions`, `SearchHistory`, `GetSessionDetail`) acquire connections from a pooled reader set with `PRAGMA query_only = ON`.
  - Reads run concurrently without interfering with the write transaction stream.

### 4. Zero-Compromise Migration & Export Discipline

- All schema evolution is tracked linearly via `PRAGMA user_version` and atomic migration arrays (`MIGRATIONS`).
- CLI tooling (`muta session export <id> --format jsonl` and `muta session import <file>`) provides lossless round-trip JSONL export/import, preserving developer text-stream transparency.

## Alternatives Considered

1. **Retaining Flat JSONL Files with SQLite Index Cache**:
   - *Rejected*: Dual-write architectures inherently suffer from split-brain drift, desynchronization during hard process aborts, and doubled I/O write amplification.
2. **Heavy Server-Client Database (e.g. Embedded PostgreSQL / SurrealDB)**:
   - *Rejected*: Violates Muta's zero-dependency single-binary workstation ethos (ADR-0013, ADR-0014).
3. **Pure Asynchronous Async-SQL (e.g. `sqlx`)**:
   - *Rejected*: Incurs heavy asynchronous trait runtime overhead without improving single-machine disk throughput compared to a dedicated OS-thread SQLite WAL writer.

## Consequences

- **Positive**:
  - Sub-millisecond execution for `ListSessions`, `GetSessionDetail`, and `SearchHistory`.
  - True ACID transaction guarantees preventing corrupt session files.
  - Elimination of custom `.lock` file orchestration across session folders.
  - Zero Tokio worker thread blocking via the Actor write pipeline.
- **Negative**:
  - Requires compiling SQLite C-amalgamation (`libsqlite3-sys`), which is handled via standard bundled feature compilation.
- **Neutral**:
  - Existing session JSONL files can be imported into `muta.db` via the startup migration bridge.

## References

- [SQLite Write-Ahead Logging](https://www.sqlite.org/wal.html)
- [SQLite FTS5 Extension](https://www.sqlite.org/fts5.html)
- ADR-0048: Session as Single Source of Truth
- ADR-0091: Command Ledger and Typed Results
- ADR-0096: Unified Session Daemon
- ADR-0158: Native Framed Transport for Local Daemon IPC
