//! Cognitive execution engine: typed, resilient out-of-band runner for Agent Harness (ADR-0167).
//!
//! # Architecture
//!
//! The [`CognitivePipeline`] provides internal cognitive execution for the Agent Harness.
//!
//! Key design invariants:
//! - **Typed Tasks**: Dispatches any task implementing [`CognitiveTask`].
//! - **Timeout Bounds**: Strict timeouts on every consult call, preventing background task leaks.
//! - **Fail-Open Resilience**: Helper methods guarantee graceful fallback if model calls fail or timeout.
//! - **JSON Normalization**: Extracts structured JSON payloads even if the model wraps them in markdown.

use std::sync::Arc;
use std::time::Duration;

use muta_contracts::{
    CognitiveTask, Message, ModelRequest, Provider, Role, SessionDigestInput, SessionDigestTask,
    StreamLoopReviewInput, StreamLoopReviewerTask, StreamLoopVerdict,
};

/// Errors that can occur during a cognitive task consultation.
#[derive(Debug)]
pub enum CognitiveError {
    Timeout(Duration),
    ProviderError(String),
    DeserializationError { error: String, raw: String },
    EmptyResponse,
}

impl std::fmt::Display for CognitiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout(d) => write!(f, "Cognitive task timed out after {d:?}"),
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

impl std::error::Error for CognitiveError {}

/// The Harness Cognitive execution pipeline.
#[derive(Clone)]
pub struct CognitivePipeline {
    provider: Arc<dyn Provider>,
}

