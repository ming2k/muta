//! Steward execution engine: typed, resilient cognitive infrastructure runner for Agent Harness.
//!
//! # Architecture
//!
//! The `Steward` provides out-of-band cognitive execution for the Agent Harness.
//!
//! Key design invariants:
//! - **Typed Tasks**: Dispatches any task implementing [`StewardTask`].
//! - **Timeout Bounds**: Strict timeouts on every consult call, preventing background task leaks.
//! - **Fail-Open Resilience**: Helper methods guarantee graceful fallback if model calls fail or timeout.
//! - **JSON Normalization**: Extracts structured JSON payloads even if the model wraps them in markdown.

use std::sync::Arc;
use std::time::Duration;

use muta_contracts::{
    Message, ModelRequest, Provider, Role, SanityCheckInput, SanityCheckVerdict,
    SanityVerifierTask, SemanticLoopInput, SemanticLoopSentinelTask, SemanticLoopVerdict,
    SessionTitleOutput, SessionTitlerInput, SessionTitlerTask, StewardTask, clean_title,
};

/// Errors that can occur during a Steward task consultation.
#[derive(Debug)]
pub enum StewardError {
    Timeout(Duration),
    ProviderError(String),
    DeserializationError { error: String, raw: String },
    EmptyResponse,
}

impl std::fmt::Display for StewardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout(d) => write!(f, "Steward task timed out after {d:?}"),
            Self::ProviderError(e) => write!(f, "Provider failed: {e}"),
            Self::DeserializationError { error, raw } => {
                write!(
                    f,
                    "Failed to deserialize structured output: {error}, raw: {raw}"
                )
            }
            Self::EmptyResponse => write!(f, "Model returned an empty response"),
        }
    }
}

impl std::error::Error for StewardError {}

/// The Steward cognitive attendant.
#[derive(Clone)]
pub struct Steward {
    provider: Arc<dyn Provider>,
}

impl Steward {
    /// Create a new Steward bound to `provider`.
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }

    /// Access the underlying provider.
    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    /// Consult the steward with a typed [`StewardTask`].
    pub async fn consult<T: StewardTask>(
        &self,
        task: T,
        input: T::Input,
    ) -> Result<T::Output, StewardError> {
        let timeout = Duration::from_millis(task.timeout_ms());
        let messages = vec![
            Message::new(Role::System, task.system_prompt()),
            Message::new(Role::User, task.render_prompt(&input)),
        ];

        let response = tokio::time::timeout(
            timeout,
            self.provider.chat(ModelRequest::ephemeral(messages)),
        )
        .await
        .map_err(|_| StewardError::Timeout(timeout))?
        .map_err(StewardError::ProviderError)?;

        let content = response.content.trim();
        if content.is_empty() {
            return Err(StewardError::EmptyResponse);
        }

        parse_structured_json::<T::Output>(content)
    }

    /// Consult with automatic fallback (Fail-Open pattern).
    ///
    /// If the steward fails, times out, or returns invalid JSON, this logs a warning
    /// and returns `fallback` to prevent blocking the production loop.
    pub async fn consult_with_fallback<T: StewardTask>(
        &self,
        task: T,
        input: T::Input,
        fallback: T::Output,
    ) -> T::Output {
        let task_name = task.name();
        match self.consult(task, input).await {
            Ok(output) => output,
            Err(err) => {
                tracing::warn!(task = %task_name, error = %err, "Steward consultation failed, using fail-open fallback");
                fallback
            }
        }
    }

    /// Specialized helper: Evaluate recent history for semantic doom loops.
    pub async fn check_semantic_loop(
        &self,
        recent_signatures: Vec<String>,
        recent_context: String,
    ) -> SemanticLoopVerdict {
        let task = SemanticLoopSentinelTask;
        let input = SemanticLoopInput {
            recent_signatures,
            recent_context,
        };
        let fallback = SemanticLoopVerdict {
            is_loop: false,
            pattern: None,
            remedy_nudge: None,
        };
        self.consult_with_fallback(task, input, fallback).await
    }

    /// Specialized helper: Verify sanity of a proposed action or string.
    pub async fn verify_sanity(
        &self,
        action_type: impl Into<String>,
        payload: impl Into<String>,
        justification: impl Into<String>,
    ) -> SanityCheckVerdict {
        let task = SanityVerifierTask;
        let input = SanityCheckInput {
            action_type: action_type.into(),
            payload: payload.into(),
            justification: justification.into(),
        };
        let fallback = SanityCheckVerdict {
            is_sane: true,
            risk_level: muta_contracts::RiskLevel::Safe,
            critique: "Verification skipped or timed out (fail-open)".to_string(),
        };
        self.consult_with_fallback(task, input, fallback).await
    }

    /// Specialized helper: Generate a clean session title from an excerpt.
    pub async fn generate_title(&self, excerpt: impl Into<String>) -> Option<String> {
        let task = SessionTitlerTask;
        let input = SessionTitlerInput {
            excerpt: excerpt.into(),
        };
        match self.consult(task, input).await {
            Ok(SessionTitleOutput { title }) => clean_title(&title),
            Err(StewardError::DeserializationError { raw, .. }) => {
                // If the model returned plain text instead of JSON, clean that directly.
                clean_title(&raw)
            }
            Err(err) => {
                tracing::warn!(error = %err, "Steward title generation failed");
                None
            }
        }
    }
}

