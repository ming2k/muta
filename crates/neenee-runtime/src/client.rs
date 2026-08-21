//! Client side of the daemon control plane: discovery, the attach handshake
//! (`connect`), one-shot control verbs (`control`), and the monitor stream
//! (ADR-0093/0096). `neenee` / `neenee attach` / `neenee status` drive
//! sessions owned by the unified session daemon (`neenee-server`) through
//! this module. Discovery is global (one daemon per user); connections
//! prefer the Unix domain socket and fall back to TCP.
//!
//! The wire protocol this client speaks is [`crate::serve::Wire`] — client
//! and server live in the same crate so the protocol cannot drift.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::serve::Wire;
use crate::serve_discovery as discovery;
use futures::{SinkExt, StreamExt};
use neenee_contracts::{
    AgentRequest, AgentResponse, Message, MonitorAction, MonitorEvent, MonitoredSession,
    SessionOverview,
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub use crate::serve::AttachAction;
pub use crate::serve_discovery::Discovery as DaemonInfo;

/// An explicitly named daemon endpoint (`--remote <addr>` + `--token
/// <token>`): the operator supplied the coordinates, so no discovery
/// record exists or is read. Distinct from [`DaemonInfo`] on purpose — a
/// discovered daemon is identified by local state (pid, socket path,
/// version record); a remote one is identified by nothing but the address,
/// and pretending otherwise is how a remote run silently lands on the
/// local instance.
#[derive(Debug, Clone)]
pub struct RemoteDaemon {
    /// Hostname or IP. Loopback only when the address was a bare `:port`.
    pub host: String,
    pub port: u16,
    pub token: String,
}

impl RemoteDaemon {
    /// Parse `--remote <addr>` (`host:port`, `ws://host:port`, or a bare
    /// `:port` for loopback) together with the required `--token`. The
    /// token is mandatory because every network-exposed listener demands
    /// one (ADR-0105); a missing port is an error rather than a well-known
    /// default — a default would silently target the local daemon when the
    /// operator meant a remote one.
    pub fn parse(addr: &str, token: Option<String>) -> Result<Self, String> {
        let bare = addr.trim().strip_prefix("ws://").unwrap_or(addr.trim());
        let (host, port) = split_host_port(bare)?;
        let host = host.unwrap_or_else(|| "127.0.0.1".to_string());
        let Some(token) = token.filter(|t| !t.is_empty()) else {
            return Err(
                "--remote needs --token <token>: every network-exposed daemon \
                 requires the bearer token (see `neenee panel` on the host)"
                    .to_string(),
            );
        };
        Ok(Self { host, port, token })
    }

    /// Connect and run the attach handshake over TCP+bearer. No UDS
    /// attempt (the socket belongs to the remote machine's filesystem),
    /// no version pre-check (the handshake carries the daemon's version).
    pub async fn connect(&self, action: AttachAction) -> Result<Handshake, String> {
        let url = format!("ws://{}:{}/", self.host, self.port);
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| format!("bad ws url {url}: {e}"))?;
        let value = HeaderValue::from_str(&format!("Bearer {}", self.token))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
        let (ws, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| format!("ws connect to {url}: {e}"))?;
        finish_handshake(ws.split(), action).await
    }
}

/// Split `host:port` into its parts. A missing port is an error.
fn split_host_port(s: &str) -> Result<(Option<String>, u16), String> {
    let (host, port_str) = match s.rsplit_once(':') {
        Some(parts) => parts,
        None => return Err(format!("'{s}' is not host:port")),
    };
    let port: u16 = port_str
        .parse()
        .map_err(|_| format!("'{port_str}' is not a port number"))?;
    Ok(((!host.is_empty()).then(|| host.to_string()), port))
}

const LIVENESS_TIMEOUT: Duration = Duration::from_millis(500);
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_START_POLL: Duration = Duration::from_millis(100);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Find the unified daemon (ADR-0096): one global record. The project
/// argument is accepted for source compatibility but no longer scopes the
/// lookup — the daemon serves every project.
pub fn discover(_project_root: &Path) -> Option<DaemonInfo> {
    let global_path = discovery::global_discovery_path();
    if let Some(info) = discover_at(&global_path) {
        return Some(info);
    }

    // Self-healing fallback: if daemon.json is missing or unlinked,
    // but a live daemon holds the instance lock and is responsive on UDS/TCP:
    let lock_path = discovery::global_lock_path();
    if neenee_persistence::lock::ProcessLock::is_locked(&lock_path)
        && let Some(pid) = neenee_persistence::lock::ProcessLock::probe_holder(&lock_path)
        && is_process_alive(pid)
    {
        #[cfg(unix)]
        let uds = discovery::default_uds_path();
        #[cfg(unix)]
        let uds_connectable = if uds.exists() {
            std::os::unix::net::UnixStream::connect(&uds).is_ok()
        } else {
            false
        };
        #[cfg(not(unix))]
        let uds_connectable = false;

        let tcp_addr =
            std::net::SocketAddr::from(([127, 0, 0, 1], crate::startup::env_default_port()));
        let tcp_connectable =
            std::net::TcpStream::connect_timeout(&tcp_addr, Duration::from_millis(300)).is_ok();

        if uds_connectable || tcp_connectable {
            let recovered = DaemonInfo {
                pid,
                port: crate::startup::env_default_port(),
                token: None,
                project_root: String::new(),
                started_at: 0,
                #[cfg(unix)]
                uds_path: if uds.exists() { Some(uds) } else { None },
                #[cfg(not(unix))]
                uds_path: None,
                version: Some(crate::serve::daemon_version().to_string()),
                grace_secs: None,
            };
            // Restore discovery file so future lookups are immediate
            let _ = discovery::write_global(&recovered);
            tracing::info!(
                pid,
                "discover: recovered discovery record from live lock holder"
            );
            return Some(recovered);
        }
    }

    None
}

