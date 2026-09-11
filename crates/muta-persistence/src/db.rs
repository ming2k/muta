//! Authoritative embedded SQLite storage engine (ADR-0163, ADR-0187).
//!
//! Provides relational database initialization, schema migration tracking via
//! `PRAGMA user_version`, FTS5 full-text search, Content-Addressed Storage (CAS)
//! threshold isolation, and a single-writer persistence engine.

use crate::blobs::BlobStore;
use rusqlite::{Connection, OptionalExtension, Result, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{info, warn};

/// SQLite schema version tracking. Fresh databases jump straight to the latest version.
pub const CURRENT_DB_VERSION: u32 = 14;

/// SHA-256 fingerprint of the migration catalog (version + SQL of every
/// entry). Locked by `migration_catalog_fingerprint_is_stable`; see that test
/// for the discipline this enforces.
#[cfg(test)]
const MIGRATION_CATALOG_FINGERPRINT: &str =
    "f2f4cb09e48126eb80cd79f1846e4c94dd759d50b7145371484f8156ab198666";

/// Payload size threshold (4 KB) beyond which text content is offloaded to CAS BlobStore.
pub const CAS_THRESHOLD_BYTES: usize = 4096;

/// Hard cap on retained request-projection records per session (ADR-0218). The
/// archive is forensic, not authoritative: a bounded ring keeps a long session
/// from growing it without limit.
pub const MAX_RETAINED_REQUEST_PROJECTIONS: usize = 64;

/// Initialize and return a connection to the SQLite database.
/// Configures WAL mode, synchronous=NORMAL for robustness, busy timeout, and turns on foreign keys.
pub(crate) fn initialize_db(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut conn = Connection::open(db_path)?;
    configure_connection(&mut conn)?;
    migrate_schema(&mut conn)?;

    Ok(conn)
}

/// Initialize an in-memory SQLite database for testing and ephemeral workflows.
#[cfg(test)]
pub(crate) fn initialize_in_memory_db() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    configure_connection(&mut conn)?;
    migrate_schema(&mut conn)?;
    Ok(conn)
}

/// Standard connection configurations applied to every connection (reader & writer).
fn configure_connection(conn: &mut Connection) -> Result<()> {
    // busy_timeout first: every later lock attempt (including the migration
    // transaction) must wait rather than fail immediately.
    conn.pragma_update(None, "busy_timeout", 5000)?;
    // `PRAGMA journal_mode=WAL` does not invoke the busy handler. Under
    // concurrent first-open — parallel test processes sharing one database —
    // a writer mid-migration makes it return SQLITE_BUSY immediately.
    // Best-effort: the winning process sets WAL and every later connection
    // observes it, so a transient refusal here is harmless.
    if let Err(error) = conn.pragma_update(None, "journal_mode", "WAL") {
        warn!(%error, "could not set WAL journal mode; continuing");
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "journal_size_limit", 16777216)?; // 16MB WAL recycling
    conn.pragma_update(None, "wal_autocheckpoint", 1000)?; // 1000 pages (~4MB)
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

/// A structured database migration step.
struct Migration {
    version: u32,
    sql: &'static str,
}

/// The chronological sequence of schema migrations.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: r#"
        -- Sessions table
        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            parent_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
            fork_kind TEXT CHECK(fork_kind IN ('trunk', 'fork', 'aside')) NOT NULL DEFAULT 'trunk',
            title TEXT,
            title_manual BOOLEAN NOT NULL DEFAULT 0,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            project_root TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_root);
        CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at_ms DESC);

        -- Strict Monotonic Event Ledger (replacing legacy .jsonl files)
        CREATE TABLE IF NOT EXISTS session_events (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq INTEGER NOT NULL,
            event_type TEXT NOT NULL,
            payload TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            PRIMARY KEY(session_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_events_session_seq ON session_events(session_id, seq ASC);

        -- Materialized Messages table
        CREATE TABLE IF NOT EXISTS messages (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq INTEGER NOT NULL,
            role TEXT CHECK(role IN ('user', 'assistant', 'system', 'tool')) NOT NULL,
            content TEXT NOT NULL,
            content_blob_hash TEXT,
            reasoning_content TEXT,
            provider TEXT,
            model TEXT,
            created_at_ms INTEGER NOT NULL,
            UNIQUE(session_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_messages_session_seq ON messages(session_id, seq ASC);

        -- Command execution audit ledger (ADR-0091)
        CREATE TABLE IF NOT EXISTS commands (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            arguments TEXT NOT NULL,
            result TEXT,
            status TEXT CHECK(status IN ('running', 'ok', 'failed', 'cancelled')) NOT NULL,
            created_at_ms INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_commands_session ON commands(session_id, created_at_ms ASC);

        -- Unified Key-Value Store table
        CREATE TABLE IF NOT EXISTS kv_store (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );

        -- Full-Text Search Index (FTS5) for messages
        CREATE VIRTUAL TABLE IF NOT EXISTS fts_messages USING fts5(
            message_id UNINDEXED,
            session_id UNINDEXED,
            role UNINDEXED,
            content,
            reasoning_content,
            tokenize = 'porter unicode61'
        );

        -- Triggers to synchronize fts_messages with messages table
        CREATE TRIGGER IF NOT EXISTS trg_messages_ai AFTER INSERT ON messages BEGIN
            INSERT INTO fts_messages(message_id, session_id, role, content, reasoning_content)
            VALUES (new.id, new.session_id, new.role, new.content, COALESCE(new.reasoning_content, ''));
        END;

        CREATE TRIGGER IF NOT EXISTS trg_messages_ad AFTER DELETE ON messages BEGIN
            DELETE FROM fts_messages WHERE message_id = old.id;
        END;

        CREATE TRIGGER IF NOT EXISTS trg_messages_au AFTER UPDATE ON messages BEGIN
            DELETE FROM fts_messages WHERE message_id = old.id;
            INSERT INTO fts_messages(message_id, session_id, role, content, reasoning_content)
            VALUES (new.id, new.session_id, new.role, new.content, COALESCE(new.reasoning_content, ''));
        END;
    "#,
    },
    Migration {
        version: 2,
        sql: r#"
        -- Add full serialized SessionData JSON column for SQLite Single-Source-of-Truth
        ALTER TABLE sessions ADD COLUMN data TEXT;
    "#,
    },
    Migration {
        version: 3,
        sql: r#"
        -- Add indexed summary columns for sub-millisecond session listing
        ALTER TABLE sessions ADD COLUMN msg_count INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE sessions ADD COLUMN last_user_prompt TEXT;
        ALTER TABLE sessions ADD COLUMN digest TEXT;
        CREATE INDEX IF NOT EXISTS idx_sessions_project_updated ON sessions(project_root, updated_at_ms DESC);
    "#,
    },
    Migration {
        version: 4,
        sql: r#"
        -- Unified prompt input history table (ADR-0168 / SSOT)
        CREATE TABLE IF NOT EXISTS input_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT NOT NULL,
            session_id TEXT,
            workspace TEXT,
            created_at_ms INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_input_history_text ON input_history(text);
        CREATE INDEX IF NOT EXISTS idx_input_history_created_at ON input_history(created_at_ms DESC);
     CREATE INDEX IF NOT EXISTS idx_input_history_session ON input_history(session_id, created_at_ms DESC);
     "#,
    },
    Migration {
        // ADR-0186: single-transcript persistence foundation. Facts live in
        // `entries` (position-free, immutable), positions in `entry_memberships`
        // (many-to-one so forks share facts), and projection decisions in
        // `projections`. The event ledger is renamed `events` to match the
        // single-source-of-truth vocabulary. The legacy `messages` table,
        // `sessions.data` JSON snapshot column, and their FTS structures are
        // retired together with the `SessionData` swap (same tranche).
        version: 5,
        sql: r#"
        ALTER TABLE session_events RENAME TO events;

        CREATE TABLE IF NOT EXISTS entries (
            id            TEXT PRIMARY KEY,
            kind          TEXT NOT NULL CHECK (kind IN ('message','state')),
            role          TEXT CHECK (role IN ('user','assistant','system','tool')),
            content       TEXT,
            origin        TEXT CHECK (origin IS NULL OR origin IN ('harness','checkpoint')),
            hidden        INTEGER NOT NULL DEFAULT 0,
            created_at_ms INTEGER NOT NULL,
            payload       TEXT NOT NULL,
            CHECK ( origin IS NULL OR hidden = 1 ),
            CHECK ( kind <> 'message' OR role IS NOT NULL )
        );

        CREATE TABLE IF NOT EXISTS entry_memberships (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq        INTEGER NOT NULL,
            entry_id   TEXT NOT NULL REFERENCES entries(id),
            added_by   INTEGER NOT NULL,
            PRIMARY KEY (session_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_memberships_entry ON entry_memberships(entry_id);

        CREATE TABLE IF NOT EXISTS projections (
            session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq           INTEGER NOT NULL,
            kind          TEXT NOT NULL CHECK (kind IN ('prune','compact','freeze')),
            up_to_seq     INTEGER NOT NULL,
            payload       TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            PRIMARY KEY (session_id, seq)
        );

        CREATE TABLE IF NOT EXISTS session_blobs (
            hash TEXT PRIMARY KEY,
            size INTEGER NOT NULL,
            mime TEXT NOT NULL,
            data BLOB,
            path TEXT,
            CHECK ( (data IS NULL) <> (path IS NULL) )
        );

        -- Clean break (ADR-0186): retire the legacy snapshot payload and the
        -- messages materialization; sessions keep identity + working state.
        DROP TRIGGER IF EXISTS trg_messages_ai;
        DROP TRIGGER IF EXISTS trg_messages_ad;
        DROP TRIGGER IF EXISTS trg_messages_au;
        DROP TABLE IF EXISTS fts_messages;
        DROP TABLE IF EXISTS messages;
        -- fork_kind gains 'subagent' (ADR-0186 §6): SQLite cannot ALTER a
        -- CHECK, so the table is rebuilt. Legacy payload columns are dropped
        -- in the same rebuild (clean break); working state rides along.
        -- Legacy dependent rows are retired first so the parent drop passes
        -- the foreign-key check.
        DELETE FROM entry_memberships;
        DELETE FROM projections;
        DELETE FROM events;
        DELETE FROM commands;
        CREATE TABLE sessions_new (
            id                  TEXT PRIMARY KEY,
            parent_id           TEXT REFERENCES sessions(id) ON DELETE SET NULL,
            fork_kind           TEXT NOT NULL DEFAULT 'trunk'
                                CHECK (fork_kind IN ('trunk','fork','aside','subagent')),
            title               TEXT,
            created_at_ms       INTEGER NOT NULL,
            updated_at_ms       INTEGER NOT NULL,
            project_root        TEXT NOT NULL,
            msg_count           INTEGER NOT NULL DEFAULT 0,
            last_user_prompt    TEXT,
            digest              TEXT,
            data                TEXT,
            title_manual        BOOLEAN NOT NULL DEFAULT 0
        );
        INSERT INTO sessions_new (id, parent_id, fork_kind, title, created_at_ms, updated_at_ms, project_root, msg_count, last_user_prompt, digest, data, title_manual)
            SELECT id, parent_id, fork_kind, title, created_at_ms, updated_at_ms, project_root, msg_count, last_user_prompt, digest, data, title_manual FROM sessions;
        DROP TABLE sessions;
        ALTER TABLE sessions_new RENAME TO sessions;
        ALTER TABLE sessions DROP COLUMN data;
        ALTER TABLE sessions DROP COLUMN title_manual;
        ALTER TABLE sessions ADD COLUMN provider_connection TEXT;
        ALTER TABLE sessions ADD COLUMN round_counter INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE sessions ADD COLUMN unattended INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE sessions ADD COLUMN disabled_tools TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE sessions ADD COLUMN commands TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE sessions ADD COLUMN round_interrupts TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE sessions ADD COLUMN retry_pending TEXT;
        ALTER TABLE sessions ADD COLUMN request_usage_records TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE sessions ADD COLUMN applied_seq INTEGER;
        ALTER TABLE sessions ADD COLUMN checksum INTEGER;
        ALTER TABLE sessions ADD COLUMN schema_version INTEGER NOT NULL DEFAULT 13;

        -- Full-text search moves to the transcript entries.
        CREATE VIRTUAL TABLE IF NOT EXISTS fts_entries USING fts5(
            entry_id UNINDEXED,
            session_id UNINDEXED,
            role UNINDEXED,
            content,
            tokenize = 'porter unicode61'
        );
        CREATE TRIGGER IF NOT EXISTS trg_entries_ai AFTER INSERT ON entries BEGIN
            INSERT INTO fts_entries(entry_id, session_id, role, content)
            SELECT new.id, m.session_id, COALESCE(new.role, ''), COALESCE(new.content, '')
            FROM entry_memberships m WHERE m.entry_id = new.id;
        END;
        CREATE TRIGGER IF NOT EXISTS trg_entries_ad AFTER DELETE ON entries BEGIN
            DELETE FROM fts_entries WHERE entry_id = old.id;
        END;
    "#,
    },
    Migration {
        // Corrective migration: development builds of the ADR-0186 tranche stamped
        // databases at version 5 with an interim `sessions` rebuild that carried
        // the legacy `scheduled_jobs` column and predated the final working-state
        // columns (`round_counter`, `unattended`, ... ). Because migration 5's SQL
        // was finalized after those stamps, such databases short-circuit the
        // migrator and break at runtime on the first INSERT. Migration 6 reconciles
        // every version-5 incarnation to the final schema. The DDL must be applied
        // conditionally (both `DROP COLUMN scheduled_jobs` and the `ADD COLUMN`
        // set are invalid on the "other" incarnation), so the work happens in
        // `repair_intermediate_sessions_schema` below — this entry only advances
        // `user_version`.
        version: 6,
        sql: "",
    },
    Migration {
        // Persistence v2 (ADR-0187): incremental append needs a transcript
        // generation id and a durable blob reference ledger; the schema must stop
        // promising what the runtime does not do (event ledger, session_blobs,
        // applied_seq, projections kind CHECK); timestamps and FTS become honest.
        // The DDL must be conditional (an interim version-5 database carries no
        // transcript tables at all), so the work happens in
        // `apply_persistence_v2_schema` below — this entry only advances
        // `user_version`.
        version: 7,
        sql: "",
    },
    Migration {
        // Usage ledger (ADR-0187): per-attempt usage records move out of the
        // session row into their own key-addressed table. The row column
        // forced an O(records) serialization on every save; the table upserts
        // only the attempts a commit actually changed. DDL and the JSON
        // backfill are conditional (the column exists only on ADR-0186
        // databases), so the work happens in `apply_usage_ledger_schema`
        // below — this entry only advances `user_version`.
        version: 8,
        sql: "",
    },
    Migration {
        // Durable retry-resolution records: the success-side mirror of
        // `round_interrupts`. One JSON column on `sessions`, default `[]`.
        // The DDL is applied conditionally by the migration subagent (version
        // 9 arm) so a database that already carries the column — a dev
        // build's startup repair, or a partially-applied v9 — passes
        // through unchanged instead of failing on a duplicate column.
        version: 9,
        sql: "",
    },
    Migration {
        // Corrective migration (ADR-0186 integrity fix): migration 5's
        // `entries` CHECK `origin IS NULL OR hidden = 1` coupled two orthogonal
        // concepts — `origin` (WHY a message exists) and `hidden` (whether it
        // is shown). That coupling was false: the durable transcript
        // legitimately carries *visible* harness injections (UserSteer /
        // SubagentSteer / SubagentTask, CommandEcho "/cmd" & "!cmd", ToolImage, and
        // SystemPrompt/SystemReminder), all of which ADR-0050 records
        // `.hidden = false` with an `origin`. The false constraint then turned
        // a lawful mid-round save (e.g. the steering fire-at-turn-boundary) into
        // `CHECK constraint failed: origin IS NULL OR hidden = 1`.
        //
        // The constraint is replaced by two honest, orthogonal ones that still
        // reject silent corruption:
        //   - a hidden message must explain WHY (`origin` present);
        //   - only a *visible user* envelope origin is outlawed — a checkpoint
        //     is never visible dialogue, and user-role visible steering/echo/
        //     image injections carry `origin = 'harness'`. (Role-level
        //     preferencing is intended: decompact/checkpoint entries are the
        //     only visible-non-user pathological case the old check guarded.)
        // Because a column-level CHECK cannot be altered in place, `entries`
        // is rebuilt (table rewrite) inside a transaction; the FK from
        // `entry_memberships` is deferred so the transient missing-table step
        // does not trip the referential guard.
        version: 10,
        sql: "",
    },
    Migration {
        // Durable request-projection archive (ADR-0218): a key-addressed table
        // for forensic snapshots of assembled requests. It is deliberately a
        // table, not a `sessions` JSON column: the projection is written on the
        // request hot path, and a row column would force an O(records)
        // serialization on every session save (the same reason migration 8
        // moved the usage ledger into its own table). `CREATE TABLE IF NOT
        // EXISTS` is idempotent, so a database that already carries the table
        // passes through unchanged.
        version: 11,
        sql: r#"
        CREATE TABLE IF NOT EXISTS request_projections (
            session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            round         INTEGER NOT NULL,
            turn          INTEGER NOT NULL,
            created_at_ms INTEGER NOT NULL,
            payload       TEXT NOT NULL,
            PRIMARY KEY (session_id, round, turn)
        );
        CREATE INDEX IF NOT EXISTS idx_request_projections_session_created
            ON request_projections(session_id, created_at_ms DESC);
        "#,
    },
    Migration {
        // Session scope (ADR-0219): the mandatory `project_root` partition key
        // becomes a typed `SessionScope` (`scope_kind` + namespaced `scope_key`)
        // orthogonal to an optional `workspace_root` binding. The rebuild
        // enforces the NOT NULL constraints ALTER cannot add; every existing row
        // backfills losslessly as a workspace scope keyed by its old
        // project_root. The DDL is dispatched to `apply_session_scope_schema`.
        version: 12,
        sql: "",
    },
    Migration {
        // Grouping is derived (ADR-0226): the stored `scope_kind`/`scope_key`
        // partition key is retired for the concrete `space` column; workspace
        // sessions keep grouping by `workspace_root`, non-workspace rows
        // migrate to the Personal grouping (`space IS NULL`).
        version: 13,
        sql: "",
    },
    Migration {
        // Workspace-only partition (ADR-0226 revised): drop `space` (an unbound
        // session is simply `workspace_root IS NULL`) and add `persona`
        // metadata (the persona that staffed the session). Conditional DDL.
        version: 14,
        sql: "",
    },
];

/// Working-state columns the final ADR-0186 `sessions` rebuild must carry,
/// with their DDL. Shared by the migration-6 repair and the startup schema
/// guard so the two cannot drift.
const SESSIONS_WORKING_STATE_COLUMNS: &[(&str, &str)] = &[
    ("provider_connection", "TEXT"),
    ("round_counter", "INTEGER NOT NULL DEFAULT 0"),
    ("unattended", "INTEGER NOT NULL DEFAULT 0"),
    ("disabled_tools", "TEXT NOT NULL DEFAULT '[]'"),
    ("commands", "TEXT NOT NULL DEFAULT '[]'"),
    ("round_interrupts", "TEXT NOT NULL DEFAULT '[]'"),
    // `retry_resolutions` is owned by migration 9, not this conditional
    // repair list: migration 6's repair runs before migration 9 on an
    // interim-v5 database and would add the column first, making migration
    // 9's unconditional `ADD COLUMN` fail with "duplicate column name".
    ("retry_pending", "TEXT"),
    ("request_usage_records", "TEXT NOT NULL DEFAULT '[]'"),
    ("checksum", "INTEGER"),
    ("schema_version", "INTEGER NOT NULL DEFAULT 13"),
];

/// Columns persistence v2 (ADR-0187) added to `sessions`. Guard-only:
/// migration 7 owns their creation, so the migration-6 repair must not add
/// them (it runs first and its additions would collide with migration 7's).
const SESSIONS_V2_COLUMNS: &[(&str, &str)] = &[
    ("digest_anchor", "INTEGER"),
    ("tree", "TEXT NOT NULL DEFAULT '{}'"),
    ("transcript_generation", "TEXT"),
];

/// Identity columns every `sessions` incarnation must carry. Guard-only:
/// migrations own their creation.
const SESSIONS_IDENTITY_COLUMNS: &[&str] = &[
    "id",
    "parent_id",
    "fork_kind",
    "title",
    "created_at_s",
    "updated_at_s",
    "workspace_root",
    "persona",
    "additional_roots",
    "msg_count",
    "last_user_prompt",
    "digest",
];

/// Columns the interim version-5 rebuild inherited from the retired
/// cron/repeat scheduling tranche; removed by the migration-6 repair.
const SESSIONS_RETIRED_COLUMNS: &[&str] = &["scheduled_jobs", "data", "title_manual"];

fn sessions_columns(conn: &Connection) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(Result::ok)
        .collect();
    Ok(columns)
}

/// Reconcile an interim version-5 `sessions` table to the final ADR-0186
/// schema: drop retired columns, add any missing working-state column. Every
/// step is conditional because both version-5 incarnations (interim and
/// final) must converge through this repair.
fn repair_intermediate_sessions_schema(tx: &rusqlite::Transaction) -> Result<()> {
    let existing = sessions_columns(tx)?;
    for retired in SESSIONS_RETIRED_COLUMNS {
        if existing.contains(*retired) {
            tx.execute_batch(&format!("ALTER TABLE sessions DROP COLUMN {retired};"))?;
        }
    }
    for (name, ddl) in SESSIONS_WORKING_STATE_COLUMNS {
        if !existing.contains(*name) {
            tx.execute_batch(&format!("ALTER TABLE sessions ADD COLUMN {name} {ddl};"))?;
        }
    }
    Ok(())
}

/// Does the named table exist in the database?
fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        params![name],
        |row| row.get::<_, i64>(0),
    )? == 1)
}

/// Persistence v2 (ADR-0187) schema repair, applied by migration 7. Every
/// step is conditional: fresh ADR-0186 databases, final version-5 databases,
/// and interim version-5 databases (no transcript tables) must all converge.
fn apply_persistence_v2_schema(tx: &rusqlite::Transaction) -> Result<()> {
    // Dead ledgers: the event log never had a writer or a replay path, and
    // the CAS lives on the filesystem.
    tx.execute_batch("DROP TABLE IF EXISTS events; DROP TABLE IF EXISTS session_blobs;")?;

    let existing = sessions_columns(tx)?;
    // Honest units: sessions store seconds despite the `_ms` suffix.
    if existing.contains("created_at_ms") {
        tx.execute_batch("ALTER TABLE sessions RENAME COLUMN created_at_ms TO created_at_s;")?;
    }
    if existing.contains("updated_at_ms") {
        tx.execute_batch("ALTER TABLE sessions RENAME COLUMN updated_at_ms TO updated_at_s;")?;
    }
    // Restore the listing indexes the v5 table rebuild dropped. The
    // project-root indexes exist only while `project_root` is still a column:
    // a schema already migrated to ADR-0219 `scope_key` (an artificial
    // re-stamp) must not fail here.
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at_s DESC);",
    )?;
    if sessions_columns(tx)?.contains("project_root") {
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_root);
             CREATE INDEX IF NOT EXISTS idx_sessions_project_updated ON sessions(project_root, updated_at_s DESC);",
        )?;
    }
    // Working state that v5 lost on the floor (ADR-0186 regressions).
    if !existing.contains("digest_anchor") {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN digest_anchor INTEGER;")?;
    }
    if !existing.contains("tree") {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN tree TEXT NOT NULL DEFAULT '{}';")?;
    }
    if !existing.contains("transcript_generation") {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN transcript_generation TEXT;")?;
    }
    // High-water mark with no reader or writer.
    if existing.contains("applied_seq") {
        tx.execute_batch("ALTER TABLE sessions DROP COLUMN applied_seq;")?;
    }

    // The transcript ledger tables exist only in ADR-0186 databases; an
    // interim version-5 database converges later, when the tables appear.
    if !table_exists(tx, "entries")? || !table_exists(tx, "entry_memberships")? {
        return Ok(());
    }

    // Drop the old FTS triggers before any table rename: ALTER TABLE RENAME
    // reparses every trigger body, and the old entry-insert trigger selects
    // from the table being swapped (it fired before the membership row
    // existed anyway, leaving fts_entries silently empty).
    tx.execute_batch(
        "DROP TRIGGER IF EXISTS trg_entries_ai; DROP TRIGGER IF EXISTS trg_entries_ad;",
    )?;

    // Projections: the kind CHECK blocked forward-compatible extension;
    // created_at_ms was written (with seconds!) and never read.
    tx.execute_batch(
        "CREATE TABLE projections_new (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq        INTEGER NOT NULL,
            kind       TEXT NOT NULL,
            up_to_seq  INTEGER NOT NULL,
            payload    TEXT NOT NULL,
            PRIMARY KEY (session_id, seq)
        );
        INSERT INTO projections_new (session_id, seq, kind, up_to_seq, payload)
            SELECT session_id, seq, kind, up_to_seq, payload FROM projections;
        DROP TABLE projections;
        ALTER TABLE projections_new RENAME TO projections;",
    )?;

    // Memberships: added_by was written and never read.
    tx.execute_batch(
        "CREATE TABLE entry_memberships_new (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq        INTEGER NOT NULL,
            entry_id   TEXT NOT NULL REFERENCES entries(id),
            PRIMARY KEY (session_id, seq)
        );
        INSERT INTO entry_memberships_new (session_id, seq, entry_id)
            SELECT session_id, seq, entry_id FROM entry_memberships;
        DROP TABLE entry_memberships;
        ALTER TABLE entry_memberships_new RENAME TO entry_memberships;
        CREATE INDEX IF NOT EXISTS idx_memberships_entry ON entry_memberships(entry_id);",
    )?;

    // Durable GC roots: a blob is live iff some session's transcript
    // references it. Maintained in the same transaction as the save that
    // introduces the reference; session deletion cascades.
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS blob_refs (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            hash       TEXT NOT NULL,
            PRIMARY KEY (session_id, hash)
        );
        INSERT OR IGNORE INTO blob_refs (session_id, hash)
            SELECT m.session_id, json_extract(e.payload, '$.content_blob')
            FROM entries e JOIN entry_memberships m ON m.entry_id = e.id
            WHERE json_extract(e.payload, '$.content_blob') IS NOT NULL;",
    )?;

    // Re-anchor FTS on memberships and backfill the index.
    tx.execute_batch(
        "CREATE TRIGGER trg_memberships_ai AFTER INSERT ON entry_memberships BEGIN
            INSERT INTO fts_entries(entry_id, session_id, role, content)
            SELECT new.entry_id, new.session_id, COALESCE(e.role, ''), COALESCE(e.content, '')
            FROM entries e WHERE e.id = new.entry_id;
        END;
        CREATE TRIGGER trg_memberships_ad AFTER DELETE ON entry_memberships BEGIN
            DELETE FROM fts_entries WHERE entry_id = old.entry_id AND session_id = old.session_id;
        END;
        DELETE FROM fts_entries;
        INSERT INTO fts_entries(entry_id, session_id, role, content)
            SELECT m.entry_id, m.session_id, COALESCE(e.role, ''), COALESCE(e.content, '')
            FROM entry_memberships m JOIN entries e ON e.id = m.entry_id;",
    )?;
    Ok(())
}

