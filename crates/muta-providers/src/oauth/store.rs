//! Strict, transactional OAuth credential storage keyed by connection id.
//!
//! A missing `auth.toml` is a normal first-run condition. Every other read or
//! decode failure is explicit: treating corruption as an empty store could let
//! a later write silently erase every login.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use muta_contracts::SecretString;
use muta_persistence::paths;
use serde::{Deserialize, Serialize};

/// One connection's OAuth token set and associated metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access: SecretString,
    pub refresh: SecretString,
    /// Unix epoch milliseconds when the access token expires.
    pub expires_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<SecretString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_email: Option<String>,
}

impl TokenSet {
    pub fn is_valid(&self) -> bool {
        !self.access.expose_secret().trim().is_empty()
    }
}

/// All token sets. Runtime lookup is exact; preset/provider fallback keys are
/// not consulted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthStore {
    #[serde(default)]
    pub tokens: BTreeMap<String, TokenSet>,
}

#[derive(Debug)]
pub enum AuthStoreError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    Serialize(toml::ser::Error),
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    Lock {
        path: PathBuf,
        source: std::io::Error,
    },
    Join(String),
}

impl std::fmt::Display for AuthStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => write!(
                f,
                "could not read OAuth credential store {}: {source}",
                path.display()
            ),
            Self::Parse { path, source } => write!(
                f,
                "OAuth credential store {} is malformed: {source}",
                path.display()
            ),
            Self::Serialize(source) => write!(f, "could not encode OAuth credentials: {source}"),
            Self::Write { path, source } => write!(
                f,
                "could not persist OAuth credential store {}: {source}",
                path.display()
            ),
            Self::Lock { path, source } => write!(
                f,
                "could not lock OAuth credential store {}: {source}",
                path.display()
            ),
            Self::Join(message) => write!(f, "OAuth credential lock task failed: {message}"),
        }
    }
}

impl std::error::Error for AuthStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } | Self::Lock { source, .. } => {
                Some(source)
            }
            Self::Parse { source, .. } => Some(source),
            Self::Serialize(source) => Some(source),
            Self::Join(_) => None,
        }
    }
}

/// Exclusive cross-process transaction over `auth.toml`.
///
/// The companion-file lock stays held until this value is dropped, including
/// during refresh-token exchange. This serializes rotating refresh tokens
/// across all muta processes and makes the subsequent write atomic as a unit.
pub struct LockedAuthStore {
    path: PathBuf,
    store: AuthStore,
    _lock: muta_persistence::fsutil::FileLock,
}

impl AuthStore {
    fn path() -> PathBuf {
        paths::get().auth_file()
    }

    /// Strictly read `auth.toml`; only `NotFound` maps to an empty store.
    pub fn load() -> Result<Self, AuthStoreError> {
        Self::load_from_path(&Self::path())
    }

