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

    /// Whether this failure is a **request-shape refusal**: the upstream
    /// rejected what we sent, as opposed to a transport, auth, quota, or
    /// server fault.
    ///
    /// Only a refusal of the request itself can have been caused by a field
    /// *inside* it, which is why the image-cause probe (ADR-0230) is scoped to
    /// these kinds. `Protocol` is included on
    /// purpose: some vendors report a refusal in-band (HTTP 200 carrying an
    /// `error` object), which surfaces as `Protocol` rather than
    /// `InvalidRequest`.
    pub const fn is_request_refusal(&self) -> bool {
        matches!(
            self.kind,
            ProviderErrorKind::InvalidRequest
                | ProviderErrorKind::Protocol
                | ProviderErrorKind::Other
        )
    }

    // Deliberately absent: any predicate that decides "was this refusal about
    // images?" from the vendor's *text* — an `is_image_rejection` reading
    // `error.message`, a marker list, an upstream code table. Every vendor
    // formats its error envelope differently and its prose drifts, so such a
    // predicate is a permanent maintenance liability whose failure modes are
    // both bad: a false negative re-bricks a session, a false positive withholds
    // a capability that works. The harness answers the question from an
    // **outcome differential** instead — retry the identical turn with the
    // attachments withheld and observe whether the refusal goes away (ADR-0230)
    // — which needs to understand no vendor format at all. The only
    // classification kept here is [`Self::is_request_refusal`], which derives
    // from the HTTP status the transport already mapped and reads none of the
    // vendor's prose.
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

#[cfg(test)]
mod request_refusal_tests {
    use super::*;

    fn error(kind: ProviderErrorKind) -> ProviderError {
        ProviderError::new("mock", kind, "anything")
    }

    #[test]
    fn request_refusal_scopes_the_probe_to_refusals_of_our_own_request() {
        // This is the only classification the image recovery consults, and it is
        // derived from the HTTP status the transport already mapped — it reads
        // none of the vendor's prose, which is the whole point (ADR-0230).
        //
        // A refusal of what we sent is the only failure a field *inside* the
        // request could explain, so those kinds may arm the probe.
        for kind in [
            ProviderErrorKind::InvalidRequest,
            ProviderErrorKind::Protocol,
            ProviderErrorKind::Other,
        ] {
            assert!(error(kind).is_request_refusal(), "{kind:?}");
        }

        // The rest are excluded, and each for a reason:
        // - ContextOverflow has its own recovery (compaction), and conflating the
        //   two would make the harness withhold images for a too-long prompt;
        // - Timeout / Upstream / RateLimited / Unavailable / Transport may have
        //   been *processed* (the attempt can be billable), so re-sending them is
        //   not the free experiment a validation refusal is;
        // - Authentication / Decode cannot be caused by an attachment.
        for kind in [
            ProviderErrorKind::Transport,
            ProviderErrorKind::Timeout,
            ProviderErrorKind::RateLimited,
            ProviderErrorKind::Authentication,
            ProviderErrorKind::ContextOverflow,
            ProviderErrorKind::Upstream,
            ProviderErrorKind::Decode,
            ProviderErrorKind::Unavailable,
        ] {
            assert!(!error(kind).is_request_refusal(), "{kind:?}");
        }
    }
}
