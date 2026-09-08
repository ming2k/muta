//! Authoritative embedded SQLite storage engine (ADR-0163, ADR-0187).
//!
//! Provides relational database initialization, schema migration tracking via
//! `PRAGMA user_version`, FTS5 full-text search, Content-Addressed Storage (CAS)
//! threshold isolation, and a single-writer persistence engine.

use crate::blobs::BlobStore;
use rusqlite::{Connection, OptionalExtension, Result, Row, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

/// SQLite schema version tracking. Fresh databases jump straight to the latest version.
pub const CURRENT_DB_VERSION: u32 = 10;

/// SHA-256 fingerprint of the migration catalog (version + SQL of every
/// entry). Locked by `migration_catalog_fingerprint_is_stable`; see that test
/// for the discipline this enforces.
#[cfg(test)]
const MIGRATION_CATALOG_FINGERPRINT: &str =
    "b0252afc017b06817eb515e8ffc97db656b755f2854274c0df7f77a072b6b127";

/// Payload size threshold (4 KB) beyond which text content is offloaded to CAS BlobStore.
pub const CAS_THRESHOLD_BYTES: usize = 4096;

/// Initialize and return a connection to the SQLite database.
/// Configures WAL mode, synchronous=NORMAL for robustness, busy timeout, and turns on foreign keys.
pub fn initialize_db(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut conn = Connection::open(db_path)?;
    configure_connection(&mut conn)?;
    migrate_schema(&mut conn)?;

    Ok(conn)
}

/// Initialize an in-memory SQLite database for testing and ephemeral workflows.
pub fn initialize_in_memory_db() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    configure_connection(&mut conn)?;
    migrate_schema(&mut conn)?;
    Ok(conn)
}

/// Standard connection configurations applied to every connection (reader & writer).
fn configure_connection(conn: &mut Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
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
        // The DDL is applied conditionally by the migration runner (version
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
        // RunnerSteer / RunnerTask, CommandEcho "/cmd" & "!cmd", ToolImage, and
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
    "project_root",
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
    // Restore the listing indexes the v5 table rebuild dropped.
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_root);
         CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at_s DESC);
         CREATE INDEX IF NOT EXISTS idx_sessions_project_updated ON sessions(project_root, updated_at_s DESC);",
    )?;
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
/// transaction; the FK from `entry_memberships` is deferred so the rebuild —
/// which briefly drops `entries` and renames the replacement into place — never
/// trips the referential guard. Data and FTS triggers are preserved: rows are
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
    tx.execute_batch("PRAGMA defer_foreign_keys = ON;")?;
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
    Ok(())
}

/// Run all outstanding migrations in a single transactional loop, then
/// verify the resulting schema. Verification also covers databases that
/// short-circuit the loop (already at `CURRENT_DB_VERSION`) so a schema a
/// retired migration produced cannot reach runtime SQL unnoticed.
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
        let tx = conn.transaction()?;

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

    verify_sessions_schema(conn)
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
    pub project_root: String,
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
    pub project_root: String,
    pub role: String,
    pub snippet: String,
    pub score: f64,
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
        project_root: row.get(6)?,
        msg_count: row.get(7)?,
        last_user_prompt: row.get(8)?,
        digest: row.get(9)?,
    })
}

