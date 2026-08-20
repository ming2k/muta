//! End-to-end lifecycle tests for the daemon run loop (ADR-0101): the
//! shutdown trigger is injected (no signals needed — that is the point of
//! the gate abstraction), and the assertions check the *phases*: discovery
//! advertisement pulled, sessions torn down, tasks joined within the
//! budget, and the exit-code contract.
//!
//! Two isolation notes:
//!
//! - The loop's filesystem footprint (discovery record, instance lock) is
//!   sandboxed by pointing `NEENEE_HOME` at a temp root **before the first
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

use neenee_contracts::MonitorEvent;
use neenee_persistence::session::SessionStore;
use neenee_runtime::host::{HostIdentity, HostOptions, LifecycleOptions, RunOutcome};
use neenee_runtime::registry::{HostedSession, SessionRegistry};
use neenee_runtime::serve::ServeExpose;
use neenee_runtime::shutdown::{DrainProbe, ShutdownGate, ShutdownReason};
use tokio::sync::{Mutex, broadcast, mpsc};

/// Sandbox the process-wide dirs once, before any `paths::get()` call can
/// cache a real-user resolution (see module docs).
///
/// ADR-0121: `NEENEE_HOME` alone redirects every category *and* the daemon
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
        unsafe { std::env::set_var("NEENEE_HOME", tmp.path()) };
        *KEEP.lock().unwrap() = Some(tmp);
    });
}

/// The ADR-0121 isolation contract, pinned at the level users experience
/// it: with `NEENEE_HOME` set, every category and the daemon's runtime
/// files resolve under the sandbox root, and the host's XDG runtime dir
/// (which the test env deliberately does not clear) cannot leak any
/// daemon-facing path back out of it.
#[test]
fn neenee_home_redirects_the_daemon_footprint() {
    sandbox_once();
    let dirs = neenee_persistence::paths::get();
    let root = std::env::var("NEENEE_HOME").unwrap();
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
            "{:?} must stay under the NEENEE_HOME sandbox root",
            dir
        );
    }
    assert_eq!(
        dirs.instance_dir(),
        std::path::PathBuf::from(&root)
            .join("neenee")
            .join("instance")
    );
    // And the daemon-facing paths derive from the instance dir.
    for path in [
        neenee_runtime::serve_discovery::global_discovery_path(),
        neenee_runtime::serve_discovery::global_lock_path(),
        #[cfg(unix)]
        neenee_runtime::serve_discovery::default_uds_path(),
    ] {
        assert!(under_root(&path), "{path:?} must follow the instance dir");
    }
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

