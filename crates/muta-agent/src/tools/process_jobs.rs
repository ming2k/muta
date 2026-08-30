//! Built-in tools for inspecting and controlling background processes and sub-runners.

use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

use muta_contracts::{BackgroundJobService, JobId, Tool, ToolAccesses, ToolContext, ToolOutput};
use muta_tool_derive::ToolSchema;

fn background_service(ctx: &ToolContext) -> Option<Arc<dyn BackgroundJobService>> {
    ctx.get::<Arc<dyn BackgroundJobService>>().cloned()
}

// ── process_poll ──

#[derive(ToolSchema, Deserialize)]
struct ProcessPollArgs {
    #[tool(desc = "The job ID returned when the background command or runner was spawned.")]
    job_id: String,
}

pub struct ProcessPollTool {
    service: Option<Arc<dyn BackgroundJobService>>,
}

impl ProcessPollTool {
    pub fn new(service: Option<Arc<dyn BackgroundJobService>>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Tool for ProcessPollTool {
    fn name(&self) -> &str {
        "process_poll"
    }

    fn description(&self) -> &str {
        "Check the status, runtime, and latest output line of a background process or sub-runner job."
    }

    fn parameters(&self) -> serde_json::Value {
        ProcessPollArgs::parameters_schema()
    }

    fn accesses(&self, _args: &str) -> ToolAccesses {
        ToolAccesses::none()
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let output = self.call_structured(arguments).await?;
        Ok(output.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        let args: ProcessPollArgs = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        let service = self
            .service
            .as_ref()
            .ok_or("Background job service is unavailable")?;
        let job_id = JobId(args.job_id);

        match service.get_job(&job_id) {
            Some(info) => {
                let json = serde_json::to_string_pretty(&info).map_err(|e| e.to_string())?;
                Ok(ToolOutput::text(json))
            }
            None => Err(format!("Job not found: {}", job_id.0)),
        }
    }
}

muta_contracts::register_tool!(ProcessPollFactory => |ctx| ProcessPollTool {
    service: background_service(ctx),
});

// ── process_logs ──

#[derive(ToolSchema, Deserialize)]
struct ProcessLogsArgs {
    #[tool(desc = "The job ID to fetch logs for.")]
    job_id: String,
    #[tool(desc = "Number of tail lines to retrieve (default 50, max 200).")]
    tail_lines: Option<usize>,
}

pub struct ProcessLogsTool {
    service: Option<Arc<dyn BackgroundJobService>>,
}

impl ProcessLogsTool {
    pub fn new(service: Option<Arc<dyn BackgroundJobService>>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Tool for ProcessLogsTool {
    fn name(&self) -> &str {
        "process_logs"
    }

    fn description(&self) -> &str {
        "Retrieve recent stdout/stderr output lines for a background process job."
    }

    fn parameters(&self) -> serde_json::Value {
        ProcessLogsArgs::parameters_schema()
    }

    fn accesses(&self, _args: &str) -> ToolAccesses {
        ToolAccesses::none()
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let output = self.call_structured(arguments).await?;
        Ok(output.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        let args: ProcessLogsArgs = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        let service = self
            .service
            .as_ref()
            .ok_or("Background job service is unavailable")?;
        let job_id = JobId(args.job_id);
        let tail = args.tail_lines.unwrap_or(50).clamp(1, 500);

        match service.get_logs(&job_id, tail) {
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
}

muta_contracts::register_tool!(ProcessLogsFactory => |ctx| ProcessLogsTool {
    service: background_service(ctx),
});

// ── process_kill ──

#[derive(ToolSchema, Deserialize)]
struct ProcessKillArgs {
    #[tool(desc = "The job ID to terminate.")]
    job_id: String,
}

pub struct ProcessKillTool {
    service: Option<Arc<dyn BackgroundJobService>>,
}

impl ProcessKillTool {
    pub fn new(service: Option<Arc<dyn BackgroundJobService>>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Tool for ProcessKillTool {
    fn name(&self) -> &str {
        "process_kill"
    }

    fn description(&self) -> &str {
        "Terminate an active background process or sub-runner job."
    }

    fn parameters(&self) -> serde_json::Value {
        ProcessKillArgs::parameters_schema()
    }

    fn accesses(&self, _args: &str) -> ToolAccesses {
        ToolAccesses::none()
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let output = self.call_structured(arguments).await?;
        Ok(output.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        let args: ProcessKillArgs = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        let service = self
            .service
            .as_ref()
            .ok_or("Background job service is unavailable")?;
        let job_id = JobId(args.job_id);

        service.kill_job(&job_id)?;
        Ok(ToolOutput::text(format!(
            "Job {} was terminated.",
            job_id.0
        )))
    }
}

muta_contracts::register_tool!(ProcessKillFactory => |ctx| ProcessKillTool {
    service: background_service(ctx),
});

// ── process_wait ──

#[derive(ToolSchema, Deserialize)]
struct ProcessWaitArgs {
    #[tool(desc = "The job ID to wait for.")]
    job_id: String,
    #[tool(desc = "Maximum seconds to wait (default 60, max 600).")]
    timeout_seconds: Option<u64>,
}

pub struct ProcessWaitTool {
    service: Option<Arc<dyn BackgroundJobService>>,
}

impl ProcessWaitTool {
    pub fn new(service: Option<Arc<dyn BackgroundJobService>>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Tool for ProcessWaitTool {
    fn name(&self) -> &str {
        "process_wait"
    }

    fn description(&self) -> &str {
        "Wait for a background job to finish and return its final outcome."
    }

    fn parameters(&self) -> serde_json::Value {
        ProcessWaitArgs::parameters_schema()
    }

    fn accesses(&self, _args: &str) -> ToolAccesses {
        ToolAccesses::none()
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let output = self.call_structured(arguments).await?;
        Ok(output.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        let args: ProcessWaitArgs = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        let service = self
            .service
            .as_ref()
            .ok_or("Background job service is unavailable")?;
        let job_id = JobId(args.job_id);
        let timeout = Duration::from_secs(args.timeout_seconds.unwrap_or(60).clamp(1, 600));

        let start = std::time::Instant::now();
        loop {
            if let Some(info) = service.get_job(&job_id) {
                if info.state.is_terminal() {
                    let logs = service.get_logs(&job_id, 20).unwrap_or_default().join("\n");
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

muta_contracts::register_tool!(ProcessWaitFactory => |ctx| ProcessWaitTool {
    service: background_service(ctx),
});

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::{BackgroundJobInfo, JobSpec, JobState};
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
    async fn test_process_tools() {
        let service: Arc<dyn BackgroundJobService> = Arc::new(MockJobService::new());
        let info = service
            .spawn_process("test-cmd".into(), Some("test".into()), None, false, None)
            .await
            .unwrap();

        // 1. process_poll
        let poll_tool = ProcessPollTool::new(Some(Arc::clone(&service)));
        let poll_out = poll_tool
            .call(&serde_json::json!({ "job_id": info.id.0 }).to_string())
            .await
            .unwrap();
        assert!(poll_out.contains(&info.id.0));

        // 2. process_logs
        let logs_tool = ProcessLogsTool::new(Some(Arc::clone(&service)));
        let logs_out = logs_tool
            .call(&serde_json::json!({ "job_id": info.id.0, "tail_lines": 5 }).to_string())
            .await
            .unwrap();
        assert!(logs_out.contains("line 1"));

        // 3. process_kill
        let kill_tool = ProcessKillTool::new(Some(Arc::clone(&service)));
        let kill_out = kill_tool
            .call(&serde_json::json!({ "job_id": info.id.0 }).to_string())
            .await
            .unwrap();
        assert!(kill_out.contains("terminated"));

        // 4. check poll after kill
        let poll_after = poll_tool
            .call(&serde_json::json!({ "job_id": info.id.0 }).to_string())
            .await
            .unwrap();
        assert!(poll_after.contains("killed"));
    }
}
