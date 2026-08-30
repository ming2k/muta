//! Background job manager for long-running process commands and asynchronous sub-runners.
//!
//! Dual-track execution model:
//! - **Track A (Process Jobs)**: OS-level subprocesses (`tokio::process`) capturing output
//!   into in-memory ring buffers and disk logs, with 0 LLM token cost.
//! - **Track B (Sub-Runner Jobs)**: Asynchronous isolated exploration subagents.
//!
//! Emits live progress events and delivers completed outcomes to the session mailbox.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{broadcast, mpsc};

use muta_contracts::{BackgroundJobInfo, BackgroundJobOutcome, JobId, JobSpec, JobState};

const DEFAULT_RING_BUFFER_CAPACITY: usize = 500;

struct JobEntry {
    info: BackgroundJobInfo,
    ring_buffer: VecDeque<String>,
    #[allow(dead_code)]
    log_file_path: Option<PathBuf>,
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
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
    Progress { job_id: JobId, line: String },
    Completed(BackgroundJobOutcome),
}

impl Default for BackgroundJobManager {
    fn default() -> Self {
        Self::new()
    }
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
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_process(
        &self,
        command: String,
        label: Option<String>,
        cwd: Option<PathBuf>,
        workspace_root: &Path,
        additional_roots: &[PathBuf],
        detached: bool,
        timeout: Option<Duration>,
    ) -> Result<BackgroundJobInfo, String> {
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
                    log_file_path: Some(log_file_path.clone()),
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

            let (line_tx, mut line_rx) = mpsc::unbounded_channel::<String>();

            // Read stdout stream
            if let Some(out) = stdout {
                let tx = line_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(out).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if tx.send(line).is_err() {
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
                        if tx.send(line).is_err() {
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

    /// Spawn an asynchronous Sub-Runner exploration job.
    pub fn spawn_runner_job(
        &self,
        runner_id: String,
        description: String,
        role: String,
        prompt: String,
    ) -> (JobId, tokio::sync::oneshot::Receiver<()>) {
        let job_id = JobId(runner_id);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let info = BackgroundJobInfo {
            id: job_id.clone(),
            spec: JobSpec::Runner {
                description: description.clone(),
                role: role.clone(),
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
                    log_file_path: None,
                    cancel_tx: Some(cancel_tx),
                    #[cfg(unix)]
                    pid: None,
                },
            );
        }

        let _ = self.event_tx.send(BackgroundJobEvent::Started(info));
        (job_id, cancel_rx)
    }

    /// Report completion of a Sub-Runner job.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_runner_job(
        &self,
        job_id: JobId,
        description: String,
        role: String,
        prompt: String,
        duration_ms: u64,
        summary: String,
        success: bool,
    ) {
        let spec = JobSpec::Runner {
            description,
            role,
            prompt,
        };
        let state = if success {
            JobState::Succeeded {
                duration_ms,
                exit_code: 0,
            }
        } else {
            JobState::Failed {
                duration_ms,
                exit_code: 1,
                error: "Runner exploration failed".to_string(),
            }
        };

        self.finish_job_with_summary(job_id, spec, state, None, summary);
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

        {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(entry) = guard.get_mut(&job_id) {
                entry.info.state = state.clone();
                entry.info.completed_at_ms = Some(now_ms);
                entry.cancel_tx = None;
            }
        }

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
        let _ = self.outcome_tx.send(outcome);
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
}

impl SessionJobService {
    pub fn new(
        manager: BackgroundJobManager,
        env: Arc<dyn muta_contracts::ExecutionEnvironment>,
    ) -> Self {
        Self { manager, env }
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
                label,
                cwd,
                self.env.workspace_root(),
                &roots,
                detached,
                timeout,
            )
            .await
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
        let mgr = BackgroundJobManager::new();
        let ws = std::env::temp_dir();
        let roots = vec![];

        let mut rx = mgr.subscribe();

        let info = mgr
            .spawn_process(
                "echo 'hello from background'".to_string(),
                Some("test-echo".to_string()),
                None,
                &ws,
                &roots,
                false,
                Some(Duration::from_secs(5)),
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
        let mgr = BackgroundJobManager::new();
        let ws = std::env::temp_dir();
        let roots = vec![];

        let mut rx = mgr.subscribe();

        let info = mgr
            .spawn_process(
                "sleep 10".to_string(),
                Some("test-sleep".to_string()),
                None,
                &ws,
                &roots,
                false,
                Some(Duration::from_secs(10)),
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
}
