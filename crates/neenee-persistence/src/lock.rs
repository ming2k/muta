//! Cross-process advisory lock using POSIX `flock(2)`.
//!
//! `fcntl(F_SETLK)` is the classic NFS-safe record lock, but within a single
//! process it silently replaces an existing lock, which makes same-process
//! unit testing impossible and can surprise callers that re-open the same
//! file. `flock` is per-file-description and `LOCK_NB` returns `EWOULDBLOCK`
//! even for the same process on a different fd, giving predictable exclusion
//! for the lifetime of the guard. For neenee's local lock files this is the
//! right trade-off.
//!
//! On non-Unix platforms the implementation is currently a no-op; a
//! Windows-aware implementation should use `LockFileEx` over the same file.

use std::fs::File;
use std::path::Path;

/// Guard returned after a successful lock acquisition. Dropping it closes the
/// underlying file descriptor, which releases the `flock`.
pub struct ProcessLock {
    #[cfg(unix)]
    #[allow(dead_code)]
    file: File,
    #[cfg(not(unix))]
    #[allow(dead_code)]
    _marker: (),
}

impl ProcessLock {
    /// Acquire an exclusive, non-blocking advisory lock on `path`.
    ///
    /// Returns an error if the lock cannot be obtained, most commonly because
    /// another `neenee` process already holds it for the same project.
    pub fn acquire(path: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use std::fs::OpenOptions;
            use std::io::{Seek, SeekFrom, Write};
            use std::os::unix::io::AsRawFd;

            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
                .map_err(|error| {
                    format!("could not open lock file {}: {}", path.display(), error)
                })?;
            let fd = file.as_raw_fd();
            let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if rc < 0 {
                let err = std::io::Error::last_os_error();
                let holder = Self::probe_holder(path)
                    .map(|pid| format!("; held by pid {pid}"))
                    .unwrap_or_default();
                return Err(format!(
                    "could not acquire advisory lock on {}: {}{holder} \
                     (another neenee instance may already be running for this project)",
                    path.display(),
                    err
                ));
            }
            let _ = file.set_len(0);
            let _ = file.seek(SeekFrom::Start(0));
            let _ = writeln!(file, "{}", std::process::id());
            let _ = file.flush();
            Ok(Self { file })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(Self { _marker: () })
        }
    }

    /// Read the PID stored inside `path` by the acquiring process, if readable.
    pub fn probe_holder(path: &Path) -> Option<u32> {
        let s = std::fs::read_to_string(path).ok()?;
        s.trim().parse::<u32>().ok()
    }

    /// Probe whether `path` is currently locked by testing a non-blocking `flock`.
    pub fn is_locked(path: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::fs::OpenOptions;
            use std::os::unix::io::AsRawFd;
            if let Ok(file) = OpenOptions::new().read(true).write(true).open(path) {
                let fd = file.as_raw_fd();
                let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
                if rc == 0 {
                    let _ = unsafe { libc::flock(fd, libc::LOCK_UN) };
                    false
                } else {
                    true
                }
            } else {
                false
            }
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            false
        }
    }

    /// Acquire an exclusive advisory lock on `path`, blocking up to
    /// `timeout` (polling the non-blocking attempt at a short interval, so a
    /// held lock surfaces within one interval of being released). Used where
    /// a *bounded queue* is wanted — most importantly the session daemon's
    /// single-instance lock (ADR-0101): a daemon spawned while the previous
    /// one is draining waits here, then binds the freshly released socket
    /// instead of clobbering it.
    ///
    /// On non-Unix platforms the lock is a no-op (see module docs), so this
    /// returns immediately.
    pub fn acquire_with_timeout(path: &Path, timeout: std::time::Duration) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let poll = std::time::Duration::from_millis(50);
            let start = std::time::Instant::now();
            loop {
                match Self::acquire(path) {
                    Ok(lock) => return Ok(lock),
                    Err(_busy) => {
                        if start.elapsed() >= timeout {
                            return Err(format!(
                                "could not acquire advisory lock on {} within {:.0}s \
                                 (another neenee daemon appears to be running)",
                                path.display(),
                                timeout.as_secs_f32()
                            ));
                        }
                    }
                }
                std::thread::sleep(poll);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (path, timeout);
            Ok(Self { _marker: () })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn second_process_cannot_acquire_same_lock() {
        let dir = std::env::temp_dir().join(format!("neenee-lock-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("neenee.lock");
        std::fs::create_dir_all(&dir).unwrap();

        let first = ProcessLock::acquire(&path).expect("first acquire should succeed");
        let second = ProcessLock::acquire(&path);
        assert!(
            second.is_err(),
            "second acquire should fail while first is held"
        );

        drop(first);
        let third = ProcessLock::acquire(&path).expect("lock should be reusable after drop");
        drop(third);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn acquire_with_timeout_waits_then_succeeds_after_release() {
        let dir = std::env::temp_dir().join(format!("neenee-lock-wait-{}", uuid::Uuid::new_v4()));
        let path = dir.join("daemon.lock");
        std::fs::create_dir_all(&dir).unwrap();

        let first = ProcessLock::acquire(&path).expect("first acquire should succeed");
        // A contended acquire inside the budget must eventually win: release
        // the holder from a helper thread while we wait.
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            drop(first);
        });
        let second = ProcessLock::acquire_with_timeout(&path, std::time::Duration::from_secs(2))
            .expect("contended acquire should succeed after the holder releases");
        drop(second);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn acquire_with_timeout_gives_up_past_the_budget() {
        let dir =
            std::env::temp_dir().join(format!("neenee-lock-timeout-{}", uuid::Uuid::new_v4()));
        let path = dir.join("daemon.lock");
        std::fs::create_dir_all(&dir).unwrap();

        let held = ProcessLock::acquire(&path).expect("first acquire should succeed");
        let second =
            ProcessLock::acquire_with_timeout(&path, std::time::Duration::from_millis(150));
        assert!(second.is_err(), "contended acquire should time out");
        drop(held);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn probe_holder_and_is_locked_report_state() {
        let dir = std::env::temp_dir().join(format!("neenee-lock-probe-{}", uuid::Uuid::new_v4()));
        let path = dir.join("daemon.lock");
        std::fs::create_dir_all(&dir).unwrap();

        assert!(!ProcessLock::is_locked(&path));
        assert_eq!(ProcessLock::probe_holder(&path), None);

        let lock = ProcessLock::acquire(&path).expect("acquire lock");
        assert!(ProcessLock::is_locked(&path));
        assert_eq!(ProcessLock::probe_holder(&path), Some(std::process::id()));

        drop(lock);
        assert!(!ProcessLock::is_locked(&path));
        let _ = std::fs::remove_dir_all(dir);
    }
}