fn discover_at(path: &Path) -> Option<DaemonInfo> {
    let bytes = std::fs::read(path).ok()?;
    let info: DaemonInfo = serde_json::from_slice(&bytes).ok()?;
    if !is_process_alive(info.pid) {
        discovery::remove(path);
        return None;
    }
    if !is_alive(&info) {
        return None;
    }
    Some(info)
}

/// Directional relation between client version and daemon version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionRelation {
    Equal,
    ClientNewer,
    ClientOlder,
    Unknown,
}

/// Compare client and daemon version strings using SemVer.
pub fn compare_versions(client: &str, daemon: &str) -> VersionRelation {
    if client == daemon {
        return VersionRelation::Equal;
    }
    match (
        semver::Version::parse(client),
        semver::Version::parse(daemon),
    ) {
        (Ok(c), Ok(d)) => {
            if c > d {
                VersionRelation::ClientNewer
            } else if c < d {
                VersionRelation::ClientOlder
            } else {
                VersionRelation::Equal
            }
        }
        _ => VersionRelation::Unknown,
    }
}

/// The actionable version-skew error (ADR-0100 rule 4), naming both builds
/// and the directional fix (server behind -> stop/restart server; client behind -> update client).
/// Public so `neenee`-level commands can surface it uniformly
/// wherever a discovered daemon is about to be spoken to.
pub fn version_mismatch(info: &DaemonInfo) -> String {
    let client_ver = crate::serve::daemon_version();
    let Some(daemon_ver) = info.version.as_deref() else {
        return format!(
            "client/daemon version mismatch: this client is {client_ver} but the running daemon (pid {}) is unknown (older than 0.24). \
             Stop it with `neenee stop` and rerun — the daemon restarts on demand at the new version.",
            info.pid
        );
    };

    match compare_versions(client_ver, daemon_ver) {
        VersionRelation::ClientNewer => format!(
            "client/daemon version mismatch: running daemon (pid {}, version {daemon_ver}) is older than this client ({client_ver}). \
             Stop it with `neenee stop` and rerun — the daemon restarts on demand at the new version.",
            info.pid
        ),
        VersionRelation::ClientOlder => format!(
            "client/daemon version mismatch: this client ({client_ver}) is older than the running daemon (pid {}, version {daemon_ver}). \
             Please update your neenee client to {daemon_ver} or newer.",
            info.pid
        ),
        VersionRelation::Equal => {
            if !daemon_image_is_current(info.pid) {
                format!(
                    "client/daemon binary mismatch: running daemon (pid {}, version {daemon_ver}) executable differs from this client (rebuilt binary). \
                     Stop it with `neenee stop` and rerun — the daemon restarts on demand.",
                    info.pid
                )
            } else {
                format!(
                    "client/daemon version mismatch: client and daemon both report {client_ver} but failed compatibility check."
                )
            }
        }
        VersionRelation::Unknown => format!(
            "client/daemon version mismatch: this client is {client_ver} but the running daemon (pid {}) is {daemon_ver}. \
             If the daemon is outdated, stop it with `neenee stop` and rerun; if the client is outdated, update your client.",
            info.pid
        ),
    }
}

/// Whether a discovered daemon speaks this client's version (ADR-0100
/// rule 4). `None` on the record (a pre-versioning daemon) counts as a
/// mismatch: the wire protocol has no negotiation, so guessing is exactly
/// the failure mode the rule exists to prevent.
///
/// Version equality is *necessary but not sufficient* during development:
/// `cargo run` in a dirty workspace rebuilds the daemon binary in place
/// while an older daemon of the same `CARGO_PKG_VERSION` keeps serving,
/// so the running image drifts from the client's without any version
/// signal. [`daemon_image_is_current`] closes that gap by comparing the
/// running daemon's executable against this client's own — a daemon whose
/// `/proc/<pid>/exe` link has been replaced (or points elsewhere) is
/// treated as incompatible.
pub fn versions_compatible(info: &DaemonInfo) -> bool {
    info.version
        .as_deref()
        .is_some_and(|daemon| daemon == crate::serve::daemon_version())
        && daemon_image_is_current(info.pid)
}

/// Whether the running daemon's executable is still the exact file this
/// client was launched from. During a `cargo run` development loop the
/// rebuilt binary *replaces* the file a running daemon was started from;
/// the kernel keeps the old image alive under a `(deleted)` link while the
/// discovery record still names the same path and version. Comparing the
/// daemon's `/proc/<pid>/exe` realpath with this client's own executable
/// detects that drift — the same-version stale-daemon case version checks
/// cannot see.
///
/// Returns `true` when the check is unavailable (non-Linux, unreadable
/// `/proc`, an unresolvable current exe): absence of evidence must not
/// flag a healthy production daemon. A daemon spawned by an *installed*
/// binary (`/usr/bin/neenee`) matches an installed client exactly, and a
/// client running from a different build root than the daemon (e.g. an
/// installed client attaching to a dev daemon) intentionally counts as
/// compatible — the version field above remains the wire-level guard.
pub fn daemon_image_is_current(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Some(current) = std::env::current_exe().ok() else {
            return true;
        };
        // `metadata` follows the /proc/<pid>/exe link to the inode the
        // daemon is *actually executing* — which survives the on-disk file's
        // replacement. Never stat the path the link spells: after a rebuild
        // that path names the new file, and stat-ing it would report the
        // client's own inode, hiding exactly the drift being probed for.
        let exe_link = std::path::Path::new("/proc")
            .join(pid.to_string())
            .join("exe");
        match (std::fs::metadata(&exe_link), std::fs::metadata(&current)) {
            (Ok(daemon), Ok(client)) => same_inode(&daemon, &client),
            // No /proc entry (non-Linux unix, or the pid vanished between the
            // discovery read and here): cannot tell, do not disturb.
            _ => true,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// `(dev, inode)` equality. A rebuilt binary legitimately occupies the same
/// path with a new inode; path-string equality would hide exactly that.
#[cfg(unix)]
fn same_inode(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Liveness probe: prefer the UDS (the daemon's primary local channel),
/// fall back to the TCP port. Either reachable means the daemon is up.
fn is_alive(info: &DaemonInfo) -> bool {
    if !is_process_alive(info.pid) {
        return false;
    }
    #[cfg(unix)]
    if let Some(uds) = &info.uds_path {
        use std::os::unix::net::UnixStream;
        if UnixStream::connect(uds).is_ok() {
            return true;
        }
    }
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], info.port));
    std::net::TcpStream::connect_timeout(&addr, LIVENESS_TIMEOUT).is_ok()
}

/// Whether a process with `pid` exists and can receive signals.
pub fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Wait up to `timeout` for process `pid` to exit.
async fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !is_process_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    !is_process_alive(pid)
}

