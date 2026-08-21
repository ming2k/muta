//! Orchestration-layer integration tests: provider retry behavior, the
//! proxy provider, retry-delay math, context-overflow classification, and
//! the self-registration of built-in tools via `inventory`. These live with
//! the code under test (they were historically parked in the `neenee-cli`
//! binary, which exercised this layer end-to-end before ADR-0096 moved
//! session hosting into the daemon).

// Tests panic on assertion failure by design; the workspace's unwrap/expect
// warnings are meant for production code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use neenee_agent::Agent;
use neenee_agent::orchestration::{
    ContextProjectionSettings, ProxyProvider, RoundContext, RoundInput, apply_jitter_ms,
    execute_round, retry_delay_ms,
};
use neenee_contracts::{
    AgentResponse, Message, Provider, ProviderStreamEvent, Role, RoundEvent, ToolContextBuilder,
    async_trait, collect_toolset,
};
use neenee_persistence::session::SessionStore;
use neenee_skills::SkillRegistry;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use futures::stream;

struct RetryOnceProvider(AtomicUsize);
struct PartialToolRetryProvider(AtomicUsize);
struct ToolThenRetryProvider {
    attempts: AtomicUsize,
    requests: Arc<Mutex<Vec<String>>>,
}
struct AlwaysRetryableProvider;
struct RetryReadTool(Arc<AtomicUsize>);

/// Minimal provider whose `chat` returns a canned reply — used by the
/// proxy-provider test to verify it does not block the async runtime.
struct MockProvider;

#[async_trait]
impl Provider for MockProvider {
    async fn chat(&self, _request: neenee_contracts::ModelRequest) -> Result<Message, String> {
        Ok(Message::new(
            Role::Assistant,
            "Hello! I am a mock AI. How can I help you today?",
        ))
    }

    async fn stream_chat(
        &self,
        _request: neenee_contracts::ModelRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
        Ok(Box::pin(stream::empty()))
    }
}

/// Most built-in tools self-register via `inventory` across the neenee-agent
/// and neenee-persistence crates. This test guards the one real
/// risk of that approach — that a crate's `inventory::submit!` nodes get
/// dropped by the linker — by asserting the assembled set contains every
/// expected built-in tool name.
#[test]
fn registry_collects_all_self_registered_tools() {
    let mut builder = ToolContextBuilder::new();
    builder.provide(Arc::new(SkillRegistry::empty()));
    builder.provide(neenee_agent::AgentIdentity::default());
    let ctx = builder.build();
    let collected = collect_toolset(&ctx);
    let names: std::collections::HashSet<&str> = collected.capability_names().collect();
    for expected in [
        "bash",
        "read_text",
        "read_image",
        "write_file",
        "edit_file",
        "grep",
        "glob",
        "list_dir",
        "ask_user",
        "webfetch",
        "websearch",
        "use_skill",
        "list_skills",
    ] {
        assert!(
            names.contains(expected),
            "self-registered tool '{expected}' missing from collected set; \
             a crate's inventory submission was likely stripped by the linker"
        );
    }
}

#[async_trait]
impl Provider for RetryOnceProvider {
    async fn chat(&self, _request: neenee_contracts::ModelRequest) -> Result<Message, String> {
        Err("non-streaming path should not be used".to_string())
    }

    async fn stream_chat(
        &self,
        _request: neenee_contracts::ModelRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: neenee_contracts::ModelRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<ProviderStreamEvent, String>>, String>
    {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::TextDelta("partial".to_string())),
                Err(neenee_contracts::retryable_error("rate limited", Some(1))),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![Ok(
                ProviderStreamEvent::TextDelta("done".to_string()),
            )])))
        }
    }
}

#[async_trait]
impl Provider for PartialToolRetryProvider {
    async fn chat(&self, _request: neenee_contracts::ModelRequest) -> Result<Message, String> {
        Err("non-streaming path should not be used".to_string())
    }