/// Extract and parse JSON from model output, stripping markdown formatting if present.
fn parse_structured_json<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, StewardError> {
    let cleaned = strip_markdown_code_fence(raw);
    serde_json::from_str(cleaned).map_err(|e| StewardError::DeserializationError {
        error: e.to_string(),
        raw: raw.to_string(),
    })
}

/// Strip wrapping ```json ... ``` code fences from model outputs.
fn strip_markdown_code_fence(s: &str) -> &str {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("```json")
        && let Some(inner) = rest.strip_suffix("```")
    {
        return inner.trim();
    }
    if let Some(rest) = trimmed.strip_prefix("```")
        && let Some(inner) = rest.strip_suffix("```")
    {
        return inner.trim();
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use muta_contracts::Message;

    struct MockProvider {
        response: Result<String, String>,
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(&self, _req: ModelRequest) -> Result<Message, String> {
            match &self.response {
                Ok(content) => Ok(Message::new(Role::Assistant, content)),
                Err(err) => Err(err.clone()),
            }
        }

        async fn stream_chat(
            &self,
            _req: ModelRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn steward_consults_and_parses_json() {
        let json = r#"{"is_loop": true, "pattern": "oscillating edit", "remedy_nudge": "Read the file first"}"#;
        let provider = Arc::new(MockProvider {
            response: Ok(format!("```json\n{json}\n```")),
        });
        let steward = Steward::new(provider);

        let verdict = steward
            .check_semantic_loop(vec!["edit_file a.rs".into()], "test context".into())
            .await;
        assert!(verdict.is_loop);
        assert_eq!(verdict.pattern.as_deref(), Some("oscillating edit"));
        assert_eq!(verdict.remedy_nudge.as_deref(), Some("Read the file first"));
    }

    #[tokio::test]
    async fn steward_fails_open_on_provider_error() {
        let provider = Arc::new(MockProvider {
            response: Err("HTTP 500 error".to_string()),
        });
        let steward = Steward::new(provider);

        let verdict = steward
            .check_semantic_loop(vec!["read_text".into()], "context".into())
            .await;
        assert!(!verdict.is_loop); // fail-open default
    }

    #[tokio::test]
    async fn steward_title_generation() {
        let json = r#"{"title": "Fix memory leak in network pool"}"#;
        let provider = Arc::new(MockProvider {
            response: Ok(json.to_string()),
        });
        let steward = Steward::new(provider);

        let title = steward.generate_title("user: memory leak fix").await;
        assert_eq!(title.as_deref(), Some("Fix memory leak in network pool"));
    }
}