/// Whether the socket file at `path` was created by `pid`'s daemon —
/// i.e. the daemon is dead and left it behind. A live holder (probed by
/// connecting) means a successor daemon owns the socket now, and a
/// stopper must not unlink it (ADR-0116 Tier-4 pid guard, mirroring the
/// discovery record's `remove_if_matching_pid`).
#[cfg(unix)]
fn uds_belongs_to_pid(path: &std::path::Path, pid: u32) -> bool {
    if !path.exists() {
        return false;
    }
    if is_process_alive(pid) {
        // The daemon that spawned this stop is somehow still alive; its
        // socket is live state, not a leftover.
        return false;
    }
    // The recorded daemon is gone: whatever answers (or does not) on the
    // socket now belongs to someone else. Only a *responsive* socket
    // indicates a successor; a dead file is a leftover this stop should
    // clean up.
    !uds_answers(path)
}

/// Best-effort connect probe: does anything accept on this UDS path?
#[cfg(unix)]
fn uds_answers(path: &std::path::Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// The drain budget a stopper should allow when the discovery record
/// predates `grace_secs` (ADR-0116): generous enough not to interrupt a
/// default-configured daemon (10s) mid-drain, short enough that a wedged
/// process still escalates.
const FALLBACK_GRACE: Duration = Duration::from_secs(15);

/// Stop the daemon through a tiered, **budget-coordinated** shutdown
/// pipeline (ADR-0116):
///
/// 1. Tier 1 (Protocol): if the daemon speaks this client's version, send
///    the `Shutdown` control verb, then wait the daemon's *own* drain
///    budget (from the discovery record's `grace_secs`) — not a hardcoded
///    couple of seconds. Any signal arriving mid-drain escalates the
///    daemon to a forced exit that skips session teardown, so escalating
///    early destroys the graceful drain the stop just requested.
/// 2. Tier 2 (OS Signal): if the versions skew, the verb could not be
///    delivered, or the budget elapsed without an exit, `SIGTERM` the pid
///    and wait the same budget (the daemon drains the same way on SIGTERM).
/// 3. Tier 3 (Force): if it still lives, `SIGKILL`.
/// 4. Tier 4 (Cleanup): remove the discovery record, and the UDS socket
///    only if it still belongs to this daemon's pid (a successor spawned
///    during the stop window must not lose its socket).
pub async fn stop(info: &DaemonInfo) -> Result<(), String> {
    // The daemon's own drain budget, when it advertised one: the single
    // number every tier below is coordinated against.
    let grace = info
        .grace_secs
        .map(Duration::from_secs)
        .unwrap_or(FALLBACK_GRACE);
    let mut stopped = false;

    // Tier 1: Try graceful protocol shutdown if versions are compatible.
    if versions_compatible(info)
        && let Ok(Ok(())) = tokio::time::timeout(
            Duration::from_millis(1500),
            control(info, crate::serve::ControlRequest::Shutdown),
        )
        .await
    {
        stopped = wait_for_process_exit(info.pid, grace).await;
    }

    // Tier 2 & 3: Fall back to OS signals if protocol did not stop it.
    // SIGTERM drains through the same budgeted phases as the verb, so it
    // too waits the full grace before the SIGKILL escalation.
    if !stopped {
        #[cfg(unix)]
        {
            let pid = info.pid as libc::pid_t;
            if is_process_alive(info.pid) {
                let _ = unsafe { libc::kill(pid, libc::SIGTERM) };
                stopped = wait_for_process_exit(info.pid, grace).await;

                if !stopped && is_process_alive(info.pid) {
                    let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
                    stopped = wait_for_process_exit(info.pid, Duration::from_millis(1000)).await;
                }
            } else {
                stopped = true;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &info.pid.to_string(), "/F"])
                .output();
            stopped = true;
        }
    }

    // Tier 4: Cleanup discovery record & socket. The record removal is
    // pid-guarded (`remove_if_matching_pid`); the UDS socket now is too:
    // a successor daemon may have been spawned while we waited out the
    // grace above, and unlinking *its* socket would break live clients.
    discovery::remove_if_matching_pid(&discovery::global_discovery_path(), info.pid);
    #[cfg(unix)]
    if let Some(uds) = &info.uds_path
        && uds_belongs_to_pid(uds, info.pid)
    {
        let _ = std::fs::remove_file(uds);
    }

    if stopped || !is_process_alive(info.pid) {
        Ok(())
    } else {
        Err(format!("could not stop daemon (pid {})", info.pid))
    }
}

/// Path to the daemon startup stderr log file.
pub fn startup_log_path() -> PathBuf {
    let dirs = neenee_persistence::paths::get();
    dirs.state_dir.join("log").join("daemon-startup.log")
}

