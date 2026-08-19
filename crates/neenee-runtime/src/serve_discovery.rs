//! Discovery file: how co-process clients find a live session daemon's
//! endpoint.
//!
//! Since ADR-0096 the record that matters is **global**: the unified daemon
//! (`neenee-server`, run by `neenee serve`) writes one `daemon.json` per user
//! ([`global_discovery_path`], in `$XDG_RUNTIME_DIR/neenee/` when a runtime
//! dir exists) once its port is bound, and removes it on clean shutdown.
//! Clients (an attaching `neenee attach` TUI, `neenee status`, the
//! dashboard) read that record to reach the already-running daemon instead of
//! spawning a second one. The module lives in this crate — not in either
//! binary — so writer and reader share one definition of the record and of
//! the path-resolution rule.
//!
//! The per-project path resolution below is the **legacy** pre-ADR-0096
//! scheme, retained for reading (and cleaning up) old records:
//!
//! - `$XDG_RUNTIME_DIR/neenee/serve/<bucket>.json` when a runtime dir exists
//!   ([`paths::Dirs::runtime_dir`]) — ephemeral tmpfs is the right home for a
//!   live process's PID/port; the record vanishes with the login session.
//! - `<data>/neenee/projects/<bucket>/serve.json` as the fallback
//!   ([`paths::Dirs::project_dir`]) — the bucket always exists because the
//!   project's sessions already live under it.
//!
//! `<bucket>` is [`paths::project_bucket_name`], the same sha256[..16] hash
//! that names the project's session bucket.

use std::path::{Path, PathBuf};

use neenee_persistence::paths::{self, Dirs};

/// The discovery record, written once the bound port is known.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Discovery {
    /// The serving process's id (staleness probe for readers).
    pub pid: u32,
    /// The bound TCP port the WebSocket listener serves.
    pub port: u16,
    /// The bearer token clients must present, when auth is active
    /// (`--public`, or loopback with `[daemon] local_auth` — ADR-0105);
    /// `null` for an unauthenticated loopback listener.
    pub token: Option<String>,
    /// The project root the host serves. Empty for the unified daemon
    /// (ADR-0096), which is project-agnostic; retained for the legacy
    /// per-project records.
    pub project_root: String,
    /// Unix seconds at startup.
    pub started_at: u64,
    /// Unix domain socket the control plane also listens on (ADR-0096).
    /// `None` for legacy records and when UDS is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uds_path: Option<PathBuf>,
    /// The daemon build's `CARGO_PKG_VERSION` (ADR-0100 rule 4): a client
    /// that reads a record whose version differs from its own refuses with
    /// an actionable both-versions message instead of speaking a wire
    /// protocol it may not share. `None` on records predating the field —
    /// treated as "unknown", which also mismatches, prompting a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// The global discovery path for the unified daemon (ADR-0096): one record
/// per user, in the runtime dir when available.
pub fn global_discovery_path() -> PathBuf {
    let dirs = paths::get();
    match &dirs.runtime_dir {
        Some(runtime) => runtime.join("daemon.json"),
        None => dirs.data_dir.join("daemon.json"),
    }
}

/// The default UDS path the daemon binds (ADR-0096).
#[cfg(unix)]
pub fn default_uds_path() -> PathBuf {
    let dirs = paths::get();
    match &dirs.runtime_dir {
        Some(runtime) => runtime.join("daemon.sock"),
        None => dirs.data_dir.join("daemon.sock"),
    }
}

/// The daemon's single-instance lock path (ADR-0101): a companion
/// `daemon.lock` next to the global discovery record. The daemon holds a
/// `flock` on it for its whole lifetime; a second daemon (spawned while the
/// first drains) blocks on it for a bounded wait instead of unlinking a live
/// daemon's UDS socket, which is exactly the clobbering race the pre-0101
/// `bind_uds` "remove stale socket file" step could not distinguish.
pub fn global_lock_path() -> PathBuf {
    let dirs = paths::get();
    match &dirs.runtime_dir {
        Some(runtime) => runtime.join("daemon.lock"),
        None => dirs.data_dir.join("daemon.lock"),
    }
}

