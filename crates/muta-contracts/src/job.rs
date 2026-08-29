//! Domain contracts for background process and subagent execution.
//!
//! Dual-track execution model (ADR-0145):
//! - **Deterministic Process Jobs**: Shell commands, long test runs, compilation, dev servers.
//!   Managed at the OS level via `tokio::process`, 0 LLM token cost.
//! - **Autonomous Sub-Runner Jobs**: Read-only exploration and analysis runners with isolated contexts.
//!
//! Both tracks report into a unified lifecycle and event notification pipe.

use std::path::PathBuf;
use std::time::Duration;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Service interface for dispatching and querying background jobs.
#[async_trait]
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
    },
    /// Sub-Runner exploration job.
    Runner {
        description: String,
        role: String,
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