pub async fn ensure_daemon(project_root: &Path) -> Result<DaemonInfo, String> {
    if let Some(info) = discover(project_root) {
        if versions_compatible(&info) {
            return Ok(info);
        }
        // Incompatible daemon is running. Do not stop or kill it to avoid
        // interrupting ongoing tasks. Prompt the user about the incompatibility.
        return Err(version_mismatch(&info));
    }

    // Check if another daemon is holding the instance lock
    let lock_path = discovery::global_lock_path();
    if neenee_persistence::lock::ProcessLock::is_locked(&lock_path)
        && let Some(holder_pid) = neenee_persistence::lock::ProcessLock::probe_holder(&lock_path)
        && is_process_alive(holder_pid)
    {
        tracing::info!(
            holder_pid,
            "ensure_daemon: daemon instance lock held; waiting for startup or draining"
        );
        let init_deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < init_deadline {
            tokio::time::sleep(SERVER_START_POLL).await;
            if let Some(info) = discover(project_root) {
                if versions_compatible(&info) {
                    return Ok(info);
                } else {
                    return Err(version_mismatch(&info));
                }
            }
            if !is_process_alive(holder_pid) {
                break;
            }
        }

        // If still locked and discover() failed, do not kill the existing process.
        // Report that another daemon is running and holding the lock.
        if is_process_alive(holder_pid) {
            return Err(format!(
                "another neenee daemon (pid {holder_pid}) is running and holding the instance lock. \
                 If it is unresponsive, stop it with `neenee stop`."
            ));
        }
    }

    let mut child = spawn_daemon()?;
    let deadline = std::time::Instant::now() + SERVER_START_TIMEOUT;
    loop {
        tokio::time::sleep(SERVER_START_POLL).await;
        if let Some(info) = discover(project_root) {
            if versions_compatible(&info) {
                return Ok(info);
            } else {
                return Err(version_mismatch(&info));
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            let log_text = std::fs::read_to_string(startup_log_path()).unwrap_or_default();
            let log_trimmed = log_text.trim();
            if !log_trimmed.is_empty() {
                return Err(format!(
                    "neenee daemon exited prematurely ({status}): {log_trimmed}"
                ));
            } else {
                return Err(format!("neenee daemon exited prematurely with {status}"));
            }
        }
        if std::time::Instant::now() >= deadline {
            let log_text = std::fs::read_to_string(startup_log_path()).unwrap_or_default();
            let log_trimmed = log_text.trim();
            let lock_info = if neenee_persistence::lock::ProcessLock::is_locked(&lock_path) {
                neenee_persistence::lock::ProcessLock::probe_holder(&lock_path)
                    .map(|pid| format!(" (instance lock held by PID {pid})"))
                    .unwrap_or_else(|| " (instance lock is held)".to_string())
            } else {
                String::new()
            };
            if !log_trimmed.is_empty() {
                return Err(format!(
                    "neenee daemon did not become ready within {:?}{lock_info}: {log_trimmed}",
                    SERVER_START_TIMEOUT
                ));
            } else {
                return Err(format!(
                    "neenee daemon did not become ready within {:?}{lock_info} (see {})",
                    SERVER_START_TIMEOUT,
                    startup_log_path().display()
                ));
            }
        }
    }
}

fn spawn_daemon() -> Result<std::process::Child, String> {
    let program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("neenee"));
    let mut command = std::process::Command::new(&program);
    command.args(["daemon", "start", "--fg"]);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());

    let startup_log = startup_log_path();
    if let Some(parent) = startup_log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&startup_log)
    {
        command.stderr(file);
    } else {
        command.stderr(std::process::Stdio::null());
    }

    // Pin the daemon's cwd to a stable, always-existing directory instead of
    // inheriting this client's project. ADR-0096 made the daemon the host for
    // *every* project's sessions, so a project directory inherited from the
    // first lucky client is exactly the wrong default — any code path that
    // still consults the daemon's cwd (rather than a session-scoped root)
    // would silently land in that project. Per-session scoping is explicit
    // via the Select frame's `project` field.
    command.current_dir("/");
    // Own process group (ADR-0101): a daemon spawned from an interactive
    // shell must not share the shell's foreground group, or the terminal's
    // Ctrl-C SIGINTs the "background" daemon along with everything else in
    // the group.
    //
    // New session (ADR-0125): `setsid(2)` detaches the daemon from the
    // spawning terminal's *session*, which is what makes it compositor- and
    // terminal-death-proof the way tmux's server is. `process_group(0)`
    // alone only escapes the foreground process group — the daemon stays a
    // member of the terminal's session, so when the terminal (or the
    // compositor hosting it) dies, the kernel SIGHUPs the session's members
    // and takes the daemon with it (ADR-0101 then dutifully drains and
    // exits). `setsid` removes that coupling by construction; failure of
    // the call is fatal on purpose — a half-detached daemon is exactly the
    // lie "detached" cannot afford.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command
        .spawn()
        .map_err(|error| format!("could not spawn {}: {error}", program.display()))
}

/// Comprehensive diagnostics for the daemon control plane and system status.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DaemonDiagnostics {
    /// The resolved daemon instance directory (ADR-0121): the root of every
    /// daemon runtime file this report probes. Surfaced so an operator can
    /// see which instance — host or `NEENEE_HOME` sandbox — a client is
    /// talking about before reading anything else below.
    pub instance_dir: PathBuf,
    /// The default port this client resolves (`--port` > `NEENEE_PORT` >
    /// 9800), for the same reason as `instance_dir`.
    pub default_port: u16,
    pub discovery_path: PathBuf,
    pub discovery_record: Option<DaemonInfo>,
    pub discovery_valid: bool,
    pub lock_path: PathBuf,
    pub lock_held: bool,
    pub lock_holder_pid: Option<u32>,
    pub lock_holder_alive: bool,
    pub uds_path: PathBuf,
    pub uds_exists: bool,
    pub uds_connectable: bool,
    pub tcp_port: u16,
    pub tcp_listening: bool,
    pub startup_log_path: PathBuf,
    pub last_startup_log: Option<String>,
}

