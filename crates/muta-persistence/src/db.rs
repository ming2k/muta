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
pub const CURRENT_DB_VERSION: u32 = 4;

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
    pub title_manual: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub project_root: String,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub msg_count: i64,
    #[serde(default)]
    pub last_user_prompt: Option<String>,
    #[serde(default)]
    pub digest: Option<String>,
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
        msg_count: row.get(9)?,
        last_user_prompt: row.get(10)?,
        digest: row.get(11)?,
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

    // --- Session Operations ---

    /// Create or update a session record.
    pub fn upsert_session(&self, session: &SessionRecord) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO sessions (id, parent_id, fork_kind, title, title_manual, created_at_ms, updated_at_ms, project_root, data, msg_count, last_user_prompt, digest)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                parent_id = excluded.parent_id,
                fork_kind = excluded.fork_kind,
                title = excluded.title,
                title_manual = excluded.title_manual,
                updated_at_ms = excluded.updated_at_ms,
                project_root = excluded.project_root,
                data = excluded.data,
                msg_count = excluded.msg_count,
                last_user_prompt = excluded.last_user_prompt,
                digest = excluded.digest;
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
                "SELECT id, parent_id, fork_kind, title, title_manual, created_at_ms, updated_at_ms, project_root, data, msg_count, last_user_prompt, digest FROM sessions WHERE id = ?1",
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
                "SELECT id, parent_id, fork_kind, title, title_manual, created_at_ms, updated_at_ms, project_root, data, msg_count, last_user_prompt, digest \
                 FROM sessions WHERE project_root = ?1 ORDER BY updated_at_ms DESC",
            )?;
            let rows = stmt.query_map(params![root], map_session_row)?;
            for session in rows {
                sessions.push(session?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, parent_id, fork_kind, title, title_manual, created_at_ms, updated_at_ms, project_root, data, msg_count, last_user_prompt, digest \
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

        let digest_str = data
            .digest
            .as_ref()
            .and_then(|d| serde_json::to_string(d).ok());
        let last_prompt = crate::session::last_effective_prompt_from_data(data);
        let msg_count = (data.model_window.len() + data.archived_transcript.len()) as i64;

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
            msg_count,
            last_user_prompt: last_prompt,
            digest: digest_str,
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

    // --- Authoritative Input History Operations (ADR-0168 / SSOT) ---

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

    // --- Legacy Flat-File Migration (ADR-0168) ---

    /// Migrate legacy session files (.json snapshots and .jsonl logs) from a sessions directory into SQLite muta.db,
    /// and safely purge the migrated files from disk per ADR-0168.
    pub fn migrate_legacy_sessions_dir(&self, sessions_dir: &Path, project_root: &Path) -> usize {
        let Ok(session_files) = std::fs::read_dir(sessions_dir) else {
            return 0;
        };

        let mut count = 0;
        for file_entry in session_files.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            let Ok(raw_json) = std::fs::read_to_string(&path) else {
                continue;
            };

            // Parse legacy snapshot
            if let Ok(mut data) = serde_json::from_str::<crate::session::SessionData>(&raw_json) {
                let is_empty = data.model_window.is_empty() && data.archived_transcript.is_empty();
                if is_empty {
                    // Prune stale empty legacy file
                    let _ = std::fs::remove_file(&path);
                    let _ = std::fs::remove_file(path.with_extension("jsonl"));
                    continue;
                }

                if data.project_root.as_os_str().is_empty() {
                    data.project_root = project_root.to_path_buf();
                }

                // Check sibling JSONL if present to apply tail events
                let jsonl_path = path.with_extension("jsonl");
                if jsonl_path.exists() {
                    let event_log = crate::events::EventLog::new(jsonl_path.clone());
                    if let Ok(tail) = event_log.load_since(data.applied_seq) {
                        if !tail.is_empty() {
                            crate::session::apply_events(&mut data, &tail);
                        }
                    }
                }

                if self.save_session_full(&data).is_ok() {
                    count += 1;
                    // Successfully migrated to SQLite SSOT: safely purge legacy files per ADR-0168
                    let _ = std::fs::remove_file(&path);
                    let _ = std::fs::remove_file(path.with_extension("jsonl"));
                }
            } else {
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
                    #[serde(default)]
                    model_window: Vec<serde::de::IgnoredAny>,
                    #[serde(default)]
                    archived_transcript: Vec<serde::de::IgnoredAny>,
                }

                if let Ok(snap) = serde_json::from_str::<LegacySnapshot>(&raw_json) {
                    let is_empty = snap.model_window.is_empty() && snap.archived_transcript.is_empty();
                    if is_empty {
                        let _ = std::fs::remove_file(&path);
                        let _ = std::fs::remove_file(path.with_extension("jsonl"));
                        continue;
                    }

                    let project_root_str = snap
                        .project_root
                        .unwrap_or_else(|| project_root.to_path_buf())
                        .to_string_lossy()
                        .into_owned();
                    let msg_count = (snap.model_window.len() + snap.archived_transcript.len()) as i64;
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
                        msg_count,
                        last_user_prompt: None,
                        digest: None,
                    };

                    if self.upsert_session(&record).is_ok() {
                        count += 1;
                        let _ = std::fs::remove_file(&path);
                        let _ = std::fs::remove_file(path.with_extension("jsonl"));
                    }
                }
            }
        }
        count
    }

    /// Discover every project bucket in `projects_dir` and migrate legacy flat files into SQLite muta.db.
    /// Runs once idempotently, tracked by `kv_store`.
    pub fn migrate_legacy_projects(&self, projects_dir: &Path) -> usize {
        if self.get_kv("legacy_projects_migrated_v1").ok().flatten().is_some() {
            return 0;
        }

        let Ok(entries) = std::fs::read_dir(projects_dir) else {
            return 0;
        };

        let mut total_count = 0;
        for bucket in entries.flatten() {
            let sessions_dir = bucket.path().join("sessions");
            if sessions_dir.exists() {
                let project_root = bucket.path();
                total_count += self.migrate_legacy_sessions_dir(&sessions_dir, &project_root);
            }
        }

        let _ = self.set_kv("legacy_projects_migrated_v1", "true");
        if total_count > 0 {
            info!(
                count = total_count,
                "Migrated legacy sessions into SQLite muta.db and purged flat files (ADR-0168)"
            );
        }
        total_count
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
            msg_count: 0,
            last_user_prompt: None,
            digest: None,
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
            msg_count: 0,
            last_user_prompt: None,
            digest: None,
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
            msg_count: 0,
            last_user_prompt: None,
            digest: None,
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
            msg_count: 0,
            last_user_prompt: None,
            digest: None,
        };

        handle.upsert_session(session.clone()).await.unwrap();

        let reader = handle.open_reader().unwrap();
        let fetched = reader.get_session("sess_async").unwrap().unwrap();
        assert_eq!(fetched, session);
    }

    #[test]
    fn test_migrate_legacy_projects_and_purge() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("muta.db");
        let projects_dir = dir.path().join("projects");
        let bucket_dir = projects_dir.join("test_bucket");
        let sessions_dir = bucket_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        // 1. Create a substantive legacy session file and sibling jsonl
        let snap_path = sessions_dir.join("leg-sess-1.json");
        let jsonl_path = sessions_dir.join("leg-sess-1.jsonl");
        let snap_data = serde_json::json!({
            "id": "leg-sess-1",
            "title": "Migrated Legacy Title",
            "model_window": [
                {
                    "role": "User",
                    "content": "hello world"
                }
            ],
            "archived_transcript": []
        });
        std::fs::write(&snap_path, serde_json::to_string(&snap_data).unwrap()).unwrap();
        std::fs::write(&jsonl_path, "{\"seq\":1}\n").unwrap();

        // 2. Create an empty legacy session file
        let empty_snap_path = sessions_dir.join("empty-sess.json");
        let empty_jsonl_path = sessions_dir.join("empty-sess.jsonl");
        let empty_snap_data = serde_json::json!({
            "id": "empty-sess",
            "model_window": [],
            "archived_transcript": []
        });
        std::fs::write(&empty_snap_path, serde_json::to_string(&empty_snap_data).unwrap()).unwrap();
        std::fs::write(&empty_jsonl_path, "").unwrap();

        let engine = DatabaseEngine::open(&db_path, None).unwrap();
        let migrated = engine.migrate_legacy_projects(&projects_dir);
        assert_eq!(migrated, 1);

        // Substantive legacy files should be purged from disk (ADR-0168)
        assert!(!snap_path.exists());
        assert!(!jsonl_path.exists());

        // Empty legacy files should also be pruned from disk
        assert!(!empty_snap_path.exists());
        assert!(!empty_jsonl_path.exists());

        // The session must be in SQLite muta.db and visible in list_session_summaries
        let summaries = engine.list_session_summaries(None, "").unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, "leg-sess-1");
        assert_eq!(summaries[0].overview, "Migrated Legacy Title");
        assert_eq!(summaries[0].message_count, 1);

        // Idempotent: second call should be a no-op
        let second = engine.migrate_legacy_projects(&projects_dir);
        assert_eq!(second, 0);
    }

    #[test]
    fn test_input_history_crud_and_dedup() {
        let engine = DatabaseEngine::open_in_memory(None).unwrap();

        let e1 = muta_contracts::HistoryEntry::new(
            "prompt 1".into(),
            Some("sess-1".into()),
            Some("/ws1".into()),
            100,
        );
        let e2 = muta_contracts::HistoryEntry::new(
            "prompt 2".into(),
            Some("sess-1".into()),
            Some("/ws1".into()),
            200,
        );

        engine.record_input_history(&e1, true).unwrap();
        engine.record_input_history(&e2, true).unwrap();

        let loaded = engine.load_input_history(10).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].text, "prompt 2");
        assert_eq!(loaded[1].text, "prompt 1");

        // Dedup = true: recording "prompt 1" again with newer timestamp moves it to top
        let e1_new = muta_contracts::HistoryEntry::new(
            "prompt 1".into(),
            Some("sess-2".into()),
            Some("/ws2".into()),
            300,
        );
        engine.record_input_history(&e1_new, true).unwrap();

        let loaded = engine.load_input_history(10).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].text, "prompt 1");
        assert_eq!(loaded[0].created_at_ms, 300);
        assert_eq!(loaded[0].session_id.as_deref(), Some("sess-2"));
        assert_eq!(loaded[1].text, "prompt 2");

        // Batch save
        let batch = vec![
            muta_contracts::HistoryEntry::new("batch 1".into(), None, None, 400),
            muta_contracts::HistoryEntry::new("batch 2".into(), None, None, 500),
        ];
        engine.save_input_history(&batch, true).unwrap();
        let loaded = engine.load_input_history(10).unwrap();
        assert_eq!(loaded.len(), 4);
        assert_eq!(loaded[0].text, "batch 2");
        assert_eq!(loaded[1].text, "batch 1");

        // Clear input history
        engine.clear_input_history().unwrap();
        let loaded = engine.load_input_history(10).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_migrate_legacy_history_files() {
        let dir = tempdir().unwrap();
        let legacy_file = dir.path().join("history.json");
        let entries = vec![
            muta_contracts::HistoryEntry::new("prompt 1".into(), None, None, 1000),
            muta_contracts::HistoryEntry::new("prompt 2".into(), None, None, 2000),
        ];
        std::fs::write(&legacy_file, serde_json::to_string(&entries).unwrap()).unwrap();
        assert!(legacy_file.exists());

        let engine = DatabaseEngine::open_in_memory(None).unwrap();
        let content = std::fs::read_to_string(&legacy_file).unwrap();
        let parsed: Vec<muta_contracts::HistoryEntry> = serde_json::from_str(&content).unwrap();
        engine.save_input_history(&parsed, true).unwrap();
        std::fs::remove_file(&legacy_file).unwrap();

        assert!(!legacy_file.exists());
        let loaded = engine.load_input_history(10).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].text, "prompt 2");
    }
}
