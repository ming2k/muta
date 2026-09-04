# 0178. Persistent Single-Writer Actor, Zero-Churn WAL Ring Buffer, and Hardware-Conscious Storage Architecture

- **Status:** Accepted
- **Date:** 2026-09-15
- **Builds on:** ADR-0014 (XDG persistence), ADR-0048 (session as single source of truth), ADR-0163 (unified SQLite event ledger & CAS), ADR-0168 (complete SQLite unification)

## Context

While ADR-0163 and ADR-0168 established the architectural blueprint for a unified SQLite engine (`muta.db`) powered by a single-writer persistence actor (`PersistenceHandle` / `PersistenceActor`), the implementation across `SessionStore` and persistence call sites retained an ad-hoc connection model:

1. **Short-Lived Connection Churn ("Open-Write-Drop")**:
   - In `crates/muta-persistence/src/session/mod.rs` (`persist_to`), every ReAct turn boundary or session mutation invoked:
     ```rust
     let engine = crate::db::DatabaseEngine::open(db_path, Some(blob_store.clone()))?;
     engine.save_session_full(&data)?;
     // engine is dropped here; SQLite connection closes
     ```
   - When the last SQLite connection to a WAL database closes, SQLite automatically initiates an implicit checkpoint and resets or deletes the `-wal` and `-shm` files.

2. **Pathological Write Amplification & Flash Device Wear**:
   - Every ReAct turn caused `-shm` and `-wal` to be created, populated with a few kilobytes, force-checkpointed into the main database, and subsequently truncated or deleted.
   - This cyclic destruction and re-creation induced heavy filesystem metadata churn (directory and inode journal updates), defeated SQLite's write-ahead log sequential buffering, and subjected flash storage (NVMe/SSD/eMMC) to severe write amplification and premature block wear.

3. **Residual Dual-Track Overhead**:
   - `SessionStore::persist_off_runtime` retained references to legacy `EventLog` (`compact_log_if_needed(&log_path, &data)`), attempting to manage flat `.jsonl` files concurrently with SQLite persistence, causing redundant disk scans.

## Decision

We execute a complete modernization of the SQLite persistence pipeline to guarantee persistent connection lifetimes, zero-churn WAL ring buffering, and hardware-conscious I/O:

```
┌──────────────────────────────────────────────────────────────────────────┐
│                   Muta Runtime & Daemon (Tokio Engine)                   │
├──────────────────────────────────────────────────────────────────────────┤
│  Read Queries (Concurrent)               Write Events (Single-Writer)    │
│  - ListSessions, SearchHistory           - AppendEvent, RecordCommand    │
│  - GetSessionDetail, GetUsageStats       - SaveSessionFull, SetKV        │
└──────────────┬───────────────────────────────────────────┬───────────────┘
               │                                           │
               ▼ (Read Pool / Engine)                      ▼ (mpsc channel)
┌──────────────────────────────┐            ┌──────────────────────────────┐
│  SQLite Reader Connections   │            │   SQLite Persistence Actor   │
│  - WAL Concurrent Reads      │            │   - Dedicated OS Worker      │
│  - Zero Lock Contention      │            │   - Single Persistent Writer │
└──────────────┬───────────────┘            └──────────────┬───────────────┘
               │                                           │
               └───────────────────┬───────────────────────┘
                                   ▼
┌──────────────────────────────────────────────────────────────────────────┐
│                  Storage Layer (OS Page Cache ➔ NVMe/SSD)                │
├──────────────────────────────────────────────────────────────────────────┤
│  muta.db                                                                 │
│  muta.db-wal (Persistent Ring Buffer; PRAGMA journal_size_limit = 16MB)  │
│  muta.db-shm (Shared Memory Index; zero deletion churn)                  │
│  blobs/      (CAS payload store for items > 4KB)                         │
└──────────────────────────────────────────────────────────────────────────┘
```

### 1. Authoritative Routing Through `PersistenceHandle`
- All session writes (`persist_off_runtime` and `save_session_full`) delegate directly to the persistent background actor via `crate::db::get_persistence_handle()`.
- The actor retains an open `DatabaseEngine` writer on its dedicated OS worker thread for the entire application lifetime.
- Because the writer connection remains permanently active during runtime, connection count never drops to zero mid-session, eliminating the cyclic deletion and re-creation of `-wal` and `-shm`.

### 2. Zero-Churn WAL & Flash-Optimized SQLite Pragmas
Every database connection is initialized with parameters calibrated for SSD durability and transactional safety:

| PRAGMA | Configuration | Rationale |
| :--- | :--- | :--- |
| `journal_mode` | `WAL` | Enables concurrent readers and sequential write-ahead logging without page locks. |
| `synchronous` | `NORMAL` | In WAL mode, `NORMAL` guarantees full database integrity across process crashes without synchronous disk head stalls on every commit, saving up to 90% flash I/O wear. |
| `journal_size_limit` | `16777216` (16 MB) | Retains the `-wal` file as a recycled ring buffer up to 16MB after checkpoints, preventing repeated filesystem allocation and deallocation cycles. |
| `wal_autocheckpoint` | `1000` (4 MB) | Smooths checkpointing across approximately 4MB boundaries, eliminating turn-by-turn checkpoint thrashing. |
| `temp_store` | `MEMORY` | Directs temporary indices, CTE materializations, and sort buffers to RAM, producing zero disk artifacts. |
| `busy_timeout` | `5000` | Eliminates spurious transient busy errors during concurrent multi-session read loads. |

### 3. Elimination of Residual Flat-File Churn
- Deprecate flat `.jsonl` compaction calls (`compact_log_if_needed`) from the SQLite write pipeline.
- SQLite `muta.db` is the sole authoritative source of truth for both sequential events (`session_events`) and materialized snapshots (`sessions`, `messages`).

## Consequences

### Positive
- **Hardware Lifespan Protection**: Completely eliminates `-shm` and `-wal` oscillation and filesystem metadata thrashing; writes are grouped as sequential append operations within the WAL ring buffer.
- **Zero-Latency Persistence**: Async mpsc dispatch from Tokio tasks completes in microseconds without holding blocking file locks or performing synchronous checkpointing.
- **Robust Crash Safety**: Full ACID transactions under `synchronous = NORMAL` protect against corruption and power failure without penalizing runtime performance.
- **Clean Architecture**: Fulfills the Single-Writer Multi-Reader contract outlined in ADR-0168.

### Negative / Neutral
- The `-wal` and `-shm` files remain visible in `$XDG_DATA_HOME/muta/` while the application or daemon is active, which is normal and expected SQLite WAL behavior.

## References
- ADR-0014: Platform-native XDG persistence architecture
- ADR-0048: Session as the single source of truth
- ADR-0163: Unified SQLite event-sourced ledger and CAS architecture
- ADR-0168: Complete SQLite unification and legacy persistence purge
