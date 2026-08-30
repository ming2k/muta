//! Dynamic OAuth credential source implementing [`muta_contracts::CredentialSource`].

use super::OAuth;
use super::config::config_by_provider_id;
use super::store::AuthStore;
use futures::future::BoxFuture;
use muta_contracts::{ChannelAuth, CredentialSource, ResolvedAuth};
use std::fmt;
use std::sync::Arc;

/// Dynamic OAuth token source for a provider connection.
///
/// Lazily and concurrency-safely resolves fresh access tokens and associated
/// metadata (account id, project id, email), refreshing ahead of expiry
/// and persisting newly minted tokens to `~/.muta/auth.toml`.
pub struct OAuthCredentialSource {
    pub provider_id: String,
    pub preset_id: Option<String>,
    pub auth: ChannelAuth,
    oauth: Option<Arc<OAuth>>,
}

impl OAuthCredentialSource {
    pub fn new(
        provider_id: impl Into<String>,
        preset_id: Option<impl Into<String>>,
        auth: ChannelAuth,
    ) -> Self {
        let provider_id = provider_id.into();
        let preset_id = preset_id.map(Into::into);

        let oauth = auth
            .oauth_provider_id()
            .and_then(config_by_provider_id)
            .map(|mut cfg| {
                cfg.provider_id = std::borrow::Cow::Owned(provider_id.clone());
                Arc::new(OAuth::new(cfg))
            });

        Self {
            provider_id,
            preset_id,
            auth,
            oauth,
        }
    }
}

impl fmt::Debug for OAuthCredentialSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthCredentialSource")
            .field("provider_id", &self.provider_id)
            .field("preset_id", &self.preset_id)
            .field("auth", &self.auth)
            .finish()
    }
}

impl CredentialSource for OAuthCredentialSource {
    fn resolve_auth<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        Box::pin(async move {
            let Some(oauth) = &self.oauth else {
                return Err(format!(
                    "OAuth configuration not found for auth variant {:?}",
                    self.auth
                ));
            };

            let store = AuthStore::load();
            let Some(stored) = store
                .get_for_provider(&self.provider_id, self.preset_id.as_deref(), self.auth)
                .cloned()
            else {
                return Err(format!(
                    "No OAuth credentials stored for provider '{}' ({:?})",
                    self.provider_id, self.auth
                ));
            };

            match oauth.resolve_access_token(stored).await {
                Ok((access, tokens)) => {
                    let mut current_store = AuthStore::load();
                    current_store.set(&self.provider_id, tokens.clone());
                    if let Err(e) = current_store.save() {
                        tracing::warn!(error = %e, provider = %self.provider_id, "could not save refreshed auth tokens");
                    }
                    Ok(ResolvedAuth {
                        token: access,
                        account_id: tokens.account_id.clone(),
                        project_id: tokens
                            .project_id
                            .clone()
                            .or_else(|| tokens.account_id.clone()),
                        user_email: tokens.user_email.clone(),
                    })
                }
                Err(e) => {
                    tracing::warn!(error = %e, provider = %self.provider_id, "OAuth token resolution failed");
                    if e.is_permanent_grant_error() {
                        tracing::error!(provider = %self.provider_id, "OAuth refresh token permanently revoked/invalid");
                        let mut current_store = AuthStore::load();
                        current_store.remove(&self.provider_id);
                        let _ = current_store.save();
                    }
                    Err(format!(
                        "OAuth token resolution failed for '{}': {e}",
                        self.provider_id
                    ))
                }
            }
        })
    }

    fn force_refresh<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        Box::pin(async move {
            let Some(oauth) = &self.oauth else {
                return Err(format!(
                    "OAuth configuration not found for auth variant {:?}",
                    self.auth
                ));
            };

            let store = AuthStore::load();
            let Some(stored) = store
                .get_for_provider(&self.provider_id, self.preset_id.as_deref(), self.auth)
                .cloned()
            else {
                return Err(format!(
                    "No OAuth credentials stored for provider '{}' ({:?})",
                    self.provider_id, self.auth
                ));
            };

            match oauth.force_resolve_access_token(stored).await {
                Ok((access, tokens)) => {
                    let mut current_store = AuthStore::load();
                    current_store.set(&self.provider_id, tokens.clone());
                    if let Err(e) = current_store.save() {
                        tracing::warn!(error = %e, provider = %self.provider_id, "could not save refreshed auth tokens");
                    }
                    Ok(ResolvedAuth {
                        token: access,
                        account_id: tokens.account_id.clone(),
                        project_id: tokens
                            .project_id
                            .clone()
                            .or_else(|| tokens.account_id.clone()),
                        user_email: tokens.user_email.clone(),
                    })
                }
                Err(e) => {
                    tracing::warn!(error = %e, provider = %self.provider_id, "OAuth force token refresh failed");
                    if e.is_permanent_grant_error() {
                        tracing::error!(provider = %self.provider_id, "OAuth refresh token permanently revoked/invalid");
                        let mut current_store = AuthStore::load();
                        current_store.remove(&self.provider_id);
                        let _ = current_store.save();
                    }
                    Err(format!(
                        "OAuth force refresh failed for '{}': {e}",
                        self.provider_id
                    ))
                }
            }
        })
    }

    fn is_oauth(&self) -> bool {
        true
    }
}
