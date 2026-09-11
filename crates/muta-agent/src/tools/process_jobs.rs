//! The `process` tool: inspecting and controlling background processes and
//! sub-agent jobs (ADR-0215 — one tool per resource lifecycle).
//!
//! The four former siblings (`process_poll`/`process_logs`/`process_kill`/
//! `process_wait`) all operated on the same `job_id` through the same
//! [`BackgroundJobService`]; they differ only in which service method they
//! call. They are now one tool with an `action` discriminant.
//!
//! `wait` is also the *acknowledgment* path (ADR-0234): it returns the settled
//! result and claims the retained delivery, so a later automatic continuation
//! for the same settlement does not repeat work the model has already seen.
//! `status` and `logs` deliberately do not claim — inspecting progress is not
//! accepting the outcome.

use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

use muta_contracts::{BackgroundJobService, JobId, Tool, ToolAccesses, ToolContext, ToolOutput};
use muta_tool_derive::ToolSchema;

fn background_service(ctx: &ToolContext) -> Option<Arc<dyn BackgroundJobService>> {
    ctx.get::<Arc<dyn BackgroundJobService>>().cloned()
}

/// Whether a snapshot describes a long-lived service that is still up.
///
/// A service's contract is "running is the success state" (ADR-0190), so only a
/// `Running`/`Ready` state on a `JobKind::Service` spec counts — a service that
/// has exited *has* settled and is waitable like anything else.
fn is_running_service(info: &muta_contracts::BackgroundJobInfo) -> bool {
    matches!(
        &info.spec,
        muta_contracts::JobSpec::Process {
            task_kind: muta_contracts::JobKind::Service,
            ..
        }
    ) && !info.state.is_terminal()
}

