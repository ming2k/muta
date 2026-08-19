//! Persistent storage for OAuth token sets, keyed by provider id.
//!
//! Mirrors `credentials.toml`'s separation-of-concerns: `config.toml` holds the
//! provider *definitions* (which channel uses OAuth, which uses an API key),
//! while the *live tokens* (access/refresh/expires) live in `auth.toml` (0600).
//! A missing or unparseable file is a normal first-run condition: best-effort
//! load returns an empty store and never blocks startup.

use std::collections::BTreeMap;
use std::fs;

use neenee_contracts::SecretString;
use neenee_persistence::paths;
use serde::{Deserialize, Serialize};

/// One provider's OAuth token set and associated metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    /// The bearer access token sent as `Authorization: Bearer <access>`.
    pub access: SecretString,
    /// The refresh token used to rotate the access token.
    pub refresh: SecretString,
    /// Unix epoch milliseconds when the access token expires (best-effort).
    pub expires_ms: i64,
    /// Provider-specific account identifier captured at login. For ChatGPT this
    /// is the `chatgpt_account_id` (sent as `ChatGPT-Account-Id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Optional OpenID Connect ID Token if issued by the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<SecretString>,
    /// Token type (e.g. "Bearer").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    /// Granted scopes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Associated GCP Project ID (for Google Antigravity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// User email address if discovered via OpenID/userinfo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_email: Option<String>,
}