    pub fn load_from_path(path: &Path) -> Result<Self, AuthStoreError> {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(AuthStoreError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        toml::from_str(&content).map_err(|source| AuthStoreError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    fn save_to_path(&self, path: &Path) -> Result<(), AuthStoreError> {
        let bytes = toml::to_string_pretty(self)
            .map_err(AuthStoreError::Serialize)?
            .into_bytes();
        muta_persistence::fsutil::atomic_write_bytes(path, &bytes).map_err(|source| {
            AuthStoreError::Write {
                path: path.to_path_buf(),
                source,
            }
        })
    }

    /// Acquire a real cross-process lock without blocking a Tokio worker, then
    /// re-read the store under the lock.
    pub async fn lock() -> Result<LockedAuthStore, AuthStoreError> {
        let path = Self::path();
        let lock_path = path.clone();
        let lock = tokio::task::spawn_blocking(move || {
            muta_persistence::fsutil::FileLock::acquire(&lock_path).map_err(|source| {
                AuthStoreError::Lock {
                    path: lock_path,
                    source,
                }
            })
        })
        .await
        .map_err(|error| AuthStoreError::Join(error.to_string()))??;
        let store = Self::load_from_path(&path)?;
        Ok(LockedAuthStore {
            path,
            store,
            _lock: lock,
        })
    }

    pub fn get(&self, connection_id: &str) -> Option<&TokenSet> {
        self.tokens.get(connection_id)
    }

    pub fn set(&mut self, connection_id: &str, tokens: TokenSet) {
        if !tokens.is_valid() {
            tracing::warn!(
                connection_id = %connection_id,
                "attempted to persist empty access token into auth store; ignoring"
            );
            return;
        }
        self.tokens.insert(connection_id.to_string(), tokens);
    }

    pub fn remove(&mut self, connection_id: &str) -> Option<TokenSet> {
        self.tokens.remove(connection_id)
    }
}

impl LockedAuthStore {
    pub fn get(&self, connection_id: &str) -> Option<&TokenSet> {
        self.store.get(connection_id)
    }

    pub fn set(&mut self, connection_id: &str, tokens: TokenSet) {
        self.store.set(connection_id, tokens);
    }

    pub fn remove(&mut self, connection_id: &str) -> Option<TokenSet> {
        self.store.remove(connection_id)
    }

    pub fn save(&self) -> Result<(), AuthStoreError> {
        self.store.save_to_path(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(access: &str) -> TokenSet {
        TokenSet {
            access: access.into(),
            refresh: format!("{access}-refresh").into(),
            expires_ms: 1_700_000_000_000,
            account_id: None,
            id_token: None,
            token_type: Some("Bearer".into()),
            scope: None,
            project_id: None,
            user_email: None,
        }
    }

    #[test]
    fn round_trips_exact_connection_namespaces() {
        let mut store = AuthStore::default();
        store.set("personal-chatgpt", tokens("personal"));
        store.set("work-chatgpt", tokens("work"));
        let serialized = toml::to_string_pretty(&store).unwrap();
        let reparsed: AuthStore = toml::from_str(&serialized).unwrap();
        assert_eq!(reparsed.get("personal-chatgpt").unwrap().access, "personal");
        assert_eq!(reparsed.get("work-chatgpt").unwrap().access, "work");
        assert!(reparsed.get("chatgpt").is_none());
    }

    #[test]
    fn malformed_store_is_an_error_not_an_empty_store() {
        let dir = std::env::temp_dir().join(format!("muta-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.toml");
        std::fs::write(&path, "[tokens\ninvalid").unwrap();
        assert!(matches!(
            AuthStore::load_from_path(&path),
            Err(AuthStoreError::Parse { .. })
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_store_is_empty() {
        let path = std::env::temp_dir()
            .join(format!("muta-auth-{}", uuid::Uuid::new_v4()))
            .join("auth.toml");
        assert!(AuthStore::load_from_path(&path).unwrap().tokens.is_empty());
    }

    #[test]
    fn rejects_empty_access_token_insertion() {
        let mut store = AuthStore::default();
        store.set("empty-conn", tokens(""));
        assert!(store.get("empty-conn").is_none());
        store.set("whitespace-conn", tokens("   "));
        assert!(store.get("whitespace-conn").is_none());
    }

    #[tokio::test]
    async fn concurrent_locked_read_modify_write_does_not_lose_tokens() {
        let dir =
            std::env::temp_dir().join(format!("muta-auth-concurrency-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.toml");

        let mut handles = Vec::new();
        for i in 0..5 {
            let p = path.clone();
            handles.push(tokio::spawn(async move {
                let lock = muta_persistence::fsutil::FileLock::acquire(&p)
                    .map_err(|e| e.to_string())
                    .unwrap();
                let store = AuthStore::load_from_path(&p).unwrap();
                let mut locked = LockedAuthStore {
                    path: p,
                    store,
                    _lock: lock,
                };
                let id = format!("connection-{}", i);
                locked.set(&id, tokens(&format!("access-{}", i)));
                locked.save().unwrap();
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        let final_store = AuthStore::load_from_path(&path).unwrap();
        for i in 0..5 {
            let id = format!("connection-{}", i);
            assert_eq!(
                final_store.get(&id).unwrap().access,
                format!("access-{}", i).as_str()
            );
        }

        let _ = std::fs::remove_dir_all(dir);
    }
}
