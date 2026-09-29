//! Process-lifecycle primitives. A daemon and an owned subprocess tree have
//! intentionally different lifetime policies.

use std::io;
use tokio::process::{Child, Command};

/// Configure a long-lived daemon to detach from the invoking terminal.
///
/// This never attaches the daemon to a kill-on-close Windows Job Object.
pub fn configure_daemon(command: &mut Command) {
    native::configure_daemon(command);
}

/// Synchronous-command counterpart used before a runtime exists.
pub fn configure_daemon_std(command: &mut std::process::Command) {
    native::configure_daemon_std(command);
}

/// Spawn a subprocess whose complete descendant tree is owned by the returned
/// guard. Configuration, spawn, and containment are one operation so callers
/// cannot accidentally run an uncontained (or Windows-suspended) child.
pub fn spawn_owned(command: &mut Command) -> io::Result<(Child, OwnedProcessTree)> {
    native::configure_owned(command);
    let child = command.spawn()?;
    match OwnedProcessTree::attach(&child) {
        Ok(tree) => Ok((child, tree)),
        Err(error) => {
            native::rollback_failed_attach(&child);
            Err(error)
        }
    }
}

/// Whether this platform can give a supervised child its own controlling
/// terminal (Unix: yes; Windows: no). Callers use this to decide between
/// offering the supervised input path and falling back to the sealed
/// immediate-EOF floor, so they never *induce* interactive behaviour the
/// platform cannot service.
#[must_use]
pub const fn controlling_terminal_supported() -> bool {
    cfg!(unix)
}

/// A supervised child's held-open input channels.
///
/// The child runs with stdout/stderr on clean pipes (so terminal control
/// sequences never reach the transcript) while *also* owning a private
/// controlling terminal: `sudo`/`gpg`/`pinentry`/`git`, which read
/// `/dev/tty` rather than stdin, get a real terminal to prompt on, and the
/// harness writes the operator's answer through [`PtyMaster::write_input`].
/// `stdin` is a held-open pipe (never closed until the command ends) so a
/// `read(stdin)`-style prompt blocks — and is therefore detectable — instead
/// of receiving an immediate EOF.
pub struct SupervisedSpawn {
    pub child: Child,
    pub tree: OwnedProcessTree,
    /// The master side of the child's terminal. The child's stdin *is* the
    /// slave side, so stdin reads and `/dev/tty` reads are the same channel and
    /// both are answered here. `None` only when the platform cannot provide a
    /// terminal.
    pub tty: Option<PtyMaster>,
}

/// Master side of a supervised child's controlling terminal.
pub struct PtyMaster {
    #[cfg(unix)]
    fd: std::os::fd::RawFd,
}

// SAFETY: a master pty fd is a plain file descriptor; it carries no thread
// affinity and all access goes through `&self` syscalls.
unsafe impl Send for PtyMaster {}
unsafe impl Sync for PtyMaster {}

/// Fork-safe child setup that acquires the control terminal. `Fn` (not
/// `FnOnce`) and captured by reference so it satisfies `pre_exec`'s bounds.
pub type PtyChildSetup = Box<dyn Fn() -> io::Result<()> + Send + Sync>;

impl PtyMaster {
    /// Write `data` to the terminal, then `\n` — the answer to a prompt the
    /// child is reading from `/dev/tty`.
    pub fn write_input(&self, data: &str) -> io::Result<()> {
        #[cfg(unix)]
        {
            let mut bytes = data.as_bytes().to_vec();
            bytes.push(b'\n');
            let mut written = 0;
            while written < bytes.len() {
                // SAFETY: `fd` is a live master fd owned by this handle; the
                // buffer is a valid slice.
                let n = unsafe {
                    libc::write(
                        self.fd,
                        bytes[written..].as_ptr().cast(),
                        bytes.len() - written,
                    )
                };
                if n < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                written += n as usize;
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = data;
            Err(io::Error::other(
                "controlling-terminal input is unsupported on this platform",
            ))
        }
    }
}

impl Drop for PtyMaster {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // SAFETY: `fd` is owned solely by this handle; closed exactly once.
            unsafe {
                libc::close(self.fd);
            }
        }
    }
}

