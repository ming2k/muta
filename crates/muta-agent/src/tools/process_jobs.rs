//! The `process` tool: inspecting and controlling background processes and
//! sub-agent jobs (ADR-0215 — one tool per resource lifecycle).
//!
//! The four former siblings (`process_poll`/`process_logs`/`process_kill`/
//! `process_wait`) all operated on the same `job_id` through the same
//! [`BackgroundJobService`]; they differ only in which service method they
//! call. They are now one tool with an `action` discriminant.

use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

use muta_contracts::{BackgroundJobService, JobId, Tool, ToolAccesses, ToolContext, ToolOutput};
use muta_tool_derive::ToolSchema;

fn background_service(ctx: &ToolContext) -> Option<Arc<dyn BackgroundJobService>> {
    ctx.get::<Arc<dyn BackgroundJobService>>().cloned()
}

#[derive(ToolSchema, Deserialize)]
struct ProcessArgs {
    #[tool(desc = "The job ID returned when the background command or sub-agent was spawned.")]
    job_id: String,
    #[tool(
        desc = "What to do with the job: 'status' (state, runtime, latest output line), 'logs' (recent stdout/stderr lines), 'wait' (block until terminal state, default 60s max 600s), or 'kill' (terminate)."
    )]
    action: String,
    #[tool(desc = "Only for action 'logs': number of tail lines to retrieve (default 50, max 200).")]
    tail_lines: Option<usize>,
    #[tool(desc = "Only for action 'wait': maximum seconds to wait (default 60, max 600).")]
    timeout_seconds: Option<u64>,
}

pub struct ProcessTool {
    service: Option<Arc<dyn BackgroundJobService>>,
}

impl ProcessTool {
    pub fn new(service: Option<Arc<dyn BackgroundJobService>>) -> Self {
        Self { service }
    }

    fn service(&self) -> Result<&Arc<dyn BackgroundJobService>, String> {
        self.service
            .as_ref()
            .ok_or_else(|| "Background job service is unavailable".to_string())
    }

    /// action = "status": the job snapshot (state, runtime, latest output line).
    fn status(&self, job_id: &JobId) -> Result<ToolOutput, String> {
        let service = self.service()?;
        match service.get_job(job_id) {
            Some(info) => {
                let json = serde_json::to_string_pretty(&info).map_err(|e| e.to_string())?;
                Ok(ToolOutput::text(json))
            }
            None => Err(format!("Job not found: {}", job_id.0)),
        }
    }

    /// action = "logs": tail stdout/stderr lines.
    fn logs(&self, job_id: &JobId, tail_lines: Option<usize>) -> Result<ToolOutput, String> {
        let service = self.service()?;
        let tail = tail_lines.unwrap_or(50).clamp(1, 200);

        match service.get_logs(job_id, tail) {
            Some(lines) => {
                if lines.is_empty() {
                    Ok(ToolOutput::text("(no output recorded yet)"))
                } else {
                    Ok(ToolOutput::text(lines.join("\n")))
                }
            }
            None => Err(format!("Job not found: {}", job_id.0)),
        }
    }

    /// action = "kill": terminate the job.
    fn kill(&self, job_id: &JobId) -> Result<ToolOutput, String> {
        let service = self.service()?;
        service.kill_job(job_id)?;
        Ok(ToolOutput::text(format!(
            "Job {} was terminated.",
            job_id.0
        )))
    }

