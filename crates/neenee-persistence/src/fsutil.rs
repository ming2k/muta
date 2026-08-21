//! Filesystem durability helpers shared by [`crate::config`] and
//! [`crate::session`].
//!
//! The functions here implement the **atomic-rename + fsync** durability
//! pattern required for crash-safe single-file updates. POSIX guarantees that
//! `rename(2)` is atomic on the same filesystem, but only `fsync(2)` forces the
//! data and metadata to durable media. Without an additional `fsync` of the
//! parent directory, ext4 in particular can reorder the directory entry update
//! such that a power loss after `rename` leaves neither the old nor the new
//! file reachable.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Owner-only protection applied to every file we write and its leaf parent:
/// Unix uses `0600`/`0700`; Windows uses protected current-user DACLs. Config
/// and session files hold secrets (API keys) and private conversation content,
/// so inherited or ambient permissions must never weaken this boundary.
/// Create the leaf parent directory of `path` (and any missing ancestors),
/// then tighten the leaf to the platform's owner-only policy.
fn create_parent_dir(path: &Path) -> std::io::Result<()> {
    neenee_platform::secure_file::create_private_parent(path)
}

/// Create `path` for writing with owner-only permissions from the moment it
/// exists, so there is never a window where the file is group/world-readable.
fn create_private_file(path: &Path) -> std::io::Result<File> {
    neenee_platform::secure_file::create_private_file(path)
}

/// Write `bytes` atomically: serialise to `<path>.tmp`, `fsync`, `rename` over
/// `path`, then best-effort `fsync` of `path`'s parent directory.
///
/// The temp file and leaf parent are owner-only from creation (`0600`/`0700`
/// on Unix, protected current-user DACLs on Windows), so secrets never pass
/// through a broadly readable state.
///
/// Returns the original [`std::io::Error`] on any failure. The temporary file
/// is best-effort cleaned up on failure (its presence is not itself corrupting —
/// the next successful write will overwrite it).
pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    create_parent_dir(path)?;
    let temporary = path.with_extension("tmp");
    let result = (|| -> std::io::Result<()> {
        let mut file = create_private_file(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        neenee_platform::secure_file::atomic_replace(&temporary, path)?;
        if let Some(parent) = path.parent()
            && let Ok(dir) = File::open(parent)
        {
            // Best-effort: fsync the directory so the rename entry reaches
            // disk. Errors here (filesystems that reject syncing a dir fd)
            // are non-fatal — the data file is already durable.
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Atomically write a pretty-printed JSON value. Convenience wrapper around
/// [`atomic_write_bytes`]. `?Sized` so it accepts slices like `&[String]`.
pub fn atomic_write_json<T: serde::Serialize + ?Sized>(
    path: &Path,
    value: &T,
) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    atomic_write_bytes(path, &bytes).map_err(|e| e.to_string())
}

/// Guard for a blocking exclusive advisory lock held on a companion
/// `<path>.lock` file. Used to serialise short read-modify-write windows on
/// shared global files (`provider_usage.json`, slash-command history, the
/// per-project embedding index) so two concurrently-running `neenee` instances
/// in the same project — or across projects — never silently lose each other's
/// updates. Dropping the guard closes the fd and releases the lock.
///
/// The lock is held on a **companion** file rather than the data file itself
/// because the data file is rewritten via temp-file + `rename(2)` (see
/// [`atomic_write_bytes`]), which swaps the underlying inode. A `flock` on the
/// data file's old fd would not protect the newly-renamed inode, so concurrent
/// writers could each believe they held the lock. The companion file is
/// opened once and never renamed, so its file-description lock reliably
/// serialises every holder for the lock's lifetime.
///
/// The companion is locked with `flock(2)` on Unix and `LockFileEx` on
/// Windows. There is no successful no-op fallback: callers either hold real
/// cross-process exclusion or receive an error.
pub struct FileLock {
    _lock: crate::lock::ProcessLock,
}

impl FileLock {
    /// Acquire a blocking exclusive lock on `<path>.lock`. Creates the
    /// companion file (and its parent directory) if missing, then blocks
    /// until an exclusive `flock(2)` is obtained.
    pub fn acquire(path: &Path) -> std::io::Result<Self> {
        let lock_path = lock_companion(path);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock = crate::lock::ProcessLock::acquire_with_timeout(
            &lock_path,
            std::time::Duration::from_secs(30),
        )
        .map_err(std::io::Error::other)?;
        Ok(Self { _lock: lock })
    }
}

/// Companion lock-file path for `path`: `<path>.lock`.
fn lock_companion(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Sample {
        name: &'static str,
        n: u32,
    }

    #[test]
    fn atomic_write_round_trips_and_removes_tmp() {
        let dir = std::env::temp_dir().join(format!("neenee-fsutil-{}", uuid::Uuid::new_v4()));
        let path = dir.join("payload.json");
        atomic_write_json(&path, &Sample { name: "ok", n: 7 }).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"name\": \"ok\""));
        assert!(text.contains("\"n\": 7"));
        assert!(
            !dir.join("payload.tmp").exists(),
            "temp file must be cleaned up"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("neenee-fsutil-{}-perm", uuid::Uuid::new_v4()));
        let path = dir.join("secret.json");
        atomic_write_json(&path, &Sample { name: "k", n: 1 }).unwrap();
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "secret file must be rw-------");
        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "secret dir must be rwx------");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_write_overwrites_existing() {
        let dir = std::env::temp_dir().join(format!("neenee-fsutil-{}-2", uuid::Uuid::new_v4()));
        let path = dir.join("payload.json");
        atomic_write_json(&path, &Sample { name: "v1", n: 1 }).unwrap();
        atomic_write_json(&path, &Sample { name: "v2", n: 2 }).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"name\": \"v2\""));
        assert!(!text.contains("v1"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