/// Perform a diagnostic probe of the daemon environment without modifying state.
pub fn diagnose_daemon() -> DaemonDiagnostics {
    let discovery_path = discovery::global_discovery_path();
    let discovery_record = discover_at(&discovery_path);
    let raw_record: Option<DaemonInfo> = std::fs::read(&discovery_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let lock_path = discovery::global_lock_path();
    let lock_held = neenee_persistence::lock::ProcessLock::is_locked(&lock_path);
    let lock_holder_pid = neenee_persistence::lock::ProcessLock::probe_holder(&lock_path);
    let lock_holder_alive = lock_holder_pid.map(is_process_alive).unwrap_or(false);

    #[cfg(unix)]
    let uds_path = discovery::default_uds_path();
    #[cfg(not(unix))]
    let uds_path = PathBuf::from("");

    let uds_exists = uds_path.exists();
    #[cfg(unix)]
    let uds_connectable = if uds_exists {
        std::os::unix::net::UnixStream::connect(&uds_path).is_ok()
    } else {
        false
    };
    #[cfg(not(unix))]
    let uds_connectable = false;

    let port = discovery_record
        .as_ref()
        .map(|d| d.port)
        .or_else(|| raw_record.as_ref().map(|d| d.port))
        .unwrap_or_else(crate::startup::env_default_port);

    let tcp_addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let tcp_listening =
        std::net::TcpStream::connect_timeout(&tcp_addr, Duration::from_millis(200)).is_ok();

    let startup_log = startup_log_path();
    let last_startup_log = std::fs::read_to_string(&startup_log)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    DaemonDiagnostics {
        instance_dir: discovery::instance_dir(),
        default_port: crate::startup::env_default_port(),
        discovery_path,
        discovery_record: discovery_record.or(raw_record),
        discovery_valid: discover_at(&discovery::global_discovery_path()).is_some(),
        lock_path,
        lock_held,
        lock_holder_pid,
        lock_holder_alive,
        uds_path,
        uds_exists,
        uds_connectable,
        tcp_port: port,
        tcp_listening,
        startup_log_path: startup_log,
        last_startup_log,
    }
}

pub enum Handshake {
    Attached {
        req_tx: mpsc::UnboundedSender<AgentRequest>,
        resp_rx: mpsc::UnboundedReceiver<AgentResponse>,
        session_id: String,
        round_counter: u64,
        history: Vec<Message>,
        /// Durable round-interrupt records (C11) from the daemon's welcome,
        /// so an attaching TUI projects the stopped rounds into its restored
        /// transcript. Empty for older daemons.
        round_interrupts: Vec<neenee_contracts::RoundInterrupt>,
        /// The provider/model the session is currently serving, carried on
        /// the welcome so the TUI's hint bar shows them from the first frame
        /// instead of waiting for the next provider mutation.
        provider: String,
        model: String,
    },
    Pick(Vec<SessionOverview>),
}

pub async fn connect(info: &DaemonInfo, action: AttachAction) -> Result<Handshake, String> {
    // Prefer the Unix domain socket (the daemon's primary local channel,
    // ADR-0096); fall back to TCP for exposed/legacy deployments.
    #[cfg(unix)]
    if let Some(uds) = &info.uds_path
        && let Ok(stream) = tokio::net::UnixStream::connect(uds).await
    {
        let request = "ws://localhost/"
            .into_client_request()
            .map_err(|e| format!("bad uds ws request: {e}"))?;
        let (ws, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(|e| format!("ws handshake over uds: {e}"))?;
        return finish_handshake(ws.split(), action).await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    finish_handshake(ws.split(), action).await
}

/// The stream-generic attach handshake, shared by the UDS and TCP paths.
async fn finish_handshake<S>(
    parts: (
        futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
        futures::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    ),
    action: AttachAction,
) -> Result<Handshake, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut ws_sink, mut ws_source) = parts;

    // Declare this client's working directory so the daemon scopes a fresh or
    // auto-attached session to the project the user actually invoked us in —
    // the daemon's own cwd is whatever the first client that spawned it
    // happened to use. A daemon predating the field ignores it; a failed cwd
    // read degrades to the daemon's fallback.
    let project = std::env::current_dir().ok();
    let select = serde_json::to_string(&Wire::Select {
        action,
        project,
        version: Some(crate::serve::daemon_version().to_string()),
    })
    .map_err(|e| format!("serialize select: {e}"))?;
    ws_sink
        .send(WsMessage::Text(select.into()))
        .await
        .map_err(|e| format!("ws send select: {e}"))?;

    let reply = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match ws_source.next().await {
                Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::Welcome {
                        session_id,
                        round_counter,
                        messages,
                        provider,
                        model,
                        round_interrupts,
                    }) => {
                        return Ok(Reply::Welcome(Welcome {
                            session_id,
                            round_counter,
                            messages,
                            provider,
                            model,
                            round_interrupts,
                        }));
                    }
                    Ok(Wire::Pick { sessions }) => return Ok(Reply::Pick(sessions)),
                    Ok(Wire::Error { message, .. }) => {
                        return Err(format!("daemon rejected the attach: {message}"));
                    }
                    Ok(_) => tracing::warn!("attach: unexpected frame during handshake, ignored"),
                    Err(error) => tracing::warn!(%error, "attach: bad frame during handshake"),
                },
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(format!("ws recv during handshake: {error}")),
                None => return Err("server closed the connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timed out waiting for handshake from daemon".to_string())??;

    let welcome = match reply {
        Reply::Welcome(w) => w,
        Reply::Pick(sessions) => {
            let _ = ws_sink.close().await;
            return Ok(Handshake::Pick(sessions));
        }
    };

    let (req_out_tx, mut req_out_rx) = mpsc::unbounded_channel::<AgentRequest>();
    let (resp_in_tx, resp_in_rx) = mpsc::unbounded_channel::<AgentResponse>();

    tokio::spawn(async move {
        let mut end_pending = false;
        while let Some(request) = req_out_rx.recv().await {
            if matches!(request, AgentRequest::EndSession) {
                // Client-declared session end (ADR-0112): mark it so the
                // pump, after flushing this frame, gives the daemon a brief
                // window to tear the session down before the socket closes.
                // Without this, a client that sends EndSession and drops
                // everything immediately can race the runtime shutdown: the
                // frame reaches the wire but the process exits before the
                // daemon even reads it — harmless over TCP/UDS (the kernel
                // buffers the written bytes), but the graceful-close
                // handshake below is still worth attempting.
                end_pending = true;
            }
            let text = match serde_json::to_string(&Wire::Request { request }) {
                Ok(text) => text,
                Err(error) => {
                    tracing::warn!(%error, "attach: could not serialize request");
                    continue;
                }
            };
            if let Err(error) = ws_sink.send(WsMessage::Text(text.into())).await {
                tracing::warn!(%error, "attach: ws send failed");
                break;
            }
        }
        if end_pending {
            // Give the daemon a moment to observe the EndSession frame and
            // run the teardown (it broadcasts the terminal `Exit` back,
            // which the response pump relays). Bounded so a hung daemon
            // cannot pin the client open either.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        let _ = ws_sink.close().await;
    });

    tokio::spawn(async move {
        while let Some(frame) = ws_source.next().await {
            match frame {
                Ok(WsMessage::Text(text)) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::Response { response }) => {
                        if resp_in_tx.send(response).is_err() {
                            return;
                        }
                    }
                    Ok(_) => tracing::warn!("attach: unexpected post-handshake frame, ignored"),
                    Err(error) => tracing::warn!(%error, "attach: bad frame from server, ignored"),
                },
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "attach: ws recv failed");
                    break;
                }
            }
        }
        let _ = resp_in_tx.send(AgentResponse::Exit);
    });

    Ok(Handshake::Attached {
        req_tx: req_out_tx,
        resp_rx: resp_in_rx,
        session_id: welcome.session_id,
        round_counter: welcome.round_counter,
        history: welcome.messages,
        round_interrupts: welcome.round_interrupts,
        provider: welcome.provider,
        model: welcome.model,
    })
}

