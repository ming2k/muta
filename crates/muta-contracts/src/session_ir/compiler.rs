//! Multi-pass request compilation pipeline for Session IR (ADR-0241).
//!
//! Compiles an in-memory [`SessionIR`] into a canonical [`ModelRequest`]
//! through four optimizing passes:
//! - **Pass 1: Active Branch Projection** (`active_leaf` walkback to root/compaction horizon)
//! - **Pass 2: Context Projection & Budgeting** (token budgeting & tool output folding)
//! - **Pass 3: Cache Boundary Analysis** (KV-cache prefix stabilization & SHA256 fingerprinting)
//! - **Pass 4: Target Lowering** (emitting a canonical [`ModelRequest`])

use super::types::{CausalNode, NodePayload, SessionIR};
use crate::capability::{ModelRequest, ToolSpec};
use crate::instructions::{InstructionBundle, InstructionSlice, InstructionTier};
use crate::message::{Message, Role};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Configuration options provided to the Session IR compiler.
#[derive(Debug, Clone, Default)]
pub struct CompilerOptions {
    /// Declarations of tools available for this generation attempt.
    pub tool_specs: Vec<ToolSpec>,
    /// Ephemeral request-local temporary context (`E_n`, ADR-0213/0217).
    pub temporary_context: Vec<Message>,
    /// Additional ephemeral instruction slice (e.g. dynamic scratchpad or task hint).
    pub ephemeral_instruction: Option<String>,
    /// Target provider label (e.g. "anthropic", "openai", "gemini").
    pub target_dialect: Option<String>,
}

/// The result produced by the Session IR compilation pipeline.
#[derive(Debug, Clone)]
pub struct CompilationArtifact {
    /// The lowered provider-neutral [`ModelRequest`].
    pub request: ModelRequest,
    /// Forensic details of the computed KV-cache boundary.
    pub cache_boundary: CacheBoundary,
    /// Metrics and statistics about the compilation passes.
    pub stats: CompilationStats,
}

/// Identification of the cacheable static prefix vs. volatile suffix (ADR-0217).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheBoundary {
    /// Deterministic SHA256 hexadecimal hash of the static instruction & historical prefix.
    pub prefix_fingerprint: String,
    /// Number of conversation messages included in the stable cacheable prefix.
    pub stable_message_count: usize,
    /// Number of conversation messages classified as ephemeral dynamic tail.
    pub volatile_message_count: usize,
    /// Byte length of the static system and workspace instructions.
    pub static_instruction_bytes: usize,
}

/// Diagnostic telemetry captured during request compilation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilationStats {
    /// Total nodes traversed in the active lineage.
    pub nodes_traversed: usize,
    /// Dialogue messages retained in the model window.
    pub messages_retained: usize,
    /// Number of oversized tool execution results folded or truncated.
    pub tool_results_truncated: usize,
    /// Rough heuristic token estimate of the total input payload.
    pub estimated_tokens: usize,
}

/// Error encountered during request compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompilerError {
    /// The session contains no active branch to project.
    EmptySession,
    /// State active_leaf pointer references a non-existent node.
    InvalidActiveLeaf(String),
}

impl std::fmt::Display for CompilerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptySession => write!(f, "Session IR has no active nodes or instructions"),
            Self::InvalidActiveLeaf(id) => write!(f, "Active leaf '{id}' not found in causal graph"),
        }
    }
}

impl std::error::Error for CompilerError {}

fn role_as_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    }
}

/// Main entry point: compile a [`SessionIR`] into a [`ModelRequest`].
pub fn compile_session_request(
    ir: &SessionIR,
    options: CompilerOptions,
) -> Result<CompilationArtifact, CompilerError> {
    // -------------------------------------------------------------------------
    // Pass 1: Active Branch Projection
    // -------------------------------------------------------------------------
    let active_nodes = pass1_active_branch_projection(ir)?;

    // -------------------------------------------------------------------------
    // Pass 2: Context Projection & Budget Allocation
    // -------------------------------------------------------------------------
    let (messages, truncated_tools) = pass2_context_budgeting(&active_nodes, ir)?;

    // -------------------------------------------------------------------------
    // Pass 3: Cache Boundary Analysis & Prefix Stabilization
    // -------------------------------------------------------------------------
    let (instructions, cache_boundary) = pass3_cache_boundary_analysis(ir, &messages, &options);

    // -------------------------------------------------------------------------
    // Pass 4: Lowering to ModelRequest
    // -------------------------------------------------------------------------
    let mut request = ModelRequest::new(messages);
    request.instructions = instructions;
    request.tool_specs = options.tool_specs;
    request.temporary_context = options.temporary_context;
    request.turn_context = Arc::default();
    request.prompt_cache_preference = crate::PromptCachePreference::default();

    let estimated_tokens = estimate_total_tokens(&request);

    let stats = CompilationStats {
        nodes_traversed: active_nodes.len(),
        messages_retained: request.messages.len(),
        tool_results_truncated: truncated_tools,
        estimated_tokens,
    };

    Ok(CompilationArtifact {
        request,
        cache_boundary,
        stats,
    })
}

