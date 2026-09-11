//! Schema catalog and migration machinery (ADR-0163/0186/0187/0236).
//!
//! Split out of `db.rs`; still inside `muta-persistence::db`, so ADR-0231's
//! one-door boundary is unchanged.
use super::*;

/// SQLite schema version tracking. Fresh databases jump straight to the latest version.
pub const CURRENT_DB_VERSION: u32 = 15;

/// SHA-256 fingerprint of the migration catalog (version + SQL of every
/// entry). Locked by `migration_catalog_fingerprint_is_stable`; see that test
/// for the discipline this enforces.
#[cfg(test)]
pub const MIGRATION_CATALOG_FINGERPRINT: &str =
    "f2f4cb09e48126eb80cd79f1846e4c94dd759d50b7145371484f8156ab198666";

/// Payload size threshold (4 KB) beyond which text content is offloaded to CAS BlobStore.
pub const CAS_THRESHOLD_BYTES: usize = 4096;

/// Hard cap on retained request-projection records per session (ADR-0218). The
/// archive is forensic, not authoritative: a bounded ring keeps a long session
/// from growing it without limit.
pub const MAX_RETAINED_REQUEST_PROJECTIONS: usize = 64;

/// Initialize and return a connection to the SQLite database.
/// Configures WAL mode, synchronous=FULL for robustness, busy timeout, and turns on foreign keys.
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
pub fn configure_connection(conn: &mut Connection) -> Result<()> {
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
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "journal_size_limit", 16777216)?; // 16MB WAL recycling
    conn.pragma_update(None, "wal_autocheckpoint", 1000)?; // 1000 pages (~4MB)
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

/// A structured database migration step.
pub struct Migration {
    pub(crate) version: u32,
    pub(crate) sql: &'static str,
}

/// The chronological sequence of schema migrations.
pub const MIGRATIONS: &[Migration] = &[
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
    Migration { version: 15, sql: "" },
];

/// Working-state columns the final ADR-0186 `sessions` rebuild must carry,
/// with their DDL. Shared by the migration-6 repair and the startup schema
/// guard so the two cannot drift.
pub const SESSIONS_WORKING_STATE_COLUMNS: &[(&str, &str)] = &[
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
pub const SESSIONS_V2_COLUMNS: &[(&str, &str)] = &[
    ("digest_anchor", "INTEGER"),
    ("tree", "TEXT NOT NULL DEFAULT '{}'"),
    ("transcript_generation", "TEXT"),
];

/// Identity columns every `sessions` incarnation must carry. Guard-only:
/// migrations own their creation.
pub const SESSIONS_IDENTITY_COLUMNS: &[&str] = &[
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
pub const SESSIONS_RETIRED_COLUMNS: &[&str] = &["scheduled_jobs", "data", "title_manual"];

pub fn sessions_columns(conn: &Connection) -> Result<std::collections::HashSet<String>> {
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
pub fn repair_intermediate_sessions_schema(tx: &rusqlite::Transaction) -> Result<()> {
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
pub fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        params![name],
        |row| row.get::<_, i64>(0),
    )? == 1)
}

/// Persistence v2 (ADR-0187) schema repair, applied by migration 7. Every
/// step is conditional: fresh ADR-0186 databases, final version-5 databases,
/// and interim version-5 databases (no transcript tables) must all converge.
pub fn apply_persistence_v2_schema(tx: &rusqlite::Transaction) -> Result<()> {
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
pub fn apply_usage_ledger_schema(tx: &rusqlite::Transaction) -> Result<()> {
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
            insert_legacy_usage_record_tx(tx, &session_id, record)?;
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
pub fn apply_entries_integrity_schema(tx: &rusqlite::Transaction) -> Result<()> {
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
pub fn apply_session_scope_schema(tx: &rusqlite::Transaction) -> Result<()> {
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
pub fn apply_session_grouping_schema(tx: &rusqlite::Transaction) -> Result<()> {
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
pub fn apply_workspace_partition_schema(tx: &rusqlite::Transaction) -> Result<()> {
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

pub fn insert_legacy_usage_record_tx(
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

pub fn insert_usage_record_tx(conn: &Connection, session_id: &str, record: &muta_contracts::RequestUsageRecord) -> Result<()> {
    if record.key.session_id != session_id {
        return Err(rusqlite::Error::InvalidParameterName("usage belongs to another session".into()));
    }
    let root: Option<String> = conn.query_row("SELECT workspace_root FROM sessions WHERE id=?1", [session_id], |r| r.get(0)).optional()?.flatten();
    let project = root.map(|p| crate::paths::project_bucket_name(Path::new(&p))).unwrap_or_default();
    let at = if record.started_at_ms > 0 { record.started_at_ms } else { unix_ms() };
    upsert_attempt(conn, &muta_contracts::usage_stats::UsageStatRecord {
        day: muta_contracts::usage_stats::day_key_from_epoch_ms(at), recorded_at_ms: at,
        project, record: record.clone(),
    })?;
    Ok(())
}

/// Fail-fast guard: verify the `sessions` table carries exactly the columns
/// the current runtime SQL writes, so a schema/runtime mismatch surfaces as
/// one clear startup error instead of scattered per-statement failures
/// (`table sessions has no column named ...`) during persistence.
pub fn verify_sessions_schema(conn: &Connection) -> Result<()> {
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
pub fn migrate_schema(conn: &mut Connection) -> Result<()> {
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
pub fn apply_migrations(conn: &mut Connection, observed_version: u32) -> Result<()> {
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
                if migration.version == 15 {
                    apply_durable_attempt_schema(&tx)?;
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
pub fn assert_referential_integrity(conn: &Connection) -> Result<()> {
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