/// Spawn a **supervised** subprocess: owned process tree + held-open stdin pipe
/// + private controlling terminal, with stdout/stderr captured on clean pipes.
///
/// On Unix the child becomes a session leader (`setsid`) and claims the slave
/// pty as its controlling terminal (`TIOCSCTTY`), so an explicit
/// `open("/dev/tty")` resolves to that terminal rather than the operator's;
/// writing to the parent-held master feeds it. The child never sees the
/// operator's real terminal. On Windows the terminal channel is unavailable,
/// so this degrades to `tty: None` (held-open stdin pipe only).
pub fn spawn_supervised(command: &mut Command) -> io::Result<SupervisedSpawn> {
    // The placeholder stdin is replaced (via `dup2` in the child setup) by the
    // pty slave once the terminal is opened; stdout/stderr stay clean pipes.
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    // Order matters: `setsid` must establish the new session before the
    // controlling-terminal acquiring `ioctl` can succeed.
    native::configure_owned(command);
    let (tty, acquire_ctty) = native::open_pty_master()?;
    if let Some(acquire) = acquire_ctty {
        // SAFETY: `pre_exec` runs between fork and exec; the closure performs
        // only async-signal-safe syscalls (open, dup2, ioctl).
        unsafe {
            command.pre_exec(acquire);
        }
    }
    let child = command.spawn()?;
    let tree = OwnedProcessTree::attach(&child)?;
    Ok(SupervisedSpawn { child, tree, tty })
}

/// Native lifetime guard for an owned subprocess tree.
///
/// Keep this guard for the intended subprocess lifetime. Explicit
/// [`Self::terminate`] and dropping the guard both kill remaining descendants,
/// including processes which outlive the direct shell child.
pub struct OwnedProcessTree {
    native: native::OwnedProcessTree,
}

impl OwnedProcessTree {
    fn attach(child: &Child) -> io::Result<Self> {
        Ok(Self {
            native: native::OwnedProcessTree::attach(child)?,
        })
    }

    pub fn terminate(&self) -> io::Result<()> {
        self.native.terminate()
    }

    /// Sample aggregate process activity and kernel wait states for this process tree.
    #[must_use]
    pub fn sample_activity(&self) -> ProcessActivitySample {
        self.native.sample_activity()
    }
}

/// Classify a single process's wait state from kernel evidence. A thin test
/// seam over the `native` classifier so the detector test and the sampler
/// share exactly one implementation.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn classify_sleeping_process(pid: u32) -> WaitState {
    native::classify_sleeping_pid(pid as libc::pid_t)
}

/// What a process group is currently blocked on, resolved from kernel
/// scheduling evidence rather than from output text. Output is agnostic to the
/// reason for a stall (a compiling build and a password prompt both emit
/// nothing), so "is this command waiting for input?" is a question about
/// process state, not stream contents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WaitState {
    /// No live process observed in the group.
    #[default]
    Gone,
    /// Group is running or runnable.
    Running,
    /// Blocked in interruptible sleep with no CPU progress, but not on an
    /// input channel the harness owns (e.g. `flock`, a mutex, an unread
    /// socket) — or blocked on a channel genuinely unidentifiable because
    /// `wchan` symbols are restricted by the kernel. Treated as ambiguous: the
    /// caller applies its idle-timeout floor rather than fast-failing.
    Sleeping,
    /// Blocked reading a channel the harness provisioned and can write into.
    /// This is the unambiguous "awaiting input" signal.
    AwaitingInput { channel: InputChannel },
}

/// Which harness-owned input channel a process is blocked reading.
///
/// Both variants name a channel the harness owns and can write an answer into.
/// In the supervised design the child's stdin *is* its controlling terminal, so
/// `Pipe` and `Terminal` are often the same underlying channel; the tag exists
/// so a reader can still tell which fd the process holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputChannel {
    /// A harness-held pipe (the child's fd 0 when not a terminal).
    Pipe,
    /// A terminal — the child's controlling terminal, reached either as its
    /// stdin (slave side) or via an explicit `open("/dev/tty")`.
    Terminal,
}

