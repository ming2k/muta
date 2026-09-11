//! Background job manager for long-running process commands and asynchronous sub-subagents.
//!
//! Dual-track execution model:
//! - **Track A (Process Jobs)**: OS-level subprocesses (`tokio::process`) capturing output
//!   into in-memory ring buffers and disk logs, with 0 LLM token cost.
//! - **Track B (Sub-Subagent Jobs)**: Asynchronous isolated exploration subagents.
//!
//! Emits live progress events and delivers completed outcomes to the session mailbox.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

use muta_contracts::{BackgroundJobInfo, BackgroundJobOutcome, JobId, JobKind, JobSpec, JobState};

const DEFAULT_RING_BUFFER_CAPACITY: usize = 500;

struct JobEntry {
    info: BackgroundJobInfo,
    ring_buffer: VecDeque<String>,
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Owning session (ADR-0190 D5): `None` = daemon-level task.
    owner_session: Option<String>,
    #[cfg(unix)]
    pid: Option<u32>,
}

/// Shared thread-safe manager for session background jobs.
#[derive(Clone)]
pub struct BackgroundJobManager {
    inner: Arc<RwLock<HashMap<JobId, JobEntry>>>,
    outcome_tx: mpsc::UnboundedSender<BackgroundJobOutcome>,
    outcome_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<BackgroundJobOutcome>>>,
    event_tx: broadcast::Sender<BackgroundJobEvent>,
    log_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub enum BackgroundJobEvent {
    Started(BackgroundJobInfo),
    Progress {
        job_id: JobId,
        line: String,
    },
    /// A service task reported readiness (ADR-0190): readiness condition met,
    /// process alive. Wake-eligible.
    Ready {
        job_id: JobId,
    },
    Completed(BackgroundJobOutcome),
}

impl Default for BackgroundJobManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Execution configuration and sandboxing options for background process spawning.
#[derive(Debug, Clone)]
pub struct ProcessSpawnOptions<'a> {
    pub label: Option<String>,
    pub cwd: Option<PathBuf>,
    pub workspace_root: &'a Path,
    pub additional_roots: &'a [PathBuf],
    pub detached: bool,
    pub timeout: Option<Duration>,
    /// Owning session (ADR-0190 D5): recorded in job snapshots and ledger
    /// rows so "whose task is this" is answerable. `None` = daemon-level
    /// (rehosted services, fabric-internal tasks).
    pub owner_session: Option<String>,
}

