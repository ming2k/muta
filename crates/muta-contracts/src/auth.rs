//! Dynamic authentication credential sources (API key, OAuth token, etc.)
//!
//! Provides a uniform [`CredentialSource`] abstraction for resolving live
//! authentication credentials just-in-time before sending requests to upstream LLMs.

use crate::SecretString;
use futures::future::BoxFuture;
use std::fmt;
use std::sync::Arc;

/// Dynamic or static source of authentication credentials.
pub trait CredentialSource: Send + Sync + fmt::Debug {
    /// Resolve the current valid token / API key.
    /// If dynamic (e.g. OAuth), automatically refreshes if expired or expiring.
    fn resolve_token<'a>(&'a self) -> BoxFuture<'a, Result<SecretString, String>>;

    /// Force a fresh token from upstream (e.g. on 401 Unauthorized self-healing retry).
    fn force_refresh<'a>(&'a self) -> BoxFuture<'a, Result<SecretString, String>>;

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
    fn resolve_token<'a>(&'a self) -> BoxFuture<'a, Result<SecretString, String>> {
        let val = self.0.clone();
        Box::pin(futures::future::ready(Ok(val)))
    }

    fn force_refresh<'a>(&'a self) -> BoxFuture<'a, Result<SecretString, String>> {
        let val = self.0.clone();
        Box::pin(futures::future::ready(Ok(val)))
    }

    fn is_oauth(&self) -> bool {
        false
    }
}

/// Convenience helper to wrap any secret into an `Arc<dyn CredentialSource>`.
pub fn static_credential(secret: impl Into<SecretString>) -> Arc<dyn CredentialSource> {
    Arc::new(StaticCredentialSource::new(secret))
}
