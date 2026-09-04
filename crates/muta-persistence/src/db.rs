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
pub const CURRENT_DB_VERSION: u32 = 2;

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
    pub title_manual: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub project_root: String,
    #[serde(default)]
    pub data: Option<String>,
}

/// Materialized message record in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageRecord {
    pub id: String,
    pub session_id: String,
    pub seq: i64,
    pub role: String,
    pub content: String,
    pub content_blob_hash: Option<String>,
    pub reasoning_content: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub created_at_ms: i64,
}

/// Command audit record in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandRecord {
    pub id: String,
    pub session_id: String,
    pub name: String,
    pub arguments: String,
    pub result: Option<String>,
    pub status: String,
    pub created_at_ms: i64,
}

/// Monotonic session event record in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEventRecord {
    pub session_id: String,
    pub seq: i64,
    pub event_type: String,
    pub payload: String,
    pub created_at_ms: i64,
}

/// History search result item from FTS5 queries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistorySearchResult {
    pub message_id: String,
    pub session_id: String,
    pub project_root: String,
    pub role: String,
    pub snippet: String,
    pub score: f64,
}

fn map_session_row(row: &Row) -> Result<SessionRecord> {
    Ok(SessionRecord {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        fork_kind: row.get(2)?,
        title: row.get(3)?,
        title_manual: row.get(4)?,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
        project_root: row.get(7)?,
        data: row.get(8)?,
    })
}