/// Pass 1: Walk backwards from `state.active_leaf` along `parent_id` pointers.
fn pass1_active_branch_projection(ir: &SessionIR) -> Result<Vec<&CausalNode>, CompilerError> {
    match &ir.state.active_leaf {
        Some(leaf_id) => {
            if !ir.history.nodes.contains_key(leaf_id) {
                return Err(CompilerError::InvalidActiveLeaf(leaf_id.clone()));
            }
            Ok(ir.history.linear_path(leaf_id))
        }
        None => Ok(Vec::new()),
    }
}

/// Pass 2: Apply token budgets and fold oversized or stale tool results.
fn pass2_context_budgeting(
    nodes: &[&CausalNode],
    ir: &SessionIR,
) -> Result<(Vec<Message>, usize), CompilerError> {
    let mut messages = Vec::with_capacity(nodes.len());
    let mut truncated_count = 0;
    let max_tool_chars = ir.policy.budget.max_tool_output_tokens.saturating_mul(4);

    for node in nodes {
        match &node.payload {
            NodePayload::Message { message } => {
                let mut msg = message.clone();
                // Apply tool result budget folding if output exceeds threshold
                if msg.role == Role::Tool && msg.content.len() > max_tool_chars {
                    let truncated_text = format!(
                        "{}...\n[Tool output truncated by Session IR compiler: {} bytes omitted]",
                        &msg.content[..max_tool_chars],
                        msg.content.len() - max_tool_chars
                    );
                    msg.content = truncated_text;
                    truncated_count += 1;
                }
                messages.push(msg);
            }
            NodePayload::Compaction { summary, .. } => {
                // Compaction nodes inject a system-role compaction summary into dialogue view
                let summary_msg = Message::new(
                    Role::System,
                    format!("[Conversation Summary Checkpoint]:\n{summary}"),
                );
                messages.push(summary_msg);
            }
            NodePayload::Termination {
                reason,
                partial_output,
                ..
            } => {
                // Interrupted turns emit a synthetic notification informing the model
                let reason_str = match reason {
                    super::types::TerminationReason::UserInterrupt => "Interrupted by user",
                    super::types::TerminationReason::Timeout => "Execution timed out",
                    super::types::TerminationReason::FatalError { error } => error.as_str(),
                    super::types::TerminationReason::Superseded => "Superseded by user message",
                };
                let content = match partial_output {
                    Some(out) => format!("[Execution stopped: {reason_str}]\nPartial output:\n{out}"),
                    None => format!("[Execution stopped: {reason_str}]"),
                };
                messages.push(Message::new(Role::System, content));
            }
            NodePayload::SystemNotice {
                source,
                notice_type,
                content,
            } => {
                let notice_msg = Message::new(
                    Role::System,
                    format!("[System Notice from {source} ({notice_type})]: {content}"),
                );
                messages.push(notice_msg);
            }
        }
    }

    Ok((messages, truncated_count))
}

/// Pass 3: Construct tiered instruction bundle and compute static KV-cache fingerprint.
fn pass3_cache_boundary_analysis(
    ir: &SessionIR,
    messages: &[Message],
    options: &CompilerOptions,
) -> (InstructionBundle, CacheBoundary) {
    let mut slices = Vec::new();
    let mut static_bytes = 0;

    // 1. Base Tier: System Persona
    if let Some(persona) = &ir.policy.rules.system_persona {
        if !persona.trim().is_empty() {
            static_bytes += persona.len();
            slices.push(InstructionSlice::new(
                "base_persona",
                InstructionTier::Base,
                persona.clone(),
            ));
        }
    }

    // 2. Session Tier: Workspace rules (AGENTS.md, conventions)
    for (i, rule) in ir.policy.rules.project_rules.iter().enumerate() {
        if !rule.trim().is_empty() {
            static_bytes += rule.len();
            slices.push(InstructionSlice::new(
                format!("project_rule_{i}"),
                InstructionTier::Session,
                rule.clone(),
            ));
        }
    }

    // 3. Ephemeral Tier: Dynamic scratchpad or task-scoped hints
    if let Some(ephemeral) = &options.ephemeral_instruction {
        if !ephemeral.trim().is_empty() {
            slices.push(InstructionSlice::new(
                "ephemeral_hint",
                InstructionTier::Ephemeral,
                ephemeral.clone(),
            ));
        }
    }

    let bundle = InstructionBundle::new(slices);

    // Compute cache boundary: all messages except the very last turn are considered stable prefix
    let total_messages = messages.len();
    let (stable_count, volatile_count) = if total_messages > 1 {
        (total_messages - 1, 1)
    } else {
        (0, total_messages)
    };

    // Calculate deterministic SHA256 prefix fingerprint
    let mut hasher = Sha256::new();
    for slice in bundle.slices_by_tier(InstructionTier::Base) {
        hasher.update(slice.content.as_bytes());
    }
    for slice in bundle.slices_by_tier(InstructionTier::Session) {
        hasher.update(slice.content.as_bytes());
    }
    for msg in &messages[..stable_count] {
        hasher.update(role_as_str(msg.role).as_bytes());
        hasher.update(msg.content.as_bytes());
    }
    let fingerprint = format!("{:x}", hasher.finalize());

    let boundary = CacheBoundary {
        prefix_fingerprint: fingerprint,
        stable_message_count: stable_count,
        volatile_message_count: volatile_count,
        static_instruction_bytes: static_bytes,
    };

    (bundle, boundary)
}