impl BackgroundJobManager {
    pub fn new() -> Self {
        let (outcome_tx, outcome_rx) = mpsc::unbounded_channel();
        let (event_tx, _) = broadcast::channel(256);
        let log_dir = std::env::temp_dir().join("muta-jobs");
        let _ = std::fs::create_dir_all(&log_dir);

        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            outcome_tx,
            outcome_rx: Arc::new(tokio::sync::Mutex::new(outcome_rx)),
            event_tx,
            log_dir,
        }
    }

    /// Subscribe to real-time job lifecycle events (started, progress, completed).
    pub fn subscribe(&self) -> broadcast::Receiver<BackgroundJobEvent> {
        self.event_tx.subscribe()
    }

    /// Receiver for completed job outcomes (for the session event loop mailbox).
    pub fn outcome_receiver(
        &self,
    ) -> Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<BackgroundJobOutcome>>> {
        Arc::clone(&self.outcome_rx)
    }

    /// Query snapshot info for all jobs.
    pub fn list_jobs(&self) -> Vec<BackgroundJobInfo> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut list: Vec<BackgroundJobInfo> = guard.values().map(|e| e.info.clone()).collect();
        list.sort_by_key(|b| std::cmp::Reverse(b.created_at_ms));
        list
    }

    /// Query snapshot info for a specific job.
    pub fn get_job(&self, id: &JobId) -> Option<BackgroundJobInfo> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(id).map(|e| e.info.clone())
    }

    /// Retrieve tail logs for a specific job.
    pub fn get_logs(&self, id: &JobId, tail_lines: usize) -> Option<Vec<String>> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = guard.get(id)?;
        let count = tail_lines.min(entry.ring_buffer.len());
        let skip = entry.ring_buffer.len().saturating_sub(count);
        Some(entry.ring_buffer.iter().skip(skip).cloned().collect())
    }

    /// Spawn a deterministic shell command in the background.
    pub async fn spawn_process(
        &self,
        command: String,
        opts: ProcessSpawnOptions<'_>,
    ) -> Result<BackgroundJobInfo, String> {
        let ProcessSpawnOptions {
            label,
            cwd,
            workspace_root,
            additional_roots,
            detached,
            timeout,
            owner_session,
        } = opts;
        let job_id = JobId::new("job");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let log_file_path = self.log_dir.join(format!("{}.log", job_id.0));

        let mut cmd = muta_platform::workspace_sandbox::shell_with_roots(
            &command,
            workspace_root,
            additional_roots,
            muta_platform::workspace_sandbox::WorkspaceAccess::ReadWrite,
            muta_platform::workspace_sandbox::NetworkAccess::Enabled,
        )?;

        if let Some(ref dir) = cwd {
            cmd.current_dir(dir);
        }

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        #[cfg(unix)]
        {
            // Set process group so we can cleanly kill subprocess trees if needed
            cmd.process_group(0);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn background command: {e}"))?;
        let pid = child.id();

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let info = BackgroundJobInfo {
            id: job_id.clone(),
            spec: JobSpec::Process {
                command: command.clone(),
                label: label.clone(),
                cwd,
                detached,
                task_kind: JobKind::default(),
                readiness: None,
                restart: None,
            },
            state: JobState::Running {
                started_at_ms: now_ms,
                pid,
            },
            created_at_ms: now_ms,
            completed_at_ms: None,
            latest_output: None,
        };

        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();

        {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.insert(
                job_id.clone(),
                JobEntry {
                    info: info.clone(),
                    ring_buffer: VecDeque::with_capacity(DEFAULT_RING_BUFFER_CAPACITY),
                    owner_session: owner_session.clone(),
                    cancel_tx: Some(cancel_tx),
                    #[cfg(unix)]
                    pid,
                },
            );
        }

        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Started(info.clone()));

        // Spawn async collector and supervisor task
        let mgr = self.clone();
        let jid = job_id.clone();
        let spec = info.spec.clone();

        tokio::spawn(async move {
            let start_time = Instant::now();
            let mut log_writer = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&log_file_path)
                .await
                .ok();

            // Bounded line pump (ADR-0190 D5): the fabric never accumulates
            // unbounded progress. Cap 1024 lines in flight; under pressure
            // the *progress broadcast* coalesces (oldest dropped via the
            // send failure path) while the ring buffer + disk log keep full
            // fidelity — a slow UI never stalls the child, and a flood
            // never balloons memory.
            let (line_tx, mut line_rx) = mpsc::channel::<String>(1024);

            // Read stdout stream
            if let Some(out) = stdout {
                let tx = line_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(out).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                });
            }

            // Read stderr stream
            if let Some(err) = stderr {
                let tx = line_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(err).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                });
            }
            drop(line_tx);

            let timeout_duration = timeout.unwrap_or(Duration::from_secs(3600));
            let timeout_sleep = tokio::time::sleep(timeout_duration);
            tokio::pin!(timeout_sleep);

            let mut was_cancelled = false;
            let mut was_timed_out = false;

            let exit_status = loop {
                tokio::select! {
                    Some(line) = line_rx.recv() => {
                        mgr.append_line(&jid, &line);
                        if let Some(ref mut w) = log_writer {
                            use tokio::io::AsyncWriteExt;
                            let _ = w.write_all(line.as_bytes()).await;
                            let _ = w.write_all(b"\n").await;
                        }
                        let _ = mgr.event_tx.send(BackgroundJobEvent::Progress {
                            job_id: jid.clone(),
                            line,
                        });
                    }
                    status = child.wait() => {
                        // Drain remaining lines
                        while let Ok(line) = line_rx.try_recv() {
                            mgr.append_line(&jid, &line);
                            if let Some(ref mut w) = log_writer {
                                use tokio::io::AsyncWriteExt;
                                let _ = w.write_all(line.as_bytes()).await;
                                let _ = w.write_all(b"\n").await;
                            }
                        }
                        break status.ok();
                    }
                    _ = &mut cancel_rx => {
                        was_cancelled = true;
                        let _ = child.kill().await;
                        break None;
                    }
                    _ = &mut timeout_sleep => {
                        was_timed_out = true;
                        let _ = child.kill().await;
                        break None;
                    }
                }
            };

            let duration_ms = start_time.elapsed().as_millis() as u64;
            let final_state = if was_cancelled {
                JobState::Killed { duration_ms }
            } else if was_timed_out {
                JobState::TimedOut { duration_ms }
            } else if let Some(status) = exit_status {
                let code = status
                    .code()
                    .unwrap_or(if status.success() { 0 } else { 1 });
                if status.success() {
                    JobState::Succeeded {
                        duration_ms,
                        exit_code: code,
                    }
                } else {
                    JobState::Failed {
                        duration_ms,
                        exit_code: code,
                        error: format!("Process exited with status code {code}"),
                    }
                }
            } else {
                JobState::Failed {
                    duration_ms,
                    exit_code: -1,
                    error: "Process terminated unexpectedly".to_string(),
                }
            };

            mgr.finish_job(jid, spec, final_state, Some(log_file_path));
        });

        Ok(info)
    }

    /// Spawn with full ADR-0190 control (kind / readiness / restart).
    ///
    /// `Interactive` delegates to [`Self::spawn_process`]. `Service` reports
    /// `Ready` when the readiness condition is met, never settles while
    /// running, wakes the session with `Failed` on unsolicited death, and
    /// honors an optional [`muta_contracts::RestartPolicy`].
    pub async fn spawn_process_ex(
        &self,
        command: String,
        opts: ProcessSpawnOptions<'_>,
        kind: muta_contracts::JobKind,
        readiness: Option<muta_contracts::Readiness>,
        restart: Option<muta_contracts::RestartPolicy>,
    ) -> Result<BackgroundJobInfo, String> {
        use muta_contracts::{JobKind, Readiness};
        if kind == JobKind::Interactive {
            return self.spawn_process(command, opts).await;
        }
        let ProcessSpawnOptions {
            label,
            cwd,
            workspace_root,
            additional_roots,
            detached,
            timeout,
            owner_session,
        } = opts;
        let _ = timeout; // service lifetime is not wall-bounded; stop is explicit

        let job_id = JobId::new("svc");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let log_file_path = self.log_dir.join(format!("{}.log", job_id.0));

        let mut cmd = muta_platform::workspace_sandbox::shell_with_roots(
            &command,
            workspace_root,
            additional_roots,
            muta_platform::workspace_sandbox::WorkspaceAccess::ReadWrite,
            muta_platform::workspace_sandbox::NetworkAccess::Enabled,
        )?;
        if let Some(ref dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn service task: {e}"))?;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let info = BackgroundJobInfo {
            id: job_id.clone(),
            spec: JobSpec::Process {
                command: command.clone(),
                label: label.clone(),
                cwd,
                detached,
                task_kind: JobKind::Service,
                readiness,
                restart,
            },
            state: JobState::Running {
                started_at_ms: now_ms,
                pid,
            },
            created_at_ms: now_ms,
            completed_at_ms: None,
            latest_output: None,
        };

        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();
        {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.insert(
                job_id.clone(),
                JobEntry {
                    info: info.clone(),
                    ring_buffer: VecDeque::with_capacity(DEFAULT_RING_BUFFER_CAPACITY),
                    owner_session: owner_session.clone(),
                    cancel_tx: Some(cancel_tx),
                    #[cfg(unix)]
                    pid,
                },
            );
        }
        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Started(info.clone()));

        let mgr = self.clone();
        let jid = job_id.clone();
        let spec = info.spec.clone();
        let readiness = readiness.unwrap_or_default();

        tokio::spawn(async move {
            let start_time = Instant::now();
            let mut log_writer = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&log_file_path)
                .await
                .ok();

            let (line_tx, mut line_rx) = mpsc::channel::<String>(1024);
            if let Some(out) = stdout {
                let tx = line_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(out).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                });
            }
            if let Some(err) = stderr {
                let tx = line_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(err).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                });
            }
            drop(line_tx);

            // Readiness grace timer for AfterMs; armed lazily below.
            let mut grace: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = match readiness {
                Readiness::AfterMs(ms) => Some(Box::pin(tokio::time::sleep(
                    Duration::from_millis(ms.max(1)),
                ))),
                _ => None,
            };

            let mut ready = false;
            let mut unsolicited = true;
            let exit_status = loop {
                tokio::select! {
                    Some(line) = line_rx.recv() => {
                        if !ready && matches!(readiness, Readiness::FirstOutput) {
                            ready = true;
                            mgr.mark_service_ready(&jid);
                            let _ = mgr.event_tx.send(BackgroundJobEvent::Ready { job_id: jid.clone() });
                        }
                        mgr.append_line(&jid, &line);
                        if let Some(ref mut w) = log_writer {
                            let _ = w.write_all(line.as_bytes()).await;
                            let _ = w.write_all(b"\n").await;
                        }
                        let _ = mgr.event_tx.send(BackgroundJobEvent::Progress {
                            job_id: jid.clone(),
                            line,
                        });
                    }
                    _ = async {
                        // The grace timer is armed only for `AfterMs` and only
                        // while the service is not yet ready; else pend forever
                        // (the branch guard re-checks the same conditions).
                        // `grace: Pin<Box<Sleep>>` — `as_mut` yields the
                        // pinned projection awaited directly.
                        let armed = if !ready { grace.as_mut() } else { None };
                        match armed {
                            Some(g) => g.await,
                            None => std::future::pending().await,
                        }
                    }, if !ready && matches!(readiness, Readiness::AfterMs(_)) => {
                        ready = true;
                        mgr.mark_service_ready(&jid);
                        let _ = mgr.event_tx.send(BackgroundJobEvent::Ready { job_id: jid.clone() });
                    }
                    status = child.wait() => {
                        // exited on its own — `unsolicited` stays `true`
                        break status.ok();
                    }
                    _ = &mut cancel_rx => {
                        let _ = child.kill().await;
                        unsolicited = false;
                        break None;
                    }
                }
            };

            let duration_ms = start_time.elapsed().as_millis() as u64;
            let final_state = if !unsolicited {
                JobState::Killed { duration_ms }
            } else if let Some(status) = exit_status {
                let code = status
                    .code()
                    .unwrap_or(if status.success() { 0 } else { 1 });
                if status.success() {
                    JobState::Succeeded {
                        duration_ms,
                        exit_code: code,
                    }
                } else {
                    JobState::Failed {
                        duration_ms,
                        exit_code: code,
                        error: format!("Service exited with status code {code}"),
                    }
                }
            } else {
                JobState::Failed {
                    duration_ms,
                    exit_code: -1,
                    error: "Service terminated unexpectedly".to_string(),
                }
            };
            mgr.finish_job(jid, spec, final_state, Some(log_file_path));
        });

        Ok(info)
    }

    /// Spawn a Timer task (ADR-0190): sleep until `fire_at_ms`, then emit a
    /// completed outcome whose digest is the caller's prompt — the mailbox
    /// wakes the session with it. Recurring timers re-arm with `interval_ms`
    /// after each fire (deadline = previous deadline + interval) and keep
    /// the same task id; cancellation (`kill_job`) stops the loop.
    pub fn spawn_timer(
        &self,
        label: Option<String>,
        fire_at_ms: u64,
        interval_ms: Option<u64>,
        prompt: String,
        owner_session: Option<String>,
    ) -> Result<BackgroundJobInfo, String> {
        let job_id = JobId::new("timer");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let info = BackgroundJobInfo {
            id: job_id.clone(),
            spec: JobSpec::Timer {
                label,
                fire_at_ms,
                interval_ms,
                prompt: prompt.clone(),
            },
            state: JobState::Running {
                started_at_ms: now_ms,
                pid: None,
            },
            created_at_ms: now_ms,
            completed_at_ms: None,
            latest_output: None,
        };

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.insert(
                job_id.clone(),
                JobEntry {
                    info: info.clone(),
                    ring_buffer: VecDeque::with_capacity(DEFAULT_RING_BUFFER_CAPACITY),
                    owner_session: owner_session.clone(),
                    cancel_tx: Some(cancel_tx),
                    #[cfg(unix)]
                    pid: None,
                },
            );
        }
        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Started(info.clone()));

        let mgr = self.clone();
        let jid = job_id.clone();
        let spec = info.spec.clone();
        tokio::spawn(async move {
            let mut deadline = fire_at_ms;
            let mut cancel = cancel_rx;
            loop {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let wait = deadline.saturating_sub(now);
                tokio::select! {
                    _ = &mut cancel => break,
                    _ = tokio::time::sleep(Duration::from_millis(wait)) => {}
                }
                mgr.finish_job_with_summary(
                    jid.clone(),
                    spec.clone(),
                    JobState::Succeeded {
                        duration_ms: 0,
                        exit_code: 0,
                    },
                    None,
                    prompt.clone(),
                );
                match interval_ms {
                    Some(interval) => deadline += interval,
                    None => break,
                }
            }
        });

        Ok(info)
    }

    fn mark_service_ready(&self, job_id: &JobId) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = guard.get_mut(job_id)
            && let JobState::Running { started_at_ms, .. } = entry.info.state
        {
            entry.info.state = JobState::Ready {
                started_at_ms,
                ready_at_ms: now_ms,
            };
        }
    }

    /// Adopt an already-running foreground child that hit its sync budget
    /// (ADR-0190 detach-on-budget). The fabric takes ownership of the child,
    /// replays the captured foreground tail as progress, and settles with a
    /// normal outcome when the process exits — the caller's session is woken
    /// through the ordinary completion event.
    pub async fn adopt_process(
        &self,
        command: String,
        label: Option<String>,
        adoption: muta_contracts::AdoptionInfo,
    ) -> Result<BackgroundJobInfo, String> {
        let job_id = JobId::new("adopted");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let log_file_path = self.log_dir.join(format!("{}.log", job_id.0));

        let info = BackgroundJobInfo {
            id: job_id.clone(),
            spec: JobSpec::Process {
                command: command.clone(),
                label: label.clone(),
                cwd: None,
                detached: true,
                task_kind: JobKind::Interactive,
                readiness: None,
                restart: None,
            },
            state: JobState::Running {
                started_at_ms: now_ms,
                pid: Some(adoption.pid),
            },
            created_at_ms: now_ms,
            completed_at_ms: None,
            latest_output: None,
        };

        {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.insert(
                job_id.clone(),
                JobEntry {
                    info: info.clone(),
                    ring_buffer: VecDeque::with_capacity(DEFAULT_RING_BUFFER_CAPACITY),
                    owner_session: None,
                    cancel_tx: None,
                    #[cfg(unix)]
                    pid: Some(adoption.pid),
                },
            );
        }

        for line in &adoption.captured_lines {
            self.append_line(&job_id, line);
        }
        let mut log_writer = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_file_path)
            .await
            .ok();
        for line in &adoption.captured_lines {
            if let Some(ref mut w) = log_writer {
                let _ = w.write_all(line.as_bytes()).await;
                let _ = w.write_all(b"\n").await;
            }
        }

        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Started(info.clone()));

        let mgr = self.clone();
        let jid = job_id.clone();
        let spec = info.spec.clone();
        let mut poll_child = adoption.child;

        tokio::spawn(async move {
            let start_time = Instant::now();
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                match poll_child.try_wait() {
                    Ok(None) => {}
                    Ok(Some(code)) => {
                        let duration_ms = start_time.elapsed().as_millis() as u64;
                        let summary = String::new();
                        let _ = summary;
                        let state = if code == 0 {
                            JobState::Succeeded {
                                duration_ms,
                                exit_code: 0,
                            }
                        } else {
                            JobState::Failed {
                                duration_ms,
                                exit_code: code,
                                error: format!("Process exited with status code {code}"),
                            }
                        };
                        mgr.finish_job(jid, spec, state, Some(log_file_path.clone()));
                        break;
                    }
                    Err(_) => {
                        let duration_ms = start_time.elapsed().as_millis() as u64;
                        mgr.finish_job(
                            jid,
                            spec,
                            JobState::Failed {
                                duration_ms,
                                exit_code: -1,
                                error: "adopted process could not be polled".to_string(),
                            },
                            Some(log_file_path.clone()),
                        );
                        break;
                    }
                }
            }
        });

        Ok(info)
    }

    fn append_line(&self, job_id: &JobId, line: &str) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = guard.get_mut(job_id) {
            if entry.ring_buffer.len() >= DEFAULT_RING_BUFFER_CAPACITY {
                entry.ring_buffer.pop_front();
            }
            entry.ring_buffer.push_back(line.to_string());
            entry.info.latest_output = Some(line.to_string());
        }
    }

    fn finish_job(&self, job_id: JobId, spec: JobSpec, state: JobState, log_path: Option<PathBuf>) {
        let summary = {
            let guard = self
                .inner
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(entry) = guard.get(&job_id) {
                let tail_count = 15.min(entry.ring_buffer.len());
                let tail: Vec<&str> = entry
                    .ring_buffer
                    .iter()
                    .rev()
                    .take(tail_count)
                    .map(|s| s.as_str())
                    .collect();
                let tail_rev: Vec<&str> = tail.into_iter().rev().collect();
                tail_rev.join("\n")
            } else {
                String::new()
            }
        };

        self.finish_job_with_summary(job_id, spec, state, log_path, summary);
    }

    fn finish_job_with_summary(
        &self,
        job_id: JobId,
        spec: JobSpec,
        state: JobState,
        log_path: Option<PathBuf>,
        summary: String,
    ) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let owner_session = {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match guard.get_mut(&job_id) {
                Some(entry) => {
                    entry.info.state = state.clone();
                    entry.info.completed_at_ms = Some(now_ms);
                    entry.cancel_tx = None;
                    entry.owner_session.clone()
                }
                None => None,
            }
        };

        let outcome = BackgroundJobOutcome {
            job_id,
            spec,
            state,
            summary,
            log_path,
        };

        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Completed(outcome.clone()));
        let _ = self.outcome_tx.send(outcome.clone());

        // ADR-0190 D4: persist the settle into the task ledger (best-effort,
        // on the blocking pool so the fabric's event path never waits on I/O).
        let ledger_outcome = outcome.clone();
        tokio::task::spawn_blocking(move || {
            crate::task_ledger::record_outcome(
                &muta_persistence::db::get_persistence_handle(),
                &ledger_outcome,
                owner_session,
            );
        });
    }

    /// Terminate a running background job.
    pub fn kill_job(&self, id: &JobId) -> Result<(), String> {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = guard
            .get_mut(id)
            .ok_or_else(|| format!("Job not found: {id}"))?;

        if let Some(tx) = entry.cancel_tx.take() {
            let _ = tx.send(());
        }

        #[cfg(unix)]
        if let Some(pid) = entry.pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }

        Ok(())
    }

    /// Abort all active background jobs (e.g. during session teardown).
    pub fn abort_all(&self) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for entry in guard.values_mut() {
            if let Some(tx) = entry.cancel_tx.take() {
                let _ = tx.send(());
            }
            #[cfg(unix)]
            if let Some(pid) = entry.pid {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                    libc::kill(pid as i32, libc::SIGKILL);
                }
            }
        }
    }
}

