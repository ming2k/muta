#![allow(clippy::unwrap_used, clippy::expect_used)]

//! ADR-0132 integration: the session-scoped delegated-autonomous posture is persisted
//! (`SessionEvent::DelegatedSet`) and restored by the bootstrap resume path —
//! so a daemon that dies mid-delegated-session reopens delegated when the
//! session is re-hosted (attach, lazy-resume, or boot rehost). These tests
//! exercise the real `bootstrap::assemble` resume path against a store on
//! disk, standing in for "process died, new process opened the same session
//! file".

use std::sync::Arc;

use muta_persistence::session::SessionStore;
use muta_runtime::UiBridge;
use muta_runtime::bootstrap::{self, BootstrapParams};
use muta_runtime::startup::SessionStart;

fn sandbox_once() {
    use std::sync::Once;
    static SANDBOX: Once = Once::new();
    static KEEP: std::sync::Mutex<Option<tempfile::TempDir>> = std::sync::Mutex::new(None);
    SANDBOX.call_once(|| {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: single-writer (the Once) and set before any test body
        // spawns; the env is never mutated again in this process.
        unsafe { std::env::set_var("MUTA_HOME", tmp.path()) };
        *KEEP.lock().unwrap() = Some(tmp);
    });
}

struct HeadlessProbe;

#[async_trait::async_trait]
impl UiBridge for HeadlessProbe {
    async fn copy_to_clipboard(&self, _text: &str) -> Result<muta_runtime::CopyOutcome, String> {
        Err("probe: headless".to_string())
    }
}

fn params(project_root: std::path::PathBuf, startup: SessionStart) -> BootstrapParams {
    let identity = muta_contracts::AgentIdentity::new("probe", "delegated probe");
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
async fn delegated_posture_survives_process_death_and_reopen() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("crashed-project");

    let store = Arc::new(SessionStore::load_for_project(project.clone()));
    store
        .replace_messages(vec![muta_contracts::Message::new(
            muta_contracts::Role::User,
            "mid-task when the daemon was killed",
        )])
        .await
        .unwrap();
    // The `/delegate on` handler's store write.
    store.set_delegated(true).await.unwrap();
    let session_id = store.id().await;

    // "Process death": nothing but the files remain. A new process resumes
    // the same session through the ordinary bootstrap path.
    let boot = assemble_for(&project, &session_id).await;
    assert!(
        boot.agent.delegated(),
        "a rehosted session must reopen in the posture it died in"
    );
    assert!(boot.session.delegated().await);
}

#[tokio::test]
async fn interactive_session_reopens_interactive() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("attended-project");

    let store = Arc::new(SessionStore::load_for_project(project.clone()));
    store
        .replace_messages(vec![muta_contracts::Message::new(
            muta_contracts::Role::User,
            "interactive session",
        )])
        .await
        .unwrap();
    let session_id = store.id().await;

    let boot = assemble_for(&project, &session_id).await;
    assert!(
        !boot.agent.delegated(),
        "an interactive session must not gain delegated mode on reopen"
    );
}

#[tokio::test]
async fn delegated_off_after_on_persists_the_de_escalation() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("toggle-project");

    let store = Arc::new(SessionStore::load_for_project(project.clone()));
    store
        .replace_messages(vec![muta_contracts::Message::new(
            muta_contracts::Role::User,
            "toggle history",
        )])
        .await
        .unwrap();
    store.set_delegated(true).await.unwrap();
    store.set_delegated(false).await.unwrap(); // `/delegate off`
    let session_id = store.id().await;

    let boot = assemble_for(&project, &session_id).await;
    assert!(
        !boot.agent.delegated(),
        "the last persisted posture (off) must win over the earlier on"
    );
}