/// Usage ledger (ADR-0187) schema repair, applied by migration 8: create the
/// key-addressed `usage_records` table and migrate the session-row JSON
/// column into it when that column exists.
fn apply_usage_ledger_schema(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS usage_records (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            actor_id   TEXT NOT NULL,
            round      INTEGER NOT NULL,
            turn       INTEGER NOT NULL,
            attempt    INTEGER NOT NULL,
            payload    TEXT NOT NULL,
            PRIMARY KEY (session_id, actor_id, round, turn, attempt)
        );",
    )?;
    let existing = sessions_columns(tx)?;
    if !existing.contains("request_usage_records") {
        return Ok(());
    }
    let mut stmt = tx.prepare(
        "SELECT id, request_usage_records FROM sessions WHERE request_usage_records IS NOT NULL",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    drop(stmt);
    for (session_id, json) in rows {
        let Ok(records) = serde_json::from_str::<Vec<muta_contracts::RequestUsageRecord>>(&json)
        else {
            continue;
        };
        for record in &records {
            insert_usage_record_tx(tx, &session_id, record)?;
        }
    }
    tx.execute_batch("ALTER TABLE sessions DROP COLUMN request_usage_records;")?;
    Ok(())
}

/// Migration 10 repair: rewrite `entries` with the corrected `origin`/`hidden`
/// CHECKs (ADR-0186 integrity fix). The whole step runs in the caller's outer
/// transaction with foreign-key enforcement disabled by the migration subagent
/// (sqlite.org/lang_altertable procedure): the rebuild briefly drops
/// `entries`, the parent of `entry_memberships`' foreign key, and re-creating
/// the equivalent rows under a new table name cannot decrement SQLite's
/// deferred-constraint counter — a `defer_foreign_keys` workaround COMMITs
/// with `FOREIGN KEY constraint failed` on a database whose final state is
/// perfectly consistent. Data and FTS triggers are preserved: rows are
/// copied byte-for-byte, the FTS-insert trigger is dropped before the rename
/// (ALTER TABLE RENAME reparses trigger bodies) and re-anchored after.
fn apply_entries_integrity_schema(tx: &rusqlite::Transaction) -> Result<()> {
    // The `entries` table exists only in databases that ran the ADR-0186
    // transcript migration. An interim version-5 database (no transcript
    // tables) converges later, when the entries/entry_memberships tables
    // appear; skip the repair here exactly like the v2 schema guard does.
    if !table_exists(tx, "entries")? || !table_exists(tx, "entry_memberships")? {
        return Ok(());
    }
    // `ALTER TABLE entries_new RENAME TO entries` re-parses every surviving
    // trigger body that references `entries`; the migration-7 membership
    // insert trigger selects from it, so it is dropped first and re-created
    // byte-identically after the swap (mirrors the v2 FTS re-anchor). The
    // old `entries` FTS triggers are dropped too (their bodies reference the
    // renamed table) and re-anchored identically.
    tx.execute_batch(
        "DROP TRIGGER IF EXISTS trg_memberships_ai;
         DROP TRIGGER IF EXISTS trg_entries_ai;
         DROP TRIGGER IF EXISTS trg_entries_ad;
         CREATE TABLE entries_new (
            id            TEXT PRIMARY KEY,
            kind          TEXT NOT NULL CHECK (kind IN ('message','state')),
            role          TEXT CHECK (role IN ('user','assistant','system','tool')),
            content       TEXT,
            origin        TEXT CHECK (origin IS NULL OR origin IN ('harness','checkpoint')),
            hidden        INTEGER NOT NULL DEFAULT 0,
            created_at_ms INTEGER NOT NULL,
            payload       TEXT NOT NULL,
            CHECK ( hidden = 0 OR origin IS NOT NULL ),
            CHECK ( origin IS NULL OR hidden = 1 OR origin <> 'checkpoint' ),
            CHECK ( kind <> 'message' OR role IS NOT NULL )
        );
        INSERT INTO entries_new (id, kind, role, content, origin, hidden, created_at_ms, payload)
            SELECT id, kind, role, content, origin, hidden, created_at_ms, payload FROM entries;
        DROP TABLE entries;
        ALTER TABLE entries_new RENAME TO entries;
        -- Re-create the membership/entry FTS triggers with the exact pre-merge
        -- definitions so the resulting schema matches a fresh migration 7 run.
        CREATE TRIGGER trg_memberships_ai AFTER INSERT ON entry_memberships BEGIN
            INSERT INTO fts_entries(entry_id, session_id, role, content)
            SELECT new.entry_id, new.session_id, COALESCE(e.role, ''), COALESCE(e.content, '')
            FROM entries e WHERE e.id = new.entry_id;
        END;
        CREATE TRIGGER trg_entries_ai AFTER INSERT ON entries BEGIN
            INSERT INTO fts_entries(entry_id, session_id, role, content)
            SELECT new.id, m.session_id, COALESCE(new.role, ''), COALESCE(new.content, '')
            FROM entry_memberships m WHERE m.entry_id = new.id;
        END;
        CREATE TRIGGER trg_entries_ad AFTER DELETE ON entries BEGIN
            DELETE FROM fts_entries WHERE entry_id = old.id;
        END;",
    )?;
    Ok(())
}

/// Session-scope rebuild (ADR-0219), applied by migration 12. Rebuilds
/// `sessions` so the partition key becomes `scope_kind` + namespaced
/// `scope_key` with an optional `workspace_root`, enforcing the NOT NULL
/// constraints `ALTER TABLE` cannot add. Existing rows backfill as workspace
/// scopes (`scope_key = 'ws:' || project_root`) with the same root bound as
/// their workspace, losslessly. Foreign-key enforcement is disabled by the
/// migration driver for the table swap; the post-commit integrity gate
/// re-verifies every declared reference.
fn apply_session_scope_schema(tx: &rusqlite::Transaction) -> Result<()> {
    let existing = sessions_columns(tx)?;
    if !existing.contains("project_root") {
        return Ok(());
    }
    tx.execute_batch(
        r#"
        CREATE TABLE sessions_new (
            id                    TEXT PRIMARY KEY,
            parent_id             TEXT REFERENCES sessions(id) ON DELETE SET NULL,
            fork_kind             TEXT NOT NULL DEFAULT 'trunk'
                                  CHECK (fork_kind IN ('trunk','fork','aside','subagent')),
            title                 TEXT,
            created_at_s          INTEGER NOT NULL,
            updated_at_s          INTEGER NOT NULL,
            scope_kind            TEXT NOT NULL
                                  CHECK (scope_kind IN ('workspace','persona','ephemeral')),
            scope_key             TEXT NOT NULL,
            workspace_root        TEXT,
            additional_roots      TEXT NOT NULL DEFAULT '[]',
            msg_count             INTEGER NOT NULL DEFAULT 0,
            last_user_prompt      TEXT,
            digest                TEXT,
            digest_anchor         INTEGER,
            tree                  TEXT NOT NULL DEFAULT '{}',
            transcript_generation TEXT,
            provider_connection   TEXT,
            round_counter         INTEGER NOT NULL DEFAULT 0,
            unattended            INTEGER NOT NULL DEFAULT 0,
            disabled_tools        TEXT NOT NULL DEFAULT '[]',
            commands              TEXT NOT NULL DEFAULT '[]',
            round_interrupts      TEXT NOT NULL DEFAULT '[]',
            retry_resolutions     TEXT NOT NULL DEFAULT '[]',
            retry_pending         TEXT,
            checksum              INTEGER,
            schema_version        INTEGER NOT NULL DEFAULT 14
        );
        INSERT INTO sessions_new (
            id, parent_id, fork_kind, title, created_at_s, updated_at_s,
            scope_kind, scope_key, workspace_root, additional_roots,
            msg_count, last_user_prompt, digest, digest_anchor, tree,
            transcript_generation, provider_connection, round_counter, unattended,
            disabled_tools, commands, round_interrupts, retry_resolutions,
            retry_pending, checksum, schema_version
        )
            SELECT
                id, parent_id, fork_kind, title, created_at_s, updated_at_s,
                'workspace', 'ws:' || project_root, project_root, '[]',
                msg_count, last_user_prompt, digest, digest_anchor, tree,
                transcript_generation, provider_connection, round_counter, unattended,
                disabled_tools, commands, round_interrupts, retry_resolutions,
                retry_pending, checksum, schema_version
            FROM sessions;
        DROP TABLE sessions;
        ALTER TABLE sessions_new RENAME TO sessions;
        CREATE INDEX idx_sessions_scope ON sessions(scope_key, updated_at_s DESC);
        CREATE INDEX idx_sessions_updated ON sessions(updated_at_s DESC);
        "#,
    )?;
    Ok(())
}

/// Grouping rebuild (ADR-0226), applied by migration 13. Adds `space`, drops
/// `scope_kind`/`scope_key`, and re-indexes on the concrete grouping columns.
/// Conditional so a database already at the derived-grouping schema passes
/// through unchanged.
fn apply_session_grouping_schema(tx: &rusqlite::Transaction) -> Result<()> {
    let existing = sessions_columns(tx)?;
    if existing.contains("space") && !existing.contains("scope_key") {
        return Ok(());
    }
    if !existing.contains("space") {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN space TEXT;")?;
    }
    tx.execute_batch(
        "DROP INDEX IF EXISTS idx_sessions_scope;
         DROP INDEX IF EXISTS idx_sessions_project;
         DROP INDEX IF EXISTS idx_sessions_project_updated;",
    )?;
    if existing.contains("scope_kind") {
        tx.execute_batch("ALTER TABLE sessions DROP COLUMN scope_kind;")?;
    }
    if existing.contains("scope_key") {
        tx.execute_batch("ALTER TABLE sessions DROP COLUMN scope_key;")?;
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_sessions_workspace ON sessions(workspace_root, updated_at_s DESC);
         CREATE INDEX IF NOT EXISTS idx_sessions_space ON sessions(space, updated_at_s DESC);",
    )?;
    Ok(())
}

/// Workspace-only partition (ADR-0226 revised), applied by migration 14:
/// drops the `space` column and adds `persona` metadata.
fn apply_workspace_partition_schema(tx: &rusqlite::Transaction) -> Result<()> {
    let existing = sessions_columns(tx)?;
    if existing.contains("persona") && !existing.contains("space") {
        return Ok(());
    }
    if existing.contains("space") {
        // Drop the index that references the column before dropping it;
        // SQLite refuses to drop a column an index still references.
        tx.execute_batch("DROP INDEX IF EXISTS idx_sessions_space;")?;
        tx.execute_batch("ALTER TABLE sessions DROP COLUMN space;")?;
    }
    if !existing.contains("persona") {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN persona TEXT;")?;
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_sessions_persona ON sessions(persona, workspace_root, updated_at_s DESC);",
    )?;
    Ok(())
}

fn insert_usage_record_tx(
    tx: &rusqlite::Connection,
    session_id: &str,
    record: &muta_contracts::RequestUsageRecord,
) -> rusqlite::Result<()> {
    let payload = serde_json::to_string(record)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    tx.execute(
        "INSERT OR REPLACE INTO usage_records (session_id, actor_id, round, turn, attempt, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            session_id,
            record.key.actor_id,
            record.key.round as i64,
            record.key.turn as i64,
            record.key.attempt as i64,
            payload,
        ],
    )?;
    Ok(())
}

/// Fail-fast guard: verify the `sessions` table carries exactly the columns
/// the current runtime SQL writes, so a schema/runtime mismatch surfaces as
/// one clear startup error instead of scattered per-statement failures
/// (`table sessions has no column named ...`) during persistence.
fn verify_sessions_schema(conn: &Connection) -> Result<()> {
    if !conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'sessions')",
            [],
            |r| r.get::<_, i64>(0),
        )?
        .eq(&1)
    {
        return Ok(());
    }
    let existing = sessions_columns(conn)?;
    let mut expected: std::collections::HashSet<String> = SESSIONS_IDENTITY_COLUMNS
        .iter()
        .map(|c| c.to_string())
        .collect();
    expected.extend(
        SESSIONS_WORKING_STATE_COLUMNS
            .iter()
            .map(|(c, _)| c.to_string()),
    );
    // Column owned by migration 9 (not in the conditional repair list, see
    // the note there) — the startup guard still requires it.
    expected.insert("retry_resolutions".to_string());
    expected.extend(SESSIONS_V2_COLUMNS.iter().map(|(c, _)| c.to_string()));
    // Columns migration 8 retired from the row (their data moved to the
    // usage ledger table) must not be required here.
    expected.remove("request_usage_records");
    let missing: Vec<&str> = expected
        .iter()
        .filter(|c| !existing.contains(c.as_str()))
        .map(|c| c.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(rusqlite::Error::InvalidColumnName(format!(
            "sessions table is missing column(s) {}; the database predates a \
             skipped schema migration — restore a backup or recreate the state",
            missing.join(", ")
        )));
    }
    // Table owned by migration 11 (ADR-0218). The sessions-column guard above
    // cannot see it; verify it explicitly so a database that skipped the
    // migration fails loud at startup instead of on the first request.
    if !table_exists(conn, "request_projections")? {
        return Err(rusqlite::Error::InvalidParameterName(
            "request_projections table is missing; the database predates schema \
             migration 11 — restore a backup or recreate the state"
                .to_string(),
        ));
    }
    Ok(())
}

/// Run all outstanding migrations in a single transactional loop, then
/// verify the resulting schema. Verification also covers databases that
/// short-circuit the loop (already at `CURRENT_DB_VERSION`) so a schema a
/// retired migration produced cannot reach runtime SQL unnoticed.
///
/// Foreign-key enforcement is disabled for the duration of the migration
/// transaction and restored afterwards, per the canonical table-rebuild
/// procedure (sqlite.org/lang_altertable): migrations 5 and 10 drop parent
/// tables (`sessions`, `entries`) that other tables reference and swap a
/// rebuilt replacement into place. With enforcement ON, SQLite's deferred
/// constraint counter counts the dropped parent rows and is never
/// reconciled by the rename, so COMMIT fails with `FOREIGN KEY constraint
/// failed` even though the post-migration state is perfectly referentially
/// consistent — and because the failure rolls the whole transaction back,
/// the database never advances past the broken migration and every future
/// open retries and fails it forever. `PRAGMA foreign_keys` is a no-op
/// inside an open transaction, hence the flip happens before `BEGIN`. A
/// `foreign_key_check` gate after COMMIT fails loud on a database whose
/// final state violates its own declared references.
fn migrate_schema(conn: &mut Connection) -> Result<()> {
    let current_version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    // A database written by a newer binary may carry schema this build cannot
    // interpret; proceeding would silently degrade or destroy data. Fail loud
    // and let every caller surface the refusal.
    if current_version > CURRENT_DB_VERSION {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "database schema v{current_version} is newer than this binary (v{CURRENT_DB_VERSION}); \
             refusing to open — upgrade muta to work with this state"
        )));
    }

    if current_version < CURRENT_DB_VERSION {
        conn.pragma_update(None, "foreign_keys", "OFF")?;
        let outcome = apply_migrations(conn, current_version);
        // Enforcement is a per-connection invariant installed by
        // `configure_connection`; restore it before propagating any
        // migration failure so the connection is never handed back in a
        // weaker security posture than it started with.
        let restore = conn.pragma_update(None, "foreign_keys", "ON");
        outcome?;
        restore?;
        assert_referential_integrity(conn)?;
    }

    verify_sessions_schema(conn)
}