    async fn stream_chat(
        &self,
        _request: neenee_contracts::ModelRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: neenee_contracts::ModelRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<ProviderStreamEvent, String>>, String>
    {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("partial-call".to_string()),
                    name: Some("retry_read".to_string()),
                    arguments: "{".to_string(),
                }),
                Err(neenee_contracts::retryable_error("stream dropped", None)),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![Ok(
                ProviderStreamEvent::TextDelta("done".to_string()),
            )])))
        }
    }
}

#[async_trait]
impl Provider for ToolThenRetryProvider {
    async fn chat(&self, _request: neenee_contracts::ModelRequest) -> Result<Message, String> {
        Err("non-streaming path should not be used".to_string())
    }

    async fn stream_chat(
        &self,
        _request: neenee_contracts::ModelRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        request: neenee_contracts::ModelRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<ProviderStreamEvent, String>>, String>
    {
        let mut messages = serde_json::to_value(&request.messages)
            .expect("messages should serialize to a JSON value");
        for message in messages
            .as_array_mut()
            .expect("serialized messages should be an array")
        {
            message
                .as_object_mut()
                .expect("serialized message should be an object")
                .remove("timestamp");
        }
        self.requests
            .lock()
            .expect("request log lock poisoned")
            .push(serde_json::to_string(&messages).expect("messages should serialize"));
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        match attempt {
            0 | 2 => Ok(Box::pin(stream::iter(vec![Ok(
                ProviderStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some(if attempt == 0 { "call" } else { "retry-call" }.to_string()),
                    name: Some("retry_read".to_string()),
                    arguments: "{}".to_string(),
                },
            )]))),
            1 => Ok(Box::pin(stream::iter(vec![Err(
                neenee_contracts::retryable_error("upstream unavailable", None),
            )]))),
            _ => Ok(Box::pin(stream::iter(vec![Ok(
                ProviderStreamEvent::TextDelta("done".to_string()),
            )]))),
        }
    }
}

#[async_trait]
impl Provider for AlwaysRetryableProvider {
    async fn chat(&self, _request: neenee_contracts::ModelRequest) -> Result<Message, String> {
        Err("non-streaming path should not be used".to_string())
    }

    async fn stream_chat(
        &self,
        _request: neenee_contracts::ModelRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        _request: neenee_contracts::ModelRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<ProviderStreamEvent, String>>, String>
    {
        // Every request fails with a retryable error so the turn exhausts
        // its retry budget without ever touching a tool.
        Ok(Box::pin(stream::iter(vec![Err(
            neenee_contracts::retryable_error("OpenAI HTTP 429 Too Many Requests", None),
        )])))
    }
}

#[async_trait]
impl neenee_contracts::Tool for RetryReadTool {
    fn name(&self) -> &str {
        "retry_read"
    }