/// ADR-0121's inheritance invariant, proven end-to-end: a daemon spawned
/// from a client whose environment carries `NEENEE_HOME` writes its
/// discovery record inside the sandbox — never into the host's runtime
/// dir, which on this machine is exactly where a live installed daemon's
/// record sits.
///
/// `client::spawn_daemon` re-execs `current_exe()`, which inside a test
/// harness is the test binary, not a CLI. So this test mirrors that spawn
/// shape exactly (`daemon start --fg`, inherited env, detached process
/// group) but with the real CLI binary cargo builds alongside the test.
/// The lifecycle-integration crate has no bin target of its own, so the
/// binary is located via the workspace layout (`target/debug/neenee`).
///
/// Isolation within the suite: the sibling tests share the process-wide
/// `sandbox_once()` root and drive `host::run_*` directly (no discovery),
/// but this test stops its daemon **through discovery** — so it must own a
/// private instance root, or its `stop` would discover (and shut down) a
/// sibling test's in-flight loop through the shared record. A dedicated
/// `NEENEE_HOME` on the spawned command's environment gives the daemon its
/// own `daemon.json`/`daemon.lock`/`daemon.sock`, which is also a purer
/// proof of the inheritance contract: the *child's* env alone decides
/// where its footprint lands.
#[tokio::test]
async fn spawned_daemon_inherits_the_neenee_home_sandbox() {
    sandbox_once(); // install the shared root before reading it back
    let own = tempfile::tempdir().unwrap();
    let own_root = own.path().to_path_buf();
    // The record path this *process* resolves (the shared sandbox). The
    // child gets its own root below; the leak assertion is that the
    // child's pid never appears in *this* record, not that this record is
    // absent — sibling tests legitimately run their loops under it.
    let shared_record = std::path::PathBuf::from(std::env::var("NEENEE_HOME").unwrap())
        .join("neenee")
        .join("instance")
        .join("daemon.json");

    // Locate the CLI binary the way a developer run would have it.
    let exe = std::env::current_exe().unwrap();
    let target_dir = exe
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "deps"))
        .and_then(|deps| deps.parent())
        .expect("test binary lives under <target>/deps");
    let cli = target_dir.join(if cfg!(windows) {
        "neenee.exe"
    } else {
        "neenee"
    });
    if !cli.exists() {
        // The binary is not part of this crate's default test build; skip
        // rather than silently proving nothing.
        eprintln!("skipping: {} not built", cli.display());
        return;
    }

    // The spawn_daemon shape: same argv, environment carrying only the
    // sandbox contract (the point under test), detached process group.
    let mut command = std::process::Command::new(&cli);
    command.args(["daemon", "start", "--fg"]);
    command
        .env("NEENEE_HOME", &own_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .current_dir("/");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let child = command.spawn().expect("spawn the sandboxed daemon");
    let daemon_pid = child.id();

    // Reap continuously: this process is the daemon's *parent*, so once
    // `daemon stop` kills it the exit must be reaped promptly or the
    // (zombie) entry makes `is_process_alive` report "alive" and the stop
    // reports failure — a parent/stopper coupling that never exists in
    // production (the stopper is a peer, not the parent).
    let mut reaper_child = child;
    let reaper = std::thread::spawn(move || reaper_child.wait());

    // The record must appear inside the child's own sandbox…
    let own_record = own_root.join("neenee").join("instance").join("daemon.json");
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut saw_record = false;
    while std::time::Instant::now() < deadline {
        if let Ok(bytes) = std::fs::read(&own_record)
            && let Ok(record) =
                serde_json::from_slice::<neenee_runtime::serve_discovery::Discovery>(&bytes)
        {
            assert_eq!(
                record.pid, daemon_pid,
                "the record inside the child's sandbox must be the child's"
            );
            assert_eq!(
                record.version.as_deref(),
                Some(neenee_runtime::serve::daemon_version()),
                "the sandboxed daemon must be this build"
            );
            saw_record = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(saw_record, "daemon never advertised inside its sandbox");

    // …and the child's pid must never have leaked into the caller's
    // instance (the ADR-0121 guarantee: the child's env alone decides
    // where its footprint lands).
    if let Ok(bytes) = std::fs::read(&shared_record)
        && let Ok(record) =
            serde_json::from_slice::<neenee_runtime::serve_discovery::Discovery>(&bytes)
    {
        assert_ne!(
            record.pid, daemon_pid,
            "a daemon given its own NEENEE_HOME must not write the caller's instance"
        );
    }

    // Stop it the operator's way: a `daemon stop` from the same sandbox
    // env must reach (only) the sandboxed daemon. Running the CLI rather
    // than the in-process client keeps version/image identity intact
    // (`versions_compatible` compares binaries, and the caller here is the
    // test binary — the daemon is the CLI).
    let mut stop = std::process::Command::new(&cli);
    stop.args(["daemon", "stop"])
        .env("NEENEE_HOME", &own_root)
        .stdin(std::process::Stdio::null());
    let stop_output = stop.output().expect("run daemon stop in the sandbox");
    assert!(
        stop_output.status.success(),
        "daemon stop must succeed: {} (stderr: {})",
        stop_output.status,
        String::from_utf8_lossy(&stop_output.stderr)
    );
    reaper
        .join()
        .expect("reap the daemon")
        .expect("the sandboxed daemon exited cleanly");
    assert!(
        !own_record.exists(),
        "the drained daemon must remove its discovery record"
    );
}