/// Snapshot of kernel execution activity for an owned process tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessActivitySample {
    /// Number of live descendant processes observed in this process group.
    pub process_count: usize,
    /// Aggregate CPU ticks (user + system time) across all live processes in the group.
    pub total_cpu_ticks: u64,
    /// Whether all live processes in the group are in interruptible sleep (`state == 'S'`).
    pub all_sleeping: bool,
    /// The most informative wait state observed across the group, in the
    /// priority order `AwaitingInput > Running > Sleeping > Gone`.
    pub wait_state: WaitState,
}

/// Stable-enough native process identity used to avoid acting on a recycled
/// PID. The birth token is an OS process creation timestamp/start tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub birth_token: u64,
}

pub fn process_identity(pid: u32) -> io::Result<ProcessIdentity> {
    native::process_identity(pid)
}

pub fn process_is_alive(identity: ProcessIdentity) -> bool {
    process_identity(identity.pid).is_ok_and(|current| current == identity)
}

/// Force-terminate exactly the process represented by `identity`.
pub fn force_terminate(identity: ProcessIdentity) -> io::Result<()> {
    if !process_is_alive(identity) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "process no longer exists or PID has been reused",
        ));
    }
    native::force_terminate(identity.pid)
}

/// Request the platform's graceful process termination, when one exists.
/// Windows daemons use the control protocol and therefore report
/// `Unsupported` here; callers may then proceed to their force tier.
pub fn request_termination(identity: ProcessIdentity) -> io::Result<()> {
    if !process_is_alive(identity) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "process no longer exists or PID has been reused",
        ));
    }
    native::request_termination(identity.pid)
}

/// Checks whether a running process's executed binary image matches the on-disk file.
pub fn process_image_matches_path(pid: u32, expected: &std::path::Path) -> bool {
    native::native_process_image_matches_path(pid, expected)
}

#[cfg(unix)]
mod native {
    use super::*;

    pub(super) fn native_process_image_matches_path(pid: u32, expected: &std::path::Path) -> bool {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let exe_link = std::path::Path::new("/proc")
                .join(pid.to_string())
                .join("exe");
            match (std::fs::metadata(&exe_link), std::fs::metadata(expected)) {
                (Ok(daemon), Ok(expected_meta)) => {
                    daemon.dev() == expected_meta.dev() && daemon.ino() == expected_meta.ino()
                }
                _ => true,
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (pid, expected);
            true
        }
    }