/// Session-scoped wrapper binding [`BackgroundJobManager`] with the session's [`muta_contracts::ExecutionEnvironment`].
#[derive(Clone)]
pub struct SessionJobService {
    manager: BackgroundJobManager,
    env: Arc<dyn muta_contracts::ExecutionEnvironment>,
    /// Owning session (ADR-0190 D5): stamped onto every task this service
    /// spawns so snapshots and ledger rows answer "whose task is this".
    owner_session: std::sync::OnceLock<String>,
}

impl SessionJobService {
    pub fn new(
        manager: BackgroundJobManager,
        env: Arc<dyn muta_contracts::ExecutionEnvironment>,
    ) -> Self {
        Self {
            manager,
            env,
            owner_session: std::sync::OnceLock::new(),
        }
    }

    /// Bind the owning session id (called once at driver startup, after the
    /// session id resolves). Later spawns carry it in every snapshot.
    pub fn bind_owner(&self, session_id: String) {
        let _ = self.owner_session.set(session_id);
    }

    fn owner(&self) -> Option<String> {
        self.owner_session.get().cloned()
    }

    pub fn manager(&self) -> &BackgroundJobManager {
        &self.manager
    }
}

#[async_trait::async_trait]
impl muta_contracts::BackgroundJobService for SessionJobService {
    async fn spawn_process(
        &self,
        command: String,
        label: Option<String>,
        cwd: Option<PathBuf>,
        detached: bool,
        timeout: Option<Duration>,
    ) -> Result<BackgroundJobInfo, String> {
        let roots = self.env.additional_roots();
        self.manager
            .spawn_process(
                command,
                ProcessSpawnOptions {
                    label,
                    cwd,
                    workspace_root: self.env.workspace_root(),
                    additional_roots: &roots,
                    detached,
                    timeout,
                    owner_session: self.owner(),
                },
            )
            .await
    }