/// Heuristic token estimator (~4 chars per token).
fn estimate_total_tokens(req: &ModelRequest) -> usize {
    let mut chars = 0;
    for slice in &req.instructions.slices {
        chars += slice.content.len();
    }
    for msg in &req.messages {
        chars += msg.content.len();
    }
    for tmp in &req.temporary_context {
        chars += tmp.content.len();
    }
    chars / 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_ir::types::{BudgetPolicy, RuleSet, SessionPolicy, TerminationReason};

    #[test]
    fn test_compiler_pipeline_full_flow() {
        let mut policy = SessionPolicy::default();
        policy.rules = RuleSet {
            system_persona: Some("You are Muta AI".to_string()),
            workspace_root: Some("/workspace".to_string()),
            project_rules: vec!["Rule 1: Be fast".to_string()],
        };
        policy.budget = BudgetPolicy {
            max_context_tokens: 100_000,
            compaction_trigger_tokens: 80_000,
            max_tool_output_tokens: 10, // 40 chars limit for test
        };

        let mut ir = SessionIR::new("session-compile-test", policy, 1000);

        // Turn 1: User
        ir.append_message("u1", 1000_000, Message::new(Role::User, "Hello"));
        // Turn 2: Assistant with normal output
        ir.append_message("a1", 1001_000, Message::new(Role::Assistant, "I will run a tool"));
        // Turn 3: Tool with very long output exceeding budget
        let huge_tool_output = "a".repeat(200);
        ir.append_message("t1", 1002_000, Message::new(Role::Tool, huge_tool_output));
        // Turn 4: Assistant interrupted
        ir.record_termination(
            "term1",
            1003_000,
            TerminationReason::UserInterrupt,
            Some("Working on it".to_string()),
            None,
            Some(1000),
        );

        let options = CompilerOptions {
            ephemeral_instruction: Some("Focus on speed".to_string()),
            ..Default::default()
        };

        let artifact = compile_session_request(&ir, options).expect("compilation should succeed");

        // Assert Pass 1 & 2
        assert_eq!(artifact.stats.nodes_traversed, 4);
        assert_eq!(artifact.stats.tool_results_truncated, 1);
        assert_eq!(artifact.request.messages.len(), 4);

        // Tool output must be truncated
        let tool_msg = &artifact.request.messages[2];
        assert!(tool_msg.content.contains("Tool output truncated by Session IR compiler"));

        // Interrupted turn must yield synthetic notification
        let term_msg = &artifact.request.messages[3];
        assert!(term_msg.content.contains("Execution stopped: Interrupted by user"));

        // Assert Pass 3 Cache Boundary
        assert_eq!(artifact.cache_boundary.stable_message_count, 3);
        assert_eq!(artifact.cache_boundary.volatile_message_count, 1);
        assert!(!artifact.cache_boundary.prefix_fingerprint.is_empty());

        // Assert Pass 4 ModelRequest Lowering
        assert_eq!(artifact.request.instructions.len(), 3); // Base persona, Project rule, Ephemeral
    }

    #[test]
    fn test_prefix_fingerprint_stability() {
        let mut policy = SessionPolicy::default();
        policy.rules.system_persona = Some("Fixed Persona".to_string());
        policy.rules.project_rules = vec!["Rule A".to_string()];

        let mut ir = SessionIR::new("session-fp-test", policy, 1000);
        ir.append_message("u1", 1000_000, Message::new(Role::User, "Msg 1"));
        ir.append_message("a1", 1001_000, Message::new(Role::Assistant, "Resp 1"));

        let art1 = compile_session_request(&ir, CompilerOptions::default()).unwrap();
        let art2 = compile_session_request(&ir, CompilerOptions::default()).unwrap();

        // Fingerprint must be strictly deterministic across identical compilations
        assert_eq!(
            art1.cache_boundary.prefix_fingerprint,
            art2.cache_boundary.prefix_fingerprint
        );
    }
}
