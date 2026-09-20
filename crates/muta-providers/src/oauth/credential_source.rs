//! Transactional OAuth credential resolution for provider connections.

use super::store::{AuthStore, LockedAuthStore, TokenSet};
use super::{ACCESS_TOKEN_REFRESH_SKEW_MS, OAuth, access_token_is_expiring};
use futures::future::BoxFuture;
use crate::oauth::presets::config_by_provider_id;
use muta_contracts::{ConnectionAuth, CredentialSource, ResolvedAuth, SecretString};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// State shared by every channel, catalog request, and runtime activation for
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

fn shared_oauth(connection_id: &str, auth: &ConnectionAuth) -> Option<Arc<ConnectionOAuth>> {
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
        let state = shared_oauth(&connection_id, &auth);
        Self {
            connection_id,
            auth,
            state,
        }
    }

    fn resolved(&self, tokens: &TokenSet) -> ResolvedAuth {
        let mut auth = ResolvedAuth::new(tokens.access.clone());
        // `ChatGptAuthMetadata` selects Codex's `chatgpt-account-id` header and
        // is meaningless (and misleading) on every other surface, so it is
        // attached only for the ChatGPT subscription integration — never for a
        // generic `account_id` attribute that another provider happens to set.
        if matches!(
            self.auth.subscription_provider(),
            Some("chatgpt" | "openai-subscription")
        ) {
            let account_id = tokens
                .get_attr("account_id")
                .map(ToString::to_string)
                .or_else(|| {
                    tokens
                        .id_token
                        .as_ref()
                        .map(SecretString::expose_secret)
                        .or(Some(tokens.access.expose_secret()))
                        .and_then(crate::oauth::token::chatgpt_account_id)
                });
            if let Some(acct) = account_id {
                auth =
                    auth.with_extension(muta_contracts::ChatGptAuthMetadata { account_id: acct });
            }
        }
        if let Some(proj) = tokens.get_attr("project_id") {
            auth = auth.with_extension(muta_contracts::GoogleAuthMetadata {
                project_id: proj.to_string(),
            });
        }
        if let Some(email) = &tokens.user_email {
            auth = auth.with_user_email(email.clone());
        }
        if let Some(qoder) =
            tokens.get_json_attr::<crate::registry::qoder::QoderStoredIdentity>("qoder")
        {
            auth = auth.with_extension(qoder.to_request_identity());
        }
        auth
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
        let stored = exact_tokens(&store, &self.connection_id, &self.auth)?;
        // A Qoder credential minted before uid resolution has an empty uid; the
        // signed surfaces reject that with `403 code 101`. Backfill it once and
        // persist, so every reader (inference and the catalog) sees it.
        let stored = self.ensure_qoder_uid(&mut store, stored).await?;

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

    /// Backfill a Qoder credential's empty uid, acquiring the store lock.
    ///
    /// The `resolve_auth` fast path (a live token) still needs the repair, so
    /// this wrapper takes the lock and delegates to
    /// [`Self::ensure_qoder_uid`]. Non-Qoder credentials short-circuit before
    /// the lock is taken.
    async fn ensure_qoder_uid_locked(&self, stored: TokenSet) -> Result<TokenSet, String> {
        let is_qoder = self.auth.subscription_provider() == Some("qoder");
        let qoder_id = stored.get_json_attr::<crate::registry::qoder::QoderStoredIdentity>("qoder");
        tracing::warn!(
            connection = %self.connection_id,
            auth = ?self.auth,
            qoder_uid = ?qoder_id.as_ref().map(|q| &q.uid),
            "QODER_ENSURE_ENTER"
        );
        if !is_qoder || qoder_id.is_none_or(|q| !q.uid.is_empty()) {
            return Ok(stored);
        }
        let mut store = AuthStore::lock().await.map_err(|error| error.to_string())?;
        self.ensure_qoder_uid(&mut store, stored).await
    }

    /// Backfill a Qoder credential's empty uid and persist it.
    async fn ensure_qoder_uid(
        &self,
        store: &mut LockedAuthStore,
        stored: TokenSet,
    ) -> Result<TokenSet, String> {
        if self.auth.subscription_provider() != Some("qoder") {
            return Ok(stored);
        }
        let Some(identity) =
            stored.get_json_attr::<crate::registry::qoder::QoderStoredIdentity>("qoder")
        else {
            return Ok(stored);
        };
        if !identity.uid.is_empty() {
            return Ok(stored);
        }
        let Ok(client) = crate::http::Http::control_plane() else {
            tracing::warn!(connection = %self.connection_id, "qoder: no control-plane client for uid resolution");
            return Ok(stored);
        };
        let resolved = super::qoder::fetch_uid(&client, stored.access.expose_secret()).await;
        if std::env::var("MUTA_QODER_DEBUG").is_ok() {
            eprintln!("QODER_UID_RESOLVE connection={} result={resolved:?}", self.connection_id);
        }
        let Ok(uid) = resolved else {
            return Ok(stored);
        };
        if uid.is_empty() {
            return Ok(stored);
        }
        let mut updated = stored.clone();
        let mut new_identity = identity;
        new_identity.uid.clone_from(&uid);
        updated.set_json_attr("qoder", &new_identity);
        store.set(&self.connection_id, updated.clone());
        store.save().map_err(|error| error.to_string())?;
        Ok(updated)
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
            // A Qoder credential with an empty uid is repaired here, on **both**
            // branches — a live token would otherwise return before the backfill
            // in `refresh_locked` ever runs.
            let stored = self.ensure_qoder_uid_locked(stored).await?;
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

fn exact_tokens(
    store: &LockedAuthStore,
    connection_id: &str,
    auth: &ConnectionAuth,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(access: &str) -> TokenSet {
        TokenSet {
            access: access.into(),
            refresh: "refresh".into(),
            expires_ms: i64::MAX,
            id_token: None,
            token_type: None,
            scope: None,
            user_email: None,
            attributes: serde_json::Map::new(),
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
        let source = OAuthCredentialSource::new("work-subscription", ConnectionAuth::subscription("chatgpt"));
        let state = source.state.expect("ChatGPT OAuth integration");
        assert_eq!(state.oauth.config().provider_id, "chatgpt");
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

        let source = OAuthCredentialSource::new("custom-chatgpt-123", ConnectionAuth::subscription("chatgpt"));
        let mut t = tokens("some-access");
        t.id_token = Some(fake_jwt.into());
        let resolved = source.resolved(&t);
        assert_eq!(
            resolved
                .extension::<muta_contracts::ChatGptAuthMetadata>()
                .map(|m| m.account_id.as_str()),
            Some("org-xyz789")
        );
    }

    #[test]
    fn opencode_account_id_does_not_leak_into_chatgpt_metadata() {
        let source =
            OAuthCredentialSource::new("opencode-go", ConnectionAuth::subscription("opencode"));
        let mut t = tokens("console-access");
        t.set_attr("account_id", "user-123");
        t.set_attr("org_id", "org-1");
        let resolved = source.resolved(&t);
        assert!(
            resolved
                .extension::<muta_contracts::ChatGptAuthMetadata>()
                .is_none(),
            "opencode must not be projected as a ChatGPT/Codex credential"
        );
    }

    #[test]
    fn independent_credential_sources_for_same_connection_share_underlying_state() {
        let source1 = OAuthCredentialSource::new("conn-shared-1", ConnectionAuth::subscription("chatgpt"));
        let source2 = OAuthCredentialSource::new("conn-shared-1", ConnectionAuth::subscription("chatgpt"));
        assert!(Arc::ptr_eq(
            &source1.state.unwrap(),
            &source2.state.unwrap()
        ));
    }
}