fn map_search_row(row: &Row) -> Result<HistorySearchResult> {
    Ok(HistorySearchResult {
        message_id: row.get(0)?,
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

impl DatabaseEngine {
    /// Open or create a database engine on a file path.
    pub fn open(db_path: &Path, blob_store: Option<BlobStore>) -> Result<Self> {
        let conn = initialize_db(db_path)?;
        Ok(Self { conn, blob_store })
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

    // --- Session Operations ---

    /// Create or update a session record.
    pub fn upsert_session(&self, session: &SessionRecord) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO sessions (id, parent_id, fork_kind, title, title_manual, created_at_ms, updated_at_ms, project_root, data)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(id) DO UPDATE SET
                parent_id = excluded.parent_id,
                fork_kind = excluded.fork_kind,
                title = excluded.title,
                title_manual = excluded.title_manual,
                updated_at_ms = excluded.updated_at_ms,
                project_root = excluded.project_root,
                data = excluded.data;
            "#,
            params![
                session.id,
                session.parent_id,
                session.fork_kind,
                session.title,
                session.title_manual,
                session.created_at_ms,
                session.updated_at_ms,
                session.project_root,
                session.data,
            ],
        )?;
        Ok(())
    }

    /// Retrieve a single session by id.
    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        self.conn
            .query_row(
                "SELECT id, parent_id, fork_kind, title, title_manual, created_at_ms, updated_at_ms, project_root, data FROM sessions WHERE id = ?1",
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
                "SELECT id, parent_id, fork_kind, title, title_manual, created_at_ms, updated_at_ms, project_root, data \
                 FROM sessions WHERE project_root = ?1 ORDER BY updated_at_ms DESC",
            )?;
            let rows = stmt.query_map(params![root], map_session_row)?;
            for session in rows {
                sessions.push(session?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, parent_id, fork_kind, title, title_manual, created_at_ms, updated_at_ms, project_root, data \
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

    /// Persist a complete [`crate::session::SessionData`] record into SQLite in a single transaction,
    /// synchronizing the `sessions` table, `messages` table (and FTS5 index).
    pub(crate) fn save_session_full(&self, data: &crate::session::SessionData) -> Result<()> {
        let fork_str = match data.fork_kind {
            muta_contracts::SessionForkKind::Trunk => "trunk",
            muta_contracts::SessionForkKind::Fork => "fork",
            muta_contracts::SessionForkKind::Aside => "aside",
        };
        let serialized_data = serde_json::to_string(data)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        let session_rec = SessionRecord {
            id: data.id.clone(),
            parent_id: data.parent_id.clone(),
            fork_kind: fork_str.to_string(),
            title: data.title.clone(),
            title_manual: data.title_manual,
            created_at_ms: data.created_at as i64,
            updated_at_ms: data.updated_at as i64,
            project_root: data.project_root.to_string_lossy().into_owned(),
            data: Some(serialized_data),
        };

        // Execute in transaction
        self.conn.execute("BEGIN IMMEDIATE", [])?;

        let res: Result<()> = (|| {
            self.upsert_session(&session_rec)?;

            // Synchronize messages for this session
            self.conn.execute("DELETE FROM messages WHERE session_id = ?1", params![session_rec.id])?;

            for (seq, msg) in data.model_window.iter().enumerate() {
                let role_str = match msg.role {
                    muta_contracts::Role::User => "user",
                    muta_contracts::Role::Assistant => "assistant",
                    muta_contracts::Role::System => "system",
                    muta_contracts::Role::Tool => "tool",
                };
                self.insert_message(MessageRecord {
                    id: format!("{}:{}", session_rec.id, seq),
                    session_id: session_rec.id.clone(),
                    seq: seq as i64,
                    role: role_str.to_string(),
                    content: msg.content.clone(),
                    content_blob_hash: msg.content_blob.clone(),
                    reasoning_content: msg.reasoning_content.clone(),
                    provider: None,
                    model: None,
                    created_at_ms: session_rec.updated_at_ms,
                })?;
            }

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

    /// Load a full [`crate::session::SessionData`] record by session ID from SQLite.
    pub(crate) fn load_session_full(&self, session_id: &str) -> Result<Option<crate::session::SessionData>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT data FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(raw) = raw {
            let data: crate::session::SessionData = serde_json::from_str(&raw)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            Ok(Some(data))
        } else {
            Ok(None)
        }
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
        let mut summaries = Vec::new();
        let sessions = self.list_sessions(project_root)?;
        for rec in sessions {
            if let Some(ref data_json) = rec.data {
                if let Ok(data) = serde_json::from_str::<crate::session::SessionData>(data_json) {
                    let msg_count = data.model_window.len() + data.archived_transcript.len();
                    if msg_count == 0 && data.id != active_id {
                        continue;
                    }
                    summaries.push(crate::session::summary_from_data(&data, data.id == active_id));
                }
            } else {
                let fork_kind = match rec.fork_kind.as_str() {
                    "fork" => muta_contracts::SessionForkKind::Fork,
                    "aside" => muta_contracts::SessionForkKind::Aside,
                    _ => muta_contracts::SessionForkKind::Trunk,
                };
                let overview = rec.title.clone().unwrap_or_else(|| "(empty session)".to_string());
                summaries.push(crate::session::SessionSummary {
                    id: rec.id.clone(),
                    parent_id: rec.parent_id,
                    fork_kind,
                    message_count: 0,
                    updated_at: rec.updated_at_ms as u64,
                    created_at: rec.created_at_ms as u64,
                    overview,
                    active: rec.id == active_id,
                    digest: None,
                });
            }
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
                message_count: data.model_window.len() + data.archived_transcript.len(),
                active: data.id == active_id,
                last_prompt,
            }))
        } else {
            Ok(None)
        }
    }

    /// Rename a session in the database.
    pub fn rename_session(&self, session_id: &str, title: Option<&str>, manual: bool) -> Result<bool> {
        let now = Utc::now().timestamp_millis();
        if let Some(mut data) = self.load_session_full(session_id)? {
            data.title = title.map(|s| s.to_string());
            data.title_manual = manual;
            data.updated_at = now as u64;
            self.save_session_full(&data)?;
            return Ok(true);
        }
        let affected = self.conn.execute(
            "UPDATE sessions SET title = ?1, title_manual = ?2, updated_at_ms = ?3 WHERE id = ?4",
            params![title, manual, now, session_id],
        )?;
        Ok(affected > 0)
    }

    /// Discover every persisted session (across all project buckets) that has armed `/schedule` jobs.
    pub fn list_armed_schedule_sessions(&self) -> Result<Vec<(String, PathBuf)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_root, data FROM sessions WHERE data LIKE '%\"scheduled_jobs\":[%'",
        )?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let project_root: String = row.get(1)?;
            let data_str: Option<String> = row.get(2)?;
            Ok((id, PathBuf::from(project_root), data_str))
        })?;

        let mut armed = Vec::new();
        for item in rows {
            let (id, root, data_str) = item?;
            if let Some(raw) = data_str {
                if let Ok(data) = serde_json::from_str::<crate::session::SessionData>(&raw) {
                    if !data.scheduled_jobs.is_empty() {
                        armed.push((id, root));
                    }
                }
            }
        }
        Ok(armed)
    }

    // --- Event Ledger Operations (ADR-0163) ---

    /// Append a single event to the monotonic event ledger.
    pub fn append_event(&self, event: &SessionEventRecord) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO session_events (session_id, seq, event_type, payload, created_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5);
            "#,
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
             FROM session_events WHERE session_id = ?1 ORDER BY seq ASC",
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

    // --- Message & CAS Operations ---

    /// Insert or replace a message record, automatically offloading content exceeding
    /// `CAS_THRESHOLD_BYTES` to the CAS `BlobStore` if configured.
    pub fn insert_message(&self, mut message: MessageRecord) -> Result<()> {
        let content_bytes = message.content.as_bytes();
        if content_bytes.len() > CAS_THRESHOLD_BYTES
            && let Some(ref blob_store) = self.blob_store
            && let Ok(hash) = blob_store.put(content_bytes)
        {
            message.content_blob_hash = Some(hash);
        }

        self.conn.execute(
            r#"
            INSERT INTO messages (id, session_id, seq, role, content, content_blob_hash, reasoning_content, provider, model, created_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(id) DO UPDATE SET
                content = excluded.content,
                content_blob_hash = excluded.content_blob_hash,
                reasoning_content = excluded.reasoning_content,
                provider = excluded.provider,
                model = excluded.model;
            "#,
            params![
                message.id,
                message.session_id,
                message.seq,
                message.role,
                message.content,
                message.content_blob_hash,
                message.reasoning_content,
                message.provider,
                message.model,
                message.created_at_ms,
            ],
        )?;
        Ok(())
    }

    /// Retrieve all messages for a session, resolving blob hashes if necessary.
    pub fn get_messages(&self, session_id: &str) -> Result<Vec<MessageRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, seq, role, content, content_blob_hash, reasoning_content, provider, model, created_at_ms \
             FROM messages WHERE session_id = ?1 ORDER BY seq ASC",
        )?;

        let rows = stmt.query_map(params![session_id], |row| {
            Ok(MessageRecord {
                id: row.get(0)?,
                session_id: row.get(1)?,
                seq: row.get(2)?,
                role: row.get(3)?,
                content: row.get(4)?,
                content_blob_hash: row.get(5)?,
                reasoning_content: row.get(6)?,
                provider: row.get(7)?,
                model: row.get(8)?,
                created_at_ms: row.get(9)?,
            })
        })?;

        let mut messages = Vec::new();
        for msg in rows {
            let mut record = msg?;
            if let (Some(hash), Some(blob_store)) = (&record.content_blob_hash, &self.blob_store)
                && let Some(blob) = blob_store.get(hash)
                && let Ok(text) = String::from_utf8(blob)
            {
                record.content = text;
            }
            messages.push(record);
        }
        Ok(messages)
    }

    // --- Command Ledger Operations (ADR-0091) ---

    /// Insert or update a command audit record.
    pub fn record_command(&self, cmd: &CommandRecord) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO commands (id, session_id, name, arguments, result, status, created_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO UPDATE SET
                arguments = excluded.arguments,
                result = excluded.result,
                status = excluded.status;
            "#,
            params![
                cmd.id,
                cmd.session_id,
                cmd.name,
                cmd.arguments,
                cmd.result,
                cmd.status,
                cmd.created_at_ms,
            ],
        )?;
        Ok(())
    }

    /// Retrieve all command records for a session.
    pub fn get_commands(&self, session_id: &str) -> Result<Vec<CommandRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, name, arguments, result, status, created_at_ms \
             FROM commands WHERE session_id = ?1 ORDER BY created_at_ms ASC",
        )?;

        let rows = stmt.query_map(params![session_id], |row| {
            Ok(CommandRecord {
                id: row.get(0)?,
                session_id: row.get(1)?,
                name: row.get(2)?,
                arguments: row.get(3)?,
                result: row.get(4)?,
                status: row.get(5)?,
                created_at_ms: row.get(6)?,
            })
        })?;

        let mut commands = Vec::new();
        for cmd in rows {
            commands.push(cmd?);
        }
        Ok(commands)
    }

    // --- Key-Value Operations ---

    /// Put a key-value entry.
    pub fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        self.conn.execute(
            r#"
            INSERT INTO kv_store (key, value, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = excluded.updated_at;
            "#,
            params![key, value, now],
        )?;
        Ok(())
    }

    /// Get a value by key.
    pub fn get_kv(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM kv_store WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
    }

    /// Delete a key-value entry.
    pub fn delete_kv(&self, key: &str) -> Result<bool> {
        let affected = self
            .conn
            .execute("DELETE FROM kv_store WHERE key = ?1", params![key])?;
        Ok(affected > 0)
    }

    // --- FTS5 Full-Text History Search (proto.muta.v1.MutaService/SearchHistory) ---

    /// Perform BM25 full-text search across messages, optionally filtered by workspace root.
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
                    f.message_id,
                    f.session_id,
                    s.project_root,
                    f.role,
                    snippet(fts_messages, 3, '<b>', '</b>', '...', 16) AS snippet,
                    bm25(fts_messages) AS score
                FROM fts_messages f
                JOIN sessions s ON f.session_id = s.id
                WHERE fts_messages MATCH ?1 AND s.project_root = ?2
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
                    f.message_id,
                    f.session_id,
                    s.project_root,
                    f.role,
                    snippet(fts_messages, 3, '<b>', '</b>', '...', 16) AS snippet,
                    bm25(fts_messages) AS score
                FROM fts_messages f
                JOIN sessions s ON f.session_id = s.id
                WHERE fts_messages MATCH ?1
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

    // --- Typed JSON KV Helpers (ADR-0168) ---

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

    // --- Legacy Flat-File Migration (ADR-0168) ---

    /// Inspect legacy `projects/` directory and import older `.json` / `.jsonl` sessions into SQLite.
    /// Idempotent: existing sessions are skipped or updated without duplicate event sequences.
    pub fn migrate_legacy_projects(&self, projects_dir: &Path) -> usize {
        let Ok(entries) = std::fs::read_dir(projects_dir) else {
            return 0;
        };

        let mut count = 0;
        for bucket in entries.flatten() {
            let sessions_dir = bucket.path().join("sessions");
            let Ok(session_files) = std::fs::read_dir(&sessions_dir) else {
                continue;
            };

            for file_entry in session_files.flatten() {
                let path = file_entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }

                // Parse legacy snapshot
                let Ok(raw_json) = std::fs::read_to_string(&path) else {
                    continue;
                };

                #[derive(Deserialize)]
                struct LegacySnapshot {
                    id: String,
                    #[serde(default)]
                    parent_id: Option<String>,
                    #[serde(default)]
                    fork_kind: Option<String>,
                    #[serde(default)]
                    title: Option<String>,
                    #[serde(default)]
                    title_manual: bool,
                    #[serde(default)]
                    created_at: Option<u64>,
                    #[serde(default)]
                    updated_at: Option<u64>,
                    #[serde(default)]
                    project_root: Option<PathBuf>,
                }

                if let Ok(snap) = serde_json::from_str::<LegacySnapshot>(&raw_json) {
                    let project_root_str = snap
                        .project_root
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let record = SessionRecord {
                        id: snap.id.clone(),
                        parent_id: snap.parent_id,
                        fork_kind: snap.fork_kind.unwrap_or_else(|| "trunk".to_string()),
                        title: snap.title,
                        title_manual: snap.title_manual,
                        created_at_ms: snap.created_at.map(|t| t as i64).unwrap_or(0),
                        updated_at_ms: snap.updated_at.map(|t| t as i64).unwrap_or(0),
                        project_root: project_root_str,
                        data: Some(raw_json.clone()),
                    };

                    if self.upsert_session(&record).is_ok() {
                        count += 1;
                    }

                    // Parse sibling JSONL if present
                    let jsonl_path = path.with_extension("jsonl");
                    if let Ok(lines) = std::fs::read_to_string(&jsonl_path) {
                        for (seq, line) in lines.lines().enumerate() {
                            if line.trim().is_empty() {
                                continue;
                            }
                            let _ = self.append_event(&SessionEventRecord {
                                session_id: snap.id.clone(),
                                seq: seq as i64 + 1,
                                event_type: "legacy_event".to_string(),
                                payload: line.to_string(),
                                created_at_ms: record.updated_at_ms,
                            });
                        }
                    }
                }
            }
        }

        if count > 0 {
            info!(
                count = count,
                "Migrated legacy sessions into SQLite muta.db"
            );
        }
        count
    }
}

