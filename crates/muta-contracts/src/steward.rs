//! Steward contracts: typed cognitive infrastructure tasks for the Agent Harness.
//!
//! # Why Steward exists
//!
//! `Master` and `Runner` are *actors* serving operational production (user conversations,
//! autonomous coding missions, tool execution). `Supervisor` is the *fleet coordinator*
//! for multi-session and daemon-level mesh routing.
//!
//! In contrast, `Steward` is the *harness cognitive attendant* — out-of-band, stateless,
//! zero-tool, single-shot cognitive transformations that serve the Agent Harness:
//! - Semantic loop and doom detection with prescriptive remediation
//! - Sanity and safety checks on critical payloads
//! - Context projection and transcript compaction
//! - Session titling and metadata extraction
//!
//! All Steward tasks implement [`StewardTask`], ensuring strong typing, JSON-schema
//! extraction, and fail-open resilience.

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Supported model tiers for Steward tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StewardModelPreference {
    /// Use the lightest, fastest, cost-efficient model (default for sentinels/titlers).
    FlashLite,
    /// Use standard fast model.
    Flash,
    /// Inherit the session's active primary model.
    InheritPrimary,
}

impl Default for StewardModelPreference {
    fn default() -> Self {
        Self::FlashLite
    }
}

/// Core trait for typed cognitive infrastructure tasks executed by the Harness Steward.
#[async_trait]
pub trait StewardTask: Send + Sync {
    /// Task input payload.
    type Input: Serialize + Send + Sync;
    /// Task output payload (must be deserializable and self-describing).
    type Output: DeserializeOwned + Send + Sync;

    /// Human-readable task name for telemetry and diagnostics.
    fn name(&self) -> &'static str;

    /// System instructions framing the steward's specialized cognitive role.
    fn system_prompt(&self) -> &'static str;

    /// Render user prompt from input.
    fn render_prompt(&self, input: &Self::Input) -> String;

    /// Target model tier for this task.
    fn model_preference(&self) -> StewardModelPreference {
        StewardModelPreference::FlashLite
    }

    /// Hard timeout limit in milliseconds.
    fn timeout_ms(&self) -> u64 {
        2000
    }
}

// ── 1. Semantic Loop Detection ──────────────────────────────────────────────

/// Input for semantic loop and doom-loop analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticLoopInput {
    /// Recent tool call signatures and actions issued in this round.
    pub recent_signatures: Vec<String>,
    /// Summary of recent assistant thoughts and observations.
    pub recent_context: String,
}

/// Structured verdict returned by the Semantic Loop Sentinel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticLoopVerdict {
    /// Whether the agent is confirmed to be in an unprogressing doom loop.
    pub is_loop: bool,
    /// Detected loop pattern (e.g. "repeated test failure with oscillating edits").
    pub pattern: Option<String>,
    /// Prescriptive, high-signal nudge to inject into the master's context to guide escape.
    pub remedy_nudge: Option<String>,
}

/// Task definition for semantic loop detection.
#[derive(Debug, Clone, Copy, Default)]
pub struct SemanticLoopSentinelTask;

impl StewardTask for SemanticLoopSentinelTask {
    type Input = SemanticLoopInput;
    type Output = SemanticLoopVerdict;

    fn name(&self) -> &'static str {
        "semantic_loop_sentinel"
    }

    fn system_prompt(&self) -> &'static str {
        "You are the Harness Loop Sentinel. Your job is to analyze recent tool calls and context to determine if an AI agent is stuck in an unprogressing doom loop (repeating failing actions, oscillating edits, or thrashing without new information).\n\
         Respond in strict JSON with schema:\n\
         {\n\
           \"is_loop\": boolean,\n\
           \"pattern\": string | null,\n\
           \"remedy_nudge\": string | null\n\
         }\n\
         If `is_loop` is true, `remedy_nudge` MUST provide a precise, constructive recommendation (1-2 sentences) explaining how the agent should pivot."
    }

    fn render_prompt(&self, input: &Self::Input) -> String {
        let signatures = input.recent_signatures.join("\n- ");
        format!(
            "Analyze the following recent tool execution signatures and context for doom loops:\n\n\
             Recent signatures:\n- {signatures}\n\n\
             Recent context excerpt:\n{}\n\n\
             Output your JSON verdict:",
            input.recent_context
        )
    }

    fn timeout_ms(&self) -> u64 {
        1500
    }
}

