//! End-to-end lifecycle tests for the daemon run loop (ADR-0101): the
//! shutdown trigger is injected (no signals needed — that is the point of
//! the gate abstraction), and the assertions check the *phases*: discovery
//! advertisement pulled, sessions torn down, tasks joined within the
//! budget, and the exit-code contract.
//!
//! Two isolation notes:
//!
//! - The loop's filesystem footprint (discovery record, instance lock) is
//!   sandboxed by pointing `NEENEE_*_DIR`/`XDG_RUNTIME_DIR` at a temp root
//!   **before the first `paths::get()` resolution in this process** (the
//!   resolver is process-global). The env is set once in a `static`
//!   initializer, and every test gets its own subdirectories via a
//!   per-test UDS path — the shared record paths live under the same
//!   sandbox root, so cross-test interference is limited to the record
//!   file itself, which each test waits out via its own outcome.
//! - `host::run_with_gate` is driven against a prehost-only registry (no
//!   session assembly); hosted sessions are inserted by hand, the same
//!   fixture style as `serve_integration`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use neenee_contracts::MonitorEvent;
use neenee_persistence::session::SessionStore;
use neenee_runtime::host::{HostIdentity, HostOptions, LifecycleOptions, RunOutcome};
use neenee_runtime::registry::{HostedSession, SessionRegistry};
use neenee_runtime::serve::ServeExpose;
use neenee_runtime::shutdown::{DrainProbe, ShutdownGate, ShutdownReason};
use tokio::sync::{Mutex, broadcast, mpsc};

/// Sandbox the process-wide dirs once, before any `paths::get()` call can
/// cache a real-user resolution (see module docs).
fn sandbox_once() {
    use std::sync::Once;
    static SANDBOX: Once = Once::new();
    static KEEP: std::sync::Mutex<Option<tempfile::TempDir>> = std::sync::Mutex::new(None);
    SANDBOX.call_once(|| {
        let tmp = tempfile::tempdir().unwrap();
        for (key, sub) in [
            ("NEENEE_CONFIG_DIR", "config"),
            ("NEENEE_DATA_DIR", "data"),
            ("NEENEE_STATE_DIR", "state"),
            ("NEENEE_CACHE_DIR", "cache"),
            ("XDG_RUNTIME_DIR", "runtime"),
        ] {
            let dir = tmp.path().join(sub);
            std::fs::create_dir_all(&dir).unwrap();
            // SAFETY: single-writer (the Once) and set before any test body
            // spawns; the env is never mutated again in this process.
            unsafe { std::env::set_var(key, &dir) };
        }
        *KEEP.lock().unwrap() = Some(tmp);
    });
}

/// A minimal `HostIdentity` for the loop: the prehost-only registry never
/// assembles, so identity values are never exercised — they only need to
/// exist.
fn test_identity() -> HostIdentity {
    HostIdentity {
        identity: neenee_contracts::AgentIdentity::new("probe", "lifecycle probe"),
        principal: neenee_contracts::PrincipalProfile::with_identity(
            "probe",
            neenee_contracts::AgentIdentity::new("probe", "lifecycle probe"),
        ),
        ui: Arc::new(HeadlessProbe),
    }
}

/// Trivial UiBridge stand-in (the loop only stores it).
struct HeadlessProbe;

#[async_trait::async_trait]
impl neenee_runtime::UiBridge for HeadlessProbe {
    async fn copy_to_clipboard(&self, _text: &str) -> Result<neenee_runtime::CopyOutcome, String> {
        Err("probe: headless".to_string())
    }
}

/// Host one hand-built session; returns the monitor subscription-friendly
/// registry (fixtures mirror `serve_integration::prehosted`, minus the tap).
async fn host_one(registry: &Arc<SessionRegistry>, project: &str) {
    let session = Arc::new(SessionStore::load_for_project(project.into()));
    let (req_tx, _req_rx) = mpsc::unbounded_channel::<neenee_contracts::AgentRequest>();
    let (bc_tx, _) = broadcast::channel::<neenee_contracts::AgentResponse>(16);
    let id = session.id().await;
    let tracker = Arc::new(Mutex::new(
        neenee_runtime::monitor::MonitorTracker::bootstrap(
            neenee_contracts::MonitoredSession::empty(id),
            neenee_contracts::SessionStatus::Idle,
        ),
    ));
    registry
        .host(HostedSession {
            project_root: project.into(),
            session,
            req_tx,
            events: bc_tx,
            cancel: tokio_util::sync::CancellationToken::new(),
            tracker,
            sync_buffer: Arc::new(Mutex::new(std::collections::VecDeque::new())),
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
        #[cfg(unix)]
        uds_path: Some(uds),
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

    let outcome = neenee_runtime::host::run_with_registry(
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
        !neenee_runtime::serve_discovery::global_discovery_path().exists(),
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
    let outcome = neenee_runtime::host::run_with_gate(
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
        neenee_runtime::host::run_with_registry(
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
    let outcome = neenee_runtime::host::run_with_gate(
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