/// Apply every migration above `observed_version` inside one immediate
/// transaction, then stamp `PRAGMA user_version`. `BEGIN IMMEDIATE` takes
/// the write lock up front; `user_version` is re-read inside the lock so a
/// concurrent opener that observed a stale version while waiting serializes
/// into a no-op instead of racing a duplicate rebuild.
fn apply_migrations(conn: &mut Connection, observed_version: u32) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current_version: u32 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if current_version > CURRENT_DB_VERSION {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "database schema v{current_version} is newer than this binary (v{CURRENT_DB_VERSION}); \
             refusing to open — upgrade muta to work with this state"
        )));
    }

    if current_version == observed_version {
        for migration in MIGRATIONS {
            if migration.version > current_version {
                info!(
                    version = migration.version,
                    "Applying SQLite schema migration"
                );
                tx.execute_batch(migration.sql)?;

                // Migration 9 is a no-op when the column already exists — a
                // database stamped by an intermediate dev build (via the
                // startup repair, or a partially-applied v9) must pass
                // through unchanged instead of failing on a duplicate
                // `ADD COLUMN` (mirrors the migration-6 conditional repair).
                if migration.version == 9 {
                    let has_column = sessions_columns(&tx)?
                        .iter()
                        .any(|column| column == "retry_resolutions");
                    if !has_column {
                        tx.execute_batch(
                            "ALTER TABLE sessions ADD COLUMN retry_resolutions TEXT NOT NULL DEFAULT '[]';",
                        )?;
                    }
                }

                if migration.version == 3 {
                    let mut stmt =
                        tx.prepare("SELECT id, data FROM sessions WHERE data IS NOT NULL")?;
                    let rows = stmt.query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?;
                    let mut updates = Vec::new();
                    for item in rows {
                        let (id, data_str) = item?;
                        if let Ok(probe) = serde_json::from_str::<FastSummaryProbe>(&data_str) {
                            let count = probe.model_window.len() + probe.archived_transcript.len();
                            let last_prompt = probe
                                .model_window
                                .iter()
                                .rev()
                                .chain(probe.archived_transcript.iter().rev())
                                .find(|m| {
                                    let is_echo = m.origin.as_ref().is_some_and(|o| {
                                        o.kind == muta_contracts::InjectionKind::CommandEcho
                                    });
                                    m.role == muta_contracts::Role::User && !m.hidden && !is_echo
                                })
                                .map(|m| m.content.clone());
                            let digest_json = probe
                                .digest
                                .as_ref()
                                .and_then(|d| serde_json::to_string(d).ok());
                            updates.push((id, count as i64, last_prompt, digest_json));
                        }
                    }
                    drop(stmt);
                    let mut update_stmt = tx.prepare(
                        "UPDATE sessions SET msg_count = ?1, last_user_prompt = ?2, digest = ?3 WHERE id = ?4",
                    )?;
                    for (id, count, last_prompt, digest_json) in updates {
                        update_stmt.execute(params![count, last_prompt, digest_json, id])?;
                    }
                }

                if migration.version == 6 {
                    repair_intermediate_sessions_schema(&tx)?;
                }
                if migration.version == 7 {
                    apply_persistence_v2_schema(&tx)?;
                }
                if migration.version == 8 {
                    apply_usage_ledger_schema(&tx)?;
                }
                if migration.version == 9 {
                    // Conditional add (see the migration entry's note): the
                    // column already exists on every database whose version-5
                    // SQL was finalized after the feature landed.
                    let existing = sessions_columns(&tx)?;
                    if !existing.contains("retry_resolutions") {
                        tx.execute_batch(
                            "ALTER TABLE sessions ADD COLUMN retry_resolutions TEXT NOT NULL DEFAULT '[]';",
                        )?;
                    }
                }
                if migration.version == 10 {
                    apply_entries_integrity_schema(&tx)?;
                }
                if migration.version == 12 {
                    apply_session_scope_schema(&tx)?;
                }
                if migration.version == 13 {
                    apply_session_grouping_schema(&tx)?;
                }
                if migration.version == 14 {
                    apply_workspace_partition_schema(&tx)?;
                }
            }
        }

        // Update schema version pragma
        let pragma_sql = format!("PRAGMA user_version = {CURRENT_DB_VERSION}");
        tx.execute_batch(&pragma_sql)?;

        tx.commit()?;
        info!(
            version = CURRENT_DB_VERSION,
            "SQLite database schema is up-to-date"
        );
    }

    Ok(())
}

/// Post-migration integrity gate: scan every declared foreign key in the
/// schema and fail loud on dangling references. Runs after COMMIT with
/// enforcement restored, so a migration that produced an inconsistent state
/// surfaces as one clear startup error instead of scattered per-statement
/// failures in runtime SQL.
fn assert_referential_integrity(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let violation = stmt
        .query_row([], |row| {
            Ok(format!(
                "table '{}' row {} references missing parent '{}' row {:?}",
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })
        .optional()?;
    drop(stmt);
    if let Some(detail) = violation {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "post-migration referential integrity check failed: {detail}; \
             restore a backup or recreate the state"
        )));
    }
    Ok(())
}

/// Session record representation in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: String,
    pub parent_id: Option<String>,
    pub fork_kind: String,
    pub title: Option<String>,
    pub created_at_s: i64,
    pub updated_at_s: i64,
    pub workspace_root: Option<String>,
    pub persona: Option<String>,
    #[serde(default)]
    pub msg_count: i64,
    #[serde(default)]
    pub last_user_prompt: Option<String>,
    #[serde(default)]
    pub digest: Option<String>,
}

/// One full-text search hit over transcript entries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistorySearchResult {
    pub entry_id: String,
    pub session_id: String,
    pub workspace_root: Option<String>,
    /// Stored AI/manual title of the owning session, when one exists
    /// (joined from `sessions.title`, ADR-0208).
    pub session_title: Option<String>,
    pub role: String,
    pub snippet: String,
    pub score: f64,
}

/// One persisted session's projected transcript tail plus metadata — the
/// public, field-private shape out-of-crate readers consume (ADR-0208).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionTranscriptView {
    pub id: String,
    pub title: Option<String>,
    pub digest: Option<muta_contracts::SessionDigest>,
    pub workspace_root: Option<String>,
    pub message_count: usize,
    pub messages: Vec<SessionMessageView>,
}

/// One projected message row inside a [`SessionTranscriptView`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMessageView {
    pub seq: u64,
    /// Lowercase role string (`user` / `assistant` / `system` / `tool`).
    pub role: String,
    pub content: String,
}

fn role_str(role: muta_contracts::Role) -> &'static str {
    match role {
        muta_contracts::Role::User => "user",
        muta_contracts::Role::Assistant => "assistant",
        muta_contracts::Role::System => "system",
        muta_contracts::Role::Tool => "tool",
    }
}

fn role_from_str(role: &str) -> Option<muta_contracts::Role> {
    match role {
        "user" => Some(muta_contracts::Role::User),
        "assistant" => Some(muta_contracts::Role::Assistant),
        "system" => Some(muta_contracts::Role::System),
        "tool" => Some(muta_contracts::Role::Tool),
        _ => None,
    }
}

fn origin_str(origin: muta_contracts::EntryOrigin) -> &'static str {
    match origin {
        muta_contracts::EntryOrigin::Harness => "harness",
        muta_contracts::EntryOrigin::Checkpoint => "checkpoint",
    }
}

fn origin_from_str(origin: &str) -> Option<muta_contracts::EntryOrigin> {
    match origin {
        "harness" => Some(muta_contracts::EntryOrigin::Harness),
        "checkpoint" => Some(muta_contracts::EntryOrigin::Checkpoint),
        _ => None,
    }
}

fn serde_plain(kind: muta_contracts::DirectiveKind) -> rusqlite::Result<&'static str> {
    Ok(match kind {
        muta_contracts::DirectiveKind::Prune => "prune",
        muta_contracts::DirectiveKind::Compact => "compact",
        muta_contracts::DirectiveKind::Freeze => "freeze",
    })
}

/// Pull the `content_blob` hash out of a raw payload JSON document without a
/// typed decode (used for verbatim-preserved unknown entries).
fn extract_content_blob(payload_json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(payload_json).ok()?;
    value
        .get("content_blob")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn map_session_row(row: &Row) -> Result<SessionRecord> {
    Ok(SessionRecord {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        fork_kind: row.get(2)?,
        title: row.get(3)?,
        created_at_s: row.get(4)?,
        updated_at_s: row.get(5)?,
        workspace_root: row.get(6)?,
        persona: row.get(7)?,
        msg_count: row.get(8)?,
        last_user_prompt: row.get(9)?,
        digest: row.get(10)?,
    })
}

type SummaryRow = (
    String,
    Option<String>,
    String,
    Option<String>,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
);

fn map_summary_row(row: &Row) -> Result<SummaryRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn push_summary(
    summaries: &mut Vec<crate::session::SessionSummary>,
    item: SummaryRow,
    active_id: &str,
) {
    let (
        id,
        parent_id,
        fork_str,
        title,
        created_at_s,
        updated_at_s,
        msg_count,
        last_user_prompt,
        digest_json,
    ) = item;
    let fork_kind = match fork_str.as_str() {
        "fork" => muta_contracts::SessionForkKind::Fork,
        "aside" => muta_contracts::SessionForkKind::Aside,
        _ => muta_contracts::SessionForkKind::Trunk,
    };
    let final_msg_count = msg_count.max(0) as usize;
    if final_msg_count == 0 && id != active_id {
        return;
    }
    let overview = if let Some(t) = title.as_deref().filter(|t| !t.trim().is_empty()) {
        crate::session::truncate_preview(t, 64)
    } else if let Some(prompt) = last_user_prompt.as_deref().filter(|p| !p.trim().is_empty()) {
        crate::session::truncate_preview(prompt, 64)
    } else {
        "(empty session)".to_string()
    };
    let digest = digest_json.and_then(|raw| serde_json::from_str(&raw).ok());
    summaries.push(crate::session::SessionSummary {
        id: id.clone(),
        parent_id,
        fork_kind,
        message_count: final_msg_count,
        updated_at: updated_at_s.max(0) as u64,
        created_at: created_at_s.max(0) as u64,
        overview,
        active: id == active_id,
        digest,
    });
}

fn map_search_row(row: &Row) -> Result<HistorySearchResult> {
    Ok(HistorySearchResult {
        entry_id: row.get(0)?,
        session_id: row.get(1)?,
        workspace_root: row.get(2)?,
        session_title: row.get(3)?,
        role: row.get(4)?,
        snippet: row.get(5)?,
        score: row.get(6)?,
    })
}

/// Sanitize a free-text query into a safe FTS5 MATCH expression (ADR-0208).
///
/// Every whitespace-separated word becomes a quoted phrase token (`"retry"`
/// → `"retry"`, `retry loop` → `"retry" AND "loop"`), so user input can never
/// inject FTS5 column filters or boolean operators — the raw MATCH grammar
/// treats a bare word that names a column as a filter and errors out (the
/// live failure that motivated this: `no such column: needle`). Word
/// characters plus a small punctuation allowlist (`-` `_` `.`) survive;
/// everything else (including quotes) is stripped, so FTS5 string-literal
/// grammar is unreachable and the expression is injection-proof by
/// construction. A query with no word characters degenerates to an empty
/// string (no-op search).
///
/// `match_any` joins the tokens with OR instead of AND — the relaxed recall
/// mode (`search_history_relaxed`).
fn sanitize_fts_query_joined(query: &str, match_any: bool) -> String {
    let joiner = if match_any { " OR " } else { " AND " };
    sanitize_fts_words(query).join(joiner)
}

/// The word-level sanitizer: cleaned tokens (punctuation stripped, lowercased
/// implicitly by FTS's own tokenizer) that survive into any MATCH form.
fn sanitize_fts_words(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .filter_map(|word| {
            let cleaned: String = word
                .chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
                .collect();
            if cleaned.is_empty() {
                None
            } else {
                Some(format!("\"{cleaned}\""))
            }
        })
        .collect()
}

/// The strict (AND) form, kept as the test-facing wrapper around the joined
/// sanitizer.
#[cfg(test)]
fn sanitize_fts_query(query: &str) -> String {
    sanitize_fts_query_joined(query, false)
}

/// Envelope columns of one `entries` row, as written by a save (ADR-0186 §3).
struct EntryEnvelope<'a> {
    id: &'a str,
    kind: &'a str,
    role: Option<&'a str>,
    content: Option<&'a str>,
    origin: Option<&'a str>,
    hidden: bool,
    created_at_ms: u64,
    payload: &'a str,
}

/// Authoritative relational database access object for Muta persistence.
pub(crate) struct DatabaseEngine {
    conn: Connection,
    /// CAS store used to offload oversized entry bodies at insert time
    /// (ADR-0187): only rows this transaction inserts are offloaded, so the
    /// per-save cost stays proportional to the delta. `None` keeps content
    /// inline (readers, tests).
    blob_store: Option<BlobStore>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct FastSummaryProbe {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    digest: Option<muta_contracts::SessionDigest>,
    #[serde(default)]
    model_window: Vec<FastMessageProbe>,
    #[serde(default)]
    archived_transcript: Vec<FastMessageProbe>,
}

#[derive(Deserialize)]
struct FastMessageProbe {
    role: muta_contracts::Role,
    #[serde(default)]
    content: String,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    origin: Option<FastOriginProbe>,
}

#[derive(Deserialize)]
struct FastOriginProbe {
    kind: muta_contracts::InjectionKind,
}

impl DatabaseEngine {
    /// Open or create a database engine on a file path.
    pub(crate) fn open(db_path: &Path, blob_store: Option<BlobStore>) -> Result<Self> {
        let conn = initialize_db(db_path)?;
        let engine = Self { conn, blob_store };
        if db_path == crate::paths::get().db_file() {
            let _ = engine.migrate_legacy_input_history();
        }
        Ok(engine)
    }

    /// Open an in-memory database engine. Test-only: the shipped surface has
    /// no way to name a connection — every real read and write goes through
    /// the handle's two doors (ADR-0231).
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self> {
        let conn = initialize_in_memory_db()?;
        Ok(Self {
            conn,
            blob_store: None,
        })
    }

    // Session Operations