// --- Asynchronous Persistence Actor (Single-Writer Pattern) ---

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
    AppendEvent {
        event: SessionEventRecord,
        ack: oneshot::Sender<Result<()>>,
    },
    InsertMessage {
        message: MessageRecord,
        ack: oneshot::Sender<Result<()>>,
    },
    RecordCommand {
        cmd: CommandRecord,
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
                        PersistenceCommand::AppendEvent { event, ack } => {
                            let res = engine.append_event(&event);
                            let _ = ack.send(res);
                        }
                        PersistenceCommand::InsertMessage { message, ack } => {
                            let res = engine.insert_message(message);
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
        self.tx
            .blocking_send(PersistenceCommand::SaveSessionFull {
                data: Box::new(data),
                ack: ack_tx,
            })
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx
            .blocking_recv()
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
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

    /// Non-blocking fire-and-forget message insert to avoid blocking synchronous writers.
    pub fn try_insert_message(&self, message: MessageRecord) {
        let (ack_tx, _) = oneshot::channel();
        let _ = self.tx.try_send(PersistenceCommand::InsertMessage {
            message,
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

    /// Asynchronously insert a message.
    pub async fn insert_message(&self, message: MessageRecord) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::InsertMessage {
                message,
                ack: ack_tx,
            })
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        ack_rx
            .await
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
    }

    /// Asynchronously record a command audit event.
    pub async fn record_command(&self, cmd: CommandRecord) -> Result<()> {
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

    /// Open a lightweight read-only connection snapshot for querying.
    pub fn open_reader(&self) -> Result<DatabaseEngine> {
        DatabaseEngine::open(&self.db_path, self.blob_store.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_db_initialization_and_migrations() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_muta.db");

        let conn = initialize_db(&db_path).expect("failed to init db");

        // Assert journal mode is WAL
        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_uppercase(), "WAL");

        // Assert schema version is correct
        let user_version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(user_version, CURRENT_DB_VERSION);
    }

    #[test]
    fn test_session_lifecycle_and_events() {
        let engine = DatabaseEngine::open_in_memory(None).unwrap();

        let session = SessionRecord {
            id: "sess_001".into(),
            parent_id: None,
            fork_kind: "trunk".into(),
            title: Some("Initial Session".into()),
            title_manual: false,
            created_at_ms: 1000,
            updated_at_ms: 1000,
            project_root: "/workspace/project".into(),
            data: None,
        };

        engine.upsert_session(&session).unwrap();

        let fetched = engine.get_session("sess_001").unwrap().unwrap();
        assert_eq!(fetched, session);

        // Test appending events
        let event = SessionEventRecord {
            session_id: "sess_001".into(),
            seq: 1,
            event_type: "prompt".into(),
            payload: r#"{"text":"hello agent"}"#.into(),
            created_at_ms: 1005,
        };
        engine.append_event(&event).unwrap();

        let events = engine.get_session_events("sess_001").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], event);
    }

    #[test]
    fn test_fts5_history_search() {
        let engine = DatabaseEngine::open_in_memory(None).unwrap();

        let session = SessionRecord {
            id: "sess_search".into(),
            parent_id: None,
            fork_kind: "trunk".into(),
            title: Some("Search Testing".into()),
            title_manual: false,
            created_at_ms: 1000,
            updated_at_ms: 1000,
            project_root: "/workspace/rust-code".into(),
            data: None,
        };
        engine.upsert_session(&session).unwrap();

        let msg = MessageRecord {
            id: "msg_001".into(),
            session_id: "sess_search".into(),
            seq: 1,
            role: "user".into(),
            content: "How do we implement SQLite FTS5 in Rust with Tokio?".into(),
            content_blob_hash: None,
            reasoning_content: Some("User is asking for an FTS5 search design".into()),
            provider: Some("anthropic".into()),
            model: Some("claude-3-7-sonnet".into()),
            created_at_ms: 1010,
        };
        engine.insert_message(msg).unwrap();

        let results = engine
            .search_history("SQLite FTS5", Some("/workspace/rust-code"), 10)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "sess_search");
        assert!(results[0].snippet.contains("SQLite"));
    }

    #[test]
    fn test_cas_blob_threshold_isolation() {
        let dir = tempdir().unwrap();
        let blobs_dir = dir.path().join("blobs");
        let blob_store = BlobStore::new(blobs_dir);

        let engine = DatabaseEngine::open_in_memory(Some(blob_store.clone())).unwrap();

        let session = SessionRecord {
            id: "sess_cas".into(),
            parent_id: None,
            fork_kind: "trunk".into(),
            title: Some("CAS Testing".into()),
            title_manual: false,
            created_at_ms: 1000,
            updated_at_ms: 1000,
            project_root: "/workspace/cas".into(),
            data: None,
        };
        engine.upsert_session(&session).unwrap();

        // Create a large payload > 4KB
        let large_content = "A".repeat(5000);
        let msg = MessageRecord {
            id: "msg_large".into(),
            session_id: "sess_cas".into(),
            seq: 1,
            role: "assistant".into(),
            content: large_content.clone(),
            content_blob_hash: None,
            reasoning_content: None,
            provider: Some("google".into()),
            model: Some("gemini-2.5-pro".into()),
            created_at_ms: 1020,
        };
        engine.insert_message(msg).unwrap();

        // Verify that message content was extracted and reconstructed
        let retrieved = engine.get_messages("sess_cas").unwrap();
        assert_eq!(retrieved.len(), 1);
        assert_eq!(retrieved[0].content, large_content);
        assert!(retrieved[0].content_blob_hash.is_some());
    }

    #[tokio::test]
    async fn test_persistence_actor_async() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("async_muta.db");

        let handle = PersistenceHandle::spawn(db_path, None);

        let session = SessionRecord {
            id: "sess_async".into(),
            parent_id: None,
            fork_kind: "trunk".into(),
            title: Some("Async Actor Session".into()),
            title_manual: true,
            created_at_ms: 2000,
            updated_at_ms: 2000,
            project_root: "/workspace/async".into(),
            data: None,
        };

        handle.upsert_session(session.clone()).await.unwrap();

        let reader = handle.open_reader().unwrap();
        let fetched = reader.get_session("sess_async").unwrap().unwrap();
        assert_eq!(fetched, session);
    }
}