/// RAII guard over the global discovery record: `Drop` removes the file, so
/// *every* exit path — graceful drain, forced escalation, a panic unwinding
/// through `host::run` — leaves the record deleted. The graceful path
/// removes it explicitly (and earlier: pulling the advertisement is the
/// *first* drain step so no new client discovers a draining daemon), at
/// which point the guard's own removal is a no-op.
#[must_use = "dropping the lease removes the discovery record"]
pub struct DiscoveryLease {
    path: Option<PathBuf>,
    pid: u32,
}

impl DiscoveryLease {
    /// Wrap an already-written record. `None` (write failed) still yields a
    /// guard — a no-op one — so callers need no branching.
    pub fn new(path: Option<PathBuf>, pid: u32) -> Self {
        Self { path, pid }
    }

    /// Remove the record now (the explicit early step of a graceful drain).
    pub fn release(&mut self) {
        if let Some(path) = self.path.take() {
            remove_if_matching_pid(&path, self.pid);
        }
    }
}

impl Drop for DiscoveryLease {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod lease_tests {
    use super::*;

    #[test]
    fn drop_removes_the_record_and_double_release_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.json");
        neenee_persistence::fsutil::atomic_write_json(
            &path,
            &Discovery {
                pid: 1,
                port: 2,
                token: None,
                project_root: String::new(),
                started_at: 3,
                uds_path: None,
                version: None,
            },
        )
        .unwrap();
        let mut lease = DiscoveryLease::new(Some(path.clone()), 1);
        lease.release(); // explicit early removal
        assert!(!path.exists());
        drop(lease); // Drop's own removal must tolerate the missing file
    }

    #[test]
    fn drop_alone_removes_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.json");
        neenee_persistence::fsutil::atomic_write_json(
            &path,
            &Discovery {
                pid: 42,
                port: 2,
                token: None,
                project_root: String::new(),
                started_at: 3,
                uds_path: None,
                version: None,
            },
        )
        .unwrap();
        drop(DiscoveryLease::new(Some(path.clone()), 42));
        assert!(!path.exists(), "Drop must remove the record");
    }

    #[test]
    fn drop_does_not_remove_newer_daemon_record() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.json");
        neenee_persistence::fsutil::atomic_write_json(
            &path,
            &Discovery {
                pid: 99,
                port: 2,
                token: None,
                project_root: String::new(),
                started_at: 3,
                uds_path: None,
                version: None,
            },
        )
        .unwrap();
        // Lease from older daemon PID 42 dropped while path contains PID 99
        drop(DiscoveryLease::new(Some(path.clone()), 42));
        assert!(path.exists(), "Drop must NOT remove newer daemon's record");
    }
}

/// Write the unified daemon's global discovery record (ADR-0096). Atomic.
pub fn write_global(record: &Discovery) -> Result<PathBuf, String> {
    let path = global_discovery_path();
    write_to(&path, record)?;
    Ok(path)
}

/// Resolve the discovery-file path for `project_root` against the
/// process-wide [`paths::get`] dirs (see module docs).
pub fn discovery_path(project_root: &Path) -> PathBuf {
    path_from_dirs(&paths::get(), project_root)
}

/// Write `record` to this project's discovery path. Atomic (temp file +
/// rename, via [`neenee_persistence::fsutil::atomic_write_json`]) so a
/// concurrent reader never sees a partial file; the `serve/` subdirectory is
/// created as needed. An existing record is overwritten — last server wins;
/// staleness validation is the reader's job. Returns the path written.
pub fn write(project_root: &Path, record: &Discovery) -> Result<PathBuf, String> {
    let path = discovery_path(project_root);
    if path.exists() {
        tracing::warn!(
            path = %path.display(),
            "serve discovery: overwriting existing discovery file (last server wins)"
        );
    }
    write_to(&path, record)?;
    Ok(path)
}

/// Best-effort removal on clean shutdown (and of stale records by readers).
/// A missing file is not an error.
pub fn remove(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            ?error,
            path = %path.display(),
            "serve discovery: could not remove discovery file"
        );
    }
}

