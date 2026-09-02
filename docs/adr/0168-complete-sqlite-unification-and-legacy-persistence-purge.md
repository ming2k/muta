# 0168. Complete SQLite Unification, Zero-Compromise Legacy Persistence Purge, and Native Storage Modernization

- **Status:** Accepted
- **Date:** 2026-09-01
- **Builds on:** ADR-0014 (XDG persistence), ADR-0048 (session as single source of truth), ADR-0091 (command ledger), ADR-0121 (instance isolation), ADR-0163 (unified sqlite event ledger & CAS)
- **Supersedes:** ADR-0018 (multi-instance session file concurrency via `flock`), ADR-0024 (legacy sqlite removal), and all flat-file session/usage storage models.

## Context

While ADR-0163 laid down the architecture for a unified SQLite engine (`muta.db`) with CAS threshold offloading and FTS5 search, the production codebase retained a dual-track persistence implementation:

1. **Dual-Track Split-Brain Risk**:
   - `SessionStore` continued reading and writing `sessions/<id>.json` snapshots and `sessions/<id>.jsonl` event logs, with expensive directory-walking (`fs::read_dir`) during session listing.
   - `usage_stats.rs` persisted telemetry across daily files (`usage/daily/YYYY-MM-DD.json`), requiring repetitive disk scans for aggregate reporting.
   - `route_settings.rs` and `connection_usage.rs` maintained independent JSON files guarded by individual POSIX file locks (`.lock` via `flock`).

2. **Residual Command-Line Confusion**:
   - The obsolete `--home <dir>` CLI option left remnants in internal structs (`PathsOverride`) and documentation, despite instance isolation having moved completely to the `MUTA_HOME` environment variable.

3. **Performance & Scalability Overhead**:
   - $O(N)$ directory scans during startup and workspace switching incurred noticeable latency and file descriptor churn.
   - Separate ad-hoc file locks were fragile under unexpected SIGKILL or multi-process concurrency.

## Decision

We execute a **clean-break, zero-compromise transition** to make SQLite the sole authoritative storage engine across all persistent application domains:

```
┌──────────────────────────────────────────────────────────────────────────┐
│                   Muta Runtime & Daemon (Tokio Engine)                   │
├──────────────────────────────────────────────────────────────────────────┤
│  Read Queries (Concurrent)               Write Events (Single-Writer)    │
│  - ListSessions, SearchHistory           - AppendEvent, RecordCommand    │
│  - GetSessionDetail, GetUsageStats       - UpsertKV, SaveRouteSettings   │
└──────────────┬───────────────────────────────────────────┬───────────────┘
               │                                           │
               ▼ (Read Pool)                               ▼ (OS Worker Thread)
┌──────────────────────────────┐            ┌──────────────────────────────┐
│  SQLite Reader Connections   │            │   SQLite Persistence Actor   │
│  - WAL Concurrent Reads      │            │   - Micro-batched Writes     │
│  - In-Memory / File B-Trees  │            │   - Zero Tokio Worker Stalls │
└──────────────┬───────────────┘            └──────────────┬───────────────┘
               │                                           │
               └───────────────────┬───────────────────────┘
                                   ▼
┌──────────────────────────────────────────────────────────────────────────┐
│                             Storage Layer                                │
├─────────────────────────────────────────────┬────────────────────────────┤
│  Single SQLite Database (muta.db)           │  CAS Blob Store (blobs/)   │
│  ├── sessions (Lineage & Metadata)          │  ├── <sha256_hash_1>       │
│  ├── session_events (Monotonic Event Log)   │  ├── <sha256_hash_2>       │
│  ├── messages (Materialized Projection)     │  └── (Payloads > 4 KB)     │
│  ├── commands (Command Ledger)              │                            │
│  ├── fts_messages (FTS5 Full-Text Index)    │                            │
│  └── kv_store (Usage, Settings & Routes)    │                            │
└─────────────────────────────────────────────┴────────────────────────────┘
```

### 1. Unified Session Store & Query Routing
- `SessionStore` delegates all queries (`list_for_project`, `get_detail`, `search_history`) directly to `DatabaseEngine` indices and SQLite FTS5.
- Mutations (`append_message`, `replace_messages`, `commit_turn`, `set_title`) dispatch structured events via `PersistenceHandle` to the single-writer persistence actor.
- File-based `.json` snapshot writing and `.jsonl` appends in `projects/<hash>/sessions/` are completely retired.

### 2. State & Telemetry Consolidation into `kv_store`
- `UsageStats`, `RouteSettings`, and `ConnectionUsage` are stored and queried directly through `muta.db` (`kv_store` or dedicated relational tables).
- All ad-hoc `.lock` companion files and cross-file flock synchronization routines are removed. SQLite's native WAL lock manager guarantees ACID compliance.

### 3. Automated One-Time Legacy Migration
- On startup, the persistence engine inspects the legacy `projects/` and `usage/` directory structures.
- Existing `.jsonl` session files and `.json` snapshots are parsed once and imported into `muta.db`.
- Successfully migrated legacy directories are safely archived or purged, eliminating obsolete disk clutter.

### 4. Total Purge of `--home` Artifacts
- The `--home` parameter is fully purged from all codebase paths, data types (`PathsOverride`), and documentation, leaving `MUTA_HOME` as the sole instance isolation mechanism.

## Consequences

### Positive
- Sub-millisecond session catalog resolution and history search.
- Clean, unified persistence boundary with zero split-brain ambiguity.
- Complete removal of fragile POSIX file locking across scattered state files.
- Simplified backup: backing up `$XDG_DATA_HOME/muta/muta.db` and `$XDG_DATA_HOME/muta/blobs/` captures 100% of application state.

### Negative
- Requires bundled SQLite compilation via `rusqlite`, which is already standard in the project stack.
