//! Dynamic authentication credential sources (API key, OAuth token, etc.)
//!
//! Provides a uniform [`CredentialSource`] abstraction for resolving live
//! authentication credentials and associated account metadata just-in-time
//! before sending requests to upstream LLMs.

use crate::SecretString;
use futures::future::BoxFuture;
use std::fmt;
use std::sync::Arc;

/// Resolved authentication credentials and account metadata for an outbound request.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ResolvedAuth {
    /// Bearer access token or raw API key.
    pub token: SecretString,
    /// ChatGPT/Codex account id (`ChatGPT-Account-Id`).
    pub account_id: Option<String>,
    /// Google Cloud project id (`cloudaicompanionProject` / `x-goog-user-project`).
    pub project_id: Option<String>,
    /// User email address if known.
    pub user_email: Option<String>,
}

impl ResolvedAuth {
    /// Create a new resolved authentication payload with a token.
    pub fn new(token: impl Into<SecretString>) -> Self {
        Self {
            token: token.into(),
            account_id: None,
            project_id: None,
            user_email: None,
        }
    }

    /// Set the account id.
    pub fn with_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
        self
    }

    /// Set the project id.
    pub fn with_project_id(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
        self
    }

    /// Set the user email.
    pub fn with_user_email(mut self, email: impl Into<String>) -> Self {
        self.user_email = Some(email.into());
        self
    }

    /// Whether this credential token is empty.
    pub fn is_empty(&self) -> bool {
        self.token.expose_secret().trim().is_empty()
    }
}

impl fmt::Debug for ResolvedAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedAuth")
            .field("token", &"[REDACTED]")
            .field("account_id", &self.account_id)
            .field("project_id", &self.project_id)
            .field("user_email", &self.user_email)
            .finish()
    }
}

/// Dynamic or static source of authentication credentials.
pub trait CredentialSource: Send + Sync + fmt::Debug {
    /// Resolve the current valid auth credentials (token + metadata).
    /// If dynamic (e.g. OAuth), automatically refreshes if expired or expiring.
    fn resolve_auth<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>>;

    /// Force a fresh token from upstream (e.g. on 401 Unauthorized self-healing retry).
    fn force_refresh<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>>;

    /// Refresh after an upstream rejection of one exact access token. Dynamic
    /// sources use the rejected value to detect that another request or process
    /// already rotated it; static sources retain the default force behavior.
    fn force_refresh_after_rejection<'a>(
        &'a self,
        _rejected_access: &'a SecretString,
    ) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        self.force_refresh()
    }

    /// Whether this credential source is ready to be used (e.g. non-empty API key).
    fn is_ready(&self) -> bool {
        true
    }

    /// Whether this credential source represents a dynamic OAuth connection.
    fn is_oauth(&self) -> bool {
        false
    }
}

/// A static API key credential source (never expires, 0ms fast-path).
#[derive(Clone, PartialEq, Eq)]
pub struct StaticCredentialSource(pub SecretString);

impl StaticCredentialSource {
    pub fn new(secret: impl Into<SecretString>) -> Self {
        Self(secret.into())
    }
}

impl fmt::Debug for StaticCredentialSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("StaticCredentialSource")
            .field(&"[REDACTED]")
            .finish()
    }
}

impl CredentialSource for StaticCredentialSource {
    fn resolve_auth<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        let val = ResolvedAuth::new(self.0.clone());
        Box::pin(futures::future::ready(Ok(val)))
    }

    fn force_refresh<'a>(&'a self) -> BoxFuture<'a, Result<ResolvedAuth, String>> {
        let val = ResolvedAuth::new(self.0.clone());
        Box::pin(futures::future::ready(Ok(val)))
    }

    fn is_ready(&self) -> bool {
        !self.0.expose_secret().trim().is_empty()
    }

    fn is_oauth(&self) -> bool {
        false
    }
}

/// Convenience helper to wrap any secret into an `Arc<dyn CredentialSource>`.
pub fn static_credential(secret: impl Into<SecretString>) -> Arc<dyn CredentialSource> {
    Arc::new(StaticCredentialSource::new(secret))
}
