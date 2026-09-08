//! Domain contracts for background process and subagent execution.
//!
//! Dual-track execution model (ADR-0145):
//! - **Deterministic Process Jobs**: Shell commands, long test runs, compilation, dev servers.
//!   Managed at the OS level via `tokio::process`, 0 LLM token cost.
//! - **Autonomous Sub-Runner Jobs**: Read-only exploration and analysis runners with isolated contexts.
//!
//! Both tracks report into a unified lifecycle and event notification pipe.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Service interface for dispatching and querying background jobs.
#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait BackgroundJobService: Send + Sync {
    /// Spawn a shell process asynchronously in the background.
    async fn spawn_process(
        &self,
        command: String,
        label: Option<String>,
        cwd: Option<PathBuf>,
        detached: bool,
        timeout: Option<Duration>,
    ) -> Result<BackgroundJobInfo, String>;

    /// Spawn a process with full ADR-0190 control (kind, readiness,
    /// restart, ownership). Defaults to `spawn_process` semantics.
    async fn spawn_process_ex(
        &self,
        command: String,
        label: Option<String>,
        cwd: Option<PathBuf>,
        detached: bool,
        timeout: Option<Duration>,
        kind: JobKind,
        readiness: Option<Readiness>,
        restart: Option<RestartPolicy>,
    ) -> Result<BackgroundJobInfo, String> {
        let _ = (kind, readiness, restart);
        self.spawn_process(command, label, cwd, detached, timeout)
            .await
    }

    /// Record the outcome of a foreground command that hit its sync budget
    /// with the process still alive (ADR-0190 detach-on-budget): the fabric
    /// adopts the already-running child and notifies on completion.
    async fn adopt_process(
        &self,
        _command: String,
        _label: Option<String>,
        _info: AdoptionInfo,
    ) -> Result<BackgroundJobInfo, String> {
        Err("adoption is not supported by this job service".to_string())
    }

    /// Arm a Timer task (ADR-0190): the command runs at `fire_at_ms` and its
    /// completion wakes the owning session via the mailbox.
    async fn spawn_timer(
        &self,
        _label: &str,
        _fire_at_ms: u64,
        _interval_ms: Option<u64>,
        _command: String,
    ) -> Result<BackgroundJobInfo, String> {
        Err("timers are not supported by this job service".to_string())
    }

    /// List all background jobs.
    fn list_jobs(&self) -> Vec<BackgroundJobInfo>;

    /// Get current snapshot of a background job.
    fn get_job(&self, id: &JobId) -> Option<BackgroundJobInfo>;

    /// Retrieve tail logs of a background job.
    fn get_logs(&self, id: &JobId, tail_lines: usize) -> Option<Vec<String>>;

    /// Kill a running background job.
    fn kill_job(&self, id: &JobId) -> Result<(), String>;

    /// Abort all active background jobs.
    fn abort_all(&self);
}

/// Unique identifier for a background job.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct JobId(pub String);

impl JobId {
    pub fn new(prefix: &str) -> Self {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        Self(format!("{}_{}", prefix, &suffix[..8]))
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for JobId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for JobId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Execution kind of a process task (ADR-0190): bounded work that should
/// complete, or a long-lived service where *running is the success state*.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum JobKind {
    /// Bounded work (build, test, one-shot script). Settling `Succeeded` is
    /// the goal; completion wakes the requesting session.
    #[serde(rename = "interactive")]
    #[default]
    Interactive,
    /// Long-lived work (dev server, watcher, daemon). Readiness (ADR-0190
    /// `Ready`) is reported once met; the task never settles while running,
    /// and an unsolicited exit settles `Failed` so the session is woken with
    /// the crash.
    #[serde(rename = "service")]
    Service,
}

/// How a service task declares itself ready (ADR-0190 §D1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "readiness", rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum Readiness {
    /// First output line after spawn (banner, "listening on …"). Default.
    #[serde(rename = "first_output")]
    #[default]
    FirstOutput,
    /// Fixed grace after spawn (ms) — for fully silent services.
    #[serde(rename = "after_ms")]
    AfterMs(u64),
    /// TCP port accepting connections on localhost.
    #[serde(rename = "port_probe")]
    PortProbe { port: u16 },
}

