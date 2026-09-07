//! Authoritative embedded SQLite storage engine and event ledger (ADR-0163).
//!
//! Provides relational database initialization, schema migration tracking via
//! `PRAGMA user_version`, FTS5 full-text search, Content-Addressed Storage (CAS)
//! threshold isolation, and a single-writer persistence engine.

use crate::blobs::BlobStore;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Result, Row, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

/// SQLite schema version tracking. Fresh databases jump straight to the latest version.
pub const CURRENT_DB_VERSION: u32 = 5;

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
    conn.pragma_update(None, "wal_autocheckpoint", 1000)?;     // 1000 pages (~4MB)
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

/// A structured database migration step.
struct Migration {
    version: u32,
    sql: &'static str,
}

/// The chronological sequence of schema migrations.
const MIGRATIONS: &[Migration] = &[Migration {
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
}, Migration {
    version: 2,
    sql: r#"
        -- Add full serialized SessionData JSON column for SQLite Single-Source-of-Truth
        ALTER TABLE sessions ADD COLUMN data TEXT;
    "#,
}, Migration {
    version: 3,
    sql: r#"
        -- Add indexed summary columns for sub-millisecond session listing
        ALTER TABLE sessions ADD COLUMN msg_count INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE sessions ADD COLUMN last_user_prompt TEXT;
        ALTER TABLE sessions ADD COLUMN digest TEXT;
        CREATE INDEX IF NOT EXISTS idx_sessions_project_updated ON sessions(project_root, updated_at_ms DESC);
    "#,
}, Migration {
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
}, Migration {
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
}];

/// Run all outstanding migrations in a single transactional loop.
fn migrate_schema(conn: &mut Connection) -> Result<()> {
    let current_version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if current_version >= CURRENT_DB_VERSION {
        return Ok(());
    }

    let tx = conn.transaction()?;

    for migration in MIGRATIONS {
        if migration.version > current_version {
            info!(
                version = migration.version,
                "Applying SQLite schema migration"
            );
            tx.execute_batch(migration.sql)?;

            if migration.version == 3 {
                let mut stmt = tx.prepare(
                    "SELECT id, data FROM sessions WHERE data IS NOT NULL",
                )?;
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
                                let is_echo = m
                                    .origin
                                    .as_ref()
                                    .is_some_and(|o| o.kind == muta_contracts::InjectionKind::CommandEcho);
                                m.role == muta_contracts::Role::User && !m.hidden && !is_echo
                            })
                            .map(|m| m.content.clone());
                        let digest_json = probe.digest.as_ref().and_then(|d| serde_json::to_string(d).ok());
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
    Ok(())
}

/// Session record representation in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: String,
    pub parent_id: Option<String>,
    pub fork_kind: String,
    pub title: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub project_root: String,
    #[serde(default)]
    pub msg_count: i64,
    #[serde(default)]
    pub last_user_prompt: Option<String>,
    #[serde(default)]
    pub digest: Option<String>,
}

