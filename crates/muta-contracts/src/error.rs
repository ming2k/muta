//! Typed failures crossing provider, tool, and harness boundaries.

/// Stable classification of a provider failure.
///
/// This enum is transport-independent so provider adapters can expose HTTP,
/// local-model, and future out-of-process failures through the same contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum ProviderErrorKind {
    Transport,
    Timeout,
    RateLimited,
    Authentication,
    InvalidRequest,
    ContextOverflow,
    Upstream,
    Decode,
    Protocol,
    Unavailable,
    Other,
}

/// Whether and when a provider request may be attempted again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RetryDisposition {
    #[default]
    Never,
    Retry {
        retry_after_ms: Option<u64>,
    },
}

/// A machine-readable provider failure with a user-facing message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProviderError {
    provider: String,
    kind: ProviderErrorKind,
    status: Option<u16>,
    retry: RetryDisposition,
    message: String,
}

impl ProviderError {
    pub fn new(
        provider: impl Into<String>,
        kind: ProviderErrorKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            kind,
            status: None,
            retry: RetryDisposition::Never,
            message: message.into(),
        }
    }

    pub fn retryable(mut self, retry_after_ms: Option<u64>) -> Self {
        self.retry = RetryDisposition::Retry { retry_after_ms };
        self
    }

    pub fn authentication(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(provider, ProviderErrorKind::Authentication, message)
    }

    pub fn invalid_request(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(provider, ProviderErrorKind::InvalidRequest, message)
    }

    pub fn protocol(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(provider, ProviderErrorKind::Protocol, message)
    }

    pub fn context_overflow(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(provider, ProviderErrorKind::ContextOverflow, message)
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    pub fn map_message(mut self, f: impl FnOnce(String) -> String) -> Self {
        self.message = f(self.message);
        self
    }

    pub fn with_retry_after_if_absent(mut self, retry_after_ms: Option<u64>) -> Self {
        if let RetryDisposition::Retry {
            retry_after_ms: ref mut existing,
        } = self.retry
            && existing.is_none()
        {
            *existing = retry_after_ms;
        }
        self
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub const fn kind(&self) -> ProviderErrorKind {
        self.kind
    }

    pub const fn status(&self) -> Option<u16> {
        self.status
    }

    pub const fn retry_disposition(&self) -> RetryDisposition {
        self.retry
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub const fn is_context_overflow(&self) -> bool {
        matches!(self.kind, ProviderErrorKind::ContextOverflow)
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderError {}

/// Stable classification of a tool failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum ToolErrorKind {
    InvalidArguments,
    Unavailable,
    PermissionDenied,
    Cancelled,
    Execution,
    Protocol,
    Other,
}

/// A typed tool failure. Tool failures are terminal for one tool call and are
/// rendered as structured [`crate::ToolOutput::Error`] values by the harness.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolError {
    kind: ToolErrorKind,
    message: String,
    details: Option<String>,
}

impl ToolError {
    pub fn new(kind: ToolErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: None,
        }
    }

    pub fn invalid_arguments(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::InvalidArguments, message)
    }

    pub fn execution(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Execution, message)
    }

    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    pub const fn kind(&self) -> ToolErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn details(&self) -> Option<&str> {
        self.details.as_deref()
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ToolError {}

impl From<String> for ToolError {
    fn from(message: String) -> Self {
        Self::execution(message)
    }
}

impl From<&str> for ToolError {
    fn from(message: &str) -> Self {
        Self::execution(message)
    }
}

/// A typed harness error.
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessError {
    Provider(ProviderError),
    /// The active round was cancelled by the user.
    Interrupted,
    /// Any other terminal failure; the message is user-facing.
    Other(String),
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(f),
            Self::Other(message) => f.write_str(message),
            Self::Interrupted => write!(f, "Interrupted"),
        }
    }
}

impl std::error::Error for HarnessError {}

impl From<String> for HarnessError {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}

impl From<ProviderError> for HarnessError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}
