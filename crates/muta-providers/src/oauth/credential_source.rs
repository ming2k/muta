//! Transactional OAuth credential resolution for provider connections.

use super::config::config_by_provider_id;
use super::store::{AuthStore, LockedAuthStore, TokenSet};
use super::{ACCESS_TOKEN_REFRESH_SKEW_MS, OAuth, access_token_is_expiring};
use futures::future::BoxFuture;
use muta_contracts::{ConnectionAuth, CredentialSource, ResolvedAuth, SecretString};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// State shared by every channel, discovery request, and runtime activation for
/// one connection. The store's cross-process lock is the actual refresh gate;
/// rejected-token identity is carried by each request rather than inferred
/// from mutable global state.
struct ConnectionOAuth {
    oauth: OAuth,
}

fn registry() -> &'static Mutex<HashMap<String, Weak<ConnectionOAuth>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Weak<ConnectionOAuth>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn shared_oauth(connection_id: &str, auth: ConnectionAuth) -> Option<Arc<ConnectionOAuth>> {
    let integration_id = auth.oauth_provider_id()?;
    let registry_key = format!("{connection_id}\0{integration_id}");
    let mut entries = registry().lock().unwrap_or_else(|error| error.into_inner());
    if let Some(existing) = entries.get(&registry_key).and_then(Weak::upgrade) {
        return Some(existing);
    }
    let config = config_by_provider_id(integration_id)?;
    let state = Arc::new(ConnectionOAuth {
        oauth: OAuth::new(config),
    });
    entries.retain(|_, value| value.strong_count() > 0);
    entries.insert(registry_key, Arc::downgrade(&state));
    Some(state)
}

/// Dynamic OAuth token source for one exact provider connection.
pub struct OAuthCredentialSource {
    pub connection_id: String,
    pub auth: ConnectionAuth,
    state: Option<Arc<ConnectionOAuth>>,
}

impl OAuthCredentialSource {
    pub fn new(connection_id: impl Into<String>, auth: ConnectionAuth) -> Self {
        let connection_id = connection_id.into();
        let state = shared_oauth(&connection_id, auth);
        Self {
            connection_id,
            auth,
            state,
        }
    }

    fn resolved(&self, tokens: &TokenSet) -> ResolvedAuth {
        let account_id = tokens.account_id.clone().or_else(|| {
            if self.auth == ConnectionAuth::ChatGptOAuth {
                tokens
                    .id_token
                    .as_ref()
                    .map(SecretString::expose_secret)
                    .or(Some(tokens.access.expose_secret()))
                    .and_then(crate::oauth::token::chatgpt_account_id)
            } else {
                None
            }
        });
        ResolvedAuth {
            token: tokens.access.clone(),
            account_id: account_id.clone(),
            project_id: tokens.project_id.clone().or(account_id),
            user_email: tokens.user_email.clone(),
        }
    }

    async fn refresh_locked(
        &self,
        force: bool,
        rejected_access: Option<&SecretString>,
    ) -> Result<ResolvedAuth, String> {
        let Some(state) = &self.state else {
            return Err(format!(
                "OAuth configuration not found for auth variant {:?}",
                self.auth
            ));
        };
        let mut store = AuthStore::lock().await.map_err(|error| error.to_string())?;
        let stored = exact_tokens(&store, &self.connection_id, self.auth)?;

        // A force-refresh is normally a reaction to a 401. If another request
        // or process already replaced the token that this caller used, retry
        // with the replacement instead of rotating the refresh token again.
        if force
            && token_rotated_since_rejection(rejected_access, &stored)
            && token_is_live(&stored)
        {
            return Ok(self.resolved(&stored));
        }
        if !force && token_is_live(&stored) {
            return Ok(self.resolved(&stored));
        }

        match state.oauth.force_resolve_access_token(stored).await {
            Ok((_access, tokens)) => {
                store.set(&self.connection_id, tokens.clone());
                // Never hand out a newly rotated token unless its replacement
                // refresh token is durable. Otherwise the next process could
                // reuse the old refresh token and invalidate the login.
                store.save().map_err(|error| error.to_string())?;
                Ok(self.resolved(&tokens))
            }
            Err(error) => {
                if error.is_permanent_grant_error() {
                    tracing::error!(
                        provider = %self.connection_id,
                        error = %error,
                        "OAuth refresh token is permanently invalid; removing exact connection credential"
                    );
                    store.remove(&self.connection_id);
                    store.save().map_err(|save_error| {
                        format!("{error}; additionally failed to remove invalid credential: {save_error}")
                    })?;
                }
                Err(format!(
                    "OAuth token resolution failed for '{}': {error}",
                    self.connection_id
                ))
            }
        }
    }
}