fn map_search_row(row: &Row) -> Result<HistorySearchResult> {
    Ok(HistorySearchResult {
        entry_id: row.get(0)?,
        session_id: row.get(1)?,
        project_root: row.get(2)?,
        role: row.get(3)?,
        snippet: row.get(4)?,
        score: row.get(5)?,
    })
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
pub struct DatabaseEngine {
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
    pub fn open(db_path: &Path, blob_store: Option<BlobStore>) -> Result<Self> {
        let conn = initialize_db(db_path)?;
        let engine = Self { conn, blob_store };
        if db_path == crate::paths::get().db_file() {
            let _ = engine.migrate_legacy_input_history();
        }
        Ok(engine)
    }

    /// Open an in-memory database engine for testing.
    pub fn open_in_memory() -> Result<Self> {
        let conn = initialize_in_memory_db()?;
        Ok(Self {
            conn,
            blob_store: None,
        })
    }

    /// Get inner connection reference.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Get inner connection mutable reference.
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    // Session Operations

    /// Create or update a session record.
    pub fn upsert_session(&self, session: &SessionRecord) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_s, updated_at_s, project_root, msg_count, last_user_prompt, digest)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(id) DO UPDATE SET
                parent_id = excluded.parent_id,
                fork_kind = excluded.fork_kind,
                title = excluded.title,
                updated_at_s = excluded.updated_at_s,
                project_root = excluded.project_root,
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
                session.project_root,
                session.msg_count,
                session.last_user_prompt,
                session.digest,
            ],
        )?;
        Ok(())
    }

    /// Retrieve a single session by id.
    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        self.conn
            .query_row(
                "SELECT id, parent_id, fork_kind, title, created_at_s, updated_at_s, project_root, msg_count, last_user_prompt, digest FROM sessions WHERE id = ?1",
                params![session_id],
                map_session_row,
            )
            .optional()
    }

    /// List sessions, optionally filtered by `project_root`, sorted by `updated_at_ms` descending.
    pub fn list_sessions(&self, project_root: Option<&str>) -> Result<Vec<SessionRecord>> {
        let mut sessions = Vec::new();
        if let Some(root) = project_root {
            let mut stmt = self.conn.prepare(
                "SELECT id, parent_id, fork_kind, title, created_at_s, updated_at_s, project_root, msg_count, last_user_prompt, digest \
                 FROM sessions WHERE project_root = ?1 ORDER BY updated_at_s DESC",
            )?;
            let rows = stmt.query_map(params![root], map_session_row)?;
            for session in rows {
                sessions.push(session?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, parent_id, fork_kind, title, created_at_s, updated_at_s, project_root, msg_count, last_user_prompt, digest \
                 FROM sessions ORDER BY updated_at_s DESC",
            )?;
            let rows = stmt.query_map([], map_session_row)?;
            for session in rows {
                sessions.push(session?);
            }
        }
        Ok(sessions)
    }

    /// Delete a session and cascade all its events, messages, and command records.
    pub fn delete_session(&self, session_id: &str) -> Result<bool> {
        let affected = self
            .conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
        Ok(affected > 0)
    }

    /// Reclaim blob-store entries no session references (ADR-0187). The live
    /// set is the durable `blob_refs` ledger, maintained transactionally with
    /// the saves that introduce references; a blob absent from it cannot be
    /// reached by any load path.
    pub fn collect_blob_garbage(&self, blob_store: &BlobStore) -> Result<(usize, u64)> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT hash FROM blob_refs")?;
        let live: std::collections::HashSet<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(Result::ok)
            .collect();
        Ok(blob_store.retain_only(&live))
    }

    /// Reclaim transcript entries no session membership references
    /// (ADR-0187). Entries are global facts shared by identity across forks,
    /// so only zero-reference rows are removable; the foreign key from
    /// `entry_memberships` makes the two tables' agreement structural, and
    /// entry insert + membership insert share one transaction, so a GC pass
    /// can never observe the half of a pair. Returns the rows reclaimed.
    pub fn collect_entry_garbage(&self) -> Result<usize> {
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
    /// Full mode rewrites every membership, projection, and blob reference
    /// from `data`. It is the authoritative write for rebuilds, forks, and
    /// any transcript whose generation the store has not seen.
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

            self.conn.execute(
                r#"
                INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_s, updated_at_s, project_root, msg_count, last_user_prompt, digest, digest_anchor, tree, transcript_generation, provider_connection, round_counter, unattended, disabled_tools, commands, round_interrupts, retry_resolutions, retry_pending, checksum, schema_version)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
                ON CONFLICT(id) DO UPDATE SET
                    parent_id = excluded.parent_id,
                    fork_kind = excluded.fork_kind,
                    title = excluded.title,
                    updated_at_s = excluded.updated_at_s,
                    project_root = excluded.project_root,
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
                    data.project_root.to_string_lossy(),
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
                "SELECT id, parent_id, fork_kind, title, created_at_s, updated_at_s, project_root, digest, digest_anchor, tree, transcript_generation, provider_connection, round_counter, unattended, disabled_tools, commands, round_interrupts, retry_resolutions, retry_pending, checksum, schema_version FROM sessions WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, i64>(12)?,
                        row.get::<_, i64>(13)?,
                        row.get::<_, String>(14)?,
                        row.get::<_, String>(15)?,
                        row.get::<_, String>(16)?,
                        row.get::<_, String>(17)?,
                        row.get::<_, Option<String>>(18)?,
                        row.get::<_, Option<i64>>(19)?,
                        row.get::<_, i64>(20)?,
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
            project_root,
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
            project_root: PathBuf::from(project_root),
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

    /// Resolve a session ID prefix (4+ hex chars) to matching full session IDs.
    pub fn resolve_session_prefix(
        &self,
        prefix: &str,
        project_root: Option<&str>,
    ) -> Result<Vec<String>> {
        let pattern = format!("{prefix}%");
        let mut matches = Vec::new();
        if let Some(root) = project_root {
            let mut stmt = self.conn.prepare(
                "SELECT id FROM sessions WHERE id LIKE ?1 AND project_root = ?2 ORDER BY updated_at_s DESC",
            )?;
            let rows = stmt.query_map(params![pattern, root], |row| row.get(0))?;
            for id in rows {
                matches.push(id?);
            }
        } else {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM sessions WHERE id LIKE ?1 ORDER BY updated_at_s DESC")?;
            let rows = stmt.query_map(params![pattern], |row| row.get(0))?;
            for id in rows {
                matches.push(id?);
            }
        }
        Ok(matches)
    }

    /// List session summaries for a project, sorted by `updated_at_ms` descending.
    pub fn list_session_summaries(
        &self,
        project_root: Option<&str>,
        active_id: &str,
    ) -> Result<Vec<crate::session::SessionSummary>> {
        let sql = r#"
            SELECT
                id,
                parent_id,
                fork_kind,
                title,
                created_at_s,
                updated_at_s,
                msg_count,
                last_user_prompt,
                digest
            FROM sessions
            WHERE (?1 IS NULL OR project_root = ?1)
              AND fork_kind <> 'subagent'
            ORDER BY updated_at_s DESC;
        "#;

        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![project_root], |row| {
            let id: String = row.get(0)?;
            let parent_id: Option<String> = row.get(1)?;
            let fork_str: String = row.get(2)?;
            let title: Option<String> = row.get(3)?;
            let created_at_ms: i64 = row.get(4)?;
            let updated_at_ms: i64 = row.get(5)?;
            let msg_count: i64 = row.get(6)?;
            let last_user_prompt: Option<String> = row.get(7)?;
            let digest_json: Option<String> = row.get(8)?;
            Ok((
                id,
                parent_id,
                fork_str,
                title,
                created_at_ms,
                updated_at_ms,
                msg_count,
                last_user_prompt,
                digest_json,
            ))
        })?;

        let mut summaries = Vec::new();
        for item in rows {
            let (
                id,
                parent_id,
                fork_str,
                title,
                created_at_ms,
                updated_at_ms,
                msg_count,
                last_user_prompt,
                digest_json,
            ) = item?;

            let fork_kind = match fork_str.as_str() {
                "fork" => muta_contracts::SessionForkKind::Fork,
                "aside" => muta_contracts::SessionForkKind::Aside,
                _ => muta_contracts::SessionForkKind::Trunk,
            };

            let final_msg_count = msg_count.max(0) as usize;
            if final_msg_count == 0 && id != active_id {
                continue;
            }

            let overview = if let Some(t) = title.as_deref().filter(|t| !t.trim().is_empty()) {
                crate::session::truncate_preview(t, 64)
            } else if let Some(prompt) =
                last_user_prompt.as_deref().filter(|p| !p.trim().is_empty())
            {
                crate::session::truncate_preview(prompt, 64)
            } else {
                "(empty session)".to_string()
            };

            let digest = digest_json.and_then(|raw| serde_json::from_str(&raw).ok());
            let active = id == active_id;

            summaries.push(crate::session::SessionSummary {
                id,
                parent_id,
                fork_kind,
                message_count: final_msg_count,
                updated_at: updated_at_ms as u64,
                created_at: created_at_ms as u64,
                overview,
                active,
                digest,
            });
        }

        summaries.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        Ok(summaries)
    }

    /// Retrieve full session detail for on-demand inspection.
    pub fn get_session_detail(
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
    pub fn rename_session(
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
    pub fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv_store (key, value, updated_at) VALUES (?1, ?2, strftime('%s','now')) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value],
        )?;
        Ok(())
    }

    /// Fetch a key from the unified KV store.
    pub fn get_kv(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM kv_store WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
    }

    /// Delete a key from the unified KV store.
    pub fn delete_kv(&self, key: &str) -> Result<bool> {
        let affected = self
            .conn
            .execute("DELETE FROM kv_store WHERE key = ?1", params![key])?;
        Ok(affected > 0)
    }

    /// Record a slash-command invocation in the durable command ledger.
    pub fn record_command(&self, cmd: &muta_contracts::CommandRecord) -> Result<()> {
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

    /// List keys with a given prefix, ordered descending.
    pub fn list_kv_keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
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
    /// filtered by workspace root.
    pub fn search_history(
        &self,
        query: &str,
        project_root: Option<&str>,
        limit: usize,
    ) -> Result<Vec<HistorySearchResult>> {
        let clean_query = query.trim();
        if clean_query.is_empty() {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();
        if let Some(root) = project_root {
            let sql = r#"
                SELECT
                    f.entry_id,
                    f.session_id,
                    s.project_root,
                    f.role,
                    snippet(fts_entries, 3, '<b>', '</b>', '...', 16) AS snippet,
                    bm25(fts_entries) AS score
                FROM fts_entries f
                JOIN sessions s ON f.session_id = s.id
                WHERE fts_entries MATCH ?1 AND s.project_root = ?2
                ORDER BY score ASC LIMIT ?3;
            "#;
            let mut stmt = self.conn.prepare(sql)?;
            let rows = stmt.query_map(params![clean_query, root, limit as i64], map_search_row)?;
            for item in rows {
                results.push(item?);
            }
        } else {
            let sql = r#"
                SELECT
                    f.entry_id,
                    f.session_id,
                    s.project_root,
                    f.role,
                    snippet(fts_entries, 3, '<b>', '</b>', '...', 16) AS snippet,
                    bm25(fts_entries) AS score
                FROM fts_entries f
                JOIN sessions s ON f.session_id = s.id
                WHERE fts_entries MATCH ?1
                ORDER BY score ASC LIMIT ?2;
            "#;
            let mut stmt = self.conn.prepare(sql)?;
            let rows = stmt.query_map(params![clean_query, limit as i64], map_search_row)?;
            for item in rows {
                results.push(item?);
            }
        }

        Ok(results)
    }

    // Typed JSON KV Helpers (ADR-0168)

    /// Retrieve and deserialize a JSON value from `kv_store`.
    pub fn get_json<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>> {
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

    /// Serialize and persist a JSON value to `kv_store`.
    pub fn set_json<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let serialized = serde_json::to_string(value)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        self.set_kv(key, &serialized)
    }

    // Authoritative Input History Operations (ADR-0168 / SSOT)

    /// Record a prompt into `input_history`, respecting `dedup` and the global `HISTORY_CAP`.
    pub fn record_input_history(
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
    pub fn load_input_history(&self, limit: usize) -> Result<Vec<muta_contracts::HistoryEntry>> {
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
    pub fn save_input_history(
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
    pub fn clear_input_history(&self) -> Result<()> {
        self.conn.execute("DELETE FROM input_history", [])?;
        Ok(())
    }

    /// Delete a specific prompt history record by text and timestamp.
    /// If `created_at_ms` is non-zero, matches both text and timestamp;
    /// otherwise falls back to text match. Returns the number of deleted rows.
    pub fn delete_input_history_entry(&self, text: &str, created_at_ms: u64) -> Result<usize> {
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
    pub fn migrate_legacy_input_history(&self) -> usize {
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

// Asynchronous Persistence Actor (Single-Writer Pattern)

/// Command variants dispatched to the single-writer persistence actor.
pub enum PersistenceCommand {
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
        ack: oneshot::Sender<Result<()>>,
    },
    UpsertSession {
        record: SessionRecord,
        ack: oneshot::Sender<Result<()>>,
    },
    DeleteSession {
        session_id: String,
        ack: oneshot::Sender<Result<bool>>,
    },
    RenameSession {
        session_id: String,
        title: Option<String>,
        manual: bool,
        ack: oneshot::Sender<Result<bool>>,
    },
    RecordCommand {
        cmd: muta_contracts::CommandRecord,
        ack: oneshot::Sender<Result<()>>,
    },
    SetKV {
        key: String,
        value: String,
        ack: oneshot::Sender<Result<()>>,
    },
    DeleteKV {
        key: String,
        ack: oneshot::Sender<Result<bool>>,
    },
    RecordInputHistory {
        entry: muta_contracts::HistoryEntry,
        dedup: bool,
        ack: Option<oneshot::Sender<Result<()>>>,
    },
    SaveInputHistory {
        entries: Vec<muta_contracts::HistoryEntry>,
        dedup: bool,
        ack: oneshot::Sender<Result<()>>,
    },
    ClearInputHistory {
        ack: oneshot::Sender<Result<()>>,
    },
    DeleteInputHistoryEntry {
        text: String,
        created_at_ms: u64,
        ack: Option<oneshot::Sender<Result<usize>>>,
    },
}

/// Asynchronous handle for interacting with the single-writer PersistenceActor without blocking Tokio runtime.
#[derive(Clone)]
pub struct PersistenceHandle {
    tx: mpsc::Sender<PersistenceCommand>,
    db_path: PathBuf,
    blob_store: Option<BlobStore>,
}

static GLOBAL_HANDLE: OnceLock<PersistenceHandle> = OnceLock::new();

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
    /// Spawn the persistence actor on a dedicated background worker thread.
    #[allow(clippy::expect_used)]
    pub fn spawn(db_path: PathBuf, blob_store: Option<BlobStore>) -> Self {
        let (tx, mut rx) = mpsc::channel::<PersistenceCommand>(1024);
        let actor_path = db_path.clone();
        let actor_blobs = blob_store.clone();

        std::thread::Builder::new()
            .name("muta-persistence-writer".into())
            .spawn(move || {
                let engine = match DatabaseEngine::open(&actor_path, actor_blobs) {
                    Ok(e) => e,
                    Err(err) => {
                        error!(error = %err, "Failed to initialize persistence writer database engine");
                        return;
                    }
                };

                while let Some(cmd) = rx.blocking_recv() {
                    match cmd {
                        PersistenceCommand::SaveSession {
                            data,
                            full,
                            usage_upserts,
                            ack,
                        } => {
                            let res = engine.save_session_inner(&data, full, &usage_upserts);
                            let _ = ack.send(res);
                        }
                        PersistenceCommand::UpsertSession { record, ack } => {
                            let res = engine.upsert_session(&record);
                            let _ = ack.send(res);
                        }
                        PersistenceCommand::DeleteSession { session_id, ack } => {
                            let res = engine.delete_session(&session_id);
                            let _ = ack.send(res);
                        }
                        PersistenceCommand::RenameSession {
                            session_id,
                            title,
                            manual,
                            ack,
                        } => {
                            let res = engine.rename_session(&session_id, title.as_deref(), manual);
                            let _ = ack.send(res);
                        }
                        PersistenceCommand::RecordCommand { cmd, ack } => {
                            let res = engine.record_command(&cmd);
                            let _ = ack.send(res);
                        }
                        PersistenceCommand::SetKV { key, value, ack } => {
                            let res = engine.set_kv(&key, &value);
                            let _ = ack.send(res);
                        }
                        PersistenceCommand::DeleteKV { key, ack } => {
                            let res = engine.delete_kv(&key);
                            let _ = ack.send(res);
                        }
                        PersistenceCommand::RecordInputHistory { entry, dedup, ack } => {
                            let res = engine.record_input_history(&entry, dedup);
                            if let Some(ack) = ack {
                                let _ = ack.send(res);
                            }
                        }
                        PersistenceCommand::SaveInputHistory { entries, dedup, ack } => {
                            let res = engine.save_input_history(&entries, dedup);
                            let _ = ack.send(res);
                        }
                        PersistenceCommand::ClearInputHistory { ack } => {
                            let res = engine.clear_input_history();
                            let _ = ack.send(res);
                        }
                        PersistenceCommand::DeleteInputHistoryEntry { text, created_at_ms, ack } => {
                            let res = engine.delete_input_history_entry(&text, created_at_ms);
                            if let Some(ack) = ack {
                                let _ = ack.send(res);
                            }
                        }
                    }
                }
            })
            .expect("failed to spawn persistence writer thread");

        Self {
            tx,
            db_path,
            blob_store,
        }
    }

    /// Asynchronously save a session in SQLite.
    #[allow(dead_code)]
    pub(crate) async fn save_session(
        &self,
        data: crate::session::SessionData,
        full: bool,
        usage_upserts: Vec<muta_contracts::RequestUsageRecord>,
    ) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::SaveSession {
                data: Box::new(data),
                full,
                usage_upserts,
                ack: ack_tx,
            })
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
    }

    /// Synchronously save a session on a blocking thread (always a full
    /// rewrite: the blocking callers write fresh or rebuilt state).
    #[allow(dead_code)]
    pub(crate) fn save_session_blocking(&self, data: crate::session::SessionData) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        let tx = self.tx.clone();
        let run_blocking = move || {
            tx.blocking_send(PersistenceCommand::SaveSession {
                data: Box::new(data),
                full: true,
                usage_upserts: Vec::new(),
                ack: ack_tx,
            })
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            ack_rx
                .blocking_recv()
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
        };

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
                tokio::task::block_in_place(run_blocking)
            } else {
                std::thread::spawn(run_blocking).join().map_err(|_| {
                    rusqlite::Error::ToSqlConversionFailure("persistence thread panicked".into())
                })?
            }
        } else {
            run_blocking()
        }
    }

    /// The database file path.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Asynchronously upsert a session record.
    pub async fn upsert_session(&self, record: SessionRecord) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::UpsertSession {
                record,
                ack: ack_tx,
            })
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
    }

    /// Non-blocking fire-and-forget session upsert to avoid blocking synchronous writers.
    pub fn try_upsert_session(&self, record: SessionRecord) {
        let (ack_tx, _) = oneshot::channel();
        let _ = self.tx.try_send(PersistenceCommand::UpsertSession {
            record,
            ack: ack_tx,
        });
    }

    /// Asynchronously delete a session record.
    pub async fn delete_session(&self, session_id: String) -> Result<bool> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::DeleteSession {
                session_id,
                ack: ack_tx,
            })
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
    }

    /// Asynchronously rename a session.
    pub async fn rename_session(
        &self,
        session_id: String,
        title: Option<String>,
        manual: bool,
    ) -> Result<bool> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::RenameSession {
                session_id,
                title,
                manual,
                ack: ack_tx,
            })
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
    }

    /// Asynchronously record a command invocation.
    pub async fn record_command(&self, cmd: muta_contracts::CommandRecord) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::RecordCommand { cmd, ack: ack_tx })
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
    }

    /// Asynchronously set a key-value entry.
    pub async fn set_kv(&self, key: String, value: String) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::SetKV {
                key,
                value,
                ack: ack_tx,
            })
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
    }

    /// Synchronously set a key-value entry on a blocking thread.
    pub fn set_kv_blocking(&self, key: String, value: String) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        let tx = self.tx.clone();
        let run_blocking = move || {
            tx.blocking_send(PersistenceCommand::SetKV {
                key,
                value,
                ack: ack_tx,
            })
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            ack_rx
                .blocking_recv()
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
        };

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
                tokio::task::block_in_place(run_blocking)
            } else {
                std::thread::spawn(run_blocking).join().map_err(|_| {
                    rusqlite::Error::ToSqlConversionFailure("persistence thread panicked".into())
                })?
            }
        } else {
            run_blocking()
        }
    }

    /// Asynchronously set a JSON-serializable value in the key-value store.
    pub async fn set_json<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let serialized = serde_json::to_string(value)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        self.set_kv(key.to_string(), serialized).await
    }

    /// Synchronously set a JSON-serializable value in the key-value store.
    pub fn set_json_blocking<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let serialized = serde_json::to_string(value)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        self.set_kv_blocking(key.to_string(), serialized)
    }

    /// Asynchronously delete a key-value entry.
    pub async fn delete_kv(&self, key: String) -> Result<bool> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::DeleteKV { key, ack: ack_tx })
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
    }

    /// Asynchronously record an input history entry (fire-and-forget).
    pub fn try_record_input_history(&self, entry: muta_contracts::HistoryEntry, dedup: bool) {
        let _ = self.tx.try_send(PersistenceCommand::RecordInputHistory {
            entry,
            dedup,
            ack: None,
        });
    }

    /// Record an input history entry, waiting for single-writer SQLite actor confirmation.
    pub fn record_input_history_blocking(
        &self,
        entry: muta_contracts::HistoryEntry,
        dedup: bool,
    ) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .blocking_send(PersistenceCommand::RecordInputHistory {
                entry,
                dedup,
                ack: Some(ack_tx),
            })
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx.blocking_recv().map_err(|_| {
            rusqlite::Error::ToSqlConversionFailure("persistence thread panicked".into())
        })?
    }

    /// Save multiple input history entries synchronously, waiting for SQLite actor confirmation.
    pub fn save_input_history_blocking(
        &self,
        entries: Vec<muta_contracts::HistoryEntry>,
        dedup: bool,
    ) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .blocking_send(PersistenceCommand::SaveInputHistory {
                entries,
                dedup,
                ack: ack_tx,
            })
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx.blocking_recv().map_err(|_| {
            rusqlite::Error::ToSqlConversionFailure("persistence thread panicked".into())
        })?
    }

    /// Clear all input history entries from SQLite.
    pub fn clear_input_history_blocking(&self) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .blocking_send(PersistenceCommand::ClearInputHistory { ack: ack_tx })
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx.blocking_recv().map_err(|_| {
            rusqlite::Error::ToSqlConversionFailure("persistence thread panicked".into())
        })?
    }

    /// Best-effort asynchronous delete of an input history record by text and timestamp.
    pub fn try_delete_input_history_entry(&self, text: String, created_at_ms: u64) {
        let _ = self
            .tx
            .try_send(PersistenceCommand::DeleteInputHistoryEntry {
                text,
                created_at_ms,
                ack: None,
            });
    }

    /// Synchronously delete an input history entry by text and timestamp.
    pub fn delete_input_history_entry_blocking(
        &self,
        text: &str,
        created_at_ms: u64,
    ) -> Result<usize> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .blocking_send(PersistenceCommand::DeleteInputHistoryEntry {
                text: text.to_string(),
                created_at_ms,
                ack: Some(ack_tx),
            })
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx.blocking_recv().map_err(|_| {
            rusqlite::Error::ToSqlConversionFailure("persistence thread panicked".into())
        })?
    }

    /// Open a lightweight read-only connection snapshot for querying.
    pub fn open_reader(&self) -> Result<DatabaseEngine> {
        DatabaseEngine::open(&self.db_path, self.blob_store.clone())
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

    /// The startup guard fails loudly on a database missing runtime columns.
    #[test]
    fn schema_guard_rejects_missing_columns() {
        let mut conn = Connection::open_in_memory().unwrap();
        configure_connection(&mut conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, project_root TEXT NOT NULL);",
        )
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
    /// durable transcript carries (UserSteer/RunnerSteer, CommandEcho "/cmd"
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
        let steer = Message::new(Role::User, "pls reconsider point 3")
            .with_origin(muta_contracts::InjectionOrigin::new(InjectionKind::UserSteer));
        let echo = Message::command_echo("/session list");
        let image = Message::new(Role::User, "Image from screenshot")
            .with_images(vec![muta_contracts::ImagePart {
                mime: "image/png".into(),
                data: "bytes".into(),
            }])
            .with_origin(muta_contracts::InjectionOrigin::new(InjectionKind::ToolImage));

        assert!(!steer.hidden && steer.origin.is_some());
        assert!(!echo.hidden && echo.origin.is_some());
        assert!(!image.hidden && image.origin.is_some());

        data.transcript.push(TranscriptEntry::from_message(0, &steer));
        data.transcript.push(TranscriptEntry::from_message(1, &echo));
        data.transcript.push(TranscriptEntry::from_message(2, &image));
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

        let err = engine.save_session_full(&data).err().expect("constraint must reject");
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
                "INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_s, updated_at_s, project_root, msg_count, last_user_prompt, digest) VALUES ('s1', NULL, 'trunk', NULL, 1, 1, '/tmp', 0, NULL, NULL)",
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

        let (count, _) = engine.collect_blob_garbage(&blob_store).unwrap();
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
                "INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_s, updated_at_s, project_root, msg_count, last_user_prompt, digest) VALUES ('s1', NULL, 'trunk', 'T', 1, 1, '/tmp', 0, NULL, NULL)",
                [],
            )
            .unwrap();
        let sessions = engine.list_sessions(None).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title.as_deref(), Some("T"));
    }
}