    fn description(&self) -> &str {
        "retry safety test"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn call(&self, _arguments: &str) -> Result<String, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("read".to_string())
    }
}

#[tokio::test]
async fn proxy_provider_does_not_block_the_async_runtime() {
    let holder: Arc<RwLock<Arc<dyn Provider>>> = Arc::new(RwLock::new(Arc::new(MockProvider)));
    let proxy = ProxyProvider::new(holder);

    let response = proxy
        .chat(neenee_contracts::ModelRequest::new(Vec::new()))
        .await
        .unwrap();

    assert!(response.content.contains("mock AI"));
}

#[test]
fn context_overflow_detection_is_conservative() {
    assert!(neenee_contracts::is_context_overflow(
        "maximum context length exceeded for this model"
    ));
    assert!(neenee_contracts::is_context_overflow(
        "too many tokens in request"
    ));
    assert!(!neenee_contracts::is_context_overflow(
        "network connection reset"
    ));
}

#[tokio::test]
async fn turn_retries_transient_provider_failure_before_tool_activity() {
    let directory =
        std::env::temp_dir().join(format!("neenee-retry-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let agent = Arc::new(Agent::new(
        Arc::new(RetryOnceProvider(AtomicUsize::new(0))),
        Vec::new(),
        neenee_agent::AgentIdentity::default(),
    ));
    let ledger = neenee_contracts::TokenSourceLedger::shared();
    agent.install_token_ledger(ledger.clone());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;

    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session_id.clone(),
            session: session.clone(),
            projection: ContextProjectionSettings {
                budget: neenee_contracts::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 3,
            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: neenee_agent::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap();
    assert!(
        session
            .model_window()
            .await
            .iter()
            .any(|message| message.content == "done")
    );
    let responses = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    let activities = responses
        .iter()
        .filter_map(|response| match response {
            AgentResponse::Round {
                event: RoundEvent::Activity(status),
                ..
            } => Some(status.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(activities.starts_with(&["saving request", "preparing context"]));
    assert_eq!(
        activities
            .iter()
            .filter(|status| **status == "waiting for model")
            .count(),
        2
    );
    assert_eq!(activities.last(), Some(&"saving response"));
    assert!(responses.iter().any(|response| matches!(
        response,
        AgentResponse::Round {
            event: RoundEvent::RetryScheduled {
                attempt: 2,
                max_attempts: 3,
                ..
            },
            ..
        }
    )));
    assert!(responses.iter().any(|response| matches!(
        response,
        AgentResponse::Round {
            event: RoundEvent::StreamDiscard,
            ..
        }
    )));
    let attempts = ledger.records_for_session(&session_id);
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].key.attempt, 1);
    assert_eq!(
        attempts[0].status,
        neenee_contracts::RequestUsageStatus::Failed
    );
    assert_eq!(attempts[1].key.attempt, 2);
    assert_eq!(
        attempts[1].status,
        neenee_contracts::RequestUsageStatus::Completed
    );
    assert_eq!(session.request_usage_records().await, attempts);
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn partial_tool_stream_is_not_executed_before_provider_retry() {
    let directory = std::env::temp_dir().join(format!(
        "neenee-retry-partial-tool-{}",
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(Agent::new(
        Arc::new(PartialToolRetryProvider(AtomicUsize::new(0))),
        vec![Arc::new(RetryReadTool(tool_calls.clone()))],
        neenee_agent::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();

    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session: session.clone(),
            projection: ContextProjectionSettings {
                budget: neenee_contracts::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 3,
            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: neenee_agent::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap();
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0);
    assert!(
        session
            .model_window()
            .await
            .iter()
            .any(|message| message.content == "done")
    );
    let responses = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(responses.iter().any(|response| matches!(
        response,
        AgentResponse::Round {
            event: RoundEvent::RetryScheduled { attempt: 2, .. },
            ..
        }
    )));
    assert!(!responses.iter().any(|response| matches!(
        response,
        AgentResponse::Round {
            event: RoundEvent::ToolCall { .. },
            ..
        }
    )));
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn turn_resumes_provider_request_after_completed_tool_activity() {
    let directory =
        std::env::temp_dir().join(format!("neenee-retry-tool-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(Agent::new(
        Arc::new(ToolThenRetryProvider {
            attempts: AtomicUsize::new(0),
            requests: requests.clone(),
        }),
        vec![Arc::new(RetryReadTool(tool_calls.clone()))],
        neenee_agent::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();

    execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session: session.clone(),
            projection: ContextProjectionSettings {
                budget: neenee_contracts::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 4,
            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: neenee_agent::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap();
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
    assert!(
        session
            .model_window()
            .await
            .iter()
            .any(|message| message.content == "done")
    );
    let requests = requests.lock().expect("request log lock poisoned");
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[1], requests[2],
        "the retry must resend the exact request checkpoint after the tool result"
    );
    assert!(
        std::iter::from_fn(|| rx.try_recv().ok()).any(|response| matches!(
            response,
            AgentResponse::Round {
                event: RoundEvent::RetryScheduled { attempt: 2, .. },
                ..
            }
        ))
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn turn_exhaustion_message_explains_retry_budget() {
    let directory =
        std::env::temp_dir().join(format!("neenee-retry-exhaust-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let agent = Arc::new(Agent::new(
        Arc::new(AlwaysRetryableProvider),
        Vec::new(),
        neenee_agent::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();

    let error = execute_round(
        RoundContext {
            agent,
            tx,
            token: CancellationToken::new(),
            session_id: session.id().await,
            session,
            projection: ContextProjectionSettings {
                budget: neenee_contracts::CompactionPolicy::default().resolve(100_000),
                preserve_rounds: 6,
                summarize: false,
                prune: false,
                prune_protect_tokens: 0,
            },
            retry_max_attempts: 3,
            retry_base_ms: 1,
            retry_max_ms: 10,
            emit_round_completed: false,
        },
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: neenee_agent::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap_err();

    let error_string = error.to_string();
    assert!(
        error_string.starts_with("OpenAI HTTP 429 Too Many Requests"),
        "should surface the provider message: {error_string}"
    );
    // All attempts but the last must announce a retry; the final failure
    // surfaces as the error above instead.
    let scheduled = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|response| {
            matches!(
                response,
                AgentResponse::Round {
                    event: RoundEvent::RetryScheduled { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        scheduled, 2,
        "should schedule retries for every attempt before giving up"
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn retry_delay_honors_headers_and_exponential_bounds() {
    assert_eq!(retry_delay_ms(1, None, 1_000, 30_000), 1_000);
    assert_eq!(retry_delay_ms(3, None, 1_000, 30_000), 4_000);
    assert_eq!(retry_delay_ms(2, Some(45_000), 1_000, 30_000), 30_000);
    assert_eq!(retry_delay_ms(1, Some(0), 1_000, 30_000), 1_000);
}

#[test]
fn apply_jitter_stays_within_half_to_full_range() {
    // Equal jitter: result ∈ [base/2, base]. A roll of 0 yields the floor,
    // a roll of the full span yields the ceiling — both bounds are closed.
    assert_eq!(apply_jitter_ms(1_000, |_| 0), 500);
    assert_eq!(apply_jitter_ms(1_000, |span| span), 1_000);
    // A mid-range roll lands exactly halfway between floor and ceiling.
    assert_eq!(apply_jitter_ms(1_000, |span| span / 2), 750);
    // Odd base still floors cleanly: [1500/2 .. 1500].
    assert_eq!(apply_jitter_ms(1_500, |_| 0), 750);
    assert_eq!(apply_jitter_ms(1_500, |span| span), 1_500);
}

#[test]
fn apply_jitter_never_exceeds_base() {
    // A pathological roll larger than the span must be clamped back to the
    // ceiling, so jitter can never push a delay past the configured cap.
    assert_eq!(apply_jitter_ms(1_000, |_| u64::MAX), 1_000);
}

#[test]
fn apply_jitter_passes_zero_through_unchanged() {
    assert_eq!(apply_jitter_ms(0, |_| 1_000), 0);
}

/// A provider that fails *terminally* (a non-retryable error) on the first
/// round and succeeds on the second — the exact shape a `/retry` resumes
/// from. `requests` records the message-history shape of every provider
/// request so the test can assert the resume re-sent exactly the committed
/// checkpoint (no duplicate user message, no half-streamed assistant text).
struct FailThenSucceedProvider {
    attempts: AtomicUsize,
    requests: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for FailThenSucceedProvider {
    async fn chat(&self, _request: neenee_contracts::ModelRequest) -> Result<Message, String> {
        Err("non-streaming path should not be used".to_string())
    }

    async fn stream_chat(
        &self,
        _request: neenee_contracts::ModelRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        request: neenee_contracts::ModelRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<ProviderStreamEvent, String>>, String>
    {
        self.requests
            .lock()
            .expect("request log lock poisoned")
            .push(serde_json::to_string(&request.messages).expect("messages should serialize"));
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            // Terminal: `parse_retryable_error` finds no envelope, so the
            // harness surfaces it and (with ADR-0128) arms the resume point.
            Ok(Box::pin(stream::iter(vec![Err(
                "terminal: model refused the request".to_string(),
            )])))
        } else {
            Ok(Box::pin(stream::iter(vec![Ok(
                ProviderStreamEvent::TextDelta("recovered".to_string()),
            )])))
        }
    }
}

/// The round-level `/retry` contract (ADR-0128): a round whose provider
/// fails terminally parks a durable resume point, and resuming through it
/// *continues the same round* — the round counter never advances, no second
/// user message is appended, and the turn sequence numbers onward instead of
/// restarting. Meanwhile a round that completed naturally leaves no point,
/// so a second `/retry` has nothing to resume.
#[tokio::test]
async fn retry_resumes_stopped_round_without_breaking_turn_sequence() {
    let directory =
        std::env::temp_dir().join(format!("neenee-retry-resume-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&directory);
    let session = Arc::new(SessionStore::for_path(directory.join("session.json")));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Arc::new(Agent::new(
        Arc::new(FailThenSucceedProvider {
            attempts: AtomicUsize::new(0),
            requests: requests.clone(),
        }),
        Vec::new(),
        neenee_agent::AgentIdentity::default(),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;
    let context = |tx| RoundContext {
        agent: Arc::clone(&agent),
        tx,
        token: CancellationToken::new(),
        session_id: session_id.clone(),
        session: Arc::clone(&session),
        projection: ContextProjectionSettings {
            budget: neenee_contracts::CompactionPolicy::default().resolve(100_000),
            preserve_rounds: 6,
            summarize: false,
            prune: false,
            prune_protect_tokens: 0,
        },
        retry_max_attempts: 3,
        retry_base_ms: 1,
        retry_max_ms: 10,
        emit_round_completed: false,
    };

    // 1. The fresh round fails terminally.
    let error = execute_round(
        context(tx.clone()),
        RoundInput {
            prompt: "work".to_string(),
            hidden: false,
            display_prompt: None,
            sent_at_ms: None,
            images: Vec::new(),
            driver: neenee_agent::orchestration::RoundDriver::Fresh,
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("terminal"), "{error}");

    // The failed round parked its resume point: same round, committed
    // history watermark, zero committed turns (nothing streamed through).
    let point = session
        .retry_pending()
        .await
        .expect("failed round must arm a retry point");
    assert_eq!(point.round, 1, "the parked point names round 1");
    assert_eq!(point.turns_committed, 0);
    assert_eq!(point.history_watermark, 1, "user message only");
    assert_eq!(session.round_counter().await, 1);

    // 2. `/retry` resumes and completes the *same* round.
    let round_before = session.round_counter().await;
    execute_round(
        context(tx),
        RoundInput::resume(neenee_contracts::RetryPoint {
            round: point.round,
            turns_committed: point.turns_committed,
            history_watermark: point.history_watermark,
            paused_ms: point.paused_ms,
            at_ms: point.at_ms,
        }),
    )
    .await
    .unwrap();

    // Round counter never advanced: the resume completed round 1, it did
    // not mint round 2. This is the headline invariant of /retry.
    assert_eq!(
        session.round_counter().await,
        round_before,
        "retry must not advance the round counter"
    );
    // The window holds user + assistant — exactly one user message (no
    // duplicate from the resume re-sending it as a fresh prompt).
    let window = session.model_window().await;
    let user_count = window
        .iter()
        .filter(|message| message.role == Role::User)
        .count();
    assert_eq!(
        user_count, 1,
        "resume must not append a second user message"
    );
    assert!(
        window.iter().any(|message| message.content == "recovered"),
        "the resumed round's answer is committed"
    );
    // Both provider requests carried the same semantic history. The system
    // prompt is projected afresh for each request and its local diagnostic
    // timestamp may advance; timestamp is not provider protocol content and
    // is removed by the request recorder above.
    let logged = requests.lock().expect("request log lock poisoned").clone();
    assert_eq!(logged.len(), 2);
    assert_eq!(
        logged[0], logged[1],
        "resume re-sends the committed checkpoint, not a mutated history"
    );

    // 3. Completion retired the point: a subsequent `/retry` has nothing
    //    to resume (the completed-round rule).
    assert!(
        session.retry_pending().await.is_none(),
        "a naturally completed round leaves no retry point"
    );

    let _ = std::fs::remove_dir_all(directory);
}
