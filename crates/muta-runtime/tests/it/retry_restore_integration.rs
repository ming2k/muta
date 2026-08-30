#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests: durable `/retry` resume point survives session close and resume
//! across the entire bootstrap, slash handler, and harness projection lifecycle.

use std::sync::Arc;

use super::sandbox_once;
use muta_agent::orchestration::send_harness_state_for_session;
use muta_contracts::{AgentResponse, LoopStatus, RetryPoint, RoundEvent};
use muta_persistence::session::SessionStore;
use muta_runtime::UiBridge;
use muta_runtime::bootstrap::{self, BootstrapParams};
use muta_runtime::startup::SessionStart;
use tokio::sync::mpsc;

struct HeadlessProbe;

#[async_trait::async_trait]
impl UiBridge for HeadlessProbe {
    async fn copy_to_clipboard(&self, _text: &str) -> Result<muta_runtime::CopyOutcome, String> {
        Err("probe: headless".to_string())
    }
}

fn params(project_root: std::path::PathBuf, startup: SessionStart) -> BootstrapParams {
    let identity = muta_contracts::AgentIdentity::new("probe", "retry probe");
    BootstrapParams {
        human_channel: None,
        identity: identity.clone(),
        master: muta_contracts::MasterPreset::with_identity("probe", identity),
        ui: Arc::new(HeadlessProbe),
        startup,
        project_root: Some(project_root),
        delegated: false,
        teardown_token: None,
    }
}

async fn assemble_for(project: &std::path::Path, resume_id: &str) -> bootstrap::Bootstrap {
    bootstrap::assemble(params(
        project.to_path_buf(),
        SessionStart::Resume(resume_id.to_string()),
    ))
    .await
    .expect("assemble succeeds")
}

#[tokio::test]
async fn retry_point_survives_process_death_and_projects_accurate_harness_state() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("retry-project");

    let store = Arc::new(SessionStore::load_for_project(project.clone()));
    store
        .replace_messages(vec![
            muta_contracts::Message::new(muta_contracts::Role::User, "run task"),
            muta_contracts::Message::new(muta_contracts::Role::Assistant, "partial progress"),
        ])
        .await
        .unwrap();
    store.set_round_counter(1).await.unwrap();

    let point = RetryPoint {
        round: 1,
        turns_committed: 1,
        history_watermark: 2,
        paused_ms: 0,
        at_ms: 1000,
    };
    store.arm_retry_pending(point.clone()).await.unwrap();
    let session_id = store.id().await;

    // Simulate "closing and resuming the session in a new process"
    let boot = assemble_for(&project, &session_id).await;

    // Verify session store has the retry point
    let pending = boot.session.retry_pending().await;
    assert_eq!(pending, Some(point.clone()));
    assert_eq!(boot.session.round_counter().await, 1);
    assert_eq!(boot.agent.round_count(), 1);

    // Verify projection emission reflects retry_pending = true
    let (tx, mut rx) = mpsc::unbounded_channel();
    send_harness_state_for_session(
        &tx,
        &session_id,
        &boot.agent,
        &boot.session,
        LoopStatus::Idle,
    )
    .await;

    let received = rx.recv().await.expect("received harness state");
    match received {
        AgentResponse::Round {
            event: RoundEvent::HarnessState(snapshot),
            ..
        } => {
            assert!(
                snapshot.retry_pending,
                "resumed session idle snapshot must broadcast retry_pending = true"
            );
            assert_eq!(snapshot.round_counter, 1);
        }
        other => panic!("unexpected event: {:?}", other),
    }
}
