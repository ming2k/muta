//! Spatiotemporal Aspect Engine implementation (ADR-0183).
//!
//! Orchestrates the five deterministic lifecycle phases of the Agent Harness:
//! 1. `PreFlight`: Fast intent classification and execution tier selection.
//! 2. `TurnIntake`: Silent environmental observation & dynamic reminder injection.
//! 3. `InFlightStream`: Token-stream monitoring & semantic loop interception.
//! 4. `ToolGating`: Anti-derailment gate (repeated call checks + doom guard + token budget).
//! 5. `RoundEol`: Post-round convergence, digest updating, and title synthesis.

use std::path::Path;
use std::sync::Arc;

use muta_contracts::{
    AspectVerdict, ExecutionTier, PreFlightRouteInput, PreFlightRouteOutput, StreamLoopReviewInput,
    StreamLoopVerdict,
};

use crate::cognitive::{CognitivePipeline, HarnessTaskPipeline};

/// Runtime engine managing the five spatiotemporal aspect phases (ADR-0183 / ADR-0211).
#[derive(Clone)]
pub struct AspectEngine {
    harness_tasks: HarnessTaskPipeline,
}

impl AspectEngine {
    /// Create a new aspect engine backed by `harness_tasks`.
    pub fn new(harness_tasks: HarnessTaskPipeline) -> Self {
        Self { harness_tasks }
    }

    /// Access the underlying harness internal task pipeline.
    pub fn harness_tasks(&self) -> &HarnessTaskPipeline {
        &self.harness_tasks
    }

    /// Legacy alias for [`Self::harness_tasks`].
    pub fn cognitive(&self) -> &CognitivePipeline {
        &self.harness_tasks
    }

    // Phase 1: Pre-flight

    /// Evaluate user turn intent and select the appropriate execution tier.
    pub async fn evaluate_pre_flight(
        &self,
        prompt: &str,
        has_active_error: bool,
    ) -> PreFlightRouteOutput {
        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            return PreFlightRouteOutput {
                tier: ExecutionTier::FastDirect,
                enable_thinking: false,
                estimated_complexity: 1,
            };
        }

        self.harness_tasks
            .route_pre_flight(PreFlightRouteInput {
                user_prompt: prompt.to_string(),
                has_active_error,
            })
            .await
    }

    // Phase 2: Turn Intake (ADR-0211: 0ms local synthesis)

    /// Sense workspace environment facts (e.g. git status) and synthesize a dynamic reminder.
    pub async fn evaluate_turn_intake(&self, workspace_cwd: Option<&Path>) -> Option<String> {
        let cwd = workspace_cwd?;
        let (branch, dirty_count, dirty_sample) = detect_workspace_git_summary(cwd).await;
        if dirty_count == 0 {
            return None;
        }

        let sample_str = if dirty_sample.len() > 3 {
            format!(
                "{} and {} more",
                dirty_sample[..3].join(", "),
                dirty_count.saturating_sub(3)
            )
        } else {
            dirty_sample.join(", ")
        };

        let mut reminder = format!(
            "Workspace git status: branch '{branch}', {dirty_count} uncommitted dirty file(s) ({sample_str})."
        );

        // Extract AST symbol hints for dirty files (ADR-0211)
        let mut ast_hints = Vec::new();
        for file_rel in dirty_sample.iter().take(3) {
            let full_path = cwd.join(file_rel);
            let ext = full_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if crate::syntax::SupportedLanguage::from_extension(&ext).is_some()
                && let Ok(content) = std::fs::read_to_string(&full_path)
            {
                let symbols = crate::syntax::extract_symbols(&ext, &content);
                if !symbols.is_empty() {
                    let mut file_syms = format!("  {file_rel}:");
                    for sym in symbols.iter().take(3) {
                        file_syms.push('\n');
                        file_syms.push_str(sym);
                    }
                    ast_hints.push(file_syms);
                }
            }
        }

        if !ast_hints.is_empty() {
            reminder.push_str("\nRecent AST symbols in modified files:\n");
            reminder.push_str(&ast_hints.join("\n"));
        }

        // Active compiler diagnostics (ADR-0211 Decision 5)
        if let Some(compiler_err) = detect_workspace_compiler_error(cwd).await {
            reminder.push_str("\nActive compiler diagnostics:\n");
            reminder.push_str(&compiler_err);
        }

        Some(reminder)
    }

    // Phase 3: In-flight Stream

    /// Confirm or clear an L1 in-flight stream loop candidate.
    pub async fn review_stream_loop(&self, input: StreamLoopReviewInput) -> StreamLoopVerdict {
        self.harness_tasks.review_stream_loop(input).await
    }

    // Phase 4: Tool Gating

    /// Evaluate tool invocation safety against repeated-call ruts and doom thresholds.
    pub fn evaluate_tool_gating(
        &self,
        tool_name: &str,
        is_repeated_rut: bool,
        is_doom_blocked: bool,
    ) -> AspectVerdict<()> {
        if is_repeated_rut {
            return AspectVerdict::Abort {
                reason: format!(
                    "Tool '{}' rejected by RepeatedCallGuard: same call failed repeatedly without convergence.",
                    tool_name
                ),
                error_code: "TOOL_REPEATED_RUT",
            };
        }

        if is_doom_blocked {
            return AspectVerdict::Abort {
                reason: format!(
                    "Tool '{}' blocked by DoomGuard: destructive mutation signature detected.",
                    tool_name
                ),
                error_code: "TOOL_DOOM_MUTATION",
            };
        }

        AspectVerdict::Continue
    }

    // Phase 5: Round EOL

    /// Round-EOL hook (ADR-0183 phase 5): fires at round convergence.
    pub fn fire_round_eol(
        &self,
        _agent: &Arc<crate::Agent>,
        _session: Arc<muta_persistence::SessionStore>,
    ) {
        // Round convergence hook. Session titling runs concurrently on
        // first-prompt admission, removing the legacy EOL digest requirement.
    }
}

