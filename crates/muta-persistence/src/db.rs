//! Embedded SQLite storage engine for Muta sessions and transactional metadata.
//!
//! Provides relational database initialization, schema migration tracking
//! via `PRAGMA user_version`, and connection pool lifecycle management.

use rusqlite::{Connection, Result};
use std::path::Path;
use tracing::info;

/// SQLite schema version tracking. Fresh databases jump straight to the latest version.
const CURRENT_DB_VERSION: u32 = 1;

/// Initialize and return a connection to the SQLite database.
/// Configures WAL mode, synchronous=NORMAL for robustness, and turns on foreign keys.
pub fn initialize_db(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut conn = Connection::open(db_path)?;

    // Uncompromising performance and safety configurations
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    migrate_schema(&mut conn)?;

    Ok(conn)
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
            fork_kind TEXT CHECK(fork_kind IN ('trunk', 'fork', 'aside')) NOT NULL,
            title TEXT,
            title_manual BOOLEAN NOT NULL DEFAULT 0,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            project_root TEXT NOT NULL
        );

        -- Messages table
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

        -- Command execution audit ledger
        CREATE TABLE IF NOT EXISTS commands (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            arguments TEXT NOT NULL,
            result TEXT,
            status TEXT CHECK(status IN ('running', 'ok', 'failed', 'cancelled')) NOT NULL,
            created_at_ms INTEGER NOT NULL
        );

        -- Unified Key-Value Store table
        CREATE TABLE IF NOT EXISTS kv_store (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );
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
}