/// Remove `path` only if the discovery record inside belongs to `expected_pid`.
/// Prevents an older daemon from unlinking a newer daemon's discovery file.
pub fn remove_if_matching_pid(path: &Path, expected_pid: u32) {
    if let Ok(bytes) = std::fs::read(path) {
        if let Ok(record) = serde_json::from_slice::<Discovery>(&bytes) {
            if record.pid != expected_pid {
                return;
            }
        }
    }
    remove(path);
}

/// The path-resolution rule, split from [`paths::get`] so tests can supply
/// their own dirs without touching process-wide env.
fn path_from_dirs(dirs: &Dirs, project_root: &Path) -> PathBuf {
    match &dirs.runtime_dir {
        Some(runtime) => runtime
            .join("serve")
            .join(format!("{}.json", paths::project_bucket_name(project_root))),
        None => dirs.project_dir(project_root).join("serve.json"),
    }
}

/// Atomic write of one record to an explicit path.
fn write_to(path: &Path, record: &Discovery) -> Result<(), String> {
    neenee_persistence::fsutil::atomic_write_json(path, record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(runtime_dir: Option<PathBuf>, data_dir: PathBuf) -> Dirs {
        Dirs {
            config_dir: data_dir.clone(),
            data_dir,
            state_dir: PathBuf::from("/unused/state"),
            cache_dir: PathBuf::from("/unused/cache"),
            runtime_dir,
        }
    }

    fn sample_record() -> Discovery {
        Discovery {
            pid: 4242,
            port: 39871,
            token: Some("deadbeef".to_string()),
            project_root: "/home/me/proj".to_string(),
            started_at: 1_755_000_000,
            uds_path: None,
            version: Some("0.26.1".to_string()),
        }
    }

    #[test]
    fn runtime_dir_is_preferred_when_present() {
        let dirs = dirs(
            Some(PathBuf::from("/run/user/1000/neenee")),
            PathBuf::from("/data/neenee"),
        );
        let root = Path::new("/home/me/proj");
        let bucket = paths::project_bucket_name(root);
        assert_eq!(
            path_from_dirs(&dirs, root),
            PathBuf::from(format!("/run/user/1000/neenee/serve/{bucket}.json"))
        );
    }

    #[test]
    fn falls_back_to_project_bucket_without_runtime_dir() {
        let dirs = dirs(None, PathBuf::from("/data/neenee"));
        let root = Path::new("/home/me/proj");
        let bucket = paths::project_bucket_name(root);
        assert_eq!(
            path_from_dirs(&dirs, root),
            PathBuf::from(format!("/data/neenee/projects/{bucket}/serve.json"))
        );
    }

    #[test]
    fn write_then_read_roundtrips_and_creates_serve_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve").join("abc123.json");
        let record = sample_record();
        write_to(&path, &record).unwrap();
        assert!(path.exists(), "record file must exist after write");
        let bytes = std::fs::read(&path).unwrap();
        let parsed: Discovery = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed, record);
    }

    #[test]
    fn remove_deletes_and_tolerates_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.json");
        // Missing file: best-effort, must not panic.
        remove(&path);
        write_to(&path, &sample_record()).unwrap();
        remove(&path);
        assert!(!path.exists(), "record file must be gone after remove");
    }

    #[test]
    fn version_roundtrips_and_defaults_to_none_for_legacy_records() {
        // A legacy record without `version` still deserializes (field is
        // optional) and reports "unknown" to version negotiation.
        let legacy = r#"{
            "pid": 1, "port": 7, "token": null, "project_root": "",
            "started_at": 9
        }"#;
        let parsed: Discovery = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.version, None);
        // And a versioned record roundtrips.
        let json = serde_json::to_string(&sample_record()).unwrap();
        let back: Discovery = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version.as_deref(), Some("0.26.1"));
        // `version` is skipped when None so legacy readers see their own shape.
        let none_version = Discovery {
            version: None,
            ..sample_record()
        };
        let json = serde_json::to_string(&none_version).unwrap();
        assert!(!json.contains("version"), "absent field must be omitted");
    }
}
