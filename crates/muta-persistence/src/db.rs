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
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

/// SQLite schema version tracking. Fresh databases jump straight to the latest version.
pub const CURRENT_DB_VERSION: u32 = 1;

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
            INSERT INTO sessions (id, parent_id, fork_kind, title, title_manual, created_at_ms, updated_at_ms, project_root)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(id) DO UPDATE SET
                parent_id = excluded.parent_id,
                fork_kind = excluded.fork_kind,
                title = excluded.title,
                title_manual = excluded.title_manual,
                updated_at_ms = excluded.updated_at_ms,
                project_root = excluded.project_root;
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
            ],
        )?;
        Ok(())
    }

    /// Retrieve a single session by id.
    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        self.conn
            .query_row(
                "SELECT id, parent_id, fork_kind, title, title_manual, created_at_ms, updated_at_ms, project_root FROM sessions WHERE id = ?1",
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
                "SELECT id, parent_id, fork_kind, title, title_manual, created_at_ms, updated_at_ms, project_root \
                 FROM sessions WHERE project_root = ?1 ORDER BY updated_at_ms DESC",
            )?;
            let rows = stmt.query_map(params![root], map_session_row)?;
            for session in rows {
                sessions.push(session?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, parent_id, fork_kind, title, title_manual, created_at_ms, updated_at_ms, project_root \
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
}

// --- Asynchronous Persistence Actor (Single-Writer Pattern) ---

/// Command variants dispatched to the single-writer persistence actor.
pub enum PersistenceCommand {
    UpsertSession {
        record: SessionRecord,
        ack: oneshot::Sender<Result<()>>,
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
}

/// Asynchronous handle for interacting with the single-writer PersistenceActor without blocking Tokio runtime.
#[derive(Clone)]
pub struct PersistenceHandle {
    tx: mpsc::Sender<PersistenceCommand>,
    db_path: PathBuf,
    blob_store: Option<BlobStore>,
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
                        PersistenceCommand::UpsertSession { record, ack } => {
                            let res = engine.upsert_session(&record);
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
        };

        handle.upsert_session(session.clone()).await.unwrap();

        let reader = handle.open_reader().unwrap();
        let fetched = reader.get_session("sess_async").unwrap().unwrap();
        assert_eq!(fetched, session);
    }
}