#[derive(ToolSchema, Deserialize)]
struct ProcessArgs {
    #[tool(desc = "The job ID returned when the background command or sub-agent was spawned.")]
    job_id: String,
    #[tool(
        desc = "What to do with the job: 'status' (state, runtime, latest output line), 'logs' (recent stdout/stderr lines), 'wait' (block until terminal state, default 60s max 600s), or 'kill' (terminate)."
    )]
    action: String,
    #[tool(
        desc = "Only for action 'logs': number of tail lines to retrieve (default 50, max 200)."
    )]
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

    /// action = "status": the job snapshot (state, runtime, latest output line)
    /// plus its settled result once it has finished (ADR-0234).
    fn status(&self, job_id: &JobId) -> Result<ToolOutput, String> {
        let service = self.service()?;
        match service.get_job(job_id) {
            Some(info) => {
                let settled = service.settled_result(job_id);
                let json = serde_json::json!({
                    "job_id": info.id.0,
                    "spec": info.spec,
                    "state": info.state,
                    "created_at_ms": info.created_at_ms,
                    "completed_at_ms": info.completed_at_ms,
                    "latest_output": info.latest_output,
                    // Present once the job has settled, regardless of whether
                    // its automatic delivery was already claimed.
                    "summary": settled
                        .as_ref()
                        .map(|o| o.summary.clone())
                        .filter(|s| !s.trim().is_empty()),
                    "log_path": settled
                        .as_ref()
                        .and_then(|o| o.log_path.as_ref())
                        .map(|p| p.display().to_string()),
                });
                Ok(ToolOutput::text(
                    serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?,
                ))
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

    /// action = "wait": block until a terminal state, then return the settled
    /// result (state, summary, log path, tail) and claim its delivery
    /// (ADR-0234).
    ///
    /// A running *service* is refused immediately instead of blocking for the
    /// whole timeout: a service is long-lived by contract ("running is the
    /// success state"), so waiting for it to settle is a category error, and
    /// burning the budget to end in a timeout error teaches nothing.
    async fn wait(
        &self,
        job_id: &JobId,
        timeout_seconds: Option<u64>,
    ) -> Result<ToolOutput, String> {
        let service = self.service()?;
        let timeout = Duration::from_secs(timeout_seconds.unwrap_or(60).clamp(1, 600));

        let start = std::time::Instant::now();
        loop {
            if let Some(info) = service.get_job(job_id) {
                if info.state.is_terminal() {
                    let logs = service.get_logs(job_id, 20).unwrap_or_default().join("\n");
                    // Claim before returning: the caller is about to see this
                    // settlement, so it must not be delivered to the model
                    // again by a later continuation.
                    let claimed = service.claim_outcomes(job_id);
                    // The entry's own record is the fallback: a settlement that
                    // was already collected still has to be reportable.
                    let settled = service.settled_result(job_id);
                    let latest = claimed.last().cloned().or_else(|| settled.clone());
                    let res = serde_json::json!({
                        "job_id": job_id.0,
                        "state": info.state,
                        "summary": latest
                            .as_ref()
                            .map(|o| o.summary.clone())
                            .filter(|s| !s.trim().is_empty()),
                        "log_path": latest
                            .as_ref()
                            .and_then(|o| o.log_path.as_ref())
                            .map(|p| p.display().to_string()),
                        "tail_logs": logs,
                        "deliveries_claimed": claimed.len(),
                    });
                    return Ok(ToolOutput::text(
                        serde_json::to_string_pretty(&res).map_err(|e| e.to_string())?,
                    ));
                }

                if is_running_service(&info) {
                    let res = serde_json::json!({
                        "job_id": job_id.0,
                        "state": info.state,
                        "status": "service_still_running",
                        "message": "This job is a service: it is expected to keep running, so there is no terminal state to wait for and no result was collected. Use action 'status' or 'logs' to inspect it, or action 'kill' to stop it.",
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
        "Inspect or control a background process or sub-agent job. One tool, one `action`: 'status' (state, runtime, latest output line), 'logs' (recent stdout/stderr), 'wait' (block until the job finishes, return its settled result — state, summary, log path, tail — and mark that result as collected), or 'kill' (terminate a job that is still running)."
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
        /// Retained but unclaimed deliveries (ADR-0234), so the wait/claim
        /// contract is observable without a real job fabric.
        pending: Mutex<std::collections::HashMap<JobId, Vec<muta_contracts::BackgroundJobOutcome>>>,
        /// The job's own readable settlement (ADR-0234), independent of claims.
        settled: Mutex<std::collections::HashMap<JobId, muta_contracts::BackgroundJobOutcome>>,
    }

    impl MockJobService {
        fn new() -> Self {
            Self {
                jobs: Mutex::new(std::collections::HashMap::new()),
                logs: Mutex::new(std::collections::HashMap::new()),
                pending: Mutex::new(std::collections::HashMap::new()),
                settled: Mutex::new(std::collections::HashMap::new()),
            }
        }

        /// Retain an outcome for delivery *and* record it as the job's settled
        /// result, mirroring the real manager's `deliver_outcome`.
        fn retain(&self, outcome: muta_contracts::BackgroundJobOutcome) {
            self.settled
                .lock()
                .unwrap()
                .insert(outcome.job_id.clone(), outcome.clone());
            self.pending
                .lock()
                .unwrap()
                .entry(outcome.job_id.clone())
                .or_default()
                .push(outcome);
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

        fn claim_outcomes(&self, id: &JobId) -> Vec<muta_contracts::BackgroundJobOutcome> {
            self.pending.lock().unwrap().remove(id).unwrap_or_default()
        }

        fn settled_result(&self, id: &JobId) -> Option<muta_contracts::BackgroundJobOutcome> {
            self.settled.lock().unwrap().get(id).cloned()
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
        assert!(out.contains("deliveries_claimed"));
    }

    /// ADR-0234: `wait` is the acknowledgment path — it returns the settled
    /// summary and claims the retained delivery exactly once, so a later
    /// automatic delivery for the same settlement cannot repeat work the model
    /// has already seen. `status` is not an acknowledgment, and a re-collected
    /// result must still be reportable.
    #[tokio::test]
    async fn test_process_tool_wait_returns_and_claims_the_settled_result() {
        let mock = Arc::new(MockJobService::new());
        let service: Arc<dyn BackgroundJobService> =
            Arc::clone(&mock) as Arc<dyn BackgroundJobService>;
        let info = service
            .spawn_process("test-cmd".into(), None, None, false, None)
            .await
            .unwrap();
        service.kill_job(&info.id).unwrap();
        mock.retain(muta_contracts::BackgroundJobOutcome {
            job_id: info.id.clone(),
            spec: info.spec.clone(),
            state: JobState::Killed { duration_ms: 500 },
            summary: "harness-authored digest".to_string(),
            log_path: Some(std::path::PathBuf::from("/tmp/muta-jobs/job.log")),
        });

        let tool = ProcessTool::new(Some(Arc::clone(&service)));

        // Inspecting state reports the settled result and is NOT an
        // acknowledgment: the delivery stays unclaimed for `wait`.
        let status = tool
            .call(&serde_json::json!({ "job_id": info.id.0, "action": "status" }).to_string())
            .await
            .unwrap();
        assert!(
            status.contains("harness-authored digest"),
            "status must surface the settled summary: {status}"
        );

        let out = tool
            .call(
                &serde_json::json!({ "job_id": info.id.0, "action": "wait", "timeout_seconds": 2 })
                    .to_string(),
            )
            .await
            .unwrap();
        assert!(
            out.contains("harness-authored digest"),
            "the settled summary must reach the caller: {out}"
        );
        assert!(out.contains("\"deliveries_claimed\": 1"), "{out}");
        assert!(
            service.claim_outcomes(&info.id).is_empty(),
            "the delivery is claimed exactly once"
        );

        // Re-collecting reports nothing new to deliver, but must not lose the
        // result: the job's own settlement stays readable.
        let again = tool
            .call(
                &serde_json::json!({ "job_id": info.id.0, "action": "wait", "timeout_seconds": 2 })
                    .to_string(),
            )
            .await
            .unwrap();
        assert!(again.contains("\"deliveries_claimed\": 0"), "{again}");
        assert!(
            again.contains("harness-authored digest"),
            "a re-collected settlement must still report its summary: {again}"
        );
    }

    /// ADR-0234: a running service has no terminal state by contract, so `wait`
    /// must refuse immediately instead of blocking out the whole budget and
    /// ending in a timeout error that teaches nothing.
    #[tokio::test]
    async fn test_process_tool_wait_refuses_a_running_service_immediately() {
        let mock = Arc::new(MockJobService::new());
        let service: Arc<dyn BackgroundJobService> =
            Arc::clone(&mock) as Arc<dyn BackgroundJobService>;
        let id = JobId("svc_1".to_string());
        mock.jobs.lock().unwrap().insert(
            id.clone(),
            BackgroundJobInfo {
                id: id.clone(),
                spec: JobSpec::Process {
                    command: "dev-server".into(),
                    label: Some("dev-server".into()),
                    cwd: None,
                    detached: false,
                    task_kind: JobKind::Service,
                    readiness: None,
                    restart: None,
                },
                state: JobState::Ready {
                    started_at_ms: 1,
                    ready_at_ms: 2,
                },
                created_at_ms: 1,
                completed_at_ms: None,
                latest_output: Some("listening on :3000".into()),
            },
        );

        let tool = ProcessTool::new(Some(Arc::clone(&service)));
        let start = std::time::Instant::now();
        let out = tool
            .call(
                // A generous budget: the refusal must not consume it.
                &serde_json::json!({ "job_id": id.0, "action": "wait", "timeout_seconds": 600 })
                    .to_string(),
            )
            .await
            .expect("a running service returns guidance, not an error");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the return must be immediate, took {:?}",
            start.elapsed()
        );
        assert!(out.contains("service_still_running"), "{out}");
        assert!(
            out.contains("no result was collected"),
            "the refusal must state that nothing was collected: {out}"
        );
    }

    /// A service that has exited *has* settled, so it stays waitable like any
    /// other job.
    #[tokio::test]
    async fn test_process_tool_wait_still_waits_for_a_service_that_died() {
        let mock = Arc::new(MockJobService::new());
        let service: Arc<dyn BackgroundJobService> =
            Arc::clone(&mock) as Arc<dyn BackgroundJobService>;
        let id = JobId("svc_dead".to_string());
        mock.jobs.lock().unwrap().insert(
            id.clone(),
            BackgroundJobInfo {
                id: id.clone(),
                spec: JobSpec::Process {
                    command: "dev-server".into(),
                    label: None,
                    cwd: None,
                    detached: false,
                    task_kind: JobKind::Service,
                    readiness: None,
                    restart: None,
                },
                state: JobState::Failed {
                    duration_ms: 900,
                    exit_code: 1,
                    error: "Service crashed".into(),
                },
                created_at_ms: 1,
                completed_at_ms: Some(901),
                latest_output: None,
            },
        );
        mock.retain(muta_contracts::BackgroundJobOutcome {
            job_id: id.clone(),
            spec: mock.jobs.lock().unwrap()[&id].spec.clone(),
            state: JobState::Failed {
                duration_ms: 900,
                exit_code: 1,
                error: "Service crashed".into(),
            },
            summary: "service crashed after 900ms".to_string(),
            log_path: None,
        });

        let tool = ProcessTool::new(Some(Arc::clone(&service)));
        let out = tool
            .call(
                &serde_json::json!({ "job_id": id.0, "action": "wait", "timeout_seconds": 2 })
                    .to_string(),
            )
            .await
            .unwrap();
        assert!(out.contains("service crashed after 900ms"), "{out}");
        assert!(out.contains("\"deliveries_claimed\": 1"), "{out}");
    }
}
