//! End-to-end lifecycle tests for the daemon run loop (ADR-0101): the
//! shutdown trigger is injected (no signals needed — that is the point of
//! the gate abstraction), and the assertions check the *phases*: discovery
//! advertisement pulled, sessions torn down, tasks joined within the
//! budget, and the exit-code contract.
//!
//! Two isolation notes:
//!
//! - The loop's filesystem footprint (discovery record, instance lock) is
//!   sandboxed by pointing `MUTA_HOME` at a temp root **before the first
//!   `paths::get()` resolution in this process** (ADR-0121). One variable
//!   redirects every category and the daemon's runtime files; the env is
//!   set once in a `static` initializer, and every test gets its own
//!   subdirectories via a per-test UDS path — the shared record paths live
//!   under the same sandbox root, so cross-test interference is limited to
//!   the record file itself, which each test waits out via its own outcome.
//! - `host::run_with_gate` is driven against a prehost-only registry (no
//!   session assembly); hosted sessions are inserted by hand, the same
//!   fixture style as `serve_integration`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use muta_contracts::MonitorEvent;
use muta_persistence::session::SessionStore;
use muta_runtime::host::{HostIdentity, HostOptions, LifecycleOptions, RunOutcome};
use muta_runtime::registry::{HostedSession, SessionRegistry};
use muta_runtime::serve::ServeExpose;
use muta_runtime::shutdown::{DrainProbe, ShutdownGate, ShutdownReason};
use tokio::sync::{Mutex, broadcast, mpsc};

/// Sandbox the process-wide dirs once, before any `paths::get()` call can
/// cache a real-user resolution (see module docs).
///
/// ADR-0121: `MUTA_HOME` alone redirects every category *and* the daemon
/// instance dir, so the five hand-assembled env vars this used to set
/// collapse to one. The root is a dedicated tempdir (not the shared
/// `sandbox` subdirs) kept alive for the process.
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

/// The ADR-0121 isolation contract, pinned at the level users experience
/// it: with `MUTA_HOME` set, every category and the daemon's runtime
/// files resolve under the sandbox root, and the host's XDG runtime dir
/// (which the test env deliberately does not clear) cannot leak any
/// daemon-facing path back out of it.
#[test]
fn muta_home_redirects_the_daemon_footprint() {
    sandbox_once();
    let dirs = muta_persistence::paths::get();
    let root = std::env::var("MUTA_HOME").unwrap();
    let under_root = |p: &std::path::Path| p.starts_with(&root);
    for dir in [
        &dirs.config_dir,
        &dirs.data_dir,
        &dirs.state_dir,
        &dirs.cache_dir,
        &dirs.instance_dir(),
    ] {
        assert!(
            under_root(dir),
            "{:?} must stay under the MUTA_HOME sandbox root",
            dir
        );
    }
    assert_eq!(
        dirs.instance_dir(),
        std::path::PathBuf::from(&root)
            .join("muta")
            .join("instance")
    );
    // And the daemon-facing paths derive from the instance dir.
    for path in [
        muta_runtime::serve_discovery::global_discovery_path(),
        muta_runtime::serve_discovery::global_lock_path(),
        #[cfg(unix)]
        muta_runtime::serve_discovery::default_uds_path(),
    ] {
        assert!(under_root(&path), "{path:?} must follow the instance dir");
    }
}

/// A minimal `HostIdentity` for the loop: the prehost-only registry never
/// assembles, so identity values are never exercised — they only need to
/// exist.
fn test_identity() -> HostIdentity {
    HostIdentity {
        identity: muta_contracts::AgentIdentity::new("probe", "lifecycle probe"),
        master: muta_contracts::MasterPreset::with_identity(
            "probe",
            muta_contracts::AgentIdentity::new("probe", "lifecycle probe"),
        ),
        ui: Arc::new(HeadlessProbe),
    }
}

/// Trivial UiBridge stand-in (the loop only stores it).
struct HeadlessProbe;

#[async_trait::async_trait]
impl muta_runtime::UiBridge for HeadlessProbe {
    async fn copy_to_clipboard(&self, _text: &str) -> Result<muta_runtime::CopyOutcome, String> {
        Err("probe: headless".to_string())
    }
}