/// Optional automatic-restart policy for service tasks (ADR-0190 §D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct RestartPolicy {
    /// Maximum respawn attempts after unsolicited failure.
    pub max_retries: u32,
    /// Base backoff between attempts, doubled per attempt.
    pub backoff_ms: u64,
}

/// The specification for a background job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum JobSpec {
    /// Deterministic shell execution job.
    Process {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
        #[serde(default)]
        detached: bool,
        /// Execution kind (ADR-0190). Legacy snapshots deserialize as
        /// `Interactive`.
        #[serde(default)]
        task_kind: JobKind,
        /// Service readiness probe; ignored for `Interactive`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        readiness: Option<Readiness>,
        /// Service restart policy; ignored for `Interactive`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restart: Option<RestartPolicy>,
    },
    /// A scheduled wake (ADR-0190 Timer spec): the fabric's clock arm. When
    /// the trigger fires, a wake turn starts with the digest below — the
    /// successor of the retired `/schedule` scheduler, driven by the same
    /// mailbox as every other task event.
    Timer {
        /// ISO-ish local datetime or cron-style descriptor (5-field cron or
        /// `in <duration>`), opaque to the fabric — the caller stores the
        /// human form; the fabric stores the absolute epoch-milliseconds
        /// `fire_at` computed at spawn time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Absolute fire time, Unix-epoch milliseconds.
        fire_at_ms: u64,
        /// For recurring timers: re-arm interval after each fire.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interval_ms: Option<u64>,
        /// The prompt delivered to the wake turn.
        prompt: String,
    },
}

/// Lifecycle state of a background job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "status", rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum JobState {
    Queued,
    Running {
        started_at_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
    /// Service task is ready (ADR-0190): readiness condition met and the
    /// task continues running. Wake-eligible.
    Ready {
        started_at_ms: u64,
        ready_at_ms: u64,
    },
    Succeeded {
        duration_ms: u64,
        exit_code: i32,
    },
    Failed {
        duration_ms: u64,
        exit_code: i32,
        error: String,
    },
    Killed {
        duration_ms: u64,
    },
    TimedOut {
        duration_ms: u64,
    },
}

impl JobState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobState::Succeeded { .. }
                | JobState::Failed { .. }
                | JobState::Killed { .. }
                | JobState::TimedOut { .. }
        )
    }

    pub fn is_running(&self) -> bool {
        matches!(self, JobState::Running { .. })
    }
}

/// Snapshot description of a background job for status polling and UI rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct BackgroundJobInfo {
    pub id: JobId,
    pub spec: JobSpec,
    pub state: JobState,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_output: Option<String>,
}

/// Outcome delivered when a background job completes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct BackgroundJobOutcome {
    pub job_id: JobId,
    pub spec: JobSpec,
    pub state: JobState,
    /// High signal-to-noise summary or tail output.
    pub summary: String,
    /// Path to complete logs on disk (if captured).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<PathBuf>,
}

/// An already-running foreground child the fabric adopts at the sync budget
/// (ADR-0190 detach-on-budget). The tool crate keeps the OS handles and hands
/// them to the runtime service through this bridge.
pub struct AdoptionInfo {
    /// Tail lines already captured by the foreground collector (chronological).
    pub captured_lines: Vec<String>,
    /// Unix pid of the direct child, for kill/monitor setup.
    pub pid: u32,
    /// Raw notification handle: the bridge polls `try_wait` on it. Typed
    /// `Box<dyn ...>` keeps contracts free of `tokio::process`.
    pub child: Box<dyn CrateChildBridge + Send>,
}

/// Platform-neutral handle over a live child process, implemented by the
/// tool/runtime layers so `muta-contracts` (pure domain, ADR-0005) stays
/// I/O-free.
pub trait CrateChildBridge: Send + Sync {
    /// Non-blocking check: `Ok(None)` while running, `Ok(Some(status))` on
    /// exit, `Err` on OS failure.
    fn try_wait(&mut self) -> Result<Option<i32>, String>;
    /// Terminate the process tree (SIGTERM/SIGKILL semantics per platform).
    fn kill(&mut self) -> Result<(), String>;
}