/// One ledger row in the `events` table (ADR-0186 working-state audit log).
#[derive(Debug, Clone)]
pub struct SessionEventRecord {
    pub session_id: String,
    pub seq: i64,
    pub event_type: String,
    pub payload: String,
    pub created_at_ms: i64,
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

fn map_session_row(row: &Row) -> Result<SessionRecord> {
    Ok(SessionRecord {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        fork_kind: row.get(2)?,
        title: row.get(3)?,
        created_at_ms: row.get(4)?,
        updated_at_ms: row.get(5)?,
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

/// Authoritative relational database access object for Muta persistence.
pub struct DatabaseEngine {
    conn: Connection,
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
    pub fn open_in_memory(blob_store: Option<BlobStore>) -> Result<Self> {
        let conn = initialize_in_memory_db()?;
        Ok(Self { conn, blob_store })
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
            INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_ms, updated_at_ms, project_root, msg_count, last_user_prompt, digest)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(id) DO UPDATE SET
                parent_id = excluded.parent_id,
                fork_kind = excluded.fork_kind,
                title = excluded.title,
                updated_at_ms = excluded.updated_at_ms,
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
                session.created_at_ms,
                session.updated_at_ms,
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
                "SELECT id, parent_id, fork_kind, title, created_at_ms, updated_at_ms, project_root, msg_count, last_user_prompt, digest FROM sessions WHERE id = ?1",
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
                "SELECT id, parent_id, fork_kind, title, created_at_ms, updated_at_ms, project_root, msg_count, last_user_prompt, digest \
                 FROM sessions WHERE project_root = ?1 ORDER BY updated_at_ms DESC",
            )?;
            let rows = stmt.query_map(params![root], map_session_row)?;
            for session in rows {
                sessions.push(session?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, parent_id, fork_kind, title, created_at_ms, updated_at_ms, project_root, msg_count, last_user_prompt, digest \
                 FROM sessions ORDER BY updated_at_ms DESC",
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

    /// Persist a complete [`crate::session::SessionData`] into SQLite in one
    /// transaction (ADR-0186): the session row (identity + working state),
    /// the session's memberships and directives, and the entries themselves
    /// (`INSERT OR IGNORE` — facts are shared by identity across forks).
    pub(crate) fn save_session_full(&self, data: &crate::session::SessionData) -> Result<()> {
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
        let msg_count = data.transcript.entries.len() as i64;

        self.conn.execute("BEGIN IMMEDIATE", [])?;
        let res: Result<()> = (|| {
            self.conn.execute(
                r#"
                INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_ms, updated_at_ms, project_root, msg_count, last_user_prompt, digest, provider_connection, round_counter, unattended, disabled_tools, commands, round_interrupts, retry_pending, request_usage_records, applied_seq, checksum, schema_version)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
                ON CONFLICT(id) DO UPDATE SET
                    parent_id = excluded.parent_id,
                    fork_kind = excluded.fork_kind,
                    title = excluded.title,
                    updated_at_ms = excluded.updated_at_ms,
                    project_root = excluded.project_root,
                    msg_count = excluded.msg_count,
                    last_user_prompt = excluded.last_user_prompt,
                    digest = excluded.digest,
                    provider_connection = excluded.provider_connection,
                    round_counter = excluded.round_counter,
                    unattended = excluded.unattended,
                    disabled_tools = excluded.disabled_tools,
                    commands = excluded.commands,
                    round_interrupts = excluded.round_interrupts,
                    retry_pending = excluded.retry_pending,
                    request_usage_records = excluded.request_usage_records,
                    applied_seq = excluded.applied_seq,
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
                    data.provider_selection.as_ref().and_then(|s| serde_json::to_string(s).ok()),
                    data.round_counter as i64,
                    data.unattended,
                    serde_json::to_string(&data.disabled_tools.iter().collect::<Vec<_>>()).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    serde_json::to_string(&data.commands).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    serde_json::to_string(&data.round_interrupts).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    data.retry_pending.as_ref().and_then(|p| serde_json::to_string(p).ok()),
                    serde_json::to_string(&data.request_usage_records).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    data.applied_seq.map(|s| s as i64),
                    data.checksum.map(|c| c as i64),
                    data.schema_version as i64,
                ],
            )?;

            // Projection decisions: the session's own view history.
            self.conn
                .execute("DELETE FROM projections WHERE session_id = ?1", params![data.id])?;
            for directive in &data.transcript.directives {
                let payload = serde_json::to_string(&directive.payload)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                self.conn.execute(
                    "INSERT INTO projections (session_id, seq, kind, up_to_seq, payload, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        data.id,
                        directive.seq as i64,
                        serde_plain(directive.kind)?,
                        directive.up_to_seq as i64,
                        payload,
                        data.updated_at as i64,
                    ],
                )?;
            }

            // Facts + memberships. Entries are global and shared by identity.
            self.conn.execute(
                "DELETE FROM entry_memberships WHERE session_id = ?1",
                params![data.id],
            )?;
            for entry in &data.transcript.entries {
                let payload = serde_json::to_string(&entry.payload)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                self.conn.execute(
                    r#"
                    INSERT INTO entries (id, kind, role, content, origin, hidden, created_at_ms, payload)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                    ON CONFLICT(id) DO NOTHING;
                    "#,
                    params![
                        entry.id,
                        if entry.kind == muta_contracts::EntryKind::State { "state" } else { "message" },
                        entry.role.map(role_str),
                        entry.content,
                        entry.origin.map(origin_str),
                        entry.hidden,
                        entry.created_at_ms as i64,
                        payload,
                    ],
                )?;
                self.conn.execute(
                    "INSERT INTO entry_memberships (session_id, seq, entry_id, added_by) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        data.id,
                        entry.seq as i64,
                        entry.id,
                        data.applied_seq.unwrap_or(0) as i64,
                    ],
                )?;
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

    /// Load a full [`crate::session::SessionData`] by session ID from SQLite.
    /// Entries whose payloads the current binary cannot decode are skipped
    /// (kept in storage; degraded in view) per the unknown-kind contract.
    pub(crate) fn load_session_full(&self, session_id: &str) -> Result<Option<crate::session::SessionData>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, parent_id, fork_kind, title, created_at_ms, updated_at_ms, project_root, digest, provider_connection, round_counter, unattended, disabled_tools, commands, round_interrupts, retry_pending, request_usage_records, applied_seq, checksum, schema_version FROM sessions WHERE id = ?1",
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
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, String>(11)?,
                        row.get::<_, String>(12)?,
                        row.get::<_, String>(13)?,
                        row.get::<_, Option<String>>(14)?,
                        row.get::<_, String>(15)?,
                        row.get::<_, Option<i64>>(16)?,
                        row.get::<_, Option<i64>>(17)?,
                        row.get::<_, i64>(18)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, parent_id, fork_kind, title, created_at_ms, updated_at_ms, project_root, digest, provider_connection, round_counter, unattended, disabled_tools, commands, round_interrupts, retry_pending, request_usage_records, applied_seq, checksum, schema_version)) = row
        else {
            return Ok(None);
        };

        let mut entries = Vec::new();
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
                    let payload: muta_contracts::EntryPayload = serde_json::from_str(&payload).ok()?;
                    Some(muta_contracts::TranscriptEntry {
                        id: eid,
                        seq: seq.max(0) as u64,
                        kind,
                        role,
                        content,
                        origin,
                        hidden: hidden != 0,
                        created_at_ms: created_at_ms.max(0) as u64,
                        payload,
                    })
                })();
                if let Some(entry) = decoded {
                    entries.push(entry);
                }
            }
        }

        let mut directives = Vec::new();
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
                    let payload: muta_contracts::DirectivePayload = serde_json::from_str(&payload).ok()?;
                    Some(muta_contracts::ProjectionDirective {
                        seq: seq.max(0) as u64,
                        kind,
                        up_to_seq: up_to_seq.max(0) as u64,
                        payload,
                    })
                })();
                if let Some(directive) = decoded {
                    directives.push(directive);
                }
            }
        }

        let digest: Option<muta_contracts::SessionDigest> = digest
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok());
        let digest_anchor = digest.as_ref().map(|_| updated_at_ms.max(0) as u64);
        Ok(Some(crate::session::SessionData {
            transcript: muta_contracts::Transcript { entries, directives },
            last_projection: None,
            digest,
            digest_anchor,
            id,
            parent_id,
            fork_kind: match fork_kind.as_str() {
                "fork" => muta_contracts::SessionForkKind::Fork,
                "aside" => muta_contracts::SessionForkKind::Aside,
                "subagent" => muta_contracts::SessionForkKind::Subagent,
                _ => muta_contracts::SessionForkKind::Trunk,
            },
            title,
            created_at: created_at_ms.max(0) as u64,
            updated_at: updated_at_ms.max(0) as u64,
            project_root: PathBuf::from(project_root),
            schema_version: if schema_version > 0 { schema_version as u32 } else { crate::session::CURRENT_SCHEMA_VERSION },
            checksum: checksum.map(|c| c as u32),
            applied_seq: applied_seq.map(|s| s.max(0) as u64),
            provider_selection: provider_connection
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok()),
            disabled_tools: serde_json::from_str(&disabled_tools).unwrap_or_default(),
            round_counter: round_counter.max(0) as u64,
            request_usage_records: serde_json::from_str(&request_usage_records).unwrap_or_default(),
            commands: serde_json::from_str(&commands).unwrap_or_default(),
            round_interrupts: serde_json::from_str(&round_interrupts).unwrap_or_default(),
            retry_pending: retry_pending
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok()),
            unattended: unattended != 0,
            tree: Default::default(),
        }))
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
                "SELECT id FROM sessions WHERE id LIKE ?1 AND project_root = ?2 ORDER BY updated_at_ms DESC",
            )?;
            let rows = stmt.query_map(params![pattern, root], |row| row.get(0))?;
            for id in rows {
                matches.push(id?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id FROM sessions WHERE id LIKE ?1 ORDER BY updated_at_ms DESC",
            )?;
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
                created_at_ms,
                updated_at_ms,
                msg_count,
                last_user_prompt,
                digest
            FROM sessions
            WHERE (?1 IS NULL OR project_root = ?1)
              AND fork_kind <> 'subagent'
            ORDER BY updated_at_ms DESC;
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
            } else if let Some(prompt) = last_user_prompt.as_deref().filter(|p| !p.trim().is_empty()) {
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
    /// surface but no longer stored.
    pub fn rename_session(&self, session_id: &str, title: Option<&str>, manual: bool) -> Result<bool> {
        let now = Utc::now().timestamp_millis();
        if let Some(mut data) = self.load_session_full(session_id)? {
            data.title = title.map(|s| s.to_string());
            data.updated_at = now as u64;
            self.save_session_full(&data)?;
            return Ok(true);
        }
        let _ = manual;
        let affected = self.conn.execute(
            "UPDATE sessions SET title = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![title, now, session_id],
        )?;
        Ok(affected > 0)
    }

    /// Discover every persisted session (across all project buckets) that has armed `/schedule` jobs.
    // Event Ledger Operations (ADR-0163)

    /// Append a single event to the monotonic event ledger.
    pub fn append_event(&self, event: &SessionEventRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO events (session_id, seq, event_type, payload, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.session_id,
                event.seq,
                event.event_type,
                event.payload,
                event.created_at_ms,
            ],
        )?;
        Ok(())
    }

    /// Load all events for a session in sequence order.
    pub fn get_session_events(&self, session_id: &str) -> Result<Vec<SessionEventRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, seq, event_type, payload, created_at_ms \
             FROM events WHERE session_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(SessionEventRecord {
                session_id: row.get(0)?,
                seq: row.get(1)?,
                event_type: row.get(2)?,
                payload: row.get(3)?,
                created_at_ms: row.get(4)?,
            })
        })?;
        let mut events = Vec::new();
        for event in rows {
            events.push(event?);
        }
        Ok(events)
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
            .query_row("SELECT value FROM kv_store WHERE key = ?1", params![key], |row| {
                row.get(0)
            })
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
        let id = format!("{}:{}:{}", cmd.name, cmd.timestamp, muta_contracts::todos::unix_now());
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
                cmd.result.as_ref().and_then(|r| serde_json::to_string(r).ok()),
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
                Some(self.conn.prepare("DELETE FROM input_history WHERE text = ?1")?)
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
            self.conn.execute(
                "DELETE FROM input_history WHERE text = ?1",
                params![text],
            )?
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
        } else if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()).map(PathBuf::from) {
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
            let Ok(entries) = serde_json::from_str::<Vec<muta_contracts::HistoryEntry>>(&content) else {
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
    SaveSessionFull {
        data: Box<crate::session::SessionData>,
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
    AppendEvent {
        event: SessionEventRecord,
        ack: oneshot::Sender<Result<()>>,
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
                        PersistenceCommand::SaveSessionFull { data, ack } => {
                            let res = engine.save_session_full(&data);
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
                        PersistenceCommand::AppendEvent { event, ack } => {
                            let res = engine.append_event(&event);
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

    /// Asynchronously save a full session in SQLite.
    #[allow(dead_code)]
    pub(crate) async fn save_session_full(&self, data: crate::session::SessionData) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::SaveSessionFull {
                data: Box::new(data),
                ack: ack_tx,
            })
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
    }

    /// Synchronously save a full session on a blocking thread.
    #[allow(dead_code)]
    pub(crate) fn save_session_full_blocking(&self, data: crate::session::SessionData) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        let tx = self.tx.clone();
        let run_blocking = move || {
            tx.blocking_send(PersistenceCommand::SaveSessionFull {
                data: Box::new(data),
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
                std::thread::spawn(run_blocking)
                    .join()
                    .map_err(|_| rusqlite::Error::ToSqlConversionFailure("persistence thread panicked".into()))?
            }
        } else {
            run_blocking()
        }
    }

    /// The database file path.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// The associated CAS blob store, if any.
    pub fn blob_store(&self) -> Option<&BlobStore> {
        self.blob_store.as_ref()
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

    /// Asynchronously append an event to the ledger.
    pub async fn append_event(&self, event: SessionEventRecord) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::AppendEvent { event, ack: ack_tx })
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
                std::thread::spawn(run_blocking)
                    .join()
                    .map_err(|_| rusqlite::Error::ToSqlConversionFailure("persistence thread panicked".into()))?
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
        ack_rx
            .blocking_recv()
            .map_err(|_| rusqlite::Error::ToSqlConversionFailure("persistence thread panicked".into()))?
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
        ack_rx
            .blocking_recv()
            .map_err(|_| rusqlite::Error::ToSqlConversionFailure("persistence thread panicked".into()))?
    }

    /// Clear all input history entries from SQLite.
    pub fn clear_input_history_blocking(&self) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .blocking_send(PersistenceCommand::ClearInputHistory { ack: ack_tx })
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx
            .blocking_recv()
            .map_err(|_| rusqlite::Error::ToSqlConversionFailure("persistence thread panicked".into()))?
    }

    /// Best-effort asynchronous delete of an input history record by text and timestamp.
    pub fn try_delete_input_history_entry(&self, text: String, created_at_ms: u64) {
        let _ = self.tx.try_send(PersistenceCommand::DeleteInputHistoryEntry {
            text,
            created_at_ms,
            ack: None,
        });
    }

    /// Synchronously delete an input history entry by text and timestamp.
    pub fn delete_input_history_entry_blocking(&self, text: &str, created_at_ms: u64) -> Result<usize> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .blocking_send(PersistenceCommand::DeleteInputHistoryEntry {
                text: text.to_string(),
                created_at_ms,
                ack: Some(ack_tx),
            })
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx
            .blocking_recv()
            .map_err(|_| rusqlite::Error::ToSqlConversionFailure("persistence thread panicked".into()))?
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
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, CURRENT_DB_VERSION);
    }

    #[test]
    fn working_state_round_trips_through_the_session_row() {
        let engine = DatabaseEngine::open_in_memory(None).unwrap();
        engine
            .conn
            .execute(
                "INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_ms, updated_at_ms, project_root, msg_count, last_user_prompt, digest) VALUES ('s1', NULL, 'trunk', 'T', 1, 1, '/tmp', 0, NULL, NULL)",
                [],
            )
            .unwrap();
        let sessions = engine.list_sessions(None).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title.as_deref(), Some("T"));
    }

    #[test]
    fn events_ledger_append_and_replay() {
        let engine = DatabaseEngine::open_in_memory(None).unwrap();
        engine
            .conn
            .execute(
                "INSERT INTO sessions (id, parent_id, fork_kind, title, created_at_ms, updated_at_ms, project_root, msg_count, last_user_prompt, digest) VALUES ('s1', NULL, 'trunk', NULL, 1, 1, '/tmp', 0, NULL, NULL)",
                [],
            )
            .unwrap();
        engine
            .append_event(&SessionEventRecord {
                session_id: "s1".into(),
                seq: 0,
                event_type: "test".into(),
                payload: "{}".into(),
                created_at_ms: 1,
            })
            .unwrap();
        let events = engine.get_session_events("s1").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "test");
    }
}