impl CognitivePipeline {
    /// Create a new cognitive pipeline bound to `provider`.
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }

    /// Access the underlying provider.
    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    /// Consult the cognitive pipeline with a typed [`CognitiveTask`].
    pub async fn consult<T: CognitiveTask>(
        &self,
        task: T,
        input: T::Input,
    ) -> Result<T::Output, CognitiveError> {
        let timeout = Duration::from_millis(task.timeout_ms());
        let instructions =
            muta_contracts::InstructionBundle::new(vec![muta_contracts::InstructionSlice::new(
                "harness.cognitive_task",
                muta_contracts::InstructionTier::Task,
                task.system_prompt(),
            )]);
        let messages = vec![Message::new(Role::User, task.render_prompt(&input))];
        tracing::debug!(task = task.name(), "cognitive pipeline consult");

        let response = tokio::time::timeout(
            timeout,
            self.provider
                .chat(ModelRequest::ephemeral(messages).with_instructions(instructions)),
        )
        .await
        .map_err(|_| CognitiveError::Timeout(timeout))?
        .map_err(|e| CognitiveError::ProviderError(e.to_string()))?;

        let content = response.message.content.as_str();
        if content.trim().is_empty() {
            return Err(CognitiveError::EmptyResponse);
        }

        task.parse_output(content)
            .map_err(|error| CognitiveError::DeserializationError {
                error,
                raw: content.to_string(),
            })
    }

    /// Consult with automatic fallback (Fail-Open pattern).
    ///
    /// If the pipeline fails, times out, or returns invalid JSON, this logs a warning
    /// and returns `fallback` to prevent blocking the production loop.
    pub async fn consult_with_fallback<T: CognitiveTask>(
        &self,
        task: T,
        input: T::Input,
        fallback: T::Output,
    ) -> T::Output {
        let task_name = task.name();
        match self.consult(task, input).await {
            Ok(output) => output,
            Err(err) => {
                tracing::warn!(task = %task_name, error = %err, "Cognitive consultation failed, using fail-open fallback");
                fallback
            }
        }
    }

    /// Confirm or clear an L1 in-flight stream-loop candidate.
    ///
    /// The output grammar is the strict bare-token contract owned by
    /// [`StreamLoopReviewerTask`]. Any timeout, provider failure, or malformed
    /// answer is fail-open `no`: an infrastructure judgment can authorize a
    /// cutoff only with an explicit valid `yes`.
    pub async fn review_stream_loop(&self, input: StreamLoopReviewInput) -> StreamLoopVerdict {
        self.consult_with_fallback(StreamLoopReviewerTask, input, StreamLoopVerdict::No)
            .await
    }

    /// Distill an excerpt (plus an optional previous digest for revision) into a structured session digest.
    ///
    /// Unlike the fail-open sentinels this returns `None` on any failure —
    /// timeout, provider error, or malformed JSON — because there is no
    /// sensible plain-text fallback for a structured digest; the caller
    /// keeps the previous digest instead.
    pub async fn generate_digest(
        &self,
        input: SessionDigestInput,
    ) -> Option<muta_contracts::SessionDigest> {
        match self.consult(SessionDigestTask, input).await {
            Ok(digest) => Some(digest),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "Session digest generation failed; keeping previous digest"
                );
                None
            }
        }
    }
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
        async fn chat(
            &self,
            _req: ModelRequest,
        ) -> Result<muta_contracts::ProviderCompletion, muta_contracts::ProviderError> {
            match &self.response {
                Ok(content) => Ok(muta_contracts::ProviderCompletion::message(Message::new(
                    Role::Assistant,
                    content,
                ))),
                Err(err) => Err(muta_contracts::ProviderError::new(
                    "mock",
                    muta_contracts::ProviderErrorKind::Other,
                    err.clone(),
                )),
            }
        }

        async fn stream_chat(
            &self,
            _req: ModelRequest,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<String, muta_contracts::ProviderError>>,
            muta_contracts::ProviderError,
        > {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn cognitive_digest_parses_and_fails_open() {
        let json = r#"{"title": "Fix auth loop", "intent": "User wants login fixed.", "history": ["Reproduced the loop"]}"#;
        let provider = Arc::new(MockProvider {
            response: Ok(format!("```json\n{json}\n```")),
        });
        let digest = CognitivePipeline::new(provider)
            .generate_digest(SessionDigestInput {
                excerpt: "user: fix the login loop".to_string(),
                previous: None,
            })
            .await
            .expect("fenced JSON parses");
        assert_eq!(digest.title, "Fix auth loop");
        assert_eq!(digest.history, vec!["Reproduced the loop".to_string()]);

        let provider = Arc::new(MockProvider {
            response: Err("HTTP 500 error".to_string()),
        });
        assert!(
            CognitivePipeline::new(provider)
                .generate_digest(SessionDigestInput {
                    excerpt: "x".to_string(),
                    previous: None,
                })
                .await
                .is_none()
        );
        let provider = Arc::new(MockProvider {
            response: Ok("not json at all".to_string()),
        });
        assert!(
            CognitivePipeline::new(provider)
                .generate_digest(SessionDigestInput {
                    excerpt: "x".to_string(),
                    previous: None,
                })
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn loop_reviewer_verdicts_are_fail_open_on_error() {
        let provider = Arc::new(MockProvider {
            response: Ok("yes".to_string()),
        });
        let verdict = CognitivePipeline::new(provider)
            .review_stream_loop(StreamLoopReviewInput {
                heuristic_candidate: "abab".to_string(),
                channel: muta_contracts::StreamLoopChannel::AssistantText,
                preceding_context: String::new(),
                assistant_text: "abababab".to_string(),
                reasoning_text: String::new(),
            })
            .await;
        assert_eq!(verdict, StreamLoopVerdict::Yes);

        let provider = Arc::new(MockProvider {
            response: Ok("no".to_string()),
        });
        let verdict = CognitivePipeline::new(provider)
            .review_stream_loop(StreamLoopReviewInput {
                heuristic_candidate: "abab".to_string(),
                channel: muta_contracts::StreamLoopChannel::AssistantText,
                preceding_context: String::new(),
                assistant_text: "abababab".to_string(),
                reasoning_text: String::new(),
            })
            .await;
        assert_eq!(verdict, StreamLoopVerdict::No);

        let provider = Arc::new(MockProvider {
            response: Ok("maybe or invalid".to_string()),
        });
        let verdict = CognitivePipeline::new(provider)
            .review_stream_loop(StreamLoopReviewInput {
                heuristic_candidate: "abab".to_string(),
                channel: muta_contracts::StreamLoopChannel::AssistantText,
                preceding_context: String::new(),
                assistant_text: "abababab".to_string(),
                reasoning_text: String::new(),
            })
            .await;
        assert_eq!(verdict, StreamLoopVerdict::No);
    }
}