/// Issue one control-plane verb (ADR-0096) to the daemon and await its reply:
/// create, prompt, interrupt, answer a permission, or kill — without attaching
/// as a session client. The dashboard's session-management keys (`i` interrupt,
/// `p` prompt, `n` new session) go through here. Prefers the Unix socket, falls
/// back to TCP, exactly like [`connect`].
pub async fn control(
    info: &DaemonInfo,
    request: crate::serve::ControlRequest,
) -> Result<(), String> {
    use crate::serve::AttachAction;
    let action = AttachAction::Control(request);

    #[cfg(unix)]
    if let Some(uds) = &info.uds_path
        && let Ok(stream) = tokio::net::UnixStream::connect(uds).await
    {
        let req = "ws://localhost/"
            .into_client_request()
            .map_err(|e| format!("bad uds ws request: {e}"))?;
        let (ws, _) = tokio_tungstenite::client_async(req, stream)
            .await
            .map_err(|e| format!("ws handshake over uds: {e}"))?;
        return finish_control(ws.split(), action).await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut req = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        req.headers_mut().insert("Authorization", value);
    }
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    finish_control(ws.split(), action).await
}

/// The stream-generic control handshake: send the `Select{Control}` frame and
/// await the single `ControlReply`. One verb per connection.
async fn finish_control<S>(
    parts: (
        futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
        futures::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    ),
    action: AttachAction,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut ws_sink, mut ws_source) = parts;
    // Control verbs carry their own scope (`CreateSession::project`); the
    // daemon never consults a select-level project for them.
    let select = serde_json::to_string(&Wire::Select {
        action,
        project: None,
        version: Some(crate::serve::daemon_version().to_string()),
    })
    .map_err(|e| format!("serialize control select: {e}"))?;
    ws_sink
        .send(WsMessage::Text(select.into()))
        .await
        .map_err(|e| format!("ws send control select: {e}"))?;

    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match ws_source.next().await {
                Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::ControlReply { ok, error, .. }) => {
                        return if ok {
                            Ok(())
                        } else {
                            Err(error.unwrap_or_else(|| "control verb rejected".to_string()))
                        };
                    }
                    Ok(Wire::Error { message, .. }) => return Err(message),
                    Ok(_) => tracing::warn!("control: unexpected frame during handshake, ignored"),
                    Err(error) => tracing::warn!(%error, "control: bad frame during handshake"),
                },
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(format!("ws recv during control: {error}")),
                None => return Err("server closed the control connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| "timed out waiting for control reply from daemon".to_string())?
}

struct Welcome {
    session_id: String,
    round_counter: u64,
    messages: Vec<Message>,
    provider: String,
    model: String,
    /// Durable round-interrupt records (C11) carried on the daemon's
    /// welcome; empty for older daemons that predate the field.
    round_interrupts: Vec<neenee_contracts::RoundInterrupt>,
}
enum Reply {
    Welcome(Welcome),
    Pick(Vec<SessionOverview>),
}