fn exact_tokens(
    store: &LockedAuthStore,
    connection_id: &str,
    auth: ConnectionAuth,
) -> Result<TokenSet, String> {
    store.get(connection_id).cloned().ok_or_else(|| {
        format!(
            "No OAuth credentials stored for connection '{}' ({auth:?}); reconnect it",
            connection_id
        )
    })
}

fn token_is_live(tokens: &TokenSet) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    !tokens.access.is_empty()
        && !access_token_is_expiring(
            Some(tokens.access.expose_secret()),
            ACCESS_TOKEN_REFRESH_SKEW_MS,
            now,
        )
        && tokens.expires_ms > now + ACCESS_TOKEN_REFRESH_SKEW_MS
}

fn token_rotated_since_rejection(
    rejected_access: Option<&SecretString>,
    stored: &TokenSet,
) -> bool {
    rejected_access.is_some_and(|rejected| rejected != &stored.access)
}

impl fmt::Debug for OAuthCredentialSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthCredentialSource")
            .field("connection_id", &self.connection_id)
            .field("auth", &self.auth)
            .finish()
    }
}

impl CredentialSource for OAuthCredentialSource {
    fn resolve_auth<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        Box::pin(async move {
            if self.state.is_none() {
                return Err(format!(
                    "OAuth configuration not found for auth variant {:?}",
                    self.auth
                ));
            }
            let store = AuthStore::load().map_err(|error| error.to_string())?;
            let stored = store.get(&self.connection_id).cloned().ok_or_else(|| {
                format!(
                    "No OAuth credentials stored for connection '{}' ({:?}); reconnect it",
                    self.connection_id, self.auth
                )
            })?;
            if token_is_live(&stored) {
                return Ok(self.resolved(&stored));
            }
            self.refresh_locked(false, None).await
        })
    }

    fn force_refresh<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        Box::pin(async move { self.refresh_locked(true, None).await })
    }

    fn force_refresh_after_rejection<'a>(
        &'a self,
        rejected_access: &'a SecretString,
    ) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        Box::pin(async move { self.refresh_locked(true, Some(rejected_access)).await })
    }

    fn is_oauth(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(access: &str) -> TokenSet {
        TokenSet {
            access: access.into(),
            refresh: "refresh".into(),
            expires_ms: i64::MAX,
            account_id: None,
            id_token: None,
            token_type: None,
            scope: None,
            project_id: None,
            user_email: None,
        }
    }

    #[test]
    fn rejected_token_identity_prevents_duplicate_rotation() {
        let stored = tokens("new-access");
        let old: SecretString = "old-access".into();
        let current: SecretString = "new-access".into();
        assert!(token_rotated_since_rejection(Some(&old), &stored));
        assert!(!token_rotated_since_rejection(Some(&current), &stored));
        assert!(!token_rotated_since_rejection(None, &stored));
    }

    #[test]
    fn connection_namespace_never_rewrites_integration_identity() {
        let source = OAuthCredentialSource::new("work-subscription", ConnectionAuth::ChatGptOAuth);
        let state = source.state.expect("ChatGPT OAuth integration");
        assert_eq!(state.oauth.config().provider_id, "chatgpt");
        assert!(state.oauth.config().is_chatgpt());
    }

    #[test]
    fn custom_id_chatgpt_connection_resolves_account_id_from_jwt() {
        use base64::Engine;
        let payload = serde_json::json!({
            "https://api.openai.com/auth": {
                "user_id": "user-123",
                "chatgpt_account_id": "org-xyz789"
            },
            "exp": 2_000_000_000
        });
        let encoded_payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        let fake_jwt = format!("eyJhbGciOiJub25lIn0.{}.signature", encoded_payload);

        let source = OAuthCredentialSource::new("custom-chatgpt-123", ConnectionAuth::ChatGptOAuth);
        let mut t = tokens("some-access");
        t.id_token = Some(fake_jwt.into());
        let resolved = source.resolved(&t);
        assert_eq!(resolved.account_id.as_deref(), Some("org-xyz789"));
    }

    #[test]
    fn independent_credential_sources_for_same_connection_share_underlying_state() {
        let source1 = OAuthCredentialSource::new("conn-shared-1", ConnectionAuth::ChatGptOAuth);
        let source2 = OAuthCredentialSource::new("conn-shared-1", ConnectionAuth::ChatGptOAuth);
        assert!(Arc::ptr_eq(
            &source1.state.unwrap(),
            &source2.state.unwrap()
        ));
    }
}