/// All stored token sets, keyed by provider id (`"xai"`, `"chatgpt"`, `"google-antigravity"`).
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
    /// unparseable. A corrupt secrets file must never block startup.
    /// Automatically migrates legacy `auth.toml` from `config_dir` to `state_dir`.
    pub fn load() -> Self {
        Self::load_from_paths(&Self::path(), &paths::get().legacy_auth_file())
    }

    /// Read auth store from `path`, falling back to migrating from `legacy_path` if `path` is missing.
    pub fn load_from_paths(path: &std::path::Path, legacy_path: &std::path::Path) -> Self {
        if !path.exists() && legacy_path.exists() {
            if let Ok(content) = fs::read_to_string(legacy_path) {
                if let Ok(store) = toml::from_str::<Self>(&content) {
                    tracing::info!(
                        from = %legacy_path.display(),
                        to = %path.display(),
                        "migrating auth.toml from config dir to state dir"
                    );
                    if let Ok(bytes) = toml::to_string_pretty(&store).map(|s| s.into_bytes()) {
                        let _ = neenee_persistence::fsutil::atomic_write_bytes(path, &bytes);
                    }
                    let _ = fs::remove_file(legacy_path);
                    return store;
                }
            }
        }
        let Ok(content) = fs::read_to_string(path) else {
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
    /// [`neenee_persistence::fsutil::atomic_write_bytes`].
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let bytes = toml::to_string_pretty(self)?.into_bytes();
        neenee_persistence::fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    /// Get the token set for a provider id, if present.
    pub fn get(&self, provider_id: &str) -> Option<&TokenSet> {
        self.tokens.get(provider_id)
    }

    /// Get the token set for a provider instance, checking in hierarchical order:
    /// 1. Exact provider instance id (e.g. "google-antigravity111", "work-chatgpt")
    /// 2. Template id if set (e.g. "antigravity-oauth", "chatgpt-oauth")
    /// 3. Standard fallback key for the auth type (e.g. "google-antigravity", "chatgpt", "copilot", "xai")
    ///
    /// This gives each custom instance its own distinct, isolated token namespace,
    /// while preserving transparent backward compatibility for legacy configs.
    pub fn get_for_provider(
        &self,
        provider_id: &str,
        template_id: Option<&str>,
        auth: neenee_contracts::ChannelAuth,
    ) -> Option<&TokenSet> {
        if let Some(tokens) = self.tokens.get(provider_id) {
            return Some(tokens);
        }
        if let Some(tid) = template_id
            && let Some(tokens) = self.tokens.get(tid)
        {
            return Some(tokens);
        }
        if let Some(fallback_key) = auth.oauth_provider_id() {
            return self.tokens.get(fallback_key);
        }
        None
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
    use neenee_contracts::ChannelAuth;

    #[test]
    fn round_trips_through_toml() {
        let mut store = AuthStore::default();
        store.set(
            "xai",
            TokenSet {
                access: "acc".into(),
                refresh: "ref".into(),
                expires_ms: 1_700_000_000_000,
                account_id: None,
                id_token: None,
                token_type: Some("Bearer".into()),
                scope: None,
                project_id: None,
                user_email: None,
            },
        );
        let serialized = toml::to_string_pretty(&store).unwrap();
        let reparsed: AuthStore = toml::from_str(&serialized).unwrap();
        let tokens = reparsed.get("xai").unwrap();
        assert_eq!(tokens.access, "acc");
        assert_eq!(tokens.refresh, "ref");
        assert_eq!(tokens.expires_ms, 1_700_000_000_000);
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
                id_token: None,
                token_type: None,
                scope: None,
                project_id: None,
                user_email: None,
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
                id_token: None,
                token_type: None,
                scope: None,
                project_id: Some("project-123".into()),
                user_email: Some("user@example.com".into()),
            },
        );
        assert_eq!(store.get("xai").unwrap().access, "a2");
        assert_eq!(
            store.get("xai").unwrap().account_id.as_deref(),
            Some("acct-1")
        );
        assert_eq!(
            store.get("xai").unwrap().project_id.as_deref(),
            Some("project-123")
        );
        // Remove.
        assert!(store.remove("xai").is_some());
        assert!(store.get("xai").is_none());
    }

    #[test]
    fn multi_instance_resolution_and_fallback() {
        let mut store = AuthStore::default();

        // 1. Set a legacy/fallback token for google-antigravity
        store.set(
            "google-antigravity",
            TokenSet {
                access: "legacy_token".into(),
                refresh: "legacy_refresh".into(),
                expires_ms: 100,
                account_id: Some("legacy_project".into()),
                id_token: None,
                token_type: None,
                scope: None,
                project_id: Some("legacy_project".into()),
                user_email: Some("legacy@gmail.com".into()),
            },
        );

        // An instance without its own token resolves the legacy fallback
        let resolved = store
            .get_for_provider(
                "google-antigravity111",
                Some("antigravity-oauth"),
                ChannelAuth::AntigravityOAuth,
            )
            .unwrap();
        assert_eq!(resolved.access, "legacy_token");

        // 2. Set an isolated token for a second instance
        store.set(
            "google-antigravity222",
            TokenSet {
                access: "work_token".into(),
                refresh: "work_refresh".into(),
                expires_ms: 200,
                account_id: Some("work_project".into()),
                id_token: None,
                token_type: None,
                scope: None,
                project_id: Some("work_project".into()),
                user_email: Some("work@company.com".into()),
            },
        );

        // Instance 222 gets its own token
        let resolved_222 = store
            .get_for_provider(
                "google-antigravity222",
                Some("antigravity-oauth"),
                ChannelAuth::AntigravityOAuth,
            )
            .unwrap();
        assert_eq!(resolved_222.access, "work_token");
        assert_eq!(resolved_222.user_email.as_deref(), Some("work@company.com"));

        // Instance 111 still gets the legacy token, completely isolated
        let resolved_111 = store
            .get_for_provider(
                "google-antigravity111",
                Some("antigravity-oauth"),
                ChannelAuth::AntigravityOAuth,
            )
            .unwrap();
        assert_eq!(resolved_111.access, "legacy_token");
    }

    #[test]
    fn test_legacy_auth_migration() {
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("neenee_test_auth_{unique_id}"));
        let config_dir = temp.join("config");
        let state_dir = temp.join("state");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();

        let legacy_file = config_dir.join("auth.toml");
        let target_file = state_dir.join("auth.toml");

        // Write legacy auth.toml in config_dir
        let mut legacy_store = AuthStore::default();
        legacy_store.set(
            "test-provider",
            TokenSet {
                access: "migrated_secret".into(),
                refresh: "migrated_refresh".into(),
                expires_ms: 12345,
                account_id: None,
                id_token: None,
                token_type: None,
                scope: None,
                project_id: None,
                user_email: None,
            },
        );
        std::fs::write(&legacy_file, toml::to_string_pretty(&legacy_store).unwrap()).unwrap();
        assert!(legacy_file.exists());

        // Loading should migrate to state_dir and remove config_dir/auth.toml
        let loaded = AuthStore::load_from_paths(&target_file, &legacy_file);
        assert_eq!(
            loaded.get("test-provider").unwrap().access,
            "migrated_secret"
        );
        assert!(
            !legacy_file.exists(),
            "legacy auth file should be removed after migration"
        );
        assert!(target_file.exists(), "state_dir auth file should exist");

        let _ = std::fs::remove_dir_all(&temp);
    }
}