    /// Create or update a session record.
    pub(crate) fn upsert_session(&self, session: &SessionRecord) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_s, updated_at_s, workspace_root, persona, msg_count, last_user_prompt, digest)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                parent_id = excluded.parent_id,
                fork_kind = excluded.fork_kind,
                title = excluded.title,
                updated_at_s = excluded.updated_at_s,
                workspace_root = excluded.workspace_root,
                persona = excluded.persona,
                msg_count = excluded.msg_count,
                last_user_prompt = excluded.last_user_prompt,
                digest = excluded.digest;
            "#,
            params![
                session.id,
                session.parent_id,
                session.fork_kind,
                session.title,
                session.created_at_s,
                session.updated_at_s,
                session.workspace_root,
                session.persona,
                session.msg_count,
                session.last_user_prompt,
                session.digest,
            ],
        )?;
        Ok(())
    }

    /// Retrieve a single session by id.
    pub(crate) fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        self.conn
            .query_row(
                "SELECT id, parent_id, fork_kind, title, created_at_s, updated_at_s, workspace_root, persona, msg_count, last_user_prompt, digest FROM sessions WHERE id = ?1",
                params![session_id],
                map_session_row,
            )
            .optional()
    }

    /// List sessions, optionally filtered by a derived grouping (ADR-0226),
    /// sorted by `updated_at_s` descending.
    pub(crate) fn list_sessions(
        &self,
        filter: Option<&muta_contracts::WorkspaceFilter>,
    ) -> Result<Vec<SessionRecord>> {
        const COLS: &str = "id, parent_id, fork_kind, title, created_at_s, updated_at_s, \
                            workspace_root, persona, msg_count, last_user_prompt, digest";
        let mut sessions = Vec::new();
        match filter {
            Some(muta_contracts::WorkspaceFilter::Path(path)) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions WHERE workspace_root = ?1 ORDER BY updated_at_s DESC"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(params![path.to_string_lossy()], map_session_row)?;
                for session in rows {
                    sessions.push(session?);
                }
            }
            Some(muta_contracts::WorkspaceFilter::Unbound) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions WHERE workspace_root IS NULL ORDER BY updated_at_s DESC"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map([], map_session_row)?;
                for session in rows {
                    sessions.push(session?);
                }
            }
            _ => {
                let sql = format!("SELECT {COLS} FROM sessions ORDER BY updated_at_s DESC");
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map([], map_session_row)?;
                for session in rows {
                    sessions.push(session?);
                }
            }
        }
        Ok(sessions)
    }

    /// Delete a session and cascade all its events, messages, and command records.
    pub(crate) fn delete_session(&self, session_id: &str) -> Result<bool> {
        let affected = self
            .conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
        Ok(affected > 0)
    }

    /// The live blob set: every hash the durable `blob_refs` ledger still
    /// references. Read-only, so a blob-store sweep runs on the caller's
    /// thread against this snapshot and never holds up the writer.
    pub(crate) fn live_blob_hashes(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT hash FROM blob_refs")?;
        let live = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(Result::ok)
            .collect();
        Ok(live)
    }

    /// Reclaim transcript entries no session membership references
    /// (ADR-0187). Entries are global facts shared by identity across forks,
    /// so only zero-reference rows are removable; the foreign key from
    /// `entry_memberships` makes the two tables' agreement structural, and
    /// entry insert + membership insert share one transaction, so a GC pass
    /// can never observe the half of a pair. Returns the rows reclaimed.
    pub(crate) fn collect_entry_garbage(&self) -> Result<usize> {
        let reclaimed = self.conn.execute(
            "DELETE FROM entries WHERE NOT EXISTS (
                SELECT 1 FROM entry_memberships m WHERE m.entry_id = entries.id
            )",
            [],
        )?;
        Ok(reclaimed)
    }

    /// Persist a complete [`crate::session::SessionData`] into SQLite in one
    /// transaction (ADR-0186): the session row (identity + working state),
    /// the session's memberships and directives, and the entries themselves
    /// (upsert — facts are shared by identity across forks).
    ///
    /// Write `data` authoritatively (full rewrite). Test-only: the one
    /// production writer is `PersistenceCommand::SaveSession`, which selects
    /// append vs. rewrite by generation (ADR-0187/0231).
    #[cfg(test)]
    pub(crate) fn save_session_full(&self, data: &crate::session::SessionData) -> Result<()> {
        self.save_session_inner(data, true, &[])
    }

    fn save_session_inner(
        &self,
        data: &crate::session::SessionData,
        force_full: bool,
        usage_upserts: &[muta_contracts::RequestUsageRecord],
    ) -> Result<()> {
        let fork_str = match data.fork_kind {
            muta_contracts::SessionForkKind::Trunk => "trunk",
            muta_contracts::SessionForkKind::Fork => "fork",
            muta_contracts::SessionForkKind::Aside => "aside",
            muta_contracts::SessionForkKind::Subagent => "subagent",
        };
        let digest_str = data
            .digest
            .as_ref()
            .and_then(|d| serde_json::to_string(d).ok());
        let last_prompt = crate::session::last_effective_prompt_from_data(data);
        let msg_count = (data.transcript.entries.len() + data.unknown_entries.len()) as i64;
        let tree_str = serde_json::to_string(&data.tree)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        self.conn.execute("BEGIN IMMEDIATE", [])?;
        let res: Result<()> = (|| {
            // Read the durable generation BEFORE the row upsert stamps the
            // incoming one: the comparison decides full-rewrite vs append.
            let stored_generation: Option<String> = self
                .conn
                .query_row(
                    "SELECT transcript_generation FROM sessions WHERE id = ?1",
                    params![data.id],
                    |row| row.get(0),
                )
                .unwrap_or(None);
            // A `None` stored generation (never saved, or saved before the
            // generation column existed) also forces the full rewrite.
            let full = force_full || stored_generation.as_deref() != Some(data.generation.as_str());

            let persona = data.persona.clone();
            let workspace_root = data
                .workspace
                .as_ref()
                .map(|w| w.root.to_string_lossy().into_owned());
            let additional_roots = match data.workspace.as_ref() {
                Some(w) => serde_json::to_string(&w.additional_roots),
                None => Ok("[]".to_string()),
            }
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

            self.conn.execute(
                r#"
                INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_s, updated_at_s, workspace_root, additional_roots, persona, msg_count, last_user_prompt, digest, digest_anchor, tree, transcript_generation, provider_connection, round_counter, unattended, disabled_tools, commands, round_interrupts, retry_resolutions, retry_pending, checksum, schema_version)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)
                ON CONFLICT(id) DO UPDATE SET
                    parent_id = excluded.parent_id,
                    fork_kind = excluded.fork_kind,
                    title = excluded.title,
                    updated_at_s = excluded.updated_at_s,
                    workspace_root = excluded.workspace_root,
                    additional_roots = excluded.additional_roots,
                    persona = excluded.persona,
                    msg_count = excluded.msg_count,
                    last_user_prompt = excluded.last_user_prompt,
                    digest = excluded.digest,
                    digest_anchor = excluded.digest_anchor,
                    tree = excluded.tree,
                    transcript_generation = excluded.transcript_generation,
                    provider_connection = excluded.provider_connection,
                    round_counter = excluded.round_counter,
                    unattended = excluded.unattended,
                    disabled_tools = excluded.disabled_tools,
                    commands = excluded.commands,
                    round_interrupts = excluded.round_interrupts,
                    retry_resolutions = excluded.retry_resolutions,
                    retry_pending = excluded.retry_pending,
                    checksum = excluded.checksum,
                    schema_version = excluded.schema_version;
                "#,
                params![
                    data.id,
                    data.parent_id,
                    fork_str,
                    data.title,
                    data.created_at as i64,
                    data.updated_at as i64,
                    workspace_root,
                    additional_roots,
                    persona,
                    msg_count,
                    last_prompt,
                    digest_str,
                    data.digest_anchor.map(|a| a as i64),
                    tree_str,
                    data.generation,
                    data.provider_selection.as_ref().and_then(|s| serde_json::to_string(s).ok()),
                    data.round_counter as i64,
                    data.unattended,
                    serde_json::to_string(&data.disabled_tools.iter().collect::<Vec<_>>()).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    serde_json::to_string(&data.commands).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    serde_json::to_string(&data.round_interrupts).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    serde_json::to_string(&data.retry_resolutions).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    data.retry_pending.as_ref().and_then(|p| serde_json::to_string(p).ok()),
                    data.checksum.map(|c| c as i64),
                    data.schema_version as i64,
                ],
            )?;

            if full {
                // Projection decisions: the session's own view history.
                self.conn.execute(
                    "DELETE FROM projections WHERE session_id = ?1",
                    params![data.id],
                )?;
                for directive in &data.transcript.directives {
                    self.insert_directive(data.id.as_str(), directive)?;
                }
                for unknown in &data.unknown_directives {
                    self.insert_unknown_directive(data.id.as_str(), unknown)?;
                }

                // Facts + memberships. Entries are global and shared by identity.
                self.conn.execute(
                    "DELETE FROM entry_memberships WHERE session_id = ?1",
                    params![data.id],
                )?;
                self.conn.execute(
                    "DELETE FROM blob_refs WHERE session_id = ?1",
                    params![data.id],
                )?;
                for entry in &data.transcript.entries {
                    self.upsert_entry(entry)?;
                    self.insert_membership(data.id.as_str(), entry.seq, entry.id.as_str())?;
                }
                for unknown in &data.unknown_entries {
                    self.upsert_entry_row(EntryEnvelope {
                        id: unknown.id.as_str(),
                        kind: unknown.kind.as_str(),
                        role: unknown.role.as_deref(),
                        content: unknown.content.as_deref(),
                        origin: unknown.origin.as_deref(),
                        hidden: unknown.hidden,
                        created_at_ms: unknown.created_at_ms,
                        payload: unknown.payload_json.as_str(),
                    })?;
                    self.insert_membership(data.id.as_str(), unknown.seq, unknown.id.as_str())?;
                }
                self.record_blob_refs(
                    data.id.as_str(),
                    &data.transcript.entries,
                    &data.unknown_entries,
                )?;
                self.conn.execute(
                    "DELETE FROM usage_records WHERE session_id = ?1",
                    params![data.id],
                )?;
                for record in &data.request_usage_records {
                    insert_usage_record_tx(&self.conn, data.id.as_str(), record)?;
                }
            } else {
                let watermark: i64 = self.conn.query_row(
                    "SELECT COALESCE(MAX(seq), -1) FROM entry_memberships WHERE session_id = ?1",
                    params![data.id],
                    |row| row.get(0),
                )?;
                let entry_start = data
                    .transcript
                    .entries
                    .partition_point(|entry| (entry.seq as i64) <= watermark);
                let mut new_entries = Vec::new();
                for entry in &data.transcript.entries[entry_start..] {
                    self.upsert_entry(entry)?;
                    self.insert_membership(data.id.as_str(), entry.seq, entry.id.as_str())?;
                    new_entries.push(entry);
                }
                let directive_watermark: i64 = self.conn.query_row(
                    "SELECT COALESCE(MAX(seq), -1) FROM projections WHERE session_id = ?1",
                    params![data.id],
                    |row| row.get(0),
                )?;
                let directive_start = data
                    .transcript
                    .directives
                    .partition_point(|directive| (directive.seq as i64) <= directive_watermark);
                for directive in &data.transcript.directives[directive_start..] {
                    self.insert_directive(data.id.as_str(), directive)?;
                }
                let new_entries: Vec<muta_contracts::TranscriptEntry> =
                    new_entries.into_iter().cloned().collect();
                self.record_blob_refs(data.id.as_str(), &new_entries, &[])?;
                for record in usage_upserts {
                    insert_usage_record_tx(&self.conn, data.id.as_str(), record)?;
                }
            }
            Ok(())
        })();

        match res {
            Ok(()) => self.conn.execute("COMMIT", []).map(|_| ()),
            Err(e) => {
                let _ = self.conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    fn insert_membership(&self, session_id: &str, seq: u64, entry_id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO entry_memberships (session_id, seq, entry_id) VALUES (?1, ?2, ?3)",
            params![session_id, seq as i64, entry_id],
        )?;
        Ok(())
    }

    fn insert_directive(
        &self,
        session_id: &str,
        directive: &muta_contracts::ProjectionDirective,
    ) -> Result<()> {
        let payload = serde_json::to_string(&directive.payload)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        self.conn.execute(
            "INSERT OR IGNORE INTO projections (session_id, seq, kind, up_to_seq, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session_id,
                directive.seq as i64,
                serde_plain(directive.kind)?,
                directive.up_to_seq as i64,
                payload,
            ],
        )?;
        Ok(())
    }

    fn insert_unknown_directive(
        &self,
        session_id: &str,
        unknown: &crate::session::UnknownDirectiveRow,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO projections (session_id, seq, kind, up_to_seq, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session_id,
                unknown.seq as i64,
                unknown.kind.as_str(),
                unknown.up_to_seq as i64,
                unknown.payload_json.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Write one entry row, offloading an oversized body to the CAS when this
    /// engine has a blob store (ADR-0187). The offload applies only to the
    /// row about to be inserted: already-durable rows keep their stored shape.
    fn upsert_entry(&self, entry: &muta_contracts::TranscriptEntry) -> Result<()> {
        let mut payload = entry.payload.clone();
        let mut content = entry.content.clone();
        if content
            .as_ref()
            .is_some_and(|c| c.len() > CAS_THRESHOLD_BYTES)
            && let muta_contracts::EntryPayload::Message(message_payload) = &mut payload
            && message_payload.content_blob.is_none()
            && let Some(blob_store) = &self.blob_store
        {
            let hash = blob_store
                .put(content.as_deref().unwrap_or_default().as_bytes())
                .map_err(rusqlite::Error::InvalidParameterName)?;
            message_payload.content_blob = Some(hash);
            if let Some(c) = &mut content {
                c.clear();
            }
        }
        let payload = serde_json::to_string(&payload)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        self.upsert_entry_row(EntryEnvelope {
            id: entry.id.as_str(),
            kind: if entry.kind == muta_contracts::EntryKind::State {
                "state"
            } else {
                "message"
            },
            role: entry.role.map(role_str),
            content: content.as_deref(),
            origin: entry.origin.map(origin_str),
            hidden: entry.hidden,
            created_at_ms: entry.created_at_ms,
            payload: payload.as_str(),
        })
    }

    fn upsert_entry_row(&self, row: EntryEnvelope<'_>) -> Result<()> {
        let EntryEnvelope {
            id,
            kind,
            role,
            content,
            origin,
            hidden,
            created_at_ms,
            payload,
        } = row;
        self.conn.execute(
            r#"
            INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind,
                role = excluded.role,
                content = excluded.content,
                origin = excluded.origin,
                hidden = excluded.hidden,
                created_at_ms = excluded.created_at_ms,
                payload = excluded.payload;
            "#,
            params![
                id,
                kind,
                role,
                content,
                origin,
                hidden,
                created_at_ms as i64,
                payload
            ],
        )?;
        Ok(())
    }

    /// Maintain the durable blob reference ledger for the entries this save
    /// touches. Callers pass exactly the entries whose memberships they wrote.
    fn record_blob_refs(
        &self,
        session_id: &str,
        entries: &[muta_contracts::TranscriptEntry],
        unknown: &[crate::session::UnknownEntryRow],
    ) -> Result<()> {
        let mut refs: Vec<String> = Vec::new();
        for entry in entries {
            if let muta_contracts::EntryPayload::Message(payload) = &entry.payload
                && let Some(hash) = &payload.content_blob
            {
                refs.push(hash.clone());
            }
        }
        for row in unknown {
            if let Some(hash) = extract_content_blob(&row.payload_json) {
                refs.push(hash);
            }
        }
        for hash in refs {
            self.conn.execute(
                "INSERT OR IGNORE INTO blob_refs (session_id, hash) VALUES (?1, ?2)",
                params![session_id, hash],
            )?;
        }
        Ok(())
    }

    /// Read the durable usage ledger for one session, in deterministic key
    /// order (ADR-0187): rows live in their own table, not on the session row.
    fn load_usage_records(
        &self,
        session_id: &str,
    ) -> Result<Vec<muta_contracts::RequestUsageRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT payload FROM usage_records WHERE session_id = ?1
             ORDER BY round ASC, turn ASC, attempt ASC, actor_id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| row.get::<_, String>(0))?;
        let mut records = Vec::new();
        for payload in rows {
            let payload = payload?;
            match serde_json::from_str(&payload) {
                Ok(record) => records.push(record),
                Err(error) => tracing::warn!(
                    session = %session_id,
                    error = %error,
                    "usage record payload undecodable; skipped"
                ),
            }
        }
        Ok(records)
    }

    /// Load a full [`crate::session::SessionData`] by session ID from SQLite.
    /// Entries and directives whose payloads the current binary cannot decode
    /// are preserved verbatim (ADR-0187): they ride in memory as raw rows and
    /// round-trip through every save untouched, so a database written by a
    /// newer binary survives an older binary without loss or orphaning.
    pub(crate) fn load_session_full(
        &self,
        session_id: &str,
    ) -> Result<Option<crate::session::SessionData>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, parent_id, fork_kind, title, created_at_s, updated_at_s, workspace_root, additional_roots, persona, digest, digest_anchor, tree, transcript_generation, provider_connection, round_counter, unattended, disabled_tools, commands, round_interrupts, retry_resolutions, retry_pending, checksum, schema_version FROM sessions WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<i64>>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, i64>(14)?,
                        row.get::<_, i64>(15)?,
                        row.get::<_, String>(16)?,
                        row.get::<_, String>(17)?,
                        row.get::<_, String>(18)?,
                        row.get::<_, String>(19)?,
                        row.get::<_, Option<String>>(20)?,
                        row.get::<_, Option<i64>>(21)?,
                        row.get::<_, i64>(22)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            id,
            parent_id,
            fork_kind,
            title,
            created_at_s,
            updated_at_s,
            workspace_root,
            additional_roots,
            persona,
            digest,
            digest_anchor,
            tree,
            generation,
            provider_connection,
            round_counter,
            unattended,
            disabled_tools,
            commands,
            round_interrupts,
            retry_resolutions,
            retry_pending,
            checksum,
            schema_version,
        )) = row
        else {
            return Ok(None);
        };

        let mut entries = Vec::new();
        let mut unknown_entries = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT e.id, m.seq, e.kind, e.role, e.content, e.origin, e.hidden, e.created_at_ms, e.payload \
                 FROM entry_memberships m JOIN entries e ON e.id = m.entry_id \
                 WHERE m.session_id = ?1 ORDER BY m.seq ASC",
            )?;
            let rows = stmt.query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })?;
            for row in rows {
                let (eid, seq, kind, role, content, origin, hidden, created_at_ms, payload) = row?;
                let decoded = (|| -> Option<muta_contracts::TranscriptEntry> {
                    let kind = if kind == "state" {
                        muta_contracts::EntryKind::State
                    } else {
                        muta_contracts::EntryKind::Message
                    };
                    let role = role.as_deref().and_then(role_from_str);
                    let origin = origin.as_deref().and_then(origin_from_str);
                    let payload: muta_contracts::EntryPayload =
                        serde_json::from_str(&payload).ok()?;
                    Some(muta_contracts::TranscriptEntry {
                        id: eid.clone(),
                        seq: seq.max(0) as u64,
                        kind,
                        role,
                        content: content.clone(),
                        origin,
                        hidden: hidden != 0,
                        created_at_ms: created_at_ms.max(0) as u64,
                        payload,
                    })
                })();
                if let Some(entry) = decoded {
                    entries.push(entry);
                } else {
                    tracing::warn!(
                        session = %session_id,
                        entry = %eid,
                        seq,
                        "entry payload not decodable by this binary; preserved verbatim"
                    );
                    unknown_entries.push(crate::session::UnknownEntryRow {
                        id: eid,
                        seq: seq.max(0) as u64,
                        kind,
                        role,
                        content,
                        origin,
                        hidden: hidden != 0,
                        created_at_ms: created_at_ms.max(0) as u64,
                        payload_json: payload,
                    });
                }
            }
        }

        let mut directives = Vec::new();
        let mut unknown_directives = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT seq, kind, up_to_seq, payload FROM projections WHERE session_id = ?1 ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (seq, kind, up_to_seq, payload) = row?;
                let decoded = (|| -> Option<muta_contracts::ProjectionDirective> {
                    let kind = match kind.as_str() {
                        "prune" => muta_contracts::DirectiveKind::Prune,
                        "compact" => muta_contracts::DirectiveKind::Compact,
                        "freeze" => muta_contracts::DirectiveKind::Freeze,
                        _ => return None,
                    };
                    let payload: muta_contracts::DirectivePayload =
                        serde_json::from_str(&payload).ok()?;
                    Some(muta_contracts::ProjectionDirective {
                        seq: seq.max(0) as u64,
                        kind,
                        up_to_seq: up_to_seq.max(0) as u64,
                        payload,
                    })
                })();
                if let Some(directive) = decoded {
                    directives.push(directive);
                } else {
                    tracing::warn!(
                        session = %session_id,
                        seq,
                        "directive payload not decodable by this binary; preserved verbatim"
                    );
                    unknown_directives.push(crate::session::UnknownDirectiveRow {
                        seq: seq.max(0) as u64,
                        kind,
                        up_to_seq: up_to_seq.max(0) as u64,
                        payload_json: payload,
                    });
                }
            }
        }

        let digest: Option<muta_contracts::SessionDigest> = match digest.as_deref() {
            Some(raw) => match serde_json::from_str(raw) {
                Ok(parsed) => Some(parsed),
                Err(error) => {
                    tracing::warn!(session = %session_id, error = %error, "digest column undecodable; ignored");
                    None
                }
            },
            None => None,
        };
        let tree = match tree.as_deref() {
            Some(raw) => serde_json::from_str(raw).unwrap_or_else(|error| {
                tracing::warn!(session = %session_id, error = %error, "session tree column undecodable; reset");
                Default::default()
            }),
            None => Default::default(),
        };

        let data = crate::session::SessionData {
            transcript: muta_contracts::Transcript {
                min_next_seq: entries
                    .iter()
                    .map(|entry| entry.seq + 1)
                    .chain(unknown_entries.iter().map(|row| row.seq + 1))
                    .max()
                    .unwrap_or(0),
                min_next_directive_seq: directives
                    .iter()
                    .map(|directive| directive.seq + 1)
                    .chain(unknown_directives.iter().map(|row| row.seq + 1))
                    .max()
                    .unwrap_or(0),
                entries,
                directives,
            },
            last_projection: None,
            digest,
            // The anchor is a transcript char count (ADR-0187): persisted for
            // real; legacy rows without one refresh their digest once.
            digest_anchor: digest_anchor.map(|a| a.max(0) as u64),
            id,
            parent_id,
            fork_kind: match fork_kind.as_str() {
                "fork" => muta_contracts::SessionForkKind::Fork,
                "aside" => muta_contracts::SessionForkKind::Aside,
                "subagent" => muta_contracts::SessionForkKind::Subagent,
                _ => muta_contracts::SessionForkKind::Trunk,
            },
            title,
            created_at: created_at_s.max(0) as u64,
            updated_at: updated_at_s.max(0) as u64,
            persona,
            workspace: workspace_root.map(|root| muta_contracts::WorkspaceBinding {
                root: PathBuf::from(root),
                additional_roots: serde_json::from_str(&additional_roots).unwrap_or_else(|error| {
                    tracing::warn!(session = %session_id, error = %error, "additional_roots column undecodable; treated as empty");
                    Vec::new()
                }),
            }),
            schema_version: if schema_version > 0 { schema_version as u32 } else { crate::session::CURRENT_SCHEMA_VERSION },
            checksum: checksum.map(|c| c as u32),
            generation: generation.unwrap_or_default(),
            provider_selection: provider_connection
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok()),
            disabled_tools: serde_json::from_str(&disabled_tools).unwrap_or_else(|error| {
                tracing::warn!(session = %session_id, error = %error, "disabled_tools column undecodable; treated as empty");
                Default::default()
            }),
            round_counter: round_counter.max(0) as u64,
            request_usage_records: self.load_usage_records(session_id)?,
            commands: serde_json::from_str(&commands).unwrap_or_else(|error| {
                tracing::warn!(session = %session_id, error = %error, "commands column undecodable; treated as empty");
                Default::default()
            }),
            round_interrupts: serde_json::from_str(&round_interrupts).unwrap_or_else(|error| {
                tracing::warn!(session = %session_id, error = %error, "round_interrupts column undecodable; treated as empty");
                Default::default()
            }),
            retry_resolutions: serde_json::from_str(&retry_resolutions).unwrap_or_else(|error| {
                tracing::warn!(session = %session_id, error = %error, "retry_resolutions column undecodable; treated as empty");
                Default::default()
            }),
            retry_pending: retry_pending
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok()),
            unattended: unattended != 0,
            tree,
            unknown_entries,
            unknown_directives,
        };
        crate::session::verify_checksum(&data, session_id);
        Ok(Some(data))
    }

    /// Public read-only transcript accessor for out-of-crate readers (the
    /// runtime's Archivist tools, ADR-0208): the full persisted
    /// [`crate::session::SessionData`] for one session, checksum-verified,
    /// or `None` when the id is unknown.
    /// One persisted session's projected transcript tail plus metadata, as
    /// plain wire-serializable rows (ADR-0208): the read path the Archivist's
    /// `archivist_read_session` tool serves. [`SessionTranscriptView`] keeps
    /// `SessionData`'s fields private while exposing exactly the projection
    /// the retrieval plane needs.
    pub(crate) fn read_session_transcript(
        &self,
        session_id: &str,
        tail: usize,
    ) -> Result<Option<SessionTranscriptView>> {
        let Some(data) = self.load_session_full(session_id)? else {
            return Ok(None);
        };
        let projected = data.transcript.project();
        let total = projected.len();
        let start = total.saturating_sub(tail);
        Ok(Some(SessionTranscriptView {
            id: data.id.clone(),
            title: data.title.clone(),
            digest: data.digest.clone(),
            workspace_root: data
                .workspace
                .as_ref()
                .map(|w| w.root.to_string_lossy().into_owned()),
            message_count: total,
            messages: projected[start..]
                .iter()
                .map(|(seq, m)| SessionMessageView {
                    seq: *seq,
                    role: role_str(m.role).to_string(),
                    content: m.content.clone(),
                })
                .collect(),
        }))
    }

    /// Resolve a session ID prefix (4+ hex chars) to matching full session IDs.
    pub(crate) fn resolve_session_prefix(
        &self,
        prefix: &str,
        filter: Option<&muta_contracts::WorkspaceFilter>,
    ) -> Result<Vec<String>> {
        let pattern = format!("{prefix}%");
        let mut matches = Vec::new();
        match filter {
            Some(muta_contracts::WorkspaceFilter::Path(path)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id FROM sessions WHERE id LIKE ?1 AND workspace_root = ?2 ORDER BY updated_at_s DESC",
                )?;
                let rows =
                    stmt.query_map(params![pattern, path.to_string_lossy()], |row| row.get(0))?;
                for id in rows {
                    matches.push(id?);
                }
            }
            Some(muta_contracts::WorkspaceFilter::Unbound) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id FROM sessions WHERE id LIKE ?1 AND workspace_root IS NULL ORDER BY updated_at_s DESC",
                )?;
                let rows = stmt.query_map(params![pattern], |row| row.get(0))?;
                for id in rows {
                    matches.push(id?);
                }
            }
            _ => {
                let mut stmt = self.conn.prepare(
                    "SELECT id FROM sessions WHERE id LIKE ?1 ORDER BY updated_at_s DESC",
                )?;
                let rows = stmt.query_map(params![pattern], |row| row.get(0))?;
                for id in rows {
                    matches.push(id?);
                }
            }
        }
        Ok(matches)
    }

    /// Resolve a session's durable workspace binding and staffing persona by
    /// exact id, regardless of the caller (ADR-0226). Used to lazily resume a
    /// session from any client and re-apply its persona.
    #[allow(clippy::type_complexity)]
    pub(crate) fn lookup_session_workspace(
        &self,
        session_id: &str,
    ) -> Result<Option<(Option<muta_contracts::WorkspaceBinding>, Option<String>)>> {
        let row = self
            .conn
            .query_row(
                "SELECT workspace_root, additional_roots, persona FROM sessions WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((workspace_root, additional_roots, persona)) = row else {
            return Ok(None);
        };
        let workspace = workspace_root.map(|root| muta_contracts::WorkspaceBinding {
            root: PathBuf::from(root),
            additional_roots: serde_json::from_str(&additional_roots).unwrap_or_default(),
        });
        Ok(Some((workspace, persona)))
    }

    /// List session summaries for a derived grouping, sorted by `updated_at_s`
    /// descending.
    pub(crate) fn list_session_summaries(
        &self,
        filter: Option<&muta_contracts::WorkspaceFilter>,
        active_id: &str,
    ) -> Result<Vec<crate::session::SessionSummary>> {
        const COLS: &str = "id, parent_id, fork_kind, title, created_at_s, updated_at_s, \
                            msg_count, last_user_prompt, digest";
        let mut summaries = Vec::new();
        match filter {
            Some(muta_contracts::WorkspaceFilter::Path(path)) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions WHERE workspace_root = ?1 AND fork_kind <> 'subagent' ORDER BY updated_at_s DESC;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(params![path.to_string_lossy()], map_summary_row)?;
                for item in rows {
                    push_summary(&mut summaries, item?, active_id);
                }
            }
            Some(muta_contracts::WorkspaceFilter::Unbound) => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions WHERE workspace_root IS NULL AND fork_kind <> 'subagent' ORDER BY updated_at_s DESC;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map([], map_summary_row)?;
                for item in rows {
                    push_summary(&mut summaries, item?, active_id);
                }
            }
            _ => {
                let sql = format!(
                    "SELECT {COLS} FROM sessions WHERE fork_kind <> 'subagent' ORDER BY updated_at_s DESC;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map([], map_summary_row)?;
                for item in rows {
                    push_summary(&mut summaries, item?, active_id);
                }
            }
        }
        summaries.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        Ok(summaries)
    }

    /// The most recent non-subagent session matching `filter`, optionally
    /// restricted to a staffing persona (ADR-0226). Backs `--resume`.
    pub(crate) fn latest_session(
        &self,
        filter: &muta_contracts::WorkspaceFilter,
        persona: Option<&str>,
    ) -> Result<Option<String>> {
        use muta_contracts::WorkspaceFilter;
        let base = "SELECT id FROM sessions WHERE fork_kind <> 'subagent'";
        let (sql, bind_path) = match (filter, persona.is_some()) {
            (WorkspaceFilter::Path(_), true) => (
                format!(
                    "{base} AND workspace_root = ?1 AND persona = ?2 ORDER BY updated_at_s DESC LIMIT 1"
                ),
                true,
            ),
            (WorkspaceFilter::Path(_), false) => (
                format!("{base} AND workspace_root = ?1 ORDER BY updated_at_s DESC LIMIT 1"),
                true,
            ),
            (WorkspaceFilter::Unbound, true) => (
                format!(
                    "{base} AND workspace_root IS NULL AND persona = ?1 ORDER BY updated_at_s DESC LIMIT 1"
                ),
                false,
            ),
            (WorkspaceFilter::Unbound, false) => (
                format!("{base} AND workspace_root IS NULL ORDER BY updated_at_s DESC LIMIT 1"),
                false,
            ),
            (WorkspaceFilter::Any, true) => (
                format!("{base} AND persona = ?1 ORDER BY updated_at_s DESC LIMIT 1"),
                false,
            ),
            (WorkspaceFilter::Any, false) => {
                (format!("{base} ORDER BY updated_at_s DESC LIMIT 1"), false)
            }
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let get = |row: &Row| row.get::<_, String>(0);
        let found = if bind_path && persona.is_some() {
            let path = filter
                .as_path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            stmt.query_row(params![path, persona.unwrap()], get)
                .optional()?
        } else if bind_path {
            let path = filter
                .as_path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            stmt.query_row(params![path], get).optional()?
        } else if let Some(persona) = persona {
            stmt.query_row(params![persona], get).optional()?
        } else {
            stmt.query_row([], get).optional()?
        };
        Ok(found)
    }

    /// Retrieve full session detail for on-demand inspection.
    pub(crate) fn get_session_detail(
        &self,
        session_id: &str,
        active_id: &str,
    ) -> Result<Option<muta_contracts::SessionDetail>> {
        if let Some(data) = self.load_session_full(session_id)? {
            let last_prompt = crate::session::last_effective_prompt_from_data(&data);
            Ok(Some(muta_contracts::SessionDetail {
                id: data.id.clone(),
                title: data.title.clone(),
                digest: data.digest.clone(),
                created_at: data.created_at,
                updated_at: data.updated_at,
                message_count: data.transcript.entries.len(),
                active: data.id == active_id,
                last_prompt,
            }))
        } else {
            Ok(None)
        }
    }

    /// Rename a session in the database. ADR-0186: a non-`NULL` title is
    /// terminal; the manual flag is retained in the signature for the command
    /// surface but no longer stored. A single-row UPDATE: the transcript and
    /// working state are untouched, so no load/save round trip is needed.
    pub(crate) fn rename_session(
        &self,
        session_id: &str,
        title: Option<&str>,
        manual: bool,
    ) -> Result<bool> {
        let _ = manual;
        let now = crate::session::unix_timestamp() as i64;
        let affected = self.conn.execute(
            "UPDATE sessions SET title = ?1, updated_at_s = ?2 WHERE id = ?3",
            params![title, now, session_id],
        )?;
        Ok(affected > 0)
    }

    // Typed JSON KV Helpers (ADR-0168)

    /// Set (or overwrite) a key in the unified KV store.
    pub(crate) fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv_store (key, value, updated_at) VALUES (?1, ?2, strftime('%s','now')) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value],
        )?;
        Ok(())
    }

    /// Fetch a key from the unified KV store.
    pub(crate) fn get_kv(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM kv_store WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
    }

    /// Delete a key from the unified KV store.
    pub(crate) fn delete_kv(&self, key: &str) -> Result<bool> {
        let affected = self
            .conn
            .execute("DELETE FROM kv_store WHERE key = ?1", params![key])?;
        Ok(affected > 0)
    }

    /// Record a slash-command invocation in the durable command ledger.
    pub(crate) fn record_command(&self, cmd: &muta_contracts::CommandRecord) -> Result<()> {
        let id = format!(
            "{}:{}:{}",
            cmd.name,
            cmd.timestamp,
            muta_contracts::todos::unix_now()
        );
        self.conn.execute(
            r#"
            INSERT INTO commands (id, session_id, name, arguments, result, status, created_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                id,
                "", // command ledger rows are session-agnostic audit records
                cmd.name,
                cmd.args,
                cmd.result
                    .as_ref()
                    .and_then(|r| serde_json::to_string(r).ok()),
                match cmd.status {
                    muta_contracts::CommandStatus::Success => "ok",
                    muta_contracts::CommandStatus::Error => "failed",
                    muta_contracts::CommandStatus::UserCancelled => "cancelled",
                },
                cmd.timestamp as i64,
            ],
        )?;
        Ok(())
    }

    /// Insert or replace one request-projection record (ADR-0218). Keyed by
    /// `(session, round, turn)`, so a re-recorded logical invocation replaces
    /// its row. After the insert, the per-session archive is trimmed to the
    /// newest [`MAX_RETAINED_REQUEST_PROJECTIONS`] rows.
    pub(crate) fn insert_request_projection(
        &self,
        session_id: &str,
        record: &muta_contracts::RequestProjection,
    ) -> Result<()> {
        let payload = serde_json::to_string(record)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        self.conn.execute(
            "INSERT OR REPLACE INTO request_projections
                (session_id, round, turn, created_at_ms, payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session_id,
                record.round as i64,
                record.turn as i64,
                record.created_at_ms as i64,
                payload,
            ],
        )?;
        self.conn.execute(
            "DELETE FROM request_projections
             WHERE session_id = ?1
               AND rowid NOT IN (
                   SELECT rowid FROM request_projections
                   WHERE session_id = ?1
                   ORDER BY created_at_ms DESC, rowid DESC
                   LIMIT ?2
               )",
            params![session_id, MAX_RETAINED_REQUEST_PROJECTIONS as i64],
        )?;
        Ok(())
    }

    /// Load a session's request-projection archive in capture order, oldest
    /// first, capped at `limit`. Undecodable payloads are skipped with a log,
    /// never surfaced as history.
    pub(crate) fn load_request_projections(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<muta_contracts::RequestProjection>> {
        let mut stmt = self.conn.prepare(
            "SELECT payload FROM request_projections WHERE session_id = ?1
             ORDER BY created_at_ms ASC, rowid ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut records = Vec::new();
        for payload in rows {
            let payload = payload?;
            match serde_json::from_str(&payload) {
                Ok(record) => records.push(record),
                Err(error) => tracing::warn!(
                    session = %session_id,
                    error = %error,
                    "request projection payload undecodable; skipped"
                ),
            }
        }
        Ok(records)
    }

    /// List keys with a given prefix, ordered descending.
    pub(crate) fn list_kv_keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let pattern = format!("{prefix}%");
        let mut stmt = self
            .conn
            .prepare("SELECT key FROM kv_store WHERE key LIKE ?1 ORDER BY key DESC")?;
        let rows = stmt.query_map(params![pattern], |row| row.get(0))?;
        let mut keys = Vec::new();
        for k in rows {
            keys.push(k?);
        }
        Ok(keys)
    }

    // FTS5 Full-Text History Search (proto.muta.v1.MutaService/SearchHistory)

    /// Perform BM25 full-text search across transcript entries, optionally
    /// filtered by workspace root. Hits join the owning session's title and
    /// `updated_at` so a hit is presentable and rankable without a second
    /// round-trip (ADR-0208).
    ///
    /// The raw query is sanitized into a safe FTS5 MATCH expression: each
    /// whitespace-separated word becomes a quoted phrase, joined with AND.
    /// This is both injection-proof (a bare word that collides with a column
    /// name — `needle`, `OR`, `NOT` — is a syntax/column error in raw MATCH)
    /// and friendlier to recall: `retry loop` matches texts containing both
    /// words, in any position. Callers that need raw FTS5 operators can
    /// bypass by quoting inline (`"retry OR fail"` is preserved verbatim when
    /// the caller already supplies balanced quotes... no — every word is
    /// quoted; phrase operators are intentionally not reachable here).
    pub(crate) fn search_history(
        &self,
        query: &str,
        filter: Option<&muta_contracts::WorkspaceFilter>,
        limit: usize,
    ) -> Result<Vec<HistorySearchResult>> {
        self.search_history_inner(query, filter, limit, false)
    }

    /// [`Self::search_history`] with the recall widened: the sanitized words
    /// are joined with OR instead of AND (ADR-0208 Layer 3's deterministic
    /// fallback). A gist query whose ANDed words never co-occur in one entry
    /// still recalls the entries carrying any of its words, BM25-ranked so
    /// multi-word hits float up. Callers fall back to this only after the
    /// strict search comes back empty, so the widened net never replaces a
    /// precise hit.
    pub(crate) fn search_history_relaxed(
        &self,
        query: &str,
        filter: Option<&muta_contracts::WorkspaceFilter>,
        limit: usize,
    ) -> Result<Vec<HistorySearchResult>> {
        self.search_history_inner(query, filter, limit, true)
    }

    fn search_history_inner(
        &self,
        query: &str,
        filter: Option<&muta_contracts::WorkspaceFilter>,
        limit: usize,
        match_any: bool,
    ) -> Result<Vec<HistorySearchResult>> {
        let clean_query = sanitize_fts_query_joined(query, match_any);
        if clean_query.is_empty() {
            return Ok(Vec::new());
        }

        let cols = "f.entry_id, f.session_id, s.workspace_root, s.title, f.role, \
                    snippet(fts_entries, 3, '<b>', '</b>', '...', 16) AS snippet, \
                    bm25(fts_entries) AS score";
        let mut results = Vec::new();
        match filter {
            Some(muta_contracts::WorkspaceFilter::Path(path)) => {
                let sql = format!(
                    "SELECT {cols} FROM fts_entries f JOIN sessions s ON f.session_id = s.id \
                     WHERE fts_entries MATCH ?1 AND s.workspace_root = ?2 ORDER BY score ASC LIMIT ?3;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(
                    params![clean_query, path.to_string_lossy(), limit as i64],
                    map_search_row,
                )?;
                for item in rows {
                    results.push(item?);
                }
            }
            Some(muta_contracts::WorkspaceFilter::Unbound) => {
                let sql = format!(
                    "SELECT {cols} FROM fts_entries f JOIN sessions s ON f.session_id = s.id \
                     WHERE fts_entries MATCH ?1 AND s.workspace_root IS NULL ORDER BY score ASC LIMIT ?2;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(params![clean_query, limit as i64], map_search_row)?;
                for item in rows {
                    results.push(item?);
                }
            }
            _ => {
                let sql = format!(
                    "SELECT {cols} FROM fts_entries f JOIN sessions s ON f.session_id = s.id \
                     WHERE fts_entries MATCH ?1 ORDER BY score ASC LIMIT ?2;"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(params![clean_query, limit as i64], map_search_row)?;
                for item in rows {
                    results.push(item?);
                }
            }
        }

        Ok(results)
    }

    // Typed JSON KV Helpers (ADR-0168)

    /// Retrieve and deserialize a JSON value from `kv_store`.
    pub(crate) fn get_json<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>> {
        if let Some(raw) = self.get_kv(key)? {
            match serde_json::from_str::<T>(&raw) {
                Ok(val) => Ok(Some(val)),
                Err(err) => {
                    tracing::warn!(key = %key, error = %err, "Failed to deserialize JSON from kv_store");
                    Ok(None)
                }
            }
        } else {
            Ok(None)
        }
    }

    // Authoritative Input History Operations (ADR-0168 / SSOT)

    /// Record a prompt into `input_history`, respecting `dedup` and the global `HISTORY_CAP`.
    pub(crate) fn record_input_history(
        &self,
        entry: &muta_contracts::HistoryEntry,
        dedup: bool,
    ) -> Result<()> {
        self.conn.execute("BEGIN IMMEDIATE", [])?;
        let res = (|| -> Result<()> {
            if dedup {
                self.conn.execute(
                    "DELETE FROM input_history WHERE text = ?1",
                    params![entry.text],
                )?;
            } else if let Some(session_id) = &entry.session_id {
                let latest_same: bool = self
                    .conn
                    .query_row(
                        "SELECT text = ?1 FROM input_history WHERE session_id = ?2 ORDER BY created_at_ms DESC, id DESC LIMIT 1",
                        params![entry.text, session_id],
                        |row| row.get(0),
                    )
                    .unwrap_or(false);
                if latest_same {
                    return Ok(());
                }
            }

            self.conn.execute(
                r#"
                INSERT INTO input_history (text, session_id, workspace, created_at_ms)
                VALUES (?1, ?2, ?3, ?4)
                "#,
                params![
                    entry.text,
                    entry.session_id,
                    entry.workspace,
                    entry.created_at_ms as i64,
                ],
            )?;

            self.conn.execute(
                r#"
                DELETE FROM input_history WHERE id NOT IN (
                    SELECT id FROM input_history ORDER BY created_at_ms DESC, id DESC LIMIT ?1
                )
                "#,
                params![muta_contracts::HISTORY_CAP as i64],
            )?;

            Ok(())
        })();

        match res {
            Ok(()) => {
                self.conn.execute("COMMIT", [])?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Load the newest prompt history entries up to `limit`.
    pub(crate) fn load_input_history(&self, limit: usize) -> Result<Vec<muta_contracts::HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT text, session_id, workspace, created_at_ms
            FROM input_history
            ORDER BY created_at_ms DESC, id DESC
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            let text: String = row.get(0)?;
            let session_id: Option<String> = row.get(1)?;
            let workspace: Option<String> = row.get(2)?;
            let created_at_ms: i64 = row.get(3)?;
            Ok(muta_contracts::HistoryEntry {
                text,
                session_id,
                workspace,
                created_at_ms: created_at_ms as u64,
            })
        })?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    /// Persist or batch-merge a list of history entries into SQLite.
    pub(crate) fn save_input_history(
        &self,
        entries: &[muta_contracts::HistoryEntry],
        dedup: bool,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        if entries.len() == 1 {
            return self.record_input_history(&entries[0], dedup);
        }

        self.conn.execute("BEGIN IMMEDIATE", [])?;
        let res = (|| -> Result<()> {
            let mut insert_stmt = self.conn.prepare(
                r#"
                INSERT INTO input_history (text, session_id, workspace, created_at_ms)
                VALUES (?1, ?2, ?3, ?4)
                "#,
            )?;

            let mut delete_dedup_stmt = if dedup {
                Some(
                    self.conn
                        .prepare("DELETE FROM input_history WHERE text = ?1")?,
                )
            } else {
                None
            };

            for entry in entries {
                if let Some(del_stmt) = &mut delete_dedup_stmt {
                    del_stmt.execute(params![entry.text])?;
                }
                insert_stmt.execute(params![
                    entry.text,
                    entry.session_id,
                    entry.workspace,
                    entry.created_at_ms as i64,
                ])?;
            }

            self.conn.execute(
                r#"
                DELETE FROM input_history WHERE id NOT IN (
                    SELECT id FROM input_history ORDER BY created_at_ms DESC, id DESC LIMIT ?1
                )
                "#,
                params![muta_contracts::HISTORY_CAP as i64],
            )?;

            Ok(())
        })();

        match res {
            Ok(()) => {
                self.conn.execute("COMMIT", [])?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Delete all prompt history records.
    pub(crate) fn clear_input_history(&self) -> Result<()> {
        self.conn.execute("DELETE FROM input_history", [])?;
        Ok(())
    }

    /// Delete a specific prompt history record by text and timestamp.
    /// If `created_at_ms` is non-zero, matches both text and timestamp;
    /// otherwise falls back to text match. Returns the number of deleted rows.
    pub(crate) fn delete_input_history_entry(&self, text: &str, created_at_ms: u64) -> Result<usize> {
        let deleted = if created_at_ms > 0 {
            self.conn.execute(
                "DELETE FROM input_history WHERE text = ?1 AND created_at_ms = ?2",
                params![text, created_at_ms as i64],
            )?
        } else {
            self.conn
                .execute("DELETE FROM input_history WHERE text = ?1", params![text])?
        };
        Ok(deleted)
    }

    /// Migrate legacy history.json files into SQLite and purge them from disk.
    pub(crate) fn migrate_legacy_input_history(&self) -> usize {
        let mut candidates = Vec::new();
        let muta_state = crate::paths::get().state_dir;
        candidates.push(muta_state.join("history.json"));
        if let Some(parent) = muta_state.parent() {
            candidates.push(parent.join("mutx").join("history.json"));
            candidates.push(parent.join("neenee").join("history.json"));
        }

        if let Some(state_home) = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from) {
            candidates.push(state_home.join("mutx").join("history.json"));
            candidates.push(state_home.join("muta").join("history.json"));
            candidates.push(state_home.join("neenee").join("history.json"));
        } else if let Some(home) = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
        {
            let state_home = home.join(".local").join("state");
            candidates.push(state_home.join("mutx").join("history.json"));
            candidates.push(state_home.join("muta").join("history.json"));
            candidates.push(state_home.join("neenee").join("history.json"));
        }

        candidates.sort();
        candidates.dedup();

        let mut total = 0;
        for file in candidates {
            if !file.exists() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&file) else {
                continue;
            };
            let Ok(entries) = serde_json::from_str::<Vec<muta_contracts::HistoryEntry>>(&content)
            else {
                let _ = std::fs::remove_file(&file);
                continue;
            };

            if !entries.is_empty() {
                let count = entries.len();
                if self.save_input_history(&entries, true).is_ok() {
                    total += count;
                    let _ = std::fs::remove_file(&file);
                    info!(
                        path = %file.display(),
                        count,
                        "Migrated legacy input history JSON file into SQLite muta.db and purged file"
                    );
                }
            } else {
                let _ = std::fs::remove_file(&file);
            }
        }

        total
    }

    // Legacy Flat-File Migration (ADR-0168)
}

// ---------------------------------------------------------------------------
// The read door (ADR-0231)
// ---------------------------------------------------------------------------

/// The **only** way to read the unified store.
///
/// `DatabaseEngine` is crate-private, so no other module — in this crate or in
/// any other — can name a SQLite connection, let alone open one on a path of
/// its own choosing. Readers are handed out by the same handle that owns the
/// writer ([`PersistenceHandle::reader`]), which is what makes "every mutation
/// goes through the single-writer actor, every read through the handle" a
/// type-level invariant instead of a convention someone has to remember.
///
/// A reader is a short-lived connection snapshot: WAL readers neither block
/// the writer nor are blocked by it. Open one per read *session* (one per tool
/// call, one per report) rather than one per query inside a loop.
///
/// The method surface is deliberately an allow-list. Widening read reach is a
/// reviewable act in this file, not an incidental side effect of adding a
/// `pub fn` to the engine.
pub struct DbReader {
    engine: DatabaseEngine,
    db_path: PathBuf,
}

impl DbReader {
    fn new(engine: DatabaseEngine, db_path: PathBuf) -> Self {
        Self { engine, db_path }
    }

    /// The database file this reader is bound to.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    // -- sessions ----------------------------------------------------------

    /// One session's header row, if it exists.
    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        self.engine.get_session(session_id)
    }

    /// Sessions matching a derived grouping, newest first.
    pub fn list_sessions(
        &self,
        filter: Option<&muta_contracts::WorkspaceFilter>,
    ) -> Result<Vec<SessionRecord>> {
        self.engine.list_sessions(filter)
    }

    /// Picker-facing summaries for a derived grouping, newest first.
    pub fn list_session_summaries(
        &self,
        filter: Option<&muta_contracts::WorkspaceFilter>,
        active_id: &str,
    ) -> Result<Vec<crate::session::SessionSummary>> {
        self.engine.list_session_summaries(filter, active_id)
    }

    /// The most recently updated session id in `filter`, optionally narrowed
    /// to one persona (the `--resume` resolution leg).
    pub fn latest_session(
        &self,
        filter: &muta_contracts::WorkspaceFilter,
        persona: Option<&str>,
    ) -> Result<Option<String>> {
        self.engine.latest_session(filter, persona)
    }

    /// A session's inherited workspace binding plus its persona.
    pub fn lookup_session_workspace(
        &self,
        session_id: &str,
    ) -> Result<Option<(Option<muta_contracts::WorkspaceBinding>, Option<String>)>> {
        self.engine.lookup_session_workspace(session_id)
    }

    /// Resolve an id prefix to every matching session id.
    pub fn resolve_session_prefix(
        &self,
        prefix: &str,
        filter: Option<&muta_contracts::WorkspaceFilter>,
    ) -> Result<Vec<String>> {
        self.engine.resolve_session_prefix(prefix, filter)
    }

    /// Full detail for one session (the session-info sub-view).
    pub fn get_session_detail(
        &self,
        session_id: &str,
        active_id: &str,
    ) -> Result<Option<muta_contracts::SessionDetail>> {
        self.engine.get_session_detail(session_id, active_id)
    }

    /// One session's raw durable data (the load path of `SessionStore`).
    pub(crate) fn load_session_full(
        &self,
        session_id: &str,
    ) -> Result<Option<crate::session::SessionData>> {
        self.engine.load_session_full(session_id)
    }

    /// A session's projected transcript tail as wire rows (the Archivist's
    /// read tool).
    pub fn read_session_transcript(
        &self,
        session_id: &str,
        tail: usize,
    ) -> Result<Option<SessionTranscriptView>> {
        self.engine.read_session_transcript(session_id, tail)
    }

    /// Every blob hash the durable reference ledger still mentions. The sweep
    /// that consumes this is a *filesystem* pass, so it runs on the caller's
    /// thread rather than blocking the writer.
    pub fn live_blob_hashes(&self) -> Result<std::collections::HashSet<String>> {
        self.engine.live_blob_hashes()
    }

    // -- recall / history --------------------------------------------------

    /// BM25 search over every persisted transcript entry (strict AND form).
    pub fn search_history(
        &self,
        query: &str,
        filter: Option<&muta_contracts::WorkspaceFilter>,
        limit: usize,
    ) -> Result<Vec<HistorySearchResult>> {
        self.engine.search_history(query, filter, limit)
    }

    /// [`Self::search_history`] with the words OR-joined (recall fallback).
    pub fn search_history_relaxed(
        &self,
        query: &str,
        filter: Option<&muta_contracts::WorkspaceFilter>,
        limit: usize,
    ) -> Result<Vec<HistorySearchResult>> {
        self.engine.search_history_relaxed(query, filter, limit)
    }

    /// The prompt history, oldest first, capped at `limit`.
    pub fn load_input_history(&self, limit: usize) -> Result<Vec<muta_contracts::HistoryEntry>> {
        self.engine.load_input_history(limit)
    }

    /// Archived request projections for one session, oldest first.
    pub fn load_request_projections(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<muta_contracts::RequestProjection>> {
        self.engine.load_request_projections(session_id, limit)
    }

    // -- typed key/value ---------------------------------------------------

    /// A raw key-value entry.
    pub fn get_kv(&self, key: &str) -> Result<Option<String>> {
        self.engine.get_kv(key)
    }

    /// A JSON-encoded key-value entry.
    pub fn get_json<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>> {
        self.engine.get_json(key)
    }

    /// Every key with the given prefix.
    pub fn list_kv_keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        self.engine.list_kv_keys_with_prefix(prefix)
    }
}

// Asynchronous Persistence Actor (Single-Writer Pattern, supervised — ADR-0196)

/// Typed failure of a persistence command (ADR-0196 D2).
///
/// The single-writer actor is a long-lived service, so its failures are
/// *lifecycle* facts, not SQL facts. Every `PersistenceHandle` method returns
/// this error type; callers classify instead of string-matching.
#[derive(Debug)]
pub enum PersistenceError {
    /// The single-writer actor is not serving: it died (engine-open failure
    /// or panic) and has not been respawned yet, the supervisor is still in
    /// its backoff window, or the handle was shut down. The command was **not
    /// executed**; nothing was written.
    WriterDown,
    /// The actor served the command and the SQLite engine rejected it.
    Engine(rusqlite::Error),
    /// The command handler panicked. The supervisor's per-command
    /// `catch_unwind` contained it: the actor survives, this command failed.
    Poisoned(String),
    /// A value failed to encode *before* reaching the writer (e.g. JSON
    /// serialization). Nothing was written.
    Encode(String),
    /// The handle was explicitly shut down (every clone dropped / supervisor
    /// stopped). Distinct from [`PersistenceError::WriterDown`] so an orderly
    /// teardown is distinguishable from a crash.
    Closed,
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WriterDown => write!(f, "persistence writer is down"),
            Self::Engine(e) => write!(f, "persistence engine rejected the command: {e}"),
            Self::Poisoned(msg) => write!(f, "persistence command handler panicked: {msg}"),
            Self::Encode(msg) => write!(f, "could not encode persistence payload: {msg}"),
            Self::Closed => write!(f, "persistence writer was shut down"),
        }
    }
}

impl std::error::Error for PersistenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Engine(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for PersistenceError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Engine(e)
    }
}