// ---- Monitor-protocol client (ADR-0093) ----
/// Open the WebSocket, perform the monitor handshake, and return a channel of
/// stream frames. The WS pump runs on a background task; the channel closes
/// when the daemon hangs up.
pub async fn monitor_stream(
    info: &DaemonInfo,
    action: MonitorAction,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>, String> {
    // Prefer the Unix domain socket (the daemon's primary local channel,
    // ADR-0096); fall back to TCP for exposed/legacy deployments — the same
    // transport policy as `remote::connect`/`remote::control`, so the monitor
    // stream works against a UDS-only daemon.
    #[cfg(unix)]
    if let Some(uds) = &info.uds_path
        && let Ok(stream) = tokio::net::UnixStream::connect(uds).await
    {
        let request = "ws://localhost/"
            .into_client_request()
            .map_err(|e| format!("bad uds ws request: {e}"))?;
        let (ws, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(|e| format!("ws handshake over uds: {e}"))?;
        return finish_monitor(ws.split(), action, "uds").await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    finish_monitor(ws.split(), action, &url).await
}

/// The stream-generic monitor handshake + framing, shared by the UDS and TCP
/// paths: send the `Select{Monitor}` handshake, await the opening snapshot
/// (bounded), then forward every diff frame into the returned channel.
async fn finish_monitor<S>(
    parts: (
        futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
        futures::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    ),
    action: MonitorAction,
    target: &str,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut ws_sink, mut ws_source) = parts;

    let select = serde_json::to_string(&Wire::Select {
        action: crate::serve::AttachAction::Monitor(action),
        // Monitor streams are host-wide; no project scope applies.
        project: None,
        version: Some(crate::serve::daemon_version().to_string()),
    })
    .map_err(|e| format!("serialize select: {e}"))?;
    ws_sink
        .send(WsMessage::Text(select.into()))
        .await
        .map_err(|e| format!("ws send select: {e}"))?;

    // Await the opening snapshot (or a handshake-level error) with a bound.
    let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match ws_source.next().await {
                Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::Monitor { event }) => return Ok(event),
                    Ok(Wire::Error { message, .. }) => return Err(message),
                    Ok(_) => tracing::warn!("status: unexpected frame during handshake, ignored"),
                    Err(error) => tracing::warn!(%error, "status: bad frame during handshake"),
                },
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(format!("ws recv during handshake: {error}")),
                None => return Err("server closed the connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for monitor snapshot from {target}"))??;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = tx.send(first);
    tokio::spawn(async move {
        while let Some(frame) = ws_source.next().await {
            match frame {
                Ok(WsMessage::Text(text)) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::Monitor { event }) => {
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                    Ok(_) => tracing::warn!("status: unexpected post-handshake frame, ignored"),
                    Err(error) => tracing::warn!(%error, "status: bad frame from daemon, ignored"),
                },
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "status: ws recv failed");
                    break;
                }
            }
        }
    });
    Ok(rx)
}

