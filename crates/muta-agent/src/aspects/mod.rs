//! Spatiotemporal Aspect Engine implementation (ADR-0183).
//!
//! Orchestrates the five deterministic lifecycle phases of the Agent Harness:
//! 1. `PreFlight`: Fast intent classification and execution tier selection.
//! 2. `TurnIntake`: Silent environmental observation & dynamic reminder injection.
//! 3. `InFlightStream`: Token-stream monitoring & semantic loop interception.
//! 4. `ToolGating`: Anti-derailment gate (repeated call checks + doom guard + token budget).
//! 5. `RoundEol`: Post-round convergence, digest updating, and title synthesis.

use std::path::Path;

use muta_contracts::{
    AspectVerdict, EnvironmentReminderOutput, EnvironmentSensorInput,
    ExecutionTier, PreFlightRouteInput, PreFlightRouteOutput, StreamLoopReviewInput,
    StreamLoopVerdict,
};

use crate::cognitive::CognitivePipeline;

/// Runtime engine managing the five spatiotemporal aspect phases.
#[derive(Clone)]
pub struct AspectEngine {
    cognitive: CognitivePipeline,
}

impl AspectEngine {
    /// Create a new aspect engine backed by `cognitive`.
    pub fn new(cognitive: CognitivePipeline) -> Self {
        Self { cognitive }
    }

    /// Access the underlying cognitive pipeline.
    pub fn cognitive(&self) -> &CognitivePipeline {
        &self.cognitive
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

        self.cognitive
            .route_pre_flight(PreFlightRouteInput {
                user_prompt: prompt.to_string(),
                has_active_error,
            })
            .await
    }

    // Phase 2: Turn Intake

    /// Sense workspace environment facts (e.g. git status) and synthesize a dynamic reminder.
    pub async fn evaluate_turn_intake(
        &self,
        workspace_cwd: Option<&Path>,
    ) -> Option<String> {
        let cwd = workspace_cwd?;
        let (branch, dirty_count, dirty_sample) = detect_workspace_git_summary(cwd).await;
        if dirty_count == 0 {
            return None;
        }

        let output: EnvironmentReminderOutput = self
            .cognitive
            .sense_environment(EnvironmentSensorInput {
                active_branch: branch,
                dirty_files_count: dirty_count,
                dirty_files_sample: dirty_sample,
                compiler_error: None,
            })
            .await;

        output.reminder_text
    }

    // Phase 3: In-flight Stream

    /// Confirm or clear an L1 in-flight stream loop candidate.
    pub async fn review_stream_loop(&self, input: StreamLoopReviewInput) -> StreamLoopVerdict {
        self.cognitive.review_stream_loop(input).await
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

    /// Asynchronously distill working memory and generate/revise session digest.
    pub async fn process_round_eol(
        &self,
        excerpt: String,
        previous_digest: Option<String>,
    ) -> Option<muta_contracts::SessionDigest> {
        self.cognitive
            .generate_digest(muta_contracts::SessionDigestInput {
                excerpt,
                previous: previous_digest,
            })
            .await
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use muta_contracts::{Message, ModelRequest, Provider, Role};
    use async_trait::async_trait;

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
