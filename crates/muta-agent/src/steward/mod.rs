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
    Message, ModelRequest, Provider, Role, SessionDigestInput, SessionDigestTask, StewardTask,
    StreamLoopReviewInput, StreamLoopReviewerTask, StreamLoopVerdict, steward_identity,
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
    ///
    /// The on-duty identity is office-first: when the task is staffed at an
    /// office ([`StewardTask::office`]), its charter-signed identity opens
    /// the system prompt anchored by the collective Steward mission;
    /// unassigned tasks keep the plain collective preamble.
    pub async fn consult<T: StewardTask>(
        &self,
        task: T,
        input: T::Input,
    ) -> Result<T::Output, StewardError> {
        let timeout = Duration::from_millis(task.timeout_ms());
        let (identity, office_id) = match task.office() {
            Some(office) => {
                let id = office.id();
                let mission = steward_identity().mission;
                let office_identity = office.identity();
                (
                    format!(
                        "{}, serving {mission}. {}",
                        office_identity.name, office_identity.mission
                    ),
                    id,
                )
            }
            None => (steward_identity().preamble(), "unassigned"),
        };
        let messages = vec![
            Message::new(
                Role::System,
                format!("{identity}\n\n{}", task.system_prompt()),
            ),
            Message::new(Role::User, task.render_prompt(&input)),
        ];
        tracing::debug!(task = task.name(), office = office_id, "steward consult");

        let response = tokio::time::timeout(
            timeout,
            self.provider.chat(ModelRequest::ephemeral(messages)),
        )
        .await
        .map_err(|_| StewardError::Timeout(timeout))?
        .map_err(|e| StewardError::ProviderError(e.to_string()))?;

        // Steward completions own their metadata. Because nothing is stashed
        // on the provider, concurrent primary requests cannot observe it.
        let content = response.message.content.as_str();
        if content.trim().is_empty() {
            return Err(StewardError::EmptyResponse);
        }

        task.parse_output(content)
            .map_err(|error| StewardError::DeserializationError {
                error,
                raw: content.to_string(),
            })
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

    /// Specialized helper: distill an excerpt (plus an optional previous
    /// digest for revision) into a structured session digest.
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
                    "Steward digest generation failed; keeping previous digest"
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
    async fn steward_digest_parses_and_fails_open() {
        let json = r#"{"title": "Fix auth loop", "intent": "User wants login fixed.", "history": ["Reproduced the loop"]}"#;
        let provider = Arc::new(MockProvider {
            response: Ok(format!("```json\n{json}\n```")),
        });
        let digest = Steward::new(provider)
            .generate_digest(SessionDigestInput {
                excerpt: "user: fix the login loop".to_string(),
                previous: None,
            })
            .await
            .expect("fenced JSON parses");
        assert_eq!(digest.title, "Fix auth loop");
        assert_eq!(digest.history, vec!["Reproduced the loop".to_string()]);

        // Provider failure and malformed JSON both fail-open to `None` —
        // metadata generation must never break a session.
        let provider = Arc::new(MockProvider {
            response: Err("HTTP 500 error".to_string()),
        });
        assert!(
            Steward::new(provider)
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
            Steward::new(provider)
                .generate_digest(SessionDigestInput {
                    excerpt: "x".to_string(),
                    previous: None,
                })
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn stream_loop_review_accepts_only_strict_yes_or_no() {
        let input = StreamLoopReviewInput {
            heuristic_candidate: "periodic suffix".to_string(),
            channel: muta_contracts::StreamLoopChannel::AssistantText,
            preceding_context: "user requested a hex dump".to_string(),
            assistant_text: "00 00 00".to_string(),
            reasoning_text: String::new(),
        };
        let yes = Steward::new(Arc::new(MockProvider {
            response: Ok("yes".to_string()),
        }))
        .review_stream_loop(input.clone())
        .await;
        assert_eq!(yes, StreamLoopVerdict::Yes);

        for malformed in ["YES", "{\"verdict\":\"yes\"}", "yes, this is a loop"] {
            let verdict = Steward::new(Arc::new(MockProvider {
                response: Ok(malformed.to_string()),
            }))
            .review_stream_loop(input.clone())
            .await;
            assert_eq!(
                verdict,
                StreamLoopVerdict::No,
                "fail open for {malformed:?}"
            );
        }
    }

    #[tokio::test]
    async fn digest_prompt_carries_previous_digest_when_revising() {
        struct CapturingProvider {
            last_user_prompt: std::sync::Mutex<String>,
        }

        #[async_trait]
        impl Provider for CapturingProvider {
            async fn chat(
                &self,
                request: ModelRequest,
            ) -> Result<muta_contracts::ProviderCompletion, muta_contracts::ProviderError>
            {
                if let Some(last) = request.messages.last() {
                    *self.last_user_prompt.lock().unwrap() = last.content.clone();
                }
                Ok(muta_contracts::ProviderCompletion::message(Message::new(
                    Role::Assistant,
                    r#"{"title":"T","intent":"I","history":[]}"#,
                )))
            }
            async fn stream_chat(
                &self,
                _request: ModelRequest,
            ) -> Result<
                futures::stream::BoxStream<'static, Result<String, muta_contracts::ProviderError>>,
                muta_contracts::ProviderError,
            > {
                Ok(Box::pin(futures::stream::empty()))
            }
        }

        let provider = Arc::new(CapturingProvider {
            last_user_prompt: std::sync::Mutex::new(String::new()),
        });
        let steward = Steward::new(provider.clone());
        steward
            .generate_digest(SessionDigestInput {
                excerpt: "user: continue".to_string(),
                previous: Some(r#"{"title":"Old","intent":"Old intent","history":[]}"#.to_string()),
            })
            .await
            .expect("valid JSON digest");
        let prompt = provider.last_user_prompt.lock().unwrap().clone();
        assert!(
            prompt.contains("Previous digest (revise it)"),
            "revision prompt must embed the previous digest"
        );
        assert!(prompt.contains("Old intent"));
    }
}