/// Host one hand-built session; returns the monitor subscription-friendly
/// registry (fixtures mirror `serve_integration::prehosted`, minus the tap).
async fn host_one(registry: &Arc<SessionRegistry>, project: &str) {
    let session = Arc::new(SessionStore::load_for_project(project.into()));
    let (req_tx, _req_rx) = mpsc::unbounded_channel::<muta_contracts::AgentRequest>();
    let (bc_tx, _) = broadcast::channel::<muta_contracts::AgentResponse>(16);
    let id = session.id().await;
    let tracker = Arc::new(Mutex::new(
        muta_runtime::monitor::MonitorTracker::bootstrap(
            muta_contracts::MonitoredSession::empty(id),
            muta_contracts::SessionStatus::Idle,
        ),
    ));
    registry
        .host(HostedSession {
            project_root: project.into(),
            human_channel: std::sync::Arc::new(
                muta_contracts::human_request::HumanChannelAccountant::new(),
            ),
            security: std::sync::Arc::new(
                muta_persistence::workspace_security::WorkspaceSecurityStore::load(),
            ),
            session,
            req_tx,
            events: bc_tx,
            cancel: tokio_util::sync::CancellationToken::new(),
            tracker,
            sync_buffer: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            command_catalog: muta_contracts::CommandCatalog::default(),
            created_at: std::time::Instant::now(),
            last_activity: tokio::sync::Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;
}

fn options(uds: std::path::PathBuf, port: u16) -> HostOptions {
    HostOptions {
        port,
        expose: ServeExpose::Local,
        token: None,
        // Tests drive the control plane without credentials.
        local_auth: false,
        port_fallback: false,
        local_endpoint: muta_platform::ipc::endpoint_for_instance(
            uds,
            &format!("lifecycle-{port}"),
        )
        .ok(),
    }
}

#[tokio::test]
async fn control_verb_drain_removes_discovery_and_exits_zero() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(SessionRegistry::prehost_only());
    host_one(&registry, tmp.path().join("proj-a").to_str().unwrap()).await;
    let mut monitor_rx = registry.subscribe_monitor();

    let gate = Arc::new(ShutdownGate::new());
    let trigger_gate = gate.clone();
    tokio::spawn(async move {
        // Request shortly after the loop starts serving.
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger_gate.request(ShutdownReason::ControlVerb, false);
    });

    let outcome = muta_runtime::host::run_with_registry(
        test_identity(),
        options(tmp.path().join("a.sock"), 0),
        gate,
        LifecycleOptions {
            shutdown_grace: Duration::from_secs(3),
            idle_exit: None,
            drain_probe: None,
        },
        registry,
    )
    .await;

    // Exit contract (ADR-0101): a completed graceful stop is 0.
    assert_eq!(outcome.exit_code(), 0);
    match &outcome {
        RunOutcome::Stopped { reason } => assert_eq!(*reason, ShutdownReason::ControlVerb),
        other => panic!("expected Stopped, got {other:?}"),
    }
    // The discovery advertisement is gone (the lease released it).
    assert!(
        !muta_runtime::serve_discovery::global_discovery_path().exists(),
        "discovery record must be removed on drain"
    );
    // The monitor bus saw the drain announcement and the session teardown.
    let mut saw_draining = false;
    let mut saw_removed = false;
    while let Ok(event) = monitor_rx.try_recv() {
        match event {
            MonitorEvent::DaemonDraining => saw_draining = true,
            MonitorEvent::SessionRemoved { .. } => saw_removed = true,
            _ => {}
        }
    }
    assert!(
        saw_draining,
        "watch clients must be told the daemon is draining"
    );
    assert!(saw_removed, "hosted sessions must be torn down on drain");
}

#[tokio::test]
async fn idle_exit_triggers_after_the_grace_period() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    // No sessions, no connections: the idle timer is the only exit path.
    // The probe polls every 5s; a near-zero grace needs one tick, so the
    // test's wall time is bounded by one probe tick.
    let registry = Arc::new(SessionRegistry::prehost_only());
    let gate = Arc::new(ShutdownGate::new());
    let _ = registry;
    let outcome = muta_runtime::host::run_with_gate(
        test_identity(),
        options(tmp.path().join("b.sock"), 0),
        gate,
        LifecycleOptions {
            shutdown_grace: Duration::from_secs(3),
            idle_exit: Some(Duration::from_millis(1)),
            drain_probe: None,
        },
    )
    .await;
    assert_eq!(outcome.exit_code(), 0);
    match &outcome {
        RunOutcome::Stopped { reason } => assert_eq!(*reason, ShutdownReason::IdleTimeout),
        other => panic!("expected IdleTimeout stop, got {other:?}"),
    }
}