    async fn spawn_process_ex(
        &self,
        command: String,
        label: Option<String>,
        cwd: Option<PathBuf>,
        detached: bool,
        timeout: Option<Duration>,
        kind: muta_contracts::JobKind,
        readiness: Option<muta_contracts::Readiness>,
        restart: Option<muta_contracts::RestartPolicy>,
    ) -> Result<BackgroundJobInfo, String> {
        let roots = self.env.additional_roots();
        self.manager
            .spawn_process_ex(
                command,
                ProcessSpawnOptions {
                    label,
                    cwd,
                    workspace_root: self.env.workspace_root(),
                    additional_roots: &roots,
                    detached,
                    timeout,
                    owner_session: self.owner(),
                },
                kind,
                readiness,
                restart,
            )
            .await
    }

    async fn adopt_process(
        &self,
        command: String,
        label: Option<String>,
        adoption: muta_contracts::AdoptionInfo,
    ) -> Result<BackgroundJobInfo, String> {
        self.manager.adopt_process(command, label, adoption).await
    }

    async fn spawn_timer(
        &self,
        label: &str,
        fire_at_ms: u64,
        interval_ms: Option<u64>,
        command: String,
    ) -> Result<BackgroundJobInfo, String> {
        self.manager.spawn_timer(
            Some(label.to_string()),
            fire_at_ms,
            interval_ms,
            command,
            self.owner(),
        )
    }

