//! Discovery file: how co-process clients find a live session server's
//! endpoint.
//!
//! Once the OS has assigned a port, the serving process (the `neenee-server`
//! binary today; any [`crate::serve::start_server`] host tomorrow) writes one
//! small JSON record into a well-known per-project location; on clean
//! shutdown it removes it. Clients (an attaching `neenee --attach` TUI, a
//! browser launcher) read the record to attach to the already-running session
//! host instead of spawning a second one. The module lives in this crate —
//! not in either binary — so writer and reader share one definition of the
//! record and of the path-resolution rule.
//!
//! Path resolution reuses [`neenee_persistence::paths`] so a client resolving
//! the same project root lands on the same file:
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
    /// (`--public`); `null` for the unauthenticated loopback default.
    pub token: Option<String>,
    /// The project root the host serves (as passed via `--project`).
    pub project_root: String,
    /// Unix seconds at startup.
    pub started_at: u64,
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
}