#[tokio::test]
async fn escalation_skips_the_graceful_phases() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("c.sock");
    let registry = Arc::new(SessionRegistry::prehost_only());
    host_one(&registry, tmp.path().join("proj-b").to_str().unwrap()).await;
    let mut monitor_rx = registry.subscribe_monitor();

    let gate = Arc::new(ShutdownGate::new());
    let probe = Arc::new(DrainProbe::new());
    let g1 = gate.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        g1.request(ShutdownReason::SignalInterrupt, false);
    });

    let run_gate = gate.clone();
    let run_probe = probe.clone();
    let run = tokio::spawn(async move {
        muta_runtime::host::run_with_registry(
            test_identity(),
            options(sock, 0),
            run_gate,
            LifecycleOptions {
                shutdown_grace: Duration::from_secs(5),
                idle_exit: None,
                drain_probe: Some(run_probe),
            },
            registry,
        )
        .await
    });

    // Deterministic escalation: the drain is parked at the probe, so the
    // second trigger lands mid-drain — never after the phases finished.
    probe.parked().await;
    gate.request(ShutdownReason::SignalTerminate, true);
    probe.release();
    let outcome = run.await.unwrap();

    match &outcome {
        // Escalation still exits 0 (the operator asked it to stop and it
        // stopped) — the aborted stragglers are named in the log.
        RunOutcome::ForcedExit { reason } => assert_eq!(*reason, ShutdownReason::SignalInterrupt),
        other => panic!("expected ForcedExit after escalation, got {other:?}"),
    }
    assert_eq!(outcome.exit_code(), 0);
    // The drain announcement still went out before the force, but the
    // graceful session teardown was skipped: no SessionRemoved may appear.
    let mut saw_draining = false;
    let mut saw_removed = false;
    while let Ok(event) = monitor_rx.try_recv() {
        match event {
            MonitorEvent::DaemonDraining => saw_draining = true,
            MonitorEvent::SessionRemoved { .. } => saw_removed = true,
            _ => {}
        }
    }
    assert!(saw_draining);
    assert!(
        !saw_removed,
        "an escalated drain must skip the graceful session teardown"
    );
}

#[tokio::test]
async fn port_bind_failure_is_a_readable_startup_failure() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    // Occupy a port so the daemon's bind fails.
    let hog = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = hog.local_addr().unwrap().port();

    let registry = Arc::new(SessionRegistry::prehost_only());
    let _ = registry;
    let outcome = muta_runtime::host::run_with_gate(
        test_identity(),
        options(tmp.path().join("d.sock"), taken),
        Arc::new(ShutdownGate::new()),
        LifecycleOptions::default(),
    )
    .await;

    match &outcome {
        RunOutcome::StartupFailed(what) => {
            // The real io::Error text, not a bare RecvError (ADR-0101).
            assert!(
                what.contains("could not bind"),
                "readable bind error: {what}"
            );
        }
        other => panic!("expected StartupFailed, got {other:?}"),
    }
    assert_eq!(outcome.exit_code(), 1);
}