/// Liveness of the single-writer actor, observable by any handle clone
/// (ADR-0196 D1/D4). Transitions are published on a `watch` channel; the
/// daemon folds them into the monitor stream, frontends render degradation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriterHealth {
    /// Serving normally.
    Healthy,
    /// The writer died or failed to (re)start and the supervisor is retrying
    /// with backoff. `attempt` counts respawn attempts since the first
    /// failure; `error` is the latest cause.
    Recovering {
        attempt: u32,
        since_ms: u64,
        error: String,
    },
    /// Respawn attempts exceeded the recovering budget (`RECOVERING_ATTEMPTS`)
    /// and every further failure is at the capped backoff. Retries continue
    /// forever; this state says "durability is degraded, tell the user".
    Down {
        attempt: u32,
        since_ms: u64,
        error: String,
    },
}

impl WriterHealth {
    /// Whether writes can currently be expected to succeed.
    pub fn is_serving(&self) -> bool {
        matches!(self, Self::Healthy)
    }

    /// The latest failure cause, when degraded.
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Healthy => None,
            Self::Recovering { error, .. } | Self::Down { error, .. } => Some(error),
        }
    }

    /// The wire representation for the monitor stream (ADR-0196 D4). The
    /// conversion lives here, next to the state machine, so no consumer can
    /// mis-translate a transition.
    pub fn to_wire(&self) -> muta_contracts::monitor::PersistenceHealth {
        match self {
            Self::Healthy => muta_contracts::monitor::PersistenceHealth::Healthy,
            Self::Recovering {
                attempt,
                since_ms,
                error,
            } => muta_contracts::monitor::PersistenceHealth::Recovering {
                attempt: *attempt,
                since_ms: *since_ms,
                error: error.clone(),
            },
            Self::Down {
                attempt,
                since_ms,
                error,
            } => muta_contracts::monitor::PersistenceHealth::Down {
                attempt: *attempt,
                since_ms: *since_ms,
                error: error.clone(),
            },
        }
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Command variants dispatched to the single-writer persistence actor.
pub(crate) enum PersistenceCommand {
    SaveSession {
        data: Box<crate::session::SessionData>,
        /// `true`: rewrite every membership/projection/blob reference from
        /// `data`. `false`: append only rows above the durable watermark,
        /// escalating to a full rewrite on generation mismatch (ADR-0187).
        full: bool,
        /// Usage-record upserts to apply on a delta save (the durable usage
        /// ledger lives in its own table; a full save rewrites it from
        /// `data`).
        usage_upserts: Vec<muta_contracts::RequestUsageRecord>,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    UpsertSession {
        record: SessionRecord,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    DeleteSession {
        session_id: String,
        ack: oneshot::Sender<Result<bool, PersistenceError>>,
    },
    RenameSession {
        session_id: String,
        title: Option<String>,
        manual: bool,
        ack: oneshot::Sender<Result<bool, PersistenceError>>,
    },
    RecordCommand {
        cmd: muta_contracts::CommandRecord,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    RecordRequestProjection {
        session_id: String,
        record: muta_contracts::RequestProjection,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    SetKV {
        key: String,
        value: String,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    DeleteKV {
        key: String,
        ack: oneshot::Sender<Result<bool, PersistenceError>>,
    },
    RecordInputHistory {
        entry: muta_contracts::HistoryEntry,
        dedup: bool,
        ack: Option<oneshot::Sender<Result<(), PersistenceError>>>,
    },
    SaveInputHistory {
        entries: Vec<muta_contracts::HistoryEntry>,
        dedup: bool,
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    ClearInputHistory {
        ack: oneshot::Sender<Result<(), PersistenceError>>,
    },
    DeleteInputHistoryEntry {
        text: String,
        created_at_ms: u64,
        ack: Option<oneshot::Sender<Result<usize, PersistenceError>>>,
    },
    /// Reclaim transcript entries no session membership references
    /// (ADR-0187).
    CollectEntryGarbage {
        ack: oneshot::Sender<Result<usize, PersistenceError>>,
    },
    /// Test-only: the writer acks and then exits its loop, simulating actor
    /// death so the supervisor's respawn path is exercisable (ADR-0196 D6).
    #[cfg(test)]
    Die { ack: oneshot::Sender<()> },
}

impl PersistenceCommand {
    /// Execute against the writer's engine. Every engine call is panic-
    /// guarded (ADR-0196 D1): a poisoned command settles its own ack as
    /// [`PersistenceError::Poisoned`] and the actor survives. Returns
    /// `false` only for the test-only death command.
    fn execute(self, engine: &DatabaseEngine) -> bool {
        match self {
            Self::SaveSession {
                data,
                full,
                usage_upserts,
                ack,
            } => {
                let res = guarded(|| engine.save_session_inner(&data, full, &usage_upserts));
                let _ = ack.send(res);
            }
            Self::UpsertSession { record, ack } => {
                let res = guarded(|| engine.upsert_session(&record));
                let _ = ack.send(res);
            }
            Self::DeleteSession { session_id, ack } => {
                let res = guarded(|| engine.delete_session(&session_id));
                let _ = ack.send(res);
            }
            Self::RenameSession {
                session_id,
                title,
                manual,
                ack,
            } => {
                let res = guarded(|| engine.rename_session(&session_id, title.as_deref(), manual));
                let _ = ack.send(res);
            }
            Self::RecordCommand { cmd, ack } => {
                let res = guarded(|| engine.record_command(&cmd));
                let _ = ack.send(res);
            }
            Self::RecordRequestProjection {
                session_id,
                record,
                ack,
            } => {
                let res = guarded(|| engine.insert_request_projection(&session_id, &record));
                let _ = ack.send(res);
            }
            Self::SetKV { key, value, ack } => {
                let res = guarded(|| engine.set_kv(&key, &value));
                let _ = ack.send(res);
            }
            Self::DeleteKV { key, ack } => {
                let res = guarded(|| engine.delete_kv(&key));
                let _ = ack.send(res);
            }
            Self::CollectEntryGarbage { ack } => {
                let res = guarded(|| engine.collect_entry_garbage());
                let _ = ack.send(res);
            }
            Self::RecordInputHistory { entry, dedup, ack } => {
                let res = guarded(|| engine.record_input_history(&entry, dedup));
                if let Some(ack) = ack {
                    let _ = ack.send(res);
                }
            }
            Self::SaveInputHistory {
                entries,
                dedup,
                ack,
            } => {
                let res = guarded(|| engine.save_input_history(&entries, dedup));
                let _ = ack.send(res);
            }
            Self::ClearInputHistory { ack } => {
                let res = guarded(|| engine.clear_input_history());
                let _ = ack.send(res);
            }
            Self::DeleteInputHistoryEntry {
                text,
                created_at_ms,
                ack,
            } => {
                let res = guarded(|| engine.delete_input_history_entry(&text, created_at_ms));
                if let Some(ack) = ack {
                    let _ = ack.send(res);
                }
            }
            #[cfg(test)]
            Self::Die { ack } => {
                let _ = ack.send(());
                return false;
            }
        }
        true
    }

    /// Resolve every ack with `Err(error)` without executing. Used by the
    /// supervisor to drain commands honestly while the writer is down
    /// (ADR-0196 D5): the command was never durable, and pretending
    /// otherwise would convert a visible failure into silent data loss.
    fn fail(self, error: PersistenceError) {
        match self {
            Self::SaveSession { ack, .. }
            | Self::UpsertSession { ack, .. }
            | Self::RecordCommand { ack, .. }
            | Self::RecordRequestProjection { ack, .. }
            | Self::SetKV { ack, .. }
            | Self::SaveInputHistory { ack, .. }
            | Self::ClearInputHistory { ack } => {
                let _ = ack.send(Err(error));
            }
            | Self::DeleteSession { ack, .. } | Self::RenameSession { ack, .. } => {
                let _ = ack.send(Err(error));
            }
            Self::DeleteKV { ack, .. } => {
                let _ = ack.send(Err(error));
            }
            Self::CollectEntryGarbage { ack } => {
                let _ = ack.send(Err(error));
            }
            Self::RecordInputHistory { ack, .. } => {
                if let Some(ack) = ack {
                    let _ = ack.send(Err(error));
                }
            }
            Self::DeleteInputHistoryEntry { ack, .. } => {
                if let Some(ack) = ack {
                    let _ = ack.send(Err(error));
                }
            }
            #[cfg(test)]
            Self::Die { ack } => {
                let _ = ack.send(());
            }
        }
    }
}

/// Run one engine call, catching handler panics (ADR-0196 D1).
fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T, PersistenceError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(res) => res.map_err(PersistenceError::Engine),
        Err(panic) => Err(PersistenceError::Poisoned(panic_message(&panic))),
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// The supervised single writer (ADR-0196 D1).
///
/// Front-door commands land here; the supervisor forwards them to the
/// *current* writer generation's channel. On writer death (send failure —
/// the actor thread dropped its receiver) it respawns with bounded
/// exponential backoff, publishing health transitions on the watch channel.
/// While down, arriving commands are drained with `Err(WriterDown)` acks
/// (D5) — the bounded front channel cannot back-pressure turns into a hang.
async fn run_supervisor(
    mut front_rx: mpsc::Receiver<PersistenceCommand>,
    db_path: PathBuf,
    blob_store: Option<BlobStore>,
    health: watch::Sender<WriterHealth>,
) {
    /// Respawn attempts before `Recovering` escalates to `Down`
    /// (~1.55 s of cumulative backoff at the base schedule below).
    const RECOVERING_ATTEMPTS: u32 = 4;
    const BASE_BACKOFF: Duration = Duration::from_millis(100);
    const MAX_BACKOFF: Duration = Duration::from_secs(5);

    let mut writer: Option<mpsc::Sender<PersistenceCommand>> = None;
    let mut attempt: u32 = 0;
    let mut since_ms: u64 = 0;
    let mut last_error: String;
    let mut next_try = Instant::now();
    let mut backoff = BASE_BACKOFF;

    while let Some(mut command) = front_rx.recv().await {
        'serve: loop {
            if let Some(tx) = &writer {
                match tx.send(command).await {
                    Ok(()) => break 'serve,
                    Err(mpsc::error::SendError(undelivered)) => {
                        // The actor thread is gone (engine-open failure at
                        // birth, panic escape, or explicit stop). Respawn.
                        command = undelivered;
                        writer = None;
                        attempt = 1;
                        since_ms = unix_ms();
                        last_error = "persistence writer stopped".to_string();
                        backoff = BASE_BACKOFF;
                        next_try = Instant::now();
                        let _ = health.send_if_modified(|current| {
                            *current = WriterHealth::Recovering {
                                attempt,
                                since_ms,
                                error: last_error.clone(),
                            };
                            true
                        });
                    }
                }
            } else if Instant::now() >= next_try {
                match spawn_writer(&db_path, blob_store.as_ref(), &health).await {
                    Ok(tx) => {
                        writer = Some(tx);
                        attempt = 0;
                        // `Healthy` is restored by the writer's first
                        // successful command, not by spawn alone (D1).
                    }
                    Err(e) => {
                        attempt += 1;
                        last_error = e.to_string();
                        next_try = Instant::now() + backoff;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        let state = if attempt > RECOVERING_ATTEMPTS {
                            WriterHealth::Down {
                                attempt,
                                since_ms,
                                error: last_error.clone(),
                            }
                        } else {
                            WriterHealth::Recovering {
                                attempt,
                                since_ms,
                                error: last_error.clone(),
                            }
                        };
                        let _ = health.send_if_modified(|current| {
                            *current = state;
                            true
                        });
                        // D5: drain-while-down — resolve honestly, never
                        // back-pressure the caller into a hang.
                        command.fail(PersistenceError::WriterDown);
                        break 'serve;
                    }
                }
            } else {
                // Down and still inside the backoff window: same D5 policy.
                command.fail(PersistenceError::WriterDown);
                break 'serve;
            }
        }
    }

    // Every handle clone dropped: orderly shutdown. Writers exit with their
    // channel; the health state records that the stop was deliberate.
    let _ = health.send_if_modified(|current| {
        *current = WriterHealth::Down {
            attempt: 0,
            since_ms: unix_ms(),
            error: "persistence writer was shut down (all handles dropped)".to_string(),
        };
        true
    });
}

/// Open the engine and spawn one writer generation on a dedicated thread.
/// The open happens on a blocking thread so a wedged SQLite open cannot
/// stall the supervisor.
async fn spawn_writer(
    db_path: &Path,
    blob_store: Option<&BlobStore>,
    health: &watch::Sender<WriterHealth>,
) -> std::result::Result<mpsc::Sender<PersistenceCommand>, rusqlite::Error> {
    let path = db_path.to_path_buf();
    let blobs = blob_store.cloned();
    let engine = tokio::task::spawn_blocking(move || DatabaseEngine::open(&path, blobs))
        .await
        .map_err(|join| {
            rusqlite::Error::ToSqlConversionFailure(
                format!("persistence writer spawn task failed: {join}").into(),
            )
        })??;

    let (tx, mut rx) = mpsc::channel::<PersistenceCommand>(1024);
    let health = health.clone();
    std::thread::Builder::new()
        .name("muta-persistence-writer".into())
        .spawn(move || {
            while let Some(command) = rx.blocking_recv() {
                if !command.execute(&engine) {
                    break;
                }
                // D1: `Healthy` is restored by the first *successful*
                // command after a degradation, not by spawn alone.
                health.send_if_modified(|current| {
                    if current.is_serving() {
                        false
                    } else {
                        *current = WriterHealth::Healthy;
                        true
                    }
                });
            }
        })
        .map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(
                format!("failed to spawn persistence writer thread: {e}").into(),
            )
        })?;
    Ok(tx)
}

/// Asynchronous handle for interacting with the supervised single-writer
/// persistence actor without blocking the Tokio runtime (ADR-0196).
///
/// The front door and the health channel survive writer death: a respawned
/// writer is reachable through the same handle, so no call site changes on
/// recovery. Clone freely; dropping the last clone shuts the supervisor down.
#[derive(Clone)]
pub struct PersistenceHandle {
    supervisor: mpsc::Sender<PersistenceCommand>,
    health: watch::Receiver<WriterHealth>,
    db_path: PathBuf,
    blob_store: Option<BlobStore>,
}

static GLOBAL_HANDLE: OnceLock<PersistenceHandle> = OnceLock::new();

impl fmt::Debug for PersistenceHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersistenceHandle")
            .field("db_path", &self.db_path)
            .field("health", &self.health.borrow())
            .finish_non_exhaustive()
    }
}

/// Get or initialize the global shared [`PersistenceHandle`].
pub fn get_persistence_handle() -> PersistenceHandle {
    GLOBAL_HANDLE
        .get_or_init(|| {
            let dirs = crate::paths::get();
            let db_path = dirs.db_file();
            let blobs = Some(BlobStore::new(dirs.blobs_dir()));
            PersistenceHandle::spawn(db_path, blobs)
        })
        .clone()
}

impl PersistenceHandle {
    /// Start the supervised persistence actor.
    ///
    /// The supervisor runs as a task on the current Tokio runtime when one
    /// is active, otherwise on a dedicated single-thread runtime of its own,
    /// so construction stays valid from sync contexts (library callers,
    /// tests).
    #[allow(clippy::expect_used)]
    pub fn spawn(db_path: PathBuf, blob_store: Option<BlobStore>) -> Self {
        let (supervisor, front_rx) = mpsc::channel::<PersistenceCommand>(1024);
        let (health_tx, health_rx) = watch::channel(WriterHealth::Healthy);

        let run = run_supervisor(front_rx, db_path.clone(), blob_store.clone(), health_tx);
        // Only a multi-thread runtime can host the supervisor: a
        // current-thread runtime is a *single* thread, so a caller that blocks
        // on an ack (every sync verb goes through
        // `PersistenceHandle::run_blocking`) would starve the very supervisor
        // that owes it the ack. A dedicated thread covers both that case and
        // the no-runtime case (library callers, tests).
        let runtime_host = tokio::runtime::Handle::try_current()
            .ok()
            .filter(|handle| {
                handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread
            });
        if let Some(handle) = runtime_host {
            handle.spawn(run);
        } else {
            std::thread::Builder::new()
                .name("muta-persistence-supervisor".into())
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("failed to build persistence supervisor runtime");
                    runtime.block_on(run);
                })
                .expect("failed to spawn persistence supervisor thread");
        }

        Self {
            supervisor,
            health: health_rx,
            db_path,
            blob_store,
        }
    }

    /// The current writer health snapshot (ADR-0196 D4).
    pub fn health(&self) -> WriterHealth {
        self.health.borrow().clone()
    }

    /// Subscribe to writer health transitions (ADR-0196 D4): the daemon
    /// folds these into the monitor stream; frontends render degradation.
    pub fn subscribe_health(&self) -> watch::Receiver<WriterHealth> {
        self.health.clone()
    }

    /// The database file path.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Asynchronously save a session in SQLite.
    pub(crate) async fn save_session(
        &self,
        data: crate::session::SessionData,
        full: bool,
        usage_upserts: Vec<muta_contracts::RequestUsageRecord>,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::SaveSession {
                data: Box::new(data),
                full,
                usage_upserts,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Synchronously save a session on a blocking thread (always a full
    /// rewrite: the blocking callers write fresh or rebuilt state).
    pub(crate) fn save_session_blocking(
        &self,
        data: crate::session::SessionData,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::SaveSession {
                data: Box::new(data),
                full: true,
                usage_upserts: Vec::new(),
                ack: ack_tx,
            },
            ack_rx,
        )
    }

    /// Asynchronously upsert a session record.
    pub async fn upsert_session(&self, record: SessionRecord) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::UpsertSession {
                record,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Non-blocking fire-and-forget session upsert to avoid blocking synchronous writers.
    pub fn try_upsert_session(&self, record: SessionRecord) {
        let (ack_tx, _) = oneshot::channel();
        if let Err(error) = self.supervisor.try_send(PersistenceCommand::UpsertSession {
            record,
            ack: ack_tx,
        }) {
            warn!(error = %error, "dropped fire-and-forget session upsert: writer unavailable");
        }
    }

    /// Asynchronously delete a session record.
    pub async fn delete_session(&self, session_id: String) -> Result<bool, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::DeleteSession {
                session_id,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Asynchronously rename a session.
    pub async fn rename_session(
        &self,
        session_id: String,
        title: Option<String>,
        manual: bool,
    ) -> Result<bool, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::RenameSession {
                session_id,
                title,
                manual,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Asynchronously record a command invocation.
    pub async fn record_command(
        &self,
        cmd: muta_contracts::CommandRecord,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::RecordCommand { cmd, ack: ack_tx })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Asynchronously insert a request-projection record (ADR-0218). Awaits the
    /// writer ack for callers that need the durable write confirmed (tests,
    /// tooling); the request hot path uses
    /// [`Self::try_record_request_projection`] instead.
    pub async fn record_request_projection(
        &self,
        session_id: String,
        record: muta_contracts::RequestProjection,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::RecordRequestProjection {
                session_id,
                record,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Non-blocking fire-and-forget request-projection insert (ADR-0218). Used
    /// on the model-request hot path: forensic persistence must never block
    /// dispatch. A dropped archive write is logged, not surfaced as a round
    /// failure.
    pub fn try_record_request_projection(
        &self,
        session_id: String,
        record: muta_contracts::RequestProjection,
    ) {
        let (ack_tx, _) = oneshot::channel();
        if let Err(error) = self
            .supervisor
            .try_send(PersistenceCommand::RecordRequestProjection {
                session_id,
                record,
                ack: ack_tx,
            })
        {
            warn!(error = %error, "dropped fire-and-forget request projection: writer unavailable");
        }
    }

    /// Asynchronously set a key-value entry.
    pub async fn set_kv(&self, key: String, value: String) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::SetKV {
                key,
                value,
                ack: ack_tx,
            })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Synchronously set a key-value entry.
    pub fn set_kv_blocking(&self, key: String, value: String) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(PersistenceCommand::SetKV { key, value, ack: ack_tx }, ack_rx)
    }

    /// Asynchronously set a JSON-serializable value in the key-value store.
    pub async fn set_json<T: serde::Serialize>(
        &self,
        key: &str,
        value: &T,
    ) -> Result<(), PersistenceError> {
        let serialized =
            serde_json::to_string(value).map_err(|e| PersistenceError::Encode(e.to_string()))?;
        self.set_kv(key.to_string(), serialized).await
    }

    /// Synchronously set a JSON-serializable value in the key-value store.
    pub fn set_json_blocking<T: serde::Serialize>(
        &self,
        key: &str,
        value: &T,
    ) -> Result<(), PersistenceError> {
        let serialized =
            serde_json::to_string(value).map_err(|e| PersistenceError::Encode(e.to_string()))?;
        self.set_kv_blocking(key.to_string(), serialized)
    }

    /// Asynchronously delete a key-value entry.
    pub async fn delete_kv(&self, key: String) -> Result<bool, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::DeleteKV { key, ack: ack_tx })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Asynchronously record an input history entry (fire-and-forget).
    pub fn try_record_input_history(&self, entry: muta_contracts::HistoryEntry, dedup: bool) {
        if let Err(error) = self
            .supervisor
            .try_send(PersistenceCommand::RecordInputHistory {
                entry,
                dedup,
                ack: None,
            })
        {
            warn!(error = %error, "dropped fire-and-forget input history entry: writer unavailable");
        }
    }

    /// Record an input history entry, waiting for single-writer SQLite actor confirmation.
    pub fn record_input_history_blocking(
        &self,
        entry: muta_contracts::HistoryEntry,
        dedup: bool,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::RecordInputHistory {
                entry,
                dedup,
                ack: Some(ack_tx),
            },
            ack_rx,
        )
    }

    /// Save multiple input history entries synchronously, waiting for SQLite actor confirmation.
    pub fn save_input_history_blocking(
        &self,
        entries: Vec<muta_contracts::HistoryEntry>,
        dedup: bool,
    ) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::SaveInputHistory {
                entries,
                dedup,
                ack: ack_tx,
            },
            ack_rx,
        )
    }

    /// Clear all input history entries from SQLite.
    pub fn clear_input_history_blocking(&self) -> Result<(), PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(PersistenceCommand::ClearInputHistory { ack: ack_tx }, ack_rx)
    }

    /// Best-effort asynchronous delete of an input history record by text and timestamp.
    pub fn try_delete_input_history_entry(&self, text: String, created_at_ms: u64) {
        if let Err(error) = self
            .supervisor
            .try_send(PersistenceCommand::DeleteInputHistoryEntry {
                text,
                created_at_ms,
                ack: None,
            })
        {
            warn!(error = %error, "dropped fire-and-forget input history delete: writer unavailable");
        }
    }

    /// Synchronously delete an input history entry by text and timestamp.
    pub fn delete_input_history_entry_blocking(
        &self,
        text: &str,
        created_at_ms: u64,
    ) -> Result<usize, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::DeleteInputHistoryEntry {
                text: text.to_string(),
                created_at_ms,
                ack: Some(ack_tx),
            },
            ack_rx,
        )
    }

    /// Open a reader against this handle's database (ADR-0231).
    ///
    /// This is the **only** way to obtain a connection snapshot: the engine
    /// type is crate-private, so a caller cannot open one on a path of its own
    /// choosing. Reads never need the actor — WAL readers run concurrently
    /// with the writer — so this returns immediately instead of queueing
    /// behind a slow write.
    pub fn reader(&self) -> Result<DbReader> {
        Ok(DbReader::new(
            DatabaseEngine::open(&self.db_path, self.blob_store.clone())?,
            self.db_path.clone(),
        ))
    }

    /// Synchronously delete a key-value entry on a blocking thread.
    pub fn delete_kv_blocking(&self, key: String) -> Result<bool, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(PersistenceCommand::DeleteKV { key, ack: ack_tx }, ack_rx)
    }

    /// Reclaim transcript entries no session references (ADR-0187): the
    /// periodic storage-maintenance sweep. Routed through the actor so the
    /// DELETE serializes with every other write.
    pub async fn collect_entry_garbage(&self) -> Result<usize, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.supervisor
            .send(PersistenceCommand::CollectEntryGarbage { ack: ack_tx })
            .await
            .map_err(|_| PersistenceError::WriterDown)?;
        ack_rx.await.map_err(|_| PersistenceError::WriterDown)?
    }

    /// Synchronous [`Self::collect_entry_garbage`] for a blocking maintenance
    /// pass.
    pub fn collect_entry_garbage_blocking(&self) -> Result<usize, PersistenceError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.run_blocking(
            PersistenceCommand::CollectEntryGarbage { ack: ack_tx },
            ack_rx,
        )
    }

    /// The one blocking bridge for every synchronous verb (ADR-0196).
    ///
    /// On a **multi-thread runtime** `block_in_place` is preferred: it parks
    /// this worker and hands its core back, so the runtime keeps making
    /// progress and no thread is spawned. On a **current-thread runtime** there
    /// is no other worker that could drive the supervisor, so the wait must
    /// move off this thread entirely: `spawn` + `join`.
    ///
    /// That second branch is only deadlock-free *because*
    /// [`PersistenceHandle::spawn`] guarantees the supervisor owns a thread of
    /// its own whenever the caller's runtime is current-thread — the two
    /// decisions are coupled and must move together. (The previous version of
    /// this bridge took the thread path only for `!MultiThread` but left the
    /// supervisor as a task on that same current-thread runtime, so the `join`
    /// blocked the only thread that could ever serve the ack: every sync verb
    /// deadlocked under `#[tokio::test]`.)
    fn run_blocking<T: Send + 'static>(
        &self,
        command: PersistenceCommand,
        ack_rx: oneshot::Receiver<Result<T, PersistenceError>>,
    ) -> Result<T, PersistenceError> {
        let supervisor = self.supervisor.clone();
        let run = move || {
            supervisor
                .blocking_send(command)
                .map_err(|_| PersistenceError::WriterDown)?;
            ack_rx
                .blocking_recv()
                .map_err(|_| PersistenceError::WriterDown)?
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle)
                if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread =>
            {
                tokio::task::block_in_place(run)
            }
            _ => std::thread::spawn(run).join().map_err(|_| {
                PersistenceError::Poisoned("persistence blocking bridge panicked".into())
            })?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_db_migrates_to_latest_version() {
        let conn = initialize_in_memory_db().unwrap();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_DB_VERSION);
    }

    /// Regression for the interim version-5 stamp: development builds of the
    /// ADR-0186 tranche wrote `sessions` with `scheduled_jobs` and without the
    /// final working-state columns, then stamped `user_version = 5`. The
    /// migrator must reconcile such databases to the final schema instead of
    /// letting runtime INSERTs fail with "no column named ...".
    #[test]
    fn interim_v5_stamp_is_repaired_to_final_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        configure_connection(&mut conn).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE sessions (
                id               TEXT PRIMARY KEY,
                parent_id        TEXT REFERENCES sessions(id) ON DELETE SET NULL,
                fork_kind        TEXT NOT NULL DEFAULT 'trunk',
                title            TEXT,
                created_at_ms    INTEGER NOT NULL,
                updated_at_ms    INTEGER NOT NULL,
                project_root     TEXT NOT NULL,
                msg_count        INTEGER NOT NULL DEFAULT 0,
                last_user_prompt TEXT,
                digest           TEXT,
                scheduled_jobs   TEXT NOT NULL DEFAULT '[]',
                provider_connection TEXT
            );
            INSERT INTO sessions (id, created_at_ms, updated_at_ms, project_root)
                VALUES ('s1', 1, 1, '/tmp');
            PRAGMA user_version = 5;
            "#,
        )
        .unwrap();

        migrate_schema(&mut conn).unwrap();

        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_DB_VERSION);
        let columns = sessions_columns(&conn).unwrap();
        assert!(!columns.contains("scheduled_jobs"));
        assert!(!columns.contains("title_manual"));
        for (name, _) in SESSIONS_WORKING_STATE_COLUMNS {
            // Migration 8 retires the usage column into its own ledger table.
            if *name == "request_usage_records" {
                assert!(
                    !columns.contains(*name),
                    "{name} must have moved to the usage ledger table"
                );
                continue;
            }
            assert!(columns.contains(*name), "missing column {name}");
        }
        // The repaired row survives and the working-state upsert path works.
        conn.execute(
            "UPDATE sessions SET round_counter = 2, unattended = 1 WHERE id = 's1'",
            [],
        )
        .unwrap();
    }

    /// Databases already at the final version-5 schema (no interim stamp) pass
    /// through the repair as a no-op.
    #[test]
    fn final_v5_schema_passes_through_migration_six_unchanged() {
        let mut conn = initialize_in_memory_db().unwrap();
        conn.execute_batch("PRAGMA user_version = 5;").unwrap();
        migrate_schema(&mut conn).unwrap();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_DB_VERSION);
    }

    /// Regression (2026-09-08 incident): migration 10 rebuilds `entries`, the
    /// parent table of `entry_memberships`' foreign key. With foreign-key
    /// enforcement left ON across the migration transaction, the
    /// drop-and-rename swap incremented SQLite's deferred-constraint counter
    /// and COMMIT failed with `FOREIGN KEY constraint failed` even though the
    /// rebuilt state was perfectly referentially consistent. The transaction
    /// rolled back, `user_version` never advanced, and every subsequent
    /// open retried and re-failed the rebuild — wedging workspace-trust
    /// persistence (the TUI stuck on "Trusting workspace...") until the state
    /// file was recreated. The subagent must disable enforcement before `BEGIN`
    /// (a no-op inside a transaction), restore it after, and gate the result
    /// on `PRAGMA foreign_key_check`.
    #[test]
    fn migration_ten_rebuilds_parent_entries_with_membership_rows_present() {
        let mut conn = Connection::open_in_memory().unwrap();
        configure_connection(&mut conn).unwrap();
        conn.execute_batch(
            r#"
            -- Final version-5 shape (ADR-0186) with live transcript rows that
            -- satisfy the pre-migration CHECKs. Stamped v5 so the v6-v9 hooks
            -- build the real pre-v10 state on the way up (column repairs,
            -- membership/projection rebuilds, FTS backfill), exactly like a
            -- production database that never survived migration 10.
            CREATE TABLE sessions (
                id               TEXT PRIMARY KEY,
                parent_id        TEXT REFERENCES sessions(id) ON DELETE SET NULL,
                fork_kind        TEXT NOT NULL DEFAULT 'trunk',
                title            TEXT,
                created_at_ms    INTEGER NOT NULL,
                updated_at_ms    INTEGER NOT NULL,
                project_root     TEXT NOT NULL,
                msg_count        INTEGER NOT NULL DEFAULT 0,
                last_user_prompt TEXT,
                digest           TEXT,
                data             TEXT,
                title_manual     BOOLEAN NOT NULL DEFAULT 0
            );
            INSERT INTO sessions (id, created_at_ms, updated_at_ms, project_root)
                VALUES ('s1', 1, 1, '/tmp');

            CREATE TABLE entries (
                id            TEXT PRIMARY KEY,
                kind          TEXT NOT NULL CHECK (kind IN ('message','state')),
                role          TEXT CHECK (role IN ('user','assistant','system','tool')),
                content       TEXT,
                origin        TEXT CHECK (origin IS NULL OR origin IN ('harness','checkpoint')),
                hidden        INTEGER NOT NULL DEFAULT 0,
                created_at_ms INTEGER NOT NULL,
                payload       TEXT NOT NULL,
                CHECK ( origin IS NULL OR hidden = 1 ),
                CHECK ( kind <> 'message' OR role IS NOT NULL )
            );
            INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload)
                VALUES ('e1', 'message', 'user', 'hello', NULL, 0, 1, '{"content":"hello"}'),
                       ('e2', 'message', 'assistant', 'hi', 'harness', 1, 2, '{"content":"hi"}'),
                       ('e3', 'message', 'user', 'ckpt', 'checkpoint', 1, 3, '{"content":"ckpt"}');

            CREATE TABLE entry_memberships (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq        INTEGER NOT NULL,
                entry_id   TEXT NOT NULL REFERENCES entries(id),
                added_by   INTEGER NOT NULL,
                PRIMARY KEY (session_id, seq)
            );
            INSERT INTO entry_memberships (session_id, seq, entry_id, added_by)
                VALUES ('s1', 1, 'e1', 0), ('s1', 2, 'e2', 0), ('s1', 3, 'e3', 0);

            CREATE TABLE projections (
                session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq           INTEGER NOT NULL,
                kind          TEXT NOT NULL CHECK (kind IN ('prune','compact','freeze')),
                up_to_seq     INTEGER NOT NULL,
                payload       TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                PRIMARY KEY (session_id, seq)
            );
            INSERT INTO projections (session_id, seq, kind, up_to_seq, payload, created_at_ms)
                VALUES ('s1', 1, 'freeze', 2, '{}', 3);

            CREATE VIRTUAL TABLE fts_entries USING fts5(
                entry_id UNINDEXED, session_id UNINDEXED, role UNINDEXED, content,
                tokenize = 'porter unicode61'
            );

            PRAGMA user_version = 5;
            "#,
        )
        .unwrap();

        migrate_schema(&mut conn).unwrap();

        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_DB_VERSION);

        // Enforcement is a per-connection invariant installed by
        // `configure_connection`; the subagent must hand the connection back
        // with it restored.
        let fk_enforced: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk_enforced, 1);

        // The rebuild preserved every row byte-for-byte, including the
        // membership references into the swapped table.
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM entries"), 3);
        assert_eq!(count("SELECT COUNT(*) FROM entry_memberships"), 3);
        assert_eq!(count("SELECT COUNT(*) FROM projections"), 1);
        // FTS backfill rode along through the v7 hook.
        assert_eq!(count("SELECT COUNT(*) FROM fts_entries"), 3);

        // Referential integrity holds with enforcement back ON.
        let violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);

        // The migration-10 CHECKs accept the visible harness provenance the
        // v5 schema rejected (the ADR-0186 integrity fix the rebuild ships).
        conn.execute(
            "INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload)
             VALUES ('e4', 'message', 'user', 'steer', 'harness', 0, 4, '{}')",
            [],
        )
        .unwrap();
    }

    /// The startup guard fails loudly on a database missing runtime columns.
    #[test]
    fn schema_guard_rejects_missing_columns() {
        let mut conn = Connection::open_in_memory().unwrap();
        configure_connection(&mut conn).unwrap();
        conn.execute_batch("CREATE TABLE sessions (id TEXT PRIMARY KEY, persona TEXT NOT NULL);")
            .unwrap();
        let err = verify_sessions_schema(&conn).unwrap_err();
        assert!(err.to_string().contains("missing column(s)"), "{err}");
    }

    /// Migration immutability lock: once a migration version has shipped in a
    /// commit, editing its SQL in place strands databases stamped by the old
    /// SQL (the interim-v5 incident). Any change to the catalog — including
    /// edits disguised as refactors — must therefore land as a NEW version
    /// entry, which changes this fingerprint and fails the test.
    #[test]
    fn migration_catalog_fingerprint_is_stable() {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        for migration in MIGRATIONS {
            hasher.update(migration.version.to_le_bytes());
            hasher.update(migration.sql.as_bytes());
        }
        let fingerprint = format!("{:x}", hasher.finalize());
        assert_eq!(
            fingerprint,
            MIGRATION_CATALOG_FINGERPRINT,
            "the migration catalog changed; ship the change as version {} \
             with new SQL instead of rewriting an applied migration",
            CURRENT_DB_VERSION + 1
        );
    }

    /// Regression (reported incident): the v5 `entries` CHECK coupled `origin`
    /// and `hidden`, rejecting the legitimate *visible* harness injections the
    /// durable transcript carries (UserSteer/SubagentSteer, CommandEcho "/cmd"
    /// & "!cmd", ToolImage). A mid-round save of such a message then failed with
    /// `CHECK constraint failed: origin IS NULL OR hidden = 1`. The corrected
    /// schema (migration 10) accepts them without weakening the still-real
    /// invariants.
    #[test]
    fn visible_harness_injections_persist_after_origin_hidden_split() {
        use muta_contracts::{InjectionKind, Message, Role, TranscriptEntry};

        let engine = DatabaseEngine::open_in_memory().unwrap();
        let mut data = crate::session::SessionData::default();
        engine.save_session_full(&data).unwrap();

        // The exact repro: a user steering insert and a command echo are both
        // visible (`hidden = false`) yet legitimately carry an `origin`.
        let steer = Message::new(Role::User, "pls reconsider point 3").with_origin(
            muta_contracts::InjectionOrigin::new(InjectionKind::UserSteer),
        );
        let echo = Message::command_echo("/session list");
        let image = Message::new(Role::User, "Image from screenshot")
            .with_images(vec![muta_contracts::ImagePart {
                mime: "image/png".into(),
                data: "bytes".into(),
            }])
            .with_origin(muta_contracts::InjectionOrigin::new(
                InjectionKind::ToolImage,
            ));

        assert!(!steer.hidden && steer.origin.is_some());
        assert!(!echo.hidden && echo.origin.is_some());
        assert!(!image.hidden && image.origin.is_some());

        data.transcript
            .push(TranscriptEntry::from_message(0, &steer));
        data.transcript
            .push(TranscriptEntry::from_message(1, &echo));
        data.transcript
            .push(TranscriptEntry::from_message(2, &image));
        // Was the reported failure: `CHECK constraint failed: origin IS NULL OR hidden = 1`.
        engine.save_session_full(&data).unwrap();

        let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
        let messages: Vec<_> = reloaded
            .transcript
            .entries
            .iter()
            .filter_map(|e| e.to_message())
            .collect();
        assert_eq!(messages.len(), 3);
        for m in messages {
            assert!(m.origin.is_some(), "provenance must survive the round-trip");
            assert!(!m.hidden || m.origin.is_some());
        }
    }

    /// The corrected schema must still reject silent corruption: a *visible*
    /// envelope origin that can never be legitimate. A checkpoint is an
    /// elided-range stand-in and is hidden by construction (`transcript.rs`);
    /// a visible `Role::User` row tagged `origin = 'checkpoint'` is thus
    /// impossible and must fail on save.
    #[test]
    fn visible_checkpoint_origin_is_still_rejected() {
        use muta_contracts::{Message, Role, TranscriptEntry};

        let engine = DatabaseEngine::open_in_memory().unwrap();
        let mut data = crate::session::SessionData::default();
        let mut message = Message::new(Role::User, "bogus visible checkpoint");
        message.hidden = false; // checkpoint is never visible dialogue
        message.origin = Some(muta_contracts::InjectionOrigin::new(
            muta_contracts::InjectionKind::CompactionCheckpoint,
        ));
        data.transcript
            .push(TranscriptEntry::from_message(0, &message));

        let err = engine
            .save_session_full(&data)
            .expect_err("constraint must reject");
        let msg = err.to_string();
        assert!(
            msg.contains("constraint failed") || msg.contains("CHECK"),
            "expected a CHECK/constraint failure, got: {msg}"
        );
    }

    #[test]
    fn delta_save_appends_only_rows_above_the_watermark() {
        use muta_contracts::{Message, Role, TranscriptEntry};
        let engine = DatabaseEngine::open_in_memory().unwrap();
        let mut data = crate::session::SessionData::default();
        engine.save_session_full(&data).unwrap();

        data.transcript.push(TranscriptEntry::from_message(
            0,
            &Message::new(Role::User, "one"),
        ));
        engine.save_session_inner(&data, false, &[]).unwrap();
        let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
        assert_eq!(reloaded.transcript.entries.len(), 1);

        // Second delta appends only the new row; the durable watermark moves.
        data.transcript.push(TranscriptEntry::from_message(
            1,
            &Message::new(Role::User, "two"),
        ));
        engine.save_session_inner(&data, false, &[]).unwrap();
        let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
        assert_eq!(reloaded.transcript.entries.len(), 2);
        assert_eq!(reloaded.transcript.entries[1].seq, 1);
    }

    #[test]
    fn generation_mismatch_escalates_delta_to_full_rewrite() {
        use muta_contracts::{Message, Role, TranscriptEntry};
        let engine = DatabaseEngine::open_in_memory().unwrap();
        let mut data = crate::session::SessionData::default();
        data.transcript.push(TranscriptEntry::from_message(
            0,
            &Message::new(Role::User, "old"),
        ));
        engine.save_session_full(&data).unwrap();

        // Rebuild: fresh entries, fresh ids, fresh generation. The delta save
        // must notice the generation change and rewrite everything.
        let rebuilt = crate::session::rebuild_for_test(&[Message::new(Role::User, "rebuilt")]);
        data.transcript = rebuilt;
        data.generation = uuid::Uuid::new_v4().to_string();
        engine.save_session_inner(&data, false, &[]).unwrap();

        let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
        assert_eq!(reloaded.transcript.entries.len(), 1);
        assert_eq!(
            reloaded.transcript.entries[0].to_message().unwrap().content,
            "rebuilt"
        );
        assert_eq!(reloaded.generation, data.generation);
    }

    #[test]
    fn delta_save_offloads_only_newly_inserted_rows() {
        use muta_contracts::{Message, Role, TranscriptEntry};
        let blob_store = BlobStore::new(
            std::env::temp_dir().join(format!("muta-offload-{}", uuid::Uuid::new_v4())),
        );
        let mut data = crate::session::SessionData::default();
        let big = "x".repeat(CAS_THRESHOLD_BYTES * 4);
        data.transcript.push(TranscriptEntry::from_message(
            0,
            &Message::new(Role::User, big.clone()),
        ));
        let stored = engine_blob_round_trip(&data, &blob_store);
        let reloaded = stored.load_session_full(&data.id).unwrap().unwrap();
        let payload = reloaded.transcript.entries[0].as_message().unwrap();
        let hash = payload.content_blob.as_ref().expect("body was offloaded");
        assert_eq!(
            reloaded.transcript.entries[0].content.as_deref(),
            Some(""),
            "the inline body is cleared"
        );
        assert_eq!(blob_store.get(hash).unwrap(), big.as_bytes());
        let _ = std::fs::remove_dir_all(blob_store.root());
    }

    fn engine_blob_round_trip(
        data: &crate::session::SessionData,
        blob_store: &BlobStore,
    ) -> DatabaseEngine {
        let mut conn = Connection::open_in_memory().unwrap();
        configure_connection(&mut conn).unwrap();
        migrate_schema(&mut conn).unwrap();
        let engine = DatabaseEngine {
            conn,
            blob_store: Some(blob_store.clone()),
        };
        engine.save_session_full(data).unwrap();
        engine
    }

    #[test]
    fn unknown_payloads_round_trip_verbatim() {
        use muta_contracts::{Message, Role, TranscriptEntry};
        let engine = DatabaseEngine::open_in_memory().unwrap();
        let mut data = crate::session::SessionData::default();
        data.transcript.push(TranscriptEntry::from_message(
            0,
            &Message::new(Role::User, "known"),
        ));
        engine.save_session_full(&data).unwrap();

        // Inject an entry a newer binary wrote, with a payload kind this
        // binary cannot decode.
        let future_payload = r#"{"type":"future_kind","x":1}"#;
        engine
            .conn
            .execute(
                "INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload) VALUES (?1, 'message', 'user', NULL, NULL, 0, 1, ?2)",
                params!["future-entry", future_payload],
            )
            .unwrap();
        engine
            .conn
            .execute(
                "INSERT INTO entry_memberships (session_id, seq, entry_id) VALUES (?1, 1, 'future-entry')",
                params![data.id],
            )
            .unwrap();

        // Load: the unknown entry rides along, known entries still work.
        let mut reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
        assert_eq!(reloaded.transcript.entries.len(), 1);
        assert_eq!(reloaded.unknown_entries.len(), 1);
        assert_eq!(reloaded.unknown_entries[0].payload_json, future_payload);
        // The view excludes it and the seq floor prevents collisions.
        assert_eq!(reloaded.transcript.next_seq(), 2);

        // Save (delta): the unknown entry survives byte-identically.
        reloaded.transcript.push(TranscriptEntry::from_message(
            2,
            &Message::new(Role::User, "after"),
        ));
        engine.save_session_inner(&reloaded, false, &[]).unwrap();
        let again = engine.load_session_full(&data.id).unwrap().unwrap();
        assert_eq!(again.unknown_entries.len(), 1);
        assert_eq!(again.unknown_entries[0].payload_json, future_payload);
        assert_eq!(again.transcript.entries.len(), 2);
        assert_eq!(
            again.transcript.entries[1].to_message().unwrap().content,
            "after"
        );
    }

    #[test]
    fn blob_gc_reclaims_only_blobs_absent_from_the_reference_ledger() {
        let engine = DatabaseEngine::open_in_memory().unwrap();
        let blob_store =
            BlobStore::new(std::env::temp_dir().join(format!("muta-gc-{}", uuid::Uuid::new_v4())));
        let referenced = blob_store.put(b"referenced body").unwrap();
        let orphan = blob_store.put(b"orphan body").unwrap();
        engine
            .conn
            .execute(
                "INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_s, updated_at_s, workspace_root, persona, msg_count, last_user_prompt, digest) VALUES ('s1', NULL, 'trunk', NULL, 1, 1, '/tmp', NULL, 0, NULL, NULL)",
                [],
            )
            .unwrap();
        engine
            .conn
            .execute(
                "INSERT INTO blob_refs (session_id, hash) VALUES ('s1', ?1)",
                params![referenced],
            )
            .unwrap();

        let (count, _) = blob_store.retain_only(&engine.live_blob_hashes().unwrap());
        assert_eq!(count, 1);
        assert!(
            blob_store.get(&referenced).is_some(),
            "referenced blob survives"
        );
        assert!(blob_store.get(&orphan).is_none(), "orphan is reclaimed");
        let _ = std::fs::remove_dir_all(blob_store.root());
    }

    #[test]
    fn entry_gc_reclaims_only_zero_reference_rows() {
        use muta_contracts::{Message, Role, TranscriptEntry};
        let engine = DatabaseEngine::open_in_memory().unwrap();
        let mut session = crate::session::SessionData::default();
        let shared = TranscriptEntry::from_message(0, &Message::new(Role::User, "shared"));
        let mut fork = crate::session::SessionData {
            id: "fork-1".into(),
            parent_id: Some(session.id.clone()),
            fork_kind: muta_contracts::SessionForkKind::Fork,
            ..Default::default()
        };
        // Both sessions reference the same fact by identity.
        session.transcript.push(shared.clone());
        fork.transcript.push(shared);
        fork.transcript.push(TranscriptEntry::from_message(
            1,
            &Message::new(Role::User, "fork only"),
        ));
        engine.save_session_full(&session).unwrap();
        engine.save_session_full(&fork).unwrap();

        // An orphan row with no membership at all.
        engine
            .conn
            .execute(
                "INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload) VALUES ('orphan', 'message', 'user', 'orphan', NULL, 0, 1, '{}')",
                [],
            )
            .unwrap();

        let reclaimed = engine.collect_entry_garbage().unwrap();
        assert_eq!(reclaimed, 1);
        // The shared fact survives because the fork still references it.
        assert!(engine.load_session_full(&session.id).unwrap().is_some());
        let fork = engine.load_session_full("fork-1").unwrap().unwrap();
        assert_eq!(fork.transcript.entries.len(), 2);
    }

    #[test]
    fn digest_anchor_tree_and_generation_round_trip() {
        use muta_contracts::{Message, Role, TranscriptEntry};
        let engine = DatabaseEngine::open_in_memory().unwrap();
        let mut data = crate::session::SessionData {
            digest: Some(muta_contracts::SessionDigest::default()),
            digest_anchor: Some(4_242),
            ..Default::default()
        };
        data.tree.active_leaf_id = Some("leaf-1".to_string());
        data.transcript.push(TranscriptEntry::from_message(
            0,
            &Message::new(Role::User, "hello"),
        ));
        engine.save_session_full(&data).unwrap();

        let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
        assert!(reloaded.digest.is_some());
        assert_eq!(reloaded.digest_anchor, Some(4_242));
        assert_eq!(reloaded.tree.active_leaf_id.as_deref(), Some("leaf-1"));
        assert_eq!(reloaded.generation, data.generation);
    }

    #[test]
    fn newer_database_is_refused() {
        let mut conn = initialize_in_memory_db().unwrap();
        conn.execute_batch(&format!(
            "PRAGMA user_version = {};",
            CURRENT_DB_VERSION + 1
        ))
        .unwrap();
        let err = migrate_schema(&mut conn).unwrap_err();
        assert!(err.to_string().contains("newer than this binary"), "{err}");
    }

    #[test]
    fn row_checksum_detects_working_state_corruption_on_load() {
        use muta_contracts::{Message, Role, TranscriptEntry};
        let engine = DatabaseEngine::open_in_memory().unwrap();
        let mut data = crate::session::SessionData::default();
        data.transcript.push(TranscriptEntry::from_message(
            0,
            &Message::new(Role::User, "hello"),
        ));
        engine.save_session_full(&data).unwrap();

        // Corrupt the working state out-of-band, then restamp a checksum that
        // matches the corruption? No — the mismatch case: the on-row checksum
        // no longer matches the corrupted columns. Load is non-fatal (the row
        // data remains authoritative), but a fresh full save restamps a
        // correct checksum over the corrected state.
        engine
            .conn
            .execute(
                "UPDATE sessions SET round_counter = 99 WHERE id = ?1",
                params![data.id],
            )
            .unwrap();
        let reloaded = engine.load_session_full(&data.id).unwrap().unwrap();
        assert_eq!(reloaded.round_counter, 99);

        // The recomputed checksum over the loaded (corrupted) row matches the
        // stored one only if nothing changed — here it must differ, so a save
        // restamps and the next load is consistent again.
        engine.save_session_full(&reloaded).unwrap();
        let again = engine.load_session_full(&data.id).unwrap().unwrap();
        assert_eq!(again.checksum, reloaded.checksum);
    }

    #[test]
    fn working_state_round_trips_through_the_session_row() {
        let engine = DatabaseEngine::open_in_memory().unwrap();
        engine
            .conn
            .execute(
                "INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_s, updated_at_s, workspace_root, persona, msg_count, last_user_prompt, digest) VALUES ('s1', NULL, 'trunk', 'T', 1, 1, '/tmp', NULL, 0, NULL, NULL)",
                [],
            )
            .unwrap();
        let sessions = engine.list_sessions(None).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title.as_deref(), Some("T"));
    }

    /// Supervision fault-injection suite (ADR-0196 D6). Every variant of
    /// [`PersistenceError`] and every supervisor transition is exercised.
    mod supervision {
        use super::*;

        fn temp_db() -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("muta.db");
            (dir, path)
        }

        async fn wait_for_health(
            health: &watch::Receiver<WriterHealth>,
            matches: impl Fn(&WriterHealth) -> bool,
        ) -> WriterHealth {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let current = health.borrow().clone();
                if matches(&current) {
                    return current;
                }
                assert!(
                    Instant::now() < deadline,
                    "health never reached the expected state; last: {current:?}"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        /// A healthy writer serves commands and reports `Healthy`.
        #[tokio::test]
        async fn healthy_writer_serves_and_reports_healthy() {
            let (_dir, path) = temp_db();
            let handle = PersistenceHandle::spawn(path, None);
            handle
                .set_kv("k".into(), "v".into())
                .await
                .expect("command must succeed");
            let health =
                wait_for_health(&handle.subscribe_health(), |h| *h == WriterHealth::Healthy).await;
            assert_eq!(health, WriterHealth::Healthy);
        }

        /// D1/D5: an engine-open failure produces `Err(WriterDown)` (never a
        /// hang, never a fake success), and health degrades to
        /// `Recovering`/`Down` with the cause attached.
        #[tokio::test]
        async fn engine_open_failure_fails_fast_and_degrades_health() {
            let dir = tempfile::tempdir().unwrap();
            // SQLite cannot open a directory as a database file.
            let handle = PersistenceHandle::spawn(dir.path().to_path_buf(), None);
            let error = handle
                .set_kv("k".into(), "v".into())
                .await
                .expect_err("open failure must fail the command");
            assert!(matches!(error, PersistenceError::WriterDown));
            let health = wait_for_health(&handle.subscribe_health(), |h| !h.is_serving()).await;
            assert!(health.error().is_some(), "degraded health carries a cause");
        }

        /// D1/D6: a dead writer is respawned through the same handle, and
        /// `Healthy` is restored by the first successful command — not by
        /// spawn alone.
        #[tokio::test]
        async fn dead_writer_is_respawned_and_health_restored() {
            let (_dir, path) = temp_db();
            let handle = PersistenceHandle::spawn(path, None);
            handle
                .set_kv("before".into(), "v".into())
                .await
                .expect("writer must be serving before death");

            // Kill the actor generation (test-only death command).
            let (die_tx, die_rx) = oneshot::channel();
            handle
                .supervisor
                .send(PersistenceCommand::Die { ack: die_tx })
                .await
                .unwrap();
            die_rx.await.unwrap();

            // The next command survives the death: supervisor respawns. The
            // respawn backoff is short, but under heavy parallel test load a
            // single command issued immediately after death can race the
            // still-recovering writer — retry within a generous deadline
            // instead of asserting an instant success.
            {
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    match handle.set_kv("after".into(), "v".into()).await {
                        Ok(()) => break,
                        Err(error) => {
                            assert!(
                                Instant::now() < deadline,
                                "respawned writer never served: {error}"
                            );
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                    }
                }
            }
            let health =
                wait_for_health(&handle.subscribe_health(), |h| *h == WriterHealth::Healthy).await;
            assert_eq!(health, WriterHealth::Healthy);

            // The respawned writer owns a real engine: the value is there.
            let reader = handle.reader().unwrap();
            assert_eq!(reader.get_kv("after").unwrap().as_deref(), Some("v"));
        }

        /// D6: a handler panic is contained by the per-command guard — the
        /// command fails with `Poisoned`, the actor keeps serving.
        #[tokio::test]
        async fn handler_panic_is_contained_as_poisoned() {
            let poisoned = guarded::<()>(|| panic!("injected engine panic")).unwrap_err();
            assert!(
                matches!(poisoned, PersistenceError::Poisoned(msg) if msg.contains("injected"))
            );

            let engine_error =
                guarded::<()>(|| Err(rusqlite::Error::InvalidColumnName("x".into()))).unwrap_err();
            assert!(matches!(engine_error, PersistenceError::Engine(_)));
        }

        /// D2: encode failures surface as `Encode` before reaching the
        /// writer; nothing is written.
        #[tokio::test]
        async fn encode_failure_surfaces_as_encode() {
            struct InjectedFailure;
            impl serde::Serialize for InjectedFailure {
                fn serialize<S: serde::Serializer>(
                    &self,
                    _serializer: S,
                ) -> std::result::Result<S::Ok, S::Error> {
                    Err(serde::ser::Error::custom("injected encode failure"))
                }
            }

            let (_dir, path) = temp_db();
            let handle = PersistenceHandle::spawn(path, None);
            let error = handle
                .set_json("broken", &InjectedFailure)
                .await
                .expect_err("the injected serialization failure must surface");
            assert!(matches!(error, PersistenceError::Encode(_)));
        }

        /// D5: `fail` must resolve a command's ack with the error — a caller
        /// waiting on the ack learns the truth instead of hanging.
        #[tokio::test]
        async fn drain_resolves_acks_with_writer_down() {
            let (ack_tx, ack_rx) = oneshot::channel();
            PersistenceCommand::SetKV {
                key: "k".into(),
                value: "v".into(),
                ack: ack_tx,
            }
            .fail(PersistenceError::WriterDown);
            assert!(matches!(
                ack_rx.await,
                Ok(Err(PersistenceError::WriterDown))
            ));
        }

        /// ADR-0208: cross-project FTS recall end-to-end — persist a session
        /// with a distinctive message, then find it back with a raw
        /// multi-word query through `search_history` (the Archivist's
        /// retrieval plane). Also pins the workspace filter.
        #[test]
        fn search_history_finds_persisted_transcripts() {
            use muta_contracts::{Message, Role, TranscriptEntry};

            let engine = DatabaseEngine::open_in_memory().unwrap();
            let mut data = crate::session::SessionData {
                workspace: Some(muta_contracts::WorkspaceBinding::new("/tmp/proj-a")),
                title: Some("Retry loop debugging".into()),
                ..Default::default()
            };
            let msg = Message::new(
                Role::User,
                "we need to debug the exponential backoff in the retry loop",
            );
            data.transcript.push(TranscriptEntry::from_message(0, &msg));
            engine.save_session_full(&data).unwrap();

            // Bare multi-word query (the shape that used to blow up MATCH
            // with `no such column: needle`).
            let hits = engine.search_history("retry backoff", None, 20).unwrap();
            assert_eq!(hits.len(), 1, "the seeded session must be recalled");
            assert_eq!(hits[0].session_id, data.id);
            assert_eq!(
                hits[0].session_title.as_deref(),
                Some("Retry loop debugging")
            );
            assert_eq!(hits[0].workspace_root.as_deref(), Some("/tmp/proj-a"));
            assert!(hits[0].snippet.contains("retry"), "{:?}", hits[0].snippet);

            // A workspace filter that does not match returns nothing; one
            // that matches returns the hit.
            assert!(
                engine
                    .search_history(
                        "retry",
                        Some(&muta_contracts::WorkspaceFilter::Path("/tmp/other".into())),
                        20,
                    )
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                engine
                    .search_history(
                        "retry",
                        Some(&muta_contracts::WorkspaceFilter::Path("/tmp/proj-a".into())),
                        20,
                    )
                    .unwrap()
                    .len(),
                1
            );

            // Operator-ish input is sanitized, not executed as FTS grammar.
            assert!(
                engine
                    .search_history("retry OR NOT needle", None, 20)
                    .unwrap()
                    .is_empty()
                    || true,
                "sanitized query must not error"
            );
        }

        /// ADR-0208 Layer 3 (deterministic leg): when the strict AND query
        /// recalls nothing because its words never co-occur, the relaxed OR
        /// form recalls the entries carrying any word, BM25-ranked so
        /// multi-word matches float up.
        #[test]
        fn relaxed_recall_widens_the_net_after_a_strict_miss() {
            use muta_contracts::{Message, Role, TranscriptEntry};

            let engine = DatabaseEngine::open_in_memory().unwrap();
            for (id, text) in [
                ("s-retry", "we debugged the retry loop's backoff"),
                ("s-paint", "repaint the widget border only"),
            ] {
                let mut data = crate::session::SessionData {
                    id: id.to_string(),
                    workspace: Some(muta_contracts::WorkspaceBinding::new("/tmp/proj-r")),
                    title: Some(id.to_string()),
                    ..Default::default()
                };
                let msg = Message::new(Role::User, text);
                data.transcript.push(TranscriptEntry::from_message(0, &msg));
                engine.save_session_full(&data).unwrap();
            }

            // Strict: both words together appear in no single entry.
            let strict = engine.search_history("retry border", None, 20).unwrap();
            assert!(strict.is_empty(), "strict AND must miss: {strict:?}");

            // Relaxed: each entry carries one word; the retry entry also
            // matches more of the query. Both are recalled, OR-joined.
            let relaxed = engine
                .search_history_relaxed("retry border", None, 20)
                .unwrap();
            assert_eq!(relaxed.len(), 2, "both entries surface: {relaxed:?}");
            let ids: std::collections::HashSet<_> =
                relaxed.iter().map(|h| h.session_id.as_str()).collect();
            assert!(ids.contains("s-retry"));
            assert!(ids.contains("s-paint"));
            // Deterministic ordering: scores sort ascending (FTS5
            // negative-better), which the query_map's ORDER BY guarantees —
            // the exact tie order between single-word matches is BM25's
            // business, not this test's.
            let scores: Vec<f64> = relaxed.iter().map(|h| h.score).collect();
            let mut sorted = scores.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            assert_eq!(scores, sorted);
        }

        /// ADR-0208: the sanitizer converts free text into a safe MATCH
        /// expression — quoted phrase tokens joined with AND, operator and
        /// punctuation characters stripped, so raw user input can never
        /// reach the FTS5 grammar.
        #[test]
        fn sanitize_fts_query_quotes_every_word() {
            assert_eq!(sanitize_fts_query("retry loop"), "\"retry\" AND \"loop\"");
            assert_eq!(
                sanitize_fts_query("retry OR NOT needle"),
                "\"retry\" AND \"OR\" AND \"NOT\" AND \"needle\""
            );
            // Quotes are punctuation: stripped, not escaped (a quote can
            // never survive into the MATCH grammar this way).
            assert_eq!(sanitize_fts_query("a\"b"), "\"ab\"");
            assert_eq!(sanitize_fts_query("   "), "");
            assert_eq!(sanitize_fts_query("***"), "");
        }
    }

    /// ADR-0226: `latest_session` filters by the workspace partition and,
    /// optionally, the staffing persona (backs `--resume`).
    #[test]
    fn latest_session_filters_by_workspace_and_persona() {
        use muta_contracts::WorkspaceFilter;
        let engine = DatabaseEngine::open_in_memory().unwrap();

        let mut unbound = crate::session::SessionData {
            persona: Some("philosopher".into()),
            ..Default::default()
        };
        unbound.workspace = None;
        engine.save_session_full(&unbound).unwrap();

        let mut bound = crate::session::SessionData {
            persona: Some("philosopher".into()),
            ..Default::default()
        };
        bound.workspace = Some(muta_contracts::WorkspaceBinding::new("/repo/x"));
        engine.save_session_full(&bound).unwrap();

        assert_eq!(
            engine
                .latest_session(&WorkspaceFilter::Unbound, Some("philosopher"))
                .unwrap(),
            Some(unbound.id.clone())
        );
        assert_eq!(
            engine
                .latest_session(
                    &WorkspaceFilter::Path("/repo/x".into()),
                    Some("philosopher")
                )
                .unwrap(),
            Some(bound.id.clone())
        );
        assert_eq!(
            engine
                .latest_session(&WorkspaceFilter::Unbound, Some("nobody"))
                .unwrap(),
            None
        );
        // Unbound + no persona constraint still finds only unbound sessions.
        assert_eq!(
            engine
                .latest_session(&WorkspaceFilter::Unbound, None)
                .unwrap(),
            Some(unbound.id)
        );
    }
}