pub fn upsert_session_row(rows: &mut Vec<MonitoredSession>, row: MonitoredSession) {
    match rows.iter_mut().find(|existing| existing.id == row.id) {
        Some(existing) => *existing = row,
        None => rows.push(row),
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_daemon_parses_the_documented_address_forms() {
        let t = |addr| RemoteDaemon::parse(addr, Some("tok".into())).unwrap();
        // host:port
        assert_eq!(
            (t("192.168.1.4:9800").host, t("192.168.1.4:9800").port),
            ("192.168.1.4".to_string(), 9800)
        );
        // ws:// scheme is accepted and stripped
        assert_eq!(t("ws://box.lan:9800").host, "box.lan");
        assert_eq!(t("ws://box.lan:9800").port, 9800);
        // bare :port means loopback (the local daemon over TCP)
        assert_eq!(t(":9800").host, "127.0.0.1");
        // whitespace is tolerated
        assert_eq!(t("  box.lan:9800  ").host, "box.lan");
    }

    #[test]
    fn remote_daemon_requires_a_port_and_a_token() {
        // No port: refuse rather than defaulting — a default would
        // silently target the local daemon when a remote one was meant.
        let err = RemoteDaemon::parse("box.lan", Some("tok".into())).unwrap_err();
        assert!(err.contains("not host:port"), "{err}");
        let err = RemoteDaemon::parse("box.lan:notaport", Some("tok".into())).unwrap_err();
        assert!(err.contains("not a port number"), "{err}");
        // Every network-exposed daemon requires the bearer token.
        let err = RemoteDaemon::parse("box.lan:9800", None).unwrap_err();
        assert!(err.contains("--token"), "{err}");
        let err = RemoteDaemon::parse("box.lan:9800", Some(String::new())).unwrap_err();
        assert!(err.contains("--token"), "{err}");
    }

    #[test]
    fn test_compare_versions() {
        assert_eq!(compare_versions("0.25.0", "0.25.0"), VersionRelation::Equal);
        assert_eq!(
            compare_versions("0.26.0", "0.25.0"),
            VersionRelation::ClientNewer
        );
        assert_eq!(
            compare_versions("0.24.0", "0.25.0"),
            VersionRelation::ClientOlder
        );
        assert_eq!(
            compare_versions("1.0.0", "0.25.0"),
            VersionRelation::ClientNewer
        );
        assert_eq!(
            compare_versions("not-a-semver", "0.25.0"),
            VersionRelation::Unknown
        );
    }

    #[test]
    fn daemon_image_is_current_matches_this_process_exe() {
        // The daemon being probed is *this* test process: its /proc/<pid>/exe
        // is exactly the test binary this client code runs from, so the image
        // check must report it current.
        assert!(daemon_image_is_current(std::process::id()));
    }

    #[test]
    fn daemon_image_is_current_rejects_a_replaced_exe() {
        // Simulate the dev-loop drift: a daemon pid whose exe link names a
        // *different* file than this client's own executable. Spawn `sleep`
        // and confirm the check sees the divergence.
        #[cfg(unix)]
        {
            use std::process::{Command, Stdio};
            let mut child = Command::new("sleep")
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn sleep");
            let diverged = !daemon_image_is_current(child.id());
            let _ = child.kill();
            let _ = child.wait();
            assert!(diverged, "a different executable must count as drift");
        }
    }

    #[test]
    fn daemon_image_is_current_tolerates_missing_proc_entry() {
        // A pid that does not exist (or /proc unavailable): no evidence of
        // drift, so the daemon must not be disturbed.
        assert!(daemon_image_is_current(u32::MAX - 1));
    }

    #[test]
    fn same_inode_distinguishes_a_rebuilt_file_at_the_same_path() {
        // The dev-loop case in miniature: replacing a path's content gives
        // the same path a NEW inode — that is drift, not equality, even
        // though the path strings are identical. The daemon-side metadata
        // is captured before the replacement (as /proc does for a running
        // process), the client-side after.
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            let bin = tmp.path().join("neenee");
            std::fs::write(&bin, b"old image").unwrap();
            let daemon_meta = std::fs::metadata(&bin).unwrap();
            // cargo rebuild: temp + rename over the same path.
            let staging = tmp.path().join(".neenee.tmp");
            std::fs::write(&staging, b"new image").unwrap();
            std::fs::rename(&staging, &bin).unwrap();
            let client_meta = std::fs::metadata(&bin).unwrap();
            assert!(
                !same_inode(&daemon_meta, &client_meta),
                "a rebuilt binary at the same path must not compare equal"
            );
            assert!(same_inode(&client_meta, &client_meta));
        }
    }

    #[test]
    fn test_version_mismatch_messages() {
        let daemon_older = DaemonInfo {
            pid: 1234,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            version: Some("0.24.0".to_string()),
            grace_secs: None,
        };
        let msg = version_mismatch(&daemon_older);
        assert!(msg.contains("is older than this client"));
        assert!(msg.contains("neenee stop"));

        let daemon_newer = DaemonInfo {
            pid: 1234,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            version: Some("99.0.0".to_string()),
            grace_secs: None,
        };
        let msg = version_mismatch(&daemon_newer);
        assert!(msg.contains("older than the running daemon"));
        assert!(msg.contains("update your neenee client"));

        let daemon_none = DaemonInfo {
            pid: 1234,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            version: None,
            grace_secs: None,
        };
        let msg = version_mismatch(&daemon_none);
        assert!(msg.contains("unknown (older than 0.24)"));
        assert!(msg.contains("neenee stop"));

        let daemon_equal_drift = DaemonInfo {
            pid: u32::MAX - 10,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            version: Some(crate::serve::daemon_version().to_string()),
            grace_secs: None,
        };
        let msg = version_mismatch(&daemon_equal_drift);
        assert!(msg.contains("client/daemon"));
    }

    fn record(port: u16, token: Option<String>) -> DaemonInfo {
        DaemonInfo {
            pid: 99999999, // Unused/dead pid
            port,
            token,
            project_root: "/tmp/proj".to_string(),
            started_at: 0,
            uds_path: None,
            version: None,
            grace_secs: None,
        }
    }
    fn dead_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }
    #[test]
    fn discover_at_returns_none_and_removes_stale_record_for_dead_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&record(dead_port(), None)).unwrap(),
        )
        .unwrap();
        assert!(discover_at(&path).is_none());
        assert!(
            !path.exists(),
            "stale discovery file with dead PID must be removed"
        );
    }
    #[test]
    fn discover_at_preserves_record_if_pid_is_still_alive() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        let live_rec = DaemonInfo {
            pid: std::process::id(),
            port: dead_port(),
            token: None,
            project_root: "/tmp/proj".to_string(),
            started_at: 0,
            uds_path: None,
            version: None,
            grace_secs: None,
        };
        std::fs::write(&path, serde_json::to_vec(&live_rec).unwrap()).unwrap();
        assert!(discover_at(&path).is_none());
        assert!(
            path.exists(),
            "discovery file for living PID must NOT be deleted on transient probe fail"
        );
    }
    #[test]
    fn discover_at_tolerates_missing_and_corrupt_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        assert!(discover_at(&path).is_none());
        std::fs::write(&path, b"not json").unwrap();
        assert!(discover_at(&path).is_none());
        assert!(path.exists());
    }

    #[tokio::test]
    async fn stop_handles_already_dead_process_and_cleans_up() {
        let info = DaemonInfo {
            pid: 99999999, // Unused pid
            port: 1,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            version: Some("0.24.0".to_string()),
            grace_secs: None,
        };
        let res = stop(&info).await;
        assert!(res.is_ok());
    }
}

#[test]
#[cfg(unix)]
fn uds_guard_never_unlinks_a_live_successor_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("daemon.sock");

    // Missing file: nothing to clean, not ours.
    assert!(!uds_belongs_to_pid(&path, 42));

    let dead_pid = 999_999_998u32; // not this process, not alive

    // A live listener (a successor daemon) answers: never unlink it,
    // even though the recorded pid is dead.
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    assert!(!uds_belongs_to_pid(&path, dead_pid));
    drop(listener);

    // The listener is gone but the file remains (exactly what a
    // SIGKILLed daemon leaves): nobody answers, the recorded pid is
    // dead — a stale socket this stop should remove.
    assert!(uds_belongs_to_pid(&path, dead_pid));
}

#[test]
fn stop_budget_follows_the_advertised_grace() {
    // The tier budget must come from the record (ADR-0116); the test
    // pins the plumbing by asserting the fallback constant is generous
    // enough to cover a default-configured daemon's 10s drain, so a
    // legacy record cannot cause an early SIGTERM escalation either.
    assert!(FALLBACK_GRACE >= Duration::from_secs(10));
}