/// ADR-0125: a hosted session with armed `/schedule` jobs is exempt from
/// idle suspension — suspending it would park its tick loop and silently
/// stop the schedule from firing. A schedule-free session under the same
/// conditions still suspends (the memory-bounding behavior ADR-0113 added).
#[tokio::test]
async fn idle_suspension_spares_sessions_with_armed_schedules() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(SessionRegistry::prehost_only());

    // Two sessions past the TTL: one with an armed job, one without. Both
    // need real content so the store persists them (empty sessions are the
    // reaper's, not the suspender's). Each is hosted through the same
    // `SessionStore` it was armed on, so the id the registry sees is the id
    // the schedule belongs to.
    async fn host_with_session(
        registry: &Arc<SessionRegistry>,
        session: Arc<SessionStore>,
        project_root: &std::path::Path,
    ) {
        let (req_tx, _req_rx) = mpsc::unbounded_channel::<muta_contracts::AgentRequest>();
        let (bc_tx, _) = broadcast::channel::<muta_contracts::AgentResponse>(16);
        let tracker = Arc::new(Mutex::new(
            muta_runtime::monitor::MonitorTracker::bootstrap(
                muta_contracts::MonitoredSession::empty(session.id().await),
                muta_contracts::SessionStatus::Idle,
            ),
        ));
        registry
            .host(HostedSession {
                project_root: project_root.to_path_buf(),
                human_channel: std::sync::Arc::new(
                    muta_contracts::human_request::HumanChannelAccountant::new(),
                ),
                security: std::sync::Arc::new(
                    muta_persistence::workspace_security::WorkspaceSecurityStore::load(),
                ),
                session,
                req_tx,
                events: bc_tx,
                cancel: tokio_util::sync::CancellationToken::new(),
                tracker,
                sync_buffer: Arc::new(Mutex::new(std::collections::VecDeque::new())),
                command_catalog: muta_contracts::CommandCatalog::default(),
                created_at: std::time::Instant::now(),
                last_activity: tokio::sync::Mutex::new(std::time::Instant::now()),
                last_seen_tick: std::sync::atomic::AtomicU64::new(0),
                activity_tick: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
                agent_for_session_end: None,
            })
            .await;
    }

    let armed_project = tmp.path().join("armed-project");
    let armed_session = Arc::new(SessionStore::load_for_project(armed_project.clone()));
    armed_session
        .set_scheduled_jobs(vec![muta_contracts::ScheduledJob::once(
            "nightly".into(),
            chrono::Utc::now() + chrono::Duration::hours(8),
            "run the nightly check".into(),
            chrono::Utc::now(),
        )])
        .await
        .unwrap();
    armed_session
        .replace_messages(vec![muta_contracts::Message::new(
            muta_contracts::Role::User,
            "arm a schedule",
        )])
        .await
        .unwrap();
    host_with_session(&registry, armed_session.clone(), &armed_project).await;

    let plain_project = tmp.path().join("plain-project");
    let plain_session = Arc::new(SessionStore::load_for_project(plain_project.clone()));
    plain_session
        .replace_messages(vec![muta_contracts::Message::new(
            muta_contracts::Role::User,
            "no schedule here",
        )])
        .await
        .unwrap();
    host_with_session(&registry, plain_session.clone(), &plain_project).await;

    // Zero TTL: everything idle-suspends unless exempted.
    let suspended = registry
        .suspend_idle_sessions_with(Duration::from_millis(0))
        .await;

    let armed_id = armed_session.id().await;
    assert!(
        !suspended.contains(&armed_id),
        "an armed schedule must keep its session resident (suspended: {suspended:?})"
    );
    let plain_id = plain_session.id().await;
    assert!(
        suspended.contains(&plain_id),
        "a schedule-free idle session should still suspend (suspended: {suspended:?})"
    );
}

// ---------------------------------------------------------------------------
// Workspace-trust reply semantics: structured option tokens (not substring
// matching) decide the persisted profile, and unrecognised answers fail
// closed.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn trust_reply_tokens_map_to_persisted_profiles() {
    use muta_contracts::WorkspaceExecutionProfile;
    use muta_persistence::workspace_security::WorkspaceSecurityStore;

    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("security.json");
    let store = WorkspaceSecurityStore::load_from(file.clone());

    // The exact tokens `serve.rs` publishes and `handlers_permission` matches.
    let full = "Grant development authority (with extensions)";
    let ws_only = "Grant development authority (workspace only)";
    let restricted = "Keep restricted";

    // Seed project contributions so `trust_extensions` has content to bind
    // to (an empty `.muta/` makes every extensions assertion trivially
    // false and hides a regression).
    std::fs::create_dir_all(tmp.path().join(".muta").join("skills")).unwrap();
    std::fs::write(tmp.path().join(".muta").join(".contrib-marker"), b"seed").unwrap();

    for (answer, execution, extensions_trusted) in [
        (full, WorkspaceExecutionProfile::Development, true),
        (ws_only, WorkspaceExecutionProfile::Development, false),
        (restricted, WorkspaceExecutionProfile::Restricted, false),
        // An *unrecognised* answer (legacy client, hand-crafted reply) must
        // fail closed to restricted, never fall through to a permissive
        // branch.
        (
            "something else",
            WorkspaceExecutionProfile::Restricted,
            false,
        ),
    ] {
        // Reset to the undecided state between rounds. `Unknown` cannot be
        // persisted through `set_execution` round-trips here (it reads
        // back as the stored profile), so reset via restricted first — the
        // assertions below are on the *result* of `apply_trust_decision`,
        // not on the reset.
        let _ = store.set_execution(tmp.path(), WorkspaceExecutionProfile::Restricted);
        let _ = store.untrust_extensions(tmp.path());
        muta_runtime::handlers_permission::apply_trust_decision(&store, tmp.path(), answer);
        let snap = store.snapshot(tmp.path());
        assert_eq!(snap.execution, execution, "answer {answer:?}");
        assert_eq!(
            snap.extensions.is_trusted(),
            extensions_trusted,
            "answer {answer:?}"
        );
        // Durable: a fresh handle over the same file sees the same state.
        let reloaded = WorkspaceSecurityStore::load_from(file.clone());
        assert_eq!(
            reloaded.snapshot(tmp.path()).execution,
            execution,
            "answer {answer:?}"
        );
    }
}