    pub(super) fn configure_daemon(command: &mut Command) {
        // SAFETY: `setsid` is async-signal-safe and performs no allocation.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    pub(super) fn configure_daemon_std(command: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;

        // SAFETY: `setsid` is async-signal-safe and performs no allocation.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    pub(super) fn configure_owned(command: &mut Command) {
        // SAFETY: `setsid` is async-signal-safe, creates a new session, establishes
        // the child as the session and process-group leader (PGID == PID), and
        // completely detaches it from the host controlling terminal (/dev/tty)
        // per ADR-0286.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    /// Open a pty pair. Returns the parent-held master handle plus the
    /// fork-safe closure that makes the child claim the slave as its
    /// controlling terminal **and** use it as stdin. The child is already a
    /// session leader from [`configure_owned`]; `pre_exec` runs after stdio
    /// setup, so the `dup2` here overrides the placeholder stdin and leaves the
    /// child reading/writing prompts on the terminal while stdout/stderr stay
    /// pipes.
    pub(super) fn open_pty_master() -> io::Result<(Option<super::PtyMaster>, Option<PtyChildSetup>)> {
        // SAFETY: `posix_openpt`/`grantpt`/`unlockpt` operate on a freshly
        // opened fd and perform no allocation.
        let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        if master < 0 {
            return Err(io::Error::last_os_error());
        }
        let handle = super::PtyMaster { fd: master };
        // SAFETY: `master` is a live pty master fd.
        if unsafe { libc::grantpt(master) } != 0 || unsafe { libc::unlockpt(master) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // A zero-sized window makes some programs that query `TIOCGWINSZ`
        // misbehave; give the terminal a conventional default size.
        let winsize = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `master` is a live pty master fd; suppressing the result is
        // fine — a failed size hint only affects cosmetic wrapping.
        unsafe {
            libc::ioctl(master, libc::TIOCSWINSZ, &winsize);
        }
        // SAFETY: `ptsname` returns a static string for the given master fd.
        let slave_ptr = unsafe { libc::ptsname(master) };
        if slave_ptr.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `slave_ptr` is a valid NUL-terminated C string.
        let slave_path = unsafe { std::ffi::CStr::from_ptr(slave_ptr) }
            .to_string_lossy()
            .into_owned();
        let acquire: PtyChildSetup = Box::new(move || {
            // SAFETY: async-signal-safe open + dup2 + ioctl between fork and exec.
            let fd = unsafe { libc::open(slave_path.as_ptr().cast(), libc::O_RDWR) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // Make the terminal the child's stdin, so a `read(stdin)` and a
            // `read("/dev/tty")` are the same channel the harness can answer.
            if unsafe { libc::dup2(fd, 0) } < 0 {
                return Err(io::Error::last_os_error());
            }
            if unsafe { libc::ioctl(0, libc::TIOCSCTTY, 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
        Ok((Some(handle), Some(acquire)))
    }

    pub(super) fn rollback_failed_attach(child: &Child) {
        if let Some(pid) = child.id() {
            // SAFETY: configure_owned made this child the process-group
            // leader. This rollback is reached only when guard creation
            // failed, before ownership can be returned to the caller.
            unsafe {
                libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            }
        }
    }

    pub(super) struct OwnedProcessTree {
        pgid: libc::pid_t,
    }

    impl OwnedProcessTree {
        pub(super) fn attach(child: &Child) -> io::Result<Self> {
            let pid = child
                .id()
                .ok_or_else(|| io::Error::other("child has no process id"))?;
            Ok(Self {
                pgid: pid as libc::pid_t,
            })
        }

        pub(super) fn terminate(&self) -> io::Result<()> {
            // SAFETY: a negative pid targets the process group established by
            // `configure_owned`. ESRCH means the tree already exited.
            let rc = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
            if rc == 0 {
                Ok(())
            } else {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }

        pub(super) fn sample_activity(&self) -> super::ProcessActivitySample {
            #[cfg(target_os = "linux")]
            {
                let mut sample = super::ProcessActivitySample {
                    process_count: 0,
                    total_cpu_ticks: 0,
                    all_sleeping: true,
                    wait_state: super::WaitState::Gone,
                };
                let Ok(entries) = std::fs::read_dir("/proc") else {
                    return sample;
                };
                for entry in entries.flatten() {
                    let Ok(file_name) = entry.file_name().into_string() else {
                        continue;
                    };
                    if !file_name.chars().all(|c| c.is_ascii_digit()) {
                        continue;
                    }
                    let Ok(pid) = file_name.parse::<libc::pid_t>() else {
                        continue;
                    };
                    let stat_path = entry.path().join("stat");
                    let Ok(stat) = std::fs::read_to_string(stat_path) else {
                        continue;
                    };
                    let Some((_, tail)) = stat.rsplit_once(") ") else {
                        continue;
                    };
                    let mut fields = tail.split_whitespace();
                    let Some(state) = fields.next() else { continue };
                    let _ppid = fields.next();
                    let Some(pgid_str) = fields.next() else { continue };
                    let Ok(proc_pgid) = pgid_str.parse::<libc::pid_t>() else { continue };
                    if proc_pgid != self.pgid {
                        continue;
                    }

                    sample.process_count += 1;
                    let sleeping = state == "S";
                    if !sleeping {
                        sample.all_sleeping = false;
                    }
                    // Fields: tail[0]=state, tail[1]=ppid, tail[2]=pgid,
                    // tail[11]=utime, tail[12]=stime (skip 8 to reach tail[11])
                    let mut remaining = fields.skip(8);
                    let utime = remaining.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                    let stime = remaining.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                    sample.total_cpu_ticks += utime + stime;

                    let candidate = if !sleeping {
                        super::WaitState::Running
                    } else {
                        classify_sleeping_pid(pid)
                    };
                    // Priority: AwaitingInput > Running > Sleeping > Gone.
                    sample.wait_state = match (sample.wait_state, candidate) {
                        (super::WaitState::AwaitingInput { .. }, _) => sample.wait_state,
                        (_, super::WaitState::AwaitingInput { channel }) => {
                            super::WaitState::AwaitingInput { channel }
                        }
                        (_, super::WaitState::Running) => super::WaitState::Running,
                        (super::WaitState::Gone, other) => other,
                        (existing, _) => existing,
                    };
                }
                sample
            }

            #[cfg(not(target_os = "linux"))]
            {
                super::ProcessActivitySample::default()
            }
        }
    }

    /// Resolve the input-blocked status of one sleeping Linux process from
    /// `/proc/<pid>/wchan` (the kernel function it sleeps in) and
    /// `/proc/<pid>/fd/0` (the channel fd 0 resolves to).
    ///
    /// `wchan` names the wait *reason*: a read on a pipe the harness holds
    /// (`pipe_wait_readable`, `anon_pipe_read`, `pipe_read`, `wait_for_partner`)
    /// or on a terminal (`wait_woken`, `n_tty_read`) is an input wait;
    /// `hrtimer_nanosleep`, `do_wait`, `poll_schedule_timeout` are legitimate
    /// quiet computation and must never be mistaken for a prompt.
    ///
    /// A terminal-family wait is authoritative on its own: the child may read
    /// `/dev/tty` on a *separate* fd while fd 0 is an unrelated pipe (exactly
    /// the `sudo`/`gpg`/`pinentry` shape), so fd 0 must not gate it. A pipe
    /// wait is only an input wait when fd 0 really is the pipe, which keeps a
    /// socket read (`wait_woken` on an fd-0 socket) from being misclassified.
    /// A process whose `wchan` is restricted by the kernel (reads `0`) falls
    /// back to the ambiguous `Sleeping` state rather than guessing.
    pub(super) fn classify_sleeping_pid(pid: libc::pid_t) -> super::WaitState {
        let wchan = std::fs::read_to_string(format!("/proc/{pid}/wchan")).unwrap_or_default();
        let wchan = wchan.trim();
        if wchan.is_empty() || wchan == "0" {
            return super::WaitState::Sleeping;
        }
        let fd0 = std::fs::read_link(format!("/proc/{pid}/fd/0"))
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stdin_is_pipe = fd0.starts_with("pipe:") || fd0 == "pipe";
        let stdin_is_terminal = fd0.starts_with("/dev/tty") || fd0.starts_with("/dev/pts/");
        if matches!(wchan, "n_tty_read" | "wait_woken") {
            return super::WaitState::AwaitingInput {
                channel: if stdin_is_terminal {
                    super::InputChannel::Terminal
                } else if stdin_is_pipe {
                    super::InputChannel::Pipe
                } else {
                    super::InputChannel::Terminal
                },
            };
        }
        if matches!(
            wchan,
            "pipe_wait_readable" | "anon_pipe_read" | "pipe_read" | "wait_for_partner"
        ) && stdin_is_pipe
        {
            return super::WaitState::AwaitingInput {
                channel: super::InputChannel::Pipe,
            };
        }
        super::WaitState::Sleeping
    }

    impl Drop for OwnedProcessTree {
        fn drop(&mut self) {
            // Descendants may still be alive after the direct child exits.
            // Ownership is lexical: dropping the guard closes that lifetime.
            let _ = self.terminate();
        }
    }

    pub(super) fn process_identity(pid: u32) -> io::Result<ProcessIdentity> {
        #[cfg(target_os = "linux")]
        {
            // Field 22 in /proc/<pid>/stat is the process start time. The
            // comm field may contain spaces and ')' so split only after its
            // final closing parenthesis.
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
            let tail = stat
                .rsplit_once(") ")
                .map(|(_, tail)| tail)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid proc stat"))?;
            let birth_token = tail
                .split_whitespace()
                .nth(19)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing start time"))?
                .parse::<u64>()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            Ok(ProcessIdentity { pid, birth_token })
        }

        #[cfg(target_os = "macos")]
        {
            use std::mem::{size_of, zeroed};

            let mut info: libc::proc_bsdinfo = unsafe { zeroed() };
            // SAFETY: `info` is valid writable storage for PROC_PIDTBSDINFO;
            // libproc returns the number of bytes written.
            let written = unsafe {
                libc::proc_pidinfo(
                    pid as libc::c_int,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&raw mut info).cast(),
                    size_of::<libc::proc_bsdinfo>() as libc::c_int,
                )
            };
            if written != size_of::<libc::proc_bsdinfo>() as libc::c_int {
                return Err(io::Error::last_os_error());
            }
            let birth_token = info
                .pbi_start_tvsec
                .checked_mul(1_000_000)
                .and_then(|seconds| seconds.checked_add(info.pbi_start_tvusec))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "process start time overflow")
                })?;
            Ok(ProcessIdentity { pid, birth_token })
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            // Portable fallback for Unix targets without a native process
            // creation-time API wired here yet.
            // SAFETY: signal zero performs permission/liveness probing only.
            if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
                Ok(ProcessIdentity {
                    pid,
                    birth_token: 0,
                })
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }

    pub(super) fn force_terminate(pid: u32) -> io::Result<()> {
        // SAFETY: the caller verified the process identity immediately before
        // this signal.
        if unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn request_termination(pid: u32) -> io::Result<()> {
        // SAFETY: the caller verified process identity immediately before
        // requesting SIGTERM.
        if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_has_a_stable_nonzero_birth_token() {
        let first = process_identity(std::process::id()).expect("current process identity");
        let second = process_identity(std::process::id()).expect("current process identity");
        assert_eq!(first, second);
        assert_ne!(first.birth_token, 0);
        assert!(process_is_alive(first));
    }

    /// The core discrimination: a `read(stdin)` block (harness-held pipe) is
    /// classified `AwaitingInput`, while a legitimate quiet sleep is not. This
    /// is what lets the supervised loop park on a real prompt without
    /// fast-failing a compiling build.
    #[cfg(target_os = "linux")]
    #[test]
    fn wait_state_distinguishes_stdin_read_from_sleep() {
        use std::process::{Command, Stdio};

        // A child holding a silent, still-open stdin pipe and blocked on read.
        let (mut reader, writer) = std::io::pipe().expect("pipe");
        let prompt = Command::new("sh")
            .arg("-c")
            .arg("read line")
            .stdin(Stdio::from(reader.try_clone().expect("clone")))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn prompt child");
        // A child legitimately sleeping.
        let sleeper = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleeper");

        std::thread::sleep(std::time::Duration::from_millis(600));
        let prompt_state = classify_sleeping_process(prompt.id());
        let sleeper_state = classify_sleeping_process(sleeper.id());
        let _ = &mut reader;

        let mut prompt = prompt;
        let mut sleeper = sleeper;
        let _ = prompt.kill();
        let _ = sleeper.kill();
        let _ = prompt.wait();
        let _ = sleeper.wait();
        drop(writer);

        assert_eq!(
            prompt_state,
            WaitState::AwaitingInput {
                channel: InputChannel::Pipe
            },
            "a live-pipe read must be classified as awaiting stdin input"
        );
        assert_eq!(
            sleeper_state,
            WaitState::Sleeping,
            "a nanosleep must NOT be mistaken for an input wait"
        );
    }

    /// The rule that fixes the `sudo`/`gpg` shape: a terminal-family wait
    /// (`wait_woken`) is an input wait even when fd 0 is an unrelated pipe,
    /// because the program reads `/dev/tty` on a separate fd. This locks in the
    /// distinction the former heuristic got wrong.
    #[cfg(target_os = "linux")]
    #[test]
    fn wait_state_classifies_stdin_terminal_read_as_terminal() {
        use std::process::{Command, Stdio};

        // Child whose stdin is a live pty slave: `read x` blocks on the
        // terminal fd 0.
        let (master, slave) = open_pty_for_test();
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("read x")
            .stdin(Stdio::from(slave))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        std::thread::sleep(std::time::Duration::from_millis(600));
        let state = classify_sleeping_process(child.id());
        let _ = child.kill();
        let _ = child.wait();
        drop(master);
        assert_eq!(
            state,
            WaitState::AwaitingInput {
                channel: InputChannel::Terminal
            },
            "a read on a terminal stdio must classify as a terminal input wait"
        );
    }

    /// Open a pty pair for tests, returning `(master_file, slave_file)`. Uses
    /// the same primitives as the production path but as `std::fs::File`s.
    #[cfg(unix)]
    fn open_pty_for_test() -> (std::fs::File, std::fs::File) {
        use std::os::fd::FromRawFd;
        // SAFETY: fresh pty open; all calls check their result.
        let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        assert!(master >= 0, "posix_openpt failed");
        assert_eq!(unsafe { libc::grantpt(master) }, 0, "grantpt failed");
        assert_eq!(unsafe { libc::unlockpt(master) }, 0, "unlockpt failed");
        let ptr = unsafe { libc::ptsname(master) };
        assert!(!ptr.is_null(), "ptsname failed");
        let path = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: path is a valid slave device name.
        let slave = unsafe { libc::open(path.as_ptr().cast(), libc::O_RDWR | libc::O_NOCTTY) };
        assert!(slave >= 0, "open slave failed");
        // SAFETY: both fds are owned and valid.
        unsafe {
            (
                std::fs::File::from_raw_fd(master),
                std::fs::File::from_raw_fd(slave),
            )
        }
    }

    /// A controlled terminal must be assignable on Unix: the child's session
    /// gets a slave pty as its controlling terminal, so an explicit
    /// `/dev/tty` read resolves to the harness-held master.
    #[cfg(unix)]
    #[tokio::test]
    async fn supervised_spawn_provides_a_controlling_terminal() {
        let marker = std::env::temp_dir().join(format!(
            "muta-ctty-{}.txt",
            uuid::Uuid::new_v4().simple()
        ));
        let script = format!("read x < /dev/tty && printf '%s' \"$x\" > {}", marker.display());
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg(script);
        let mut spawned = spawn_supervised(&mut command).expect("supervised spawn");
        assert!(spawned.tty.is_some(), "a pty master must be provided");
        spawned
            .tty
            .as_ref()
            .unwrap()
            .write_input("hunter2")
            .expect("write to master");
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), spawned.child.wait())
            .await
            .expect("child did not exit")
            .expect("wait");
        assert!(status.success(), "child exited with {status:?}");
        let body = std::fs::read_to_string(&marker).unwrap_or_default();
        let _ = std::fs::remove_file(&marker);
        assert_eq!(body, "hunter2", "the child did not read our /dev/tty answer");
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::mem::{size_of, zeroed};
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED, GetProcessTimes, OpenProcess,
        OpenThread, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, ResumeThread,
        THREAD_SUSPEND_RESUME, TerminateProcess,
    };

    pub(super) fn configure_daemon(command: &mut Command) {
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    pub(super) fn configure_daemon_std(command: &mut std::process::Command) {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    pub(super) fn configure_owned(command: &mut Command) {
        // Suspend before the first user instruction so `attach` can place the
        // process in its Job Object before it has any opportunity to spawn an
        // uncontained descendant. `attach` resumes the primary thread only
        // after assignment succeeds.
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }

    /// Windows has no controlling-terminal equivalent here, so a supervised
    /// spawn degrades to a held-open stdin pipe only. The command exporting a
    /// terminal does not skip this: the caller reports `unsupported` rather
    /// than silently misbehaving.
    pub(super) fn open_pty_master(
    ) -> io::Result<(Option<super::PtyMaster>, Option<super::PtyChildSetup>)> {
        Ok((None, None))
    }

    pub(super) struct OwnedProcessTree {
        job: HANDLE,
    }

    unsafe impl Send for OwnedProcessTree {}
    unsafe impl Sync for OwnedProcessTree {}

    impl OwnedProcessTree {
        pub(super) fn attach(child: &Child) -> io::Result<Self> {
            // SAFETY: null attributes/name request an unnamed, non-inheritable
            // job owned solely by this guard.
            let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }

            let tree = Self { job };

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `info` matches the requested information class.
            let configured = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                let error = io::Error::last_os_error();
                return Err(error);
            }

            let process = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("child has no process handle"))?
                as HANDLE;
            // SAFETY: Tokio owns a live process handle for `child`; the job
            // handle remains owned by this guard.
            if unsafe { AssignProcessToJobObject(tree.job, process) } == 0 {
                let error = io::Error::last_os_error();
                return Err(error);
            }
            if let Err(error) = resume_primary_thread(
                child
                    .id()
                    .ok_or_else(|| io::Error::other("child has no process id"))?,
            ) {
                // Dropping `tree` closes the kill-on-close Job Object, so a
                // process whose primary thread cannot be resumed never leaks.
                return Err(error);
            }
            Ok(tree)
        }

        pub(super) fn terminate(&self) -> io::Result<()> {
            // SAFETY: this guard owns a live job handle.
            if unsafe { TerminateJobObject(self.job, 1) } != 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }

        pub(super) fn sample_activity(&self) -> super::ProcessActivitySample {
            super::ProcessActivitySample::default()
        }
    }

    pub(super) fn rollback_failed_attach(child: &Child) {
        if let Some(process) = child.raw_handle() {
            // A configure_owned child is still suspended here and cannot run
            // cleanup of its own. TerminateProcess is the only safe rollback
            // if Job Object creation/assignment failed.
            unsafe {
                TerminateProcess(process as HANDLE, 1);
            }
        }
    }

    fn resume_primary_thread(pid: u32) -> io::Result<()> {
        // CreateProcess starts this process with exactly one suspended thread.
        // ToolHelp is used because std/Tokio intentionally expose the process
        // handle but not the primary-thread handle returned by CreateProcess.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        struct Snapshot(HANDLE);
        impl Drop for Snapshot {
            fn drop(&mut self) {
                unsafe { CloseHandle(self.0) };
            }
        }
        let snapshot = Snapshot(snapshot);
        let mut entry: THREADENTRY32 = unsafe { zeroed() };
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut found = unsafe { Thread32First(snapshot.0, &mut entry) } != 0;
        while found {
            if entry.th32OwnerProcessID == pid {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let resumed = unsafe { ResumeThread(thread) };
                unsafe { CloseHandle(thread) };
                if resumed == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            found = unsafe { Thread32Next(snapshot.0, &mut entry) } != 0;
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "suspended child primary thread was not found",
        ))
    }

    impl Drop for OwnedProcessTree {
        fn drop(&mut self) {
            // KILL_ON_JOB_CLOSE is the final containment guarantee.
            unsafe { CloseHandle(self.job) };
        }
    }

    struct ProcessHandle(HANDLE);

    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    fn open(pid: u32, access: u32) -> io::Result<ProcessHandle> {
        // SAFETY: OpenProcess validates pid/access and returns an owned handle.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(ProcessHandle(handle))
        }
    }

    pub(super) fn process_identity(pid: u32) -> io::Result<ProcessIdentity> {
        let process = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let mut created: FILETIME = unsafe { zeroed() };
        let mut exited: FILETIME = unsafe { zeroed() };
        let mut kernel: FILETIME = unsafe { zeroed() };
        let mut user: FILETIME = unsafe { zeroed() };
        // SAFETY: all FILETIME pointers are valid writable outputs.
        if unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        let birth_token = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
        Ok(ProcessIdentity { pid, birth_token })
    }

    pub(super) fn force_terminate(pid: u32) -> io::Result<()> {
        let process = open(pid, PROCESS_TERMINATE)?;
        // SAFETY: handle carries PROCESS_TERMINATE access.
        if unsafe { TerminateProcess(process.0, 1) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn request_termination(_pid: u32) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows daemon shutdown is protocol-driven",
        ))
    }

    pub(super) fn native_process_image_matches_path(
        _pid: u32,
        _expected: &std::path::Path,
    ) -> bool {
        true
    }
}