    /// action = "wait": block until a terminal state, then return the tail.
    async fn wait(&self, job_id: &JobId, timeout_seconds: Option<u64>) -> Result<ToolOutput, String> {
        let service = self.service()?;
        let timeout = Duration::from_secs(timeout_seconds.unwrap_or(60).clamp(1, 600));

        let start = std::time::Instant::now();
        loop {
            if let Some(info) = service.get_job(job_id) {
                if info.state.is_terminal() {
                    let logs = service
                        .get_logs(job_id, 20)
                        .unwrap_or_default()
                        .join("\n");
                    let res = serde_json::json!({
                        "job_id": job_id.0,
                        "state": info.state,
                        "tail_logs": logs,
                    });
                    return Ok(ToolOutput::text(
                        serde_json::to_string_pretty(&res).map_err(|e| e.to_string())?,
                    ));
                }
            } else {
                return Err(format!("Job not found: {}", job_id.0));
            }

            if start.elapsed() >= timeout {
                return Err(format!("Timed out waiting for job {}", job_id.0));
            }

            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

#[async_trait]
impl Tool for ProcessTool {
    fn name(&self) -> &str {
        "process"
    }

    fn description(&self) -> &str {
        "Inspect or control a background process or sub-agent job. One tool, one `action`: 'status' (state, runtime, latest output line), 'logs' (recent stdout/stderr), 'wait' (block until the job finishes and return its final outcome), or 'kill' (terminate it)."
    }

    fn parameters(&self) -> serde_json::Value {
        ProcessArgs::parameters_schema()
    }

    fn accesses(&self, _args: &str) -> ToolAccesses {
        ToolAccesses::none()
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let output = self.call_structured(arguments).await?;
        Ok(output.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        let args: ProcessArgs = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        let job_id = JobId(args.job_id);

        match args.action.as_str() {
            "status" => self.status(&job_id),
            "logs" => self.logs(&job_id, args.tail_lines),
            "wait" => self.wait(&job_id, args.timeout_seconds).await,
            "kill" => self.kill(&job_id),
            other => Err(format!(
                "Unknown action '{other}'. Use 'status', 'logs', 'wait', or 'kill'."
            )),
        }
    }
}

muta_contracts::register_tool!(ProcessFactory => |ctx| ProcessTool {
    service: background_service(ctx),
});

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::{BackgroundJobInfo, JobKind, JobSpec, JobState};
    use std::sync::Mutex;

    struct MockJobService {
        jobs: Mutex<std::collections::HashMap<JobId, BackgroundJobInfo>>,
        logs: Mutex<std::collections::HashMap<JobId, Vec<String>>>,
    }

    impl MockJobService {
        fn new() -> Self {
            Self {
                jobs: Mutex::new(std::collections::HashMap::new()),
                logs: Mutex::new(std::collections::HashMap::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl BackgroundJobService for MockJobService {
        async fn spawn_process(
            &self,
            command: String,
            label: Option<String>,
            cwd: Option<std::path::PathBuf>,
            detached: bool,
            _timeout: Option<Duration>,
        ) -> Result<BackgroundJobInfo, String> {
            let id = JobId::new("mock");
            let info = BackgroundJobInfo {
                id: id.clone(),
                spec: JobSpec::Process {
                    command,
                    label,
                    cwd,
                    detached,
                    task_kind: JobKind::default(),
                    readiness: None,
                    restart: None,
                },
                state: JobState::Running {
                    started_at_ms: 1000,
                    pid: Some(1234),
                },
                created_at_ms: 1000,
                completed_at_ms: None,
                latest_output: Some("mock output".into()),
            };
            self.jobs.lock().unwrap().insert(id.clone(), info.clone());
            self.logs
                .lock()
                .unwrap()
                .insert(id, vec!["line 1".into(), "line 2".into()]);
            Ok(info)
        }

        fn list_jobs(&self) -> Vec<BackgroundJobInfo> {
            self.jobs.lock().unwrap().values().cloned().collect()
        }

        fn get_job(&self, id: &JobId) -> Option<BackgroundJobInfo> {
            self.jobs.lock().unwrap().get(id).cloned()
        }

        fn get_logs(&self, id: &JobId, tail_lines: usize) -> Option<Vec<String>> {
            self.logs
                .lock()
                .unwrap()
                .get(id)
                .map(|l| l.iter().take(tail_lines).cloned().collect())
        }

        fn kill_job(&self, id: &JobId) -> Result<(), String> {
            let mut guard = self.jobs.lock().unwrap();
            if let Some(j) = guard.get_mut(id) {
                j.state = JobState::Killed { duration_ms: 500 };
                Ok(())
            } else {
                Err("Not found".into())
            }
        }

        fn abort_all(&self) {}
    }

    #[tokio::test]
    async fn test_process_tool_actions() {
        let service: Arc<dyn BackgroundJobService> = Arc::new(MockJobService::new());
        let info = service
            .spawn_process("test-cmd".into(), Some("test".into()), None, false, None)
            .await
            .unwrap();
        let tool = ProcessTool::new(Some(Arc::clone(&service)));

        // 1. status
        let status_out = tool
            .call(&serde_json::json!({ "job_id": info.id.0, "action": "status" }).to_string())
            .await
            .unwrap();
        assert!(status_out.contains(&info.id.0));

        // 2. logs
        let logs_out = tool
            .call(
                &serde_json::json!({ "job_id": info.id.0, "action": "logs", "tail_lines": 5 })
                    .to_string(),
            )
            .await
            .unwrap();
        assert!(logs_out.contains("line 1"));

        // 3. kill
        let kill_out = tool
            .call(&serde_json::json!({ "job_id": info.id.0, "action": "kill" }).to_string())
            .await
            .unwrap();
        assert!(kill_out.contains("terminated"));

        // 4. status after kill reports the terminal state
        let status_after = tool
            .call(&serde_json::json!({ "job_id": info.id.0, "action": "status" }).to_string())
            .await
            .unwrap();
        assert!(status_after.contains("killed"));
    }

    #[tokio::test]
    async fn test_process_tool_rejects_unknown_action() {
        let service: Arc<dyn BackgroundJobService> = Arc::new(MockJobService::new());
        let tool = ProcessTool::new(Some(service));
        let err = tool
            .call(&serde_json::json!({ "job_id": "x", "action": "poll" }).to_string())
            .await
            .unwrap_err();
        assert!(err.contains("Unknown action"));
    }

    #[tokio::test]
    async fn test_process_tool_wait_until_terminal() {
        let service: Arc<dyn BackgroundJobService> = Arc::new(MockJobService::new());
        let info = service
            .spawn_process("test-cmd".into(), None, None, false, None)
            .await
            .unwrap();
        service.kill_job(&info.id).unwrap();
        let tool = ProcessTool::new(Some(service));
        let out = tool
            .call(
                &serde_json::json!({ "job_id": info.id.0, "action": "wait", "timeout_seconds": 2 })
                    .to_string(),
            )
            .await
            .unwrap();
        assert!(out.contains("tail_logs"));
    }
}