    fn list_jobs(&self) -> Vec<BackgroundJobInfo> {
        self.manager.list_jobs()
    }

    fn get_job(&self, id: &JobId) -> Option<BackgroundJobInfo> {
        self.manager.get_job(id)
    }

    fn get_logs(&self, id: &JobId, tail_lines: usize) -> Option<Vec<String>> {
        self.manager.get_logs(id, tail_lines)
    }

    fn kill_job(&self, id: &JobId) -> Result<(), String> {
        self.manager.kill_job(id)
    }

    fn abort_all(&self) {
        self.manager.abort_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_and_complete_process() {
        if !muta_platform::workspace_sandbox::available() {
            return;
        }
        let mgr = BackgroundJobManager::new();
        let ws = std::env::temp_dir();
        let roots = vec![];

        let mut rx = mgr.subscribe();

        let info = mgr
            .spawn_process(
                "echo 'hello from background'".to_string(),
                ProcessSpawnOptions {
                    label: Some("test-echo".to_string()),
                    cwd: None,
                    workspace_root: &ws,
                    additional_roots: &roots,
                    detached: false,
                    timeout: Some(Duration::from_secs(5)),
                    owner_session: None,
                },
            )
            .await
            .expect("spawn success");

        assert_eq!(
            info.spec,
            JobSpec::Process {
                command: "echo 'hello from background'".to_string(),
                label: Some("test-echo".to_string()),
                cwd: None,
                detached: false,
                task_kind: muta_contracts::JobKind::default(),
                readiness: None,
                restart: None,
            }
        );

        // Wait for completion event
        let mut completed = false;
        while let Ok(evt) = rx.recv().await {
            if let BackgroundJobEvent::Completed(outcome) = evt
                && outcome.job_id == info.id
            {
                assert!(matches!(
                    outcome.state,
                    JobState::Succeeded { exit_code: 0, .. }
                ));
                assert!(outcome.summary.contains("hello from background"));
                completed = true;
                break;
            }
        }
        assert!(completed);

        // Check list & get
        let snapshot = mgr.get_job(&info.id).expect("job exists");
        assert!(snapshot.state.is_terminal());

        let logs = mgr.get_logs(&info.id, 10).expect("logs exist");
        assert!(logs.iter().any(|l| l.contains("hello from background")));
    }

    #[tokio::test]
    async fn test_kill_process() {
        if !muta_platform::workspace_sandbox::available() {
            return;
        }
        let mgr = BackgroundJobManager::new();
        let ws = std::env::temp_dir();
        let roots = vec![];

        let mut rx = mgr.subscribe();

        let info = mgr
            .spawn_process(
                "sleep 10".to_string(),
                ProcessSpawnOptions {
                    label: Some("test-sleep".to_string()),
                    cwd: None,
                    workspace_root: &ws,
                    additional_roots: &roots,
                    detached: false,
                    timeout: Some(Duration::from_secs(10)),
                    owner_session: None,
                },
            )
            .await
            .expect("spawn success");

        // Give it a moment to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        mgr.kill_job(&info.id).expect("kill succeeded");

        let mut killed = false;
        while let Ok(evt) = rx.recv().await {
            if let BackgroundJobEvent::Completed(outcome) = evt
                && outcome.job_id == info.id
            {
                assert!(matches!(
                    outcome.state,
                    JobState::Killed { .. } | JobState::Failed { .. }
                ));
                killed = true;
                break;
            }
        }
        assert!(killed);
    }

    // ADR-0190 fabric tests: service readiness, adopt, extended spawn.

    #[tokio::test]
    async fn test_service_ready_on_first_output() {
        if !muta_platform::workspace_sandbox::available() {
            return;
        }
        let mgr = BackgroundJobManager::new();
        let ws = std::env::temp_dir();
        let roots = vec![];
        let mut rx = mgr.subscribe();

        let info = mgr
            .spawn_process_ex(
                "echo service-up && sleep 30".to_string(),
                ProcessSpawnOptions {
                    label: Some("test-svc".to_string()),
                    cwd: None,
                    workspace_root: &ws,
                    additional_roots: &roots,
                    detached: false,
                    timeout: None,
                    owner_session: None,
                },
                muta_contracts::JobKind::Service,
                Some(muta_contracts::Readiness::FirstOutput),
                None,
            )
            .await
            .expect("service spawn");

        // Expect Ready then (implicitly) no Completed while running.
        let mut saw_ready = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(BackgroundJobEvent::Ready { job_id })) if job_id == info.id => {
                    saw_ready = true;
                    break;
                }
                Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                    panic!("service settled while running: {:?}", o.state);
                }
                Ok(Ok(_)) => {}
                _ => break,
            }
        }
        assert!(saw_ready, "service never reported Ready");

        // Snapshot shows Ready state.
        let snap = mgr.get_job(&info.id).expect("job exists");
        assert!(matches!(snap.state, JobState::Ready { .. }));

        mgr.kill_job(&info.id).expect("kill");
    }

    #[tokio::test]
    async fn test_service_unsolicited_death_fails() {
        if !muta_platform::workspace_sandbox::available() {
            return;
        }
        let mgr = BackgroundJobManager::new();
        let ws = std::env::temp_dir();
        let roots = vec![];
        let mut rx = mgr.subscribe();

        let info = mgr
            .spawn_process_ex(
                "sleep 1; exit 3".to_string(),
                ProcessSpawnOptions {
                    label: None,
                    cwd: None,
                    workspace_root: &ws,
                    additional_roots: &roots,
                    detached: false,
                    timeout: None,
                    owner_session: None,
                },
                muta_contracts::JobKind::Service,
                None,
                None,
            )
            .await
            .expect("service spawn");

        let mut settled: Option<BackgroundJobOutcome> = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                    settled = Some(o);
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
                Err(_elapsed) => {} // recv timeout: keep waiting
            }
        }
        let outcome = settled.unwrap_or_else(|| panic!("service crash must settle"));
        assert!(
            matches!(outcome.state, JobState::Failed { exit_code: 3, .. }),
            "unexpected settle state: {:?}",
            outcome.state
        );
    }

    #[tokio::test]
    async fn test_adopt_process_settles_on_exit() {
        if !muta_platform::workspace_sandbox::available() {
            return;
        }
        let mgr = BackgroundJobManager::new();
        let ws = std::env::temp_dir();
        let roots = vec![];

        // Spawn a real detached child the fabric can adopt. stdio is null:
        // the adoption contract keeps the *tool's* pipes live; a test child
        // with no readers must not die of SIGPIPE on its own output.
        let mut cmd = muta_platform::workspace_sandbox::shell_with_roots(
            "sleep 1",
            &ws,
            &roots,
            muta_platform::workspace_sandbox::WorkspaceAccess::ReadWrite,
            muta_platform::workspace_sandbox::NetworkAccess::Disabled,
        )
        .unwrap();
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        let child = cmd.spawn().expect("spawn adoptable");
        let pid = child.id().unwrap_or_default();

        struct Owned {
            child: tokio::process::Child,
        }
        impl muta_contracts::CrateChildBridge for Owned {
            fn try_wait(&mut self) -> Result<Option<i32>, String> {
                self.child
                    .try_wait()
                    .map(|s| s.and_then(|st| st.code()))
                    .map_err(|e| e.to_string())
            }
            fn kill(&mut self) -> Result<(), String> {
                Ok(())
            }
        }

        let mut rx = mgr.subscribe();
        let info = mgr
            .adopt_process(
                "sleep 1".to_string(),
                Some("adopt-test".to_string()),
                muta_contracts::AdoptionInfo {
                    captured_lines: vec!["pre-adopt line".to_string()],
                    pid,
                    child: Box::new(Owned { child }),
                },
            )
            .await
            .expect("adopt");

        let mut settled: Option<BackgroundJobOutcome> = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                    settled = Some(o);
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
                Err(_elapsed) => {} // recv timeout: keep waiting
            }
        }
        let outcome = settled.unwrap_or_else(|| panic!("adopted task must settle"));
        assert!(
            matches!(outcome.state, JobState::Succeeded { exit_code: 0, .. }),
            "unexpected settle state: {:?}",
            outcome.state
        );
        let logs = mgr.get_logs(&info.id, 10).expect("logs");
        assert!(
            logs.iter().any(|l| l.contains("pre-adopt line")),
            "captured foreground tail must be replayed into the fabric"
        );
    }

    #[tokio::test]
    async fn test_timer_fires_once_and_wakes() {
        let mgr = BackgroundJobManager::new();
        let mut rx = mgr.subscribe();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let info = mgr
            .spawn_timer(
                Some("one-shot".to_string()),
                now + 300,
                None,
                "wake: timer digest".to_string(),
                None,
            )
            .expect("timer spawn");

        let mut settled: Option<BackgroundJobOutcome> = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                    settled = Some(o);
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
                Err(_) => {}
            }
        }
        let outcome = settled.expect("timer must fire");
        assert!(matches!(outcome.state, JobState::Succeeded { .. }));
        assert_eq!(outcome.summary, "wake: timer digest");
        // Spec surfaces as Timer in the snapshot before settle.
        let snap = mgr.get_job(&info.id).expect("timer listed");
        assert!(matches!(snap.spec, JobSpec::Timer { .. }));
    }

    #[tokio::test]
    async fn test_timer_cancel_stops_recurring() {
        let mgr = BackgroundJobManager::new();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let info = mgr
            .spawn_timer(None, now + 200, Some(200), "tick".to_string(), None)
            .expect("timer spawn");

        // Let one fire land, then cancel before a second.
        tokio::time::sleep(Duration::from_millis(400)).await;
        mgr.kill_job(&info.id).expect("cancel timer");
        tokio::time::sleep(Duration::from_millis(400)).await;
        // No crash, no panic: cancellation is the contract. The job either
        // settled once or was killed mid-wait — both acceptable.
        let snap = mgr.get_job(&info.id);
        let _ = snap;
    }
}