// ── 2. Sanity and Safety Check ──────────────────────────────────────────────

/// Input for sanity verification of critical payloads or actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SanityCheckInput {
    /// Action or payload description to evaluate.
    pub action_type: String,
    /// Raw payload or command to verify.
    pub payload: String,
    /// Contextual justification or goal.
    pub justification: String,
}

/// Risk classification for sanity checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Safe,
    Suspicious,
    Dangerous,
}

/// Structured verdict returned by the Sanity Verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanityCheckVerdict {
    /// Whether the action is judged sane and safe to proceed.
    pub is_sane: bool,
    /// Risk level.
    pub risk_level: RiskLevel,
    /// Explanation or critique.
    pub critique: String,
}

/// Task definition for payload sanity checks.
#[derive(Debug, Clone, Copy, Default)]
pub struct SanityVerifierTask;

impl StewardTask for SanityVerifierTask {
    type Input = SanityCheckInput;
    type Output = SanityCheckVerdict;

    fn name(&self) -> &'static str {
        "sanity_verifier"
    }

    fn system_prompt(&self) -> &'static str {
        "You are the Harness Sanity Verifier. Your job is to verify whether an action or string proposed by the agent is rational, safe, and aligned with its stated justification.\n\
         Respond in strict JSON with schema:\n\
         {\n\
           \"is_sane\": boolean,\n\
           \"risk_level\": \"safe\" | \"suspicious\" | \"dangerous\",\n\
           \"critique\": string\n\
         }"
    }

    fn render_prompt(&self, input: &Self::Input) -> String {
        format!(
            "Evaluate action: {}\nJustification: {}\nPayload:\n```\n{}\n```\n\nOutput JSON verdict:",
            input.action_type, input.justification, input.payload
        )
    }

    fn timeout_ms(&self) -> u64 {
        1500
    }
}

// ── 3. Session Titling ──────────────────────────────────────────────────────

/// Input for session titling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTitlerInput {
    /// Condensed excerpt of the conversation.
    pub excerpt: String,
}

/// Structured output for session titling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTitleOutput {
    /// Cleaned, concise title (3 to 7 words).
    pub title: String,
}

/// Task definition for session titling.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionTitlerTask;

impl StewardTask for SessionTitlerTask {
    type Input = SessionTitlerInput;
    type Output = SessionTitleOutput;

    fn name(&self) -> &'static str {
        "session_titler"
    }

    fn system_prompt(&self) -> &'static str {
        "You are a session-titling steward. You are shown an excerpt of a conversation and asked for a short title that captures what the session is about.\n\
         Respond in strict JSON with schema: {\"title\": \"<3-7 words title>\"}.\n\
         Name the concrete subject (a feature, file, bug, or task). Write in the same language as the conversation."
    }

    fn render_prompt(&self, input: &Self::Input) -> String {
        format!(
            "Generate a title for this conversation:\n\n{}\n\nOutput JSON:",
            input.excerpt
        )
    }

    fn timeout_ms(&self) -> u64 {
        2500
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_declare_valid_metadata() {
        let sentinel = SemanticLoopSentinelTask;
        assert_eq!(sentinel.name(), "semantic_loop_sentinel");
        assert!(sentinel.timeout_ms() > 0);

        let verifier = SanityVerifierTask;
        assert_eq!(verifier.name(), "sanity_verifier");

        let titler = SessionTitlerTask;
        assert_eq!(titler.name(), "session_titler");
    }

    #[test]
    fn verdicts_round_trip_json() {
        let verdict = SemanticLoopVerdict {
            is_loop: true,
            pattern: Some("oscillating edits in test.rs".to_string()),
            remedy_nudge: Some("Read the error message carefully before editing".to_string()),
        };
        let s = serde_json::to_string(&verdict).unwrap();
        let back: SemanticLoopVerdict = serde_json::from_str(&s).unwrap();
        assert_eq!(verdict, back);
    }
}