/// Helper to inspect local git status for Turn-Intake environment sensing.
async fn detect_workspace_git_summary(cwd: &Path) -> (String, usize, Vec<String>) {
    let branch_output = tokio::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .await;

    let branch = branch_output
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let status_output = tokio::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .await;

    let mut dirty_count = 0;
    let mut dirty_sample = Vec::new();

    if let Ok(output) = status_output
        && output.status.success()
        && let Ok(text) = String::from_utf8(output.stdout)
    {
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            dirty_count += 1;
            if dirty_sample.len() < 5 {
                dirty_sample.push(line.to_string());
            }
        }
    }

    (branch, dirty_count, dirty_sample)
}

/// Helper to detect fast compiler/linter diagnostics for Turn-Intake environment sensing (ADR-0211 Decision 5).
async fn detect_workspace_compiler_error(cwd: &Path) -> Option<String> {
    if cwd.join("Cargo.toml").exists() {
        let child = tokio::process::Command::new("cargo")
            .args(["check", "-q", "--message-format=short"])
            .current_dir(cwd)
            .output();

        let Ok(Ok(output)) =
            tokio::time::timeout(std::time::Duration::from_millis(2000), child).await
        else {
            return None;
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let errors: Vec<&str> = stderr
                .lines()
                .filter(|line| line.contains("error[") || line.contains("error:"))
                .take(3)
                .collect();
            if !errors.is_empty() {
                return Some(errors.join("\n"));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use muta_contracts::{Message, ModelRequest, Provider, Role};
    use std::sync::Arc;

    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(
            &self,
            _req: ModelRequest,
        ) -> Result<muta_contracts::ProviderCompletion, muta_contracts::ProviderError> {
            Ok(muta_contracts::ProviderCompletion::message(Message::new(
                Role::Assistant,
                r#"{"tier":"fast_direct","enable_thinking":false,"estimated_complexity":1}"#,
            )))
        }

        async fn stream_chat(
            &self,
            _req: ModelRequest,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<String, muta_contracts::ProviderError>>,
            muta_contracts::ProviderError,
        > {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn aspect_engine_pre_flight_evaluates() {
        let engine = AspectEngine::new(CognitivePipeline::new(Arc::new(MockProvider)));
        let out = engine.evaluate_pre_flight("hi", false).await;
        assert_eq!(out.tier, ExecutionTier::FastDirect);
    }

    #[test]
    fn aspect_engine_tool_gating_rejects_rut() {
        let engine = AspectEngine::new(CognitivePipeline::new(Arc::new(MockProvider)));
        let verdict = engine.evaluate_tool_gating("bash", true, false);
        assert!(verdict.is_abort());
    }

    #[test]
    fn aspect_engine_tool_gating_continues_clean() {
        let engine = AspectEngine::new(CognitivePipeline::new(Arc::new(MockProvider)));
        let verdict = engine.evaluate_tool_gating("bash", false, false);
        assert!(verdict.is_continue());
    }
}
