//! Persistent storage for OAuth token sets, keyed by provider id.
//!
//! Mirrors `credentials.toml`'s separation-of-concerns: `config.toml` holds the
//! provider *definitions* (which channel uses OAuth, which uses an API key),
//! while the *live tokens* (access/refresh/expires) live in `auth.toml` (0600).
//! A missing or unparseable file is a normal first-run condition: best-effort
//! load returns an empty store and never blocks startup.

use std::collections::BTreeMap;
use std::fs;

use neenee_store::paths;
use serde::{Deserialize, Serialize};

/// One provider's OAuth token set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    /// The bearer access token sent as `Authorization: Bearer <access>`.
    pub access: String,
    /// The refresh token used to rotate the access token. xAI rotates these,
    /// so every successful refresh updates this field on disk.
    pub refresh: String,
    /// Unix epoch milliseconds when the access token expires (best-effort; some
    /// providers don't always return `expires_in`, so the JWT `exp` check is
    /// the load-bearing freshness signal at request time).
    pub expires_ms: i64,
    /// Provider-specific account identifier captured at login. For ChatGPT this
    /// is the `chatgpt_account_id` (sent as the `ChatGPT-Account-Id` header on
    /// inference calls). `None`/absent for providers that don't use it (xAI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

/// All stored token sets, keyed by provider id (`"xai"`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthStore {
    #[serde(default)]
    pub tokens: BTreeMap<String, TokenSet>,
}

impl AuthStore {
    fn path() -> std::path::PathBuf {
        paths::get().auth_file()
    }

    /// Read `auth.toml`, returning an empty store when the file is missing or
    /// unparseable. A corrupt secrets file must never block startup, so this is
    /// best-effort and only logs a warning.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(content) = fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str(&content) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not parse auth file; ignoring",
                );
                Self::default()
            }
        }
    }

    /// Persist atomically with owner-only permissions (0600) via
    /// [`neenee_store::fsutil::atomic_write_bytes`]. An empty store writes an
    /// empty file so the on-disk state is always valid.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let bytes = toml::to_string_pretty(self)?.into_bytes();
        neenee_store::fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    /// Get the token set for a provider id, if present.
    pub fn get(&self, provider_id: &str) -> Option<&TokenSet> {
        self.tokens.get(provider_id)
    }

    /// Insert or replace a provider's token set.
    pub fn set(&mut self, provider_id: &str, tokens: TokenSet) {
        self.tokens.insert(provider_id.to_string(), tokens);
    }

    /// Remove a provider's token set (logout).
    pub fn remove(&mut self, provider_id: &str) -> Option<TokenSet> {
        self.tokens.remove(provider_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let mut store = AuthStore::default();
        store.set(
            "xai",
            TokenSet {
                access: "acc".to_string(),
                refresh: "ref".to_string(),
                expires_ms: 1_700_000_000_000,
                account_id: None,
            },
        );
        let serialized = toml::to_string_pretty(&store).unwrap();
        let reparsed: AuthStore = toml::from_str(&serialized).unwrap();
        let tokens = reparsed.get("xai").unwrap();
        assert_eq!(tokens.access, "acc");
        assert_eq!(tokens.refresh, "ref");
        assert_eq!(tokens.expires_ms, 1_700_000_000_000);
        // Round-trips the [tokens] table shape.
        assert!(serialized.contains("[tokens.xai]"));
    }

    #[test]
    fn empty_store_round_trips() {
        let store = AuthStore::default();
        let s = toml::to_string_pretty(&store).unwrap();
        let reparsed: AuthStore = toml::from_str(&s).unwrap();
        assert!(reparsed.tokens.is_empty());
    }

    #[test]
    fn set_replace_remove() {
        let mut store = AuthStore::default();
        assert!(store.get("xai").is_none());
        store.set(
            "xai",
            TokenSet {
                access: "a".into(),
                refresh: "r".into(),
                expires_ms: 0,
                account_id: None,
            },
        );
        assert!(store.get("xai").is_some());
        // Replace.
        store.set(
            "xai",
            TokenSet {
                access: "a2".into(),
                refresh: "r2".into(),
                expires_ms: 1,
                account_id: Some("acct-1".into()),
            },
        );
        assert_eq!(store.get("xai").unwrap().access, "a2");
        assert_eq!(
            store.get("xai").unwrap().account_id.as_deref(),
            Some("acct-1")
        );
        // Remove.
        assert!(store.remove("xai").is_some());
        assert!(store.get("xai").is_none());
    }

    #[test]
    fn account_id_round_trips_when_set_and_omitted_when_absent() {
        let mut store = AuthStore::default();
        store.set(
            "chatgpt",
            TokenSet {
                access: "a".into(),
                refresh: "r".into(),
                expires_ms: 9,
                account_id: Some("acct-xyz".into()),
            },
        );
        let serialized = toml::to_string_pretty(&store).unwrap();
        // account_id present for the chatgpt set.
        assert!(serialized.contains("account_id = \"acct-xyz\""));
        let reparsed: AuthStore = toml::from_str(&serialized).unwrap();
        assert_eq!(
            reparsed.get("chatgpt").unwrap().account_id.as_deref(),
            Some("acct-xyz")
        );
        // An auth.toml written before account_id existed still parses (the
        // field defaults to None).
        let legacy = "[tokens.xai]\naccess = \"a\"\nrefresh = \"r\"\nexpires_ms = 1\n";
        let parsed: AuthStore = toml::from_str(legacy).unwrap();
        assert!(parsed.get("xai").unwrap().account_id.is_none());
    }
}
