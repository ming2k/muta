//! Turn-aware split compaction on Session IR CausalNodes (ADR-0255).
//!
//! Features:
//! - Turn-aware atomic cut point selection that never cuts on a Tool Result.
//! - Conservative token estimation per CausalNode.
//! - LLM structured summarization into dense context checkpoints.

use super::file_tracker::FileOperations;
use muta_contracts::{CausalNode, Message, ModelRequest, NodePayload, Provider, Role};
use std::sync::Arc;
use tokio::time::Duration;

const COMPACTION_TIMEOUT: Duration = Duration::from_secs(45);

const COMPACTION_SYSTEM_PROMPT: &str = "\
You are a context compaction engine for an AI coding assistant. \
Your task is to summarize older conversation turns into a dense, factual context summary \
so that ongoing work can continue with minimal token overhead without losing crucial facts.";

const COMPACTION_USER_INSTRUCTIONS: &str = "\
Summarize the conversation history above into a structured context checkpoint.

Follow this format:

## Primary Goal & Current Objective
[What is the overall goal and what was being worked on?]

## Key Facts & Architecture Notes
- [Crucial decisions, paths, port numbers, dependencies, or architectural facts]

## Completed Actions
- [x] [What was accomplished and verified]

## Current State & In-Flight Context
- [What was in progress right before this checkpoint]

## Critical Error Messages & Observations
- [Any specific error messages or unexpected behavior observed]

Preserve exact identifiers, function names, file paths, and technical details.";

/// Result of finding a valid cut point for compaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutPointResult {
    /// Index in nodes slice of the first node to KEEP in active context.
    pub first_kept_index: usize,
    /// ID of the first node kept.
    pub first_kept_entry_id: String,
    /// Whether the cut splits an in-flight multi-step turn.
    pub is_split_turn: bool,
    /// Estimated tokens in the messages before the cut point.
    pub tokens_before: usize,
}

/// Conservative token estimation for a CausalNode (chars / 4 heuristic, ADR-0255).
pub fn estimate_causal_node_tokens(node: &CausalNode) -> usize {
    match &node.payload {
        NodePayload::Message { message } => {
            let mut chars = message.content.len();
            if let Some(ref calls) = message.tool_calls {
                for c in calls {
                    chars += c.name.len() + c.arguments.len() + 16;
                }
            }
            chars.div_ceil(4)
        }
        NodePayload::Compaction { summary, .. } => summary.len().div_ceil(4),
        NodePayload::Termination { partial_output, .. } => partial_output
            .as_ref()
            .map(|s| s.len().div_ceil(4))
            .unwrap_or(32),
        NodePayload::SystemNotice { content, .. } => content.len().div_ceil(4),
    }
}

/// Find a turn-aware cut point on CausalNodes that preserves approximately `keep_recent_tokens` of newest context.
/// Never cuts on a Tool Result — always keeps tool calls and their results together (ADR-0255).
pub fn find_cut_point_nodes(
    nodes: &[&CausalNode],
    keep_recent_tokens: usize,
) -> Option<CutPointResult> {
    if nodes.is_empty() {
        return None;
    }

    let mut total_tokens = 0;
    let token_counts: Vec<usize> = nodes
        .iter()
        .map(|n| estimate_causal_node_tokens(n))
        .collect();
    for &tokens in &token_counts {
        total_tokens += tokens;
    }

    if total_tokens <= keep_recent_tokens {
        return None; // No compaction needed yet
    }

    let mut accumulated_tokens = 0;
    let mut cut_index = 0;

    // Scan backwards from newest entry
    for i in (0..nodes.len()).rev() {
        accumulated_tokens += token_counts[i];
        if accumulated_tokens >= keep_recent_tokens {
            cut_index = i;
            break;
        }
    }

    // Adjust cut_index so we never cut in the middle of a ToolResult
    // If the node at cut_index is a ToolResult, walk backward to find the Assistant call that triggered it
    while cut_index > 0 {
        if let NodePayload::Message { ref message } = nodes[cut_index].payload
            && message.role == Role::Tool
        {
            cut_index -= 1;
            continue;
        }
        break;
    }

    if cut_index == 0 || cut_index >= nodes.len() {
        return None;
    }

    let first_kept_entry_id = nodes[cut_index].id.clone();
    let is_split_turn = if let NodePayload::Message { ref message } = nodes[cut_index].payload {
        message.role == Role::Assistant
    } else {
        false
    };

    let tokens_before: usize = token_counts[..cut_index].iter().sum();

    Some(CutPointResult {
        first_kept_index: cut_index,
        first_kept_entry_id,
        is_split_turn,
        tokens_before,
    })
}

/// Serialize CausalNodes to clean text representation for LLM summarization (ADR-0255).
pub fn serialize_nodes_for_summary(nodes: &[&CausalNode]) -> String {
    let mut out = String::new();
    for node in nodes {
        match &node.payload {
            NodePayload::Message { message } => {
                let role_label = match message.role {
                    Role::User => "User",
                    Role::Assistant => "Assistant",
                    Role::Tool => "Tool Result",
                    Role::System => "System",
                };
                out.push_str(&format!("[{}]\n{}\n\n", role_label, message.content));
            }
            NodePayload::Compaction { summary, .. } => {
                out.push_str(&format!("[Previous Summary Checkpoint]\n{}\n\n", summary));
            }
            NodePayload::Termination {
                reason,
                partial_output,
                ..
            } => {
                let reason_str = match reason {
                    muta_contracts::TerminationReason::UserInterrupt => "Interrupted by user",
                    muta_contracts::TerminationReason::Timeout => "Execution timed out",
                    muta_contracts::TerminationReason::FatalError { error } => error.as_str(),
                    muta_contracts::TerminationReason::Superseded => "Superseded by user message",
                };
                let output = partial_output.as_deref().unwrap_or("");
                out.push_str(&format!(
                    "[Execution Stopped: {}]\n{}\n\n",
                    reason_str, output
                ));
            }
            NodePayload::SystemNotice {
                source, content, ..
            } => {
                out.push_str(&format!("[System Notice from {}]\n{}\n\n", source, content));
            }
        }
    }
    out
}

/// Execute turn-aware split compaction on CausalNode lineage (ADR-0255).
pub async fn compact_causal_nodes(
    provider: Arc<dyn Provider>,
    nodes: &[&CausalNode],
    cut_point: &CutPointResult,
    previous_summary: Option<&str>,
    extra_context: &[String],
) -> Result<NodePayload, String> {
    let nodes_to_compact = &nodes[..cut_point.first_kept_index];
    if nodes_to_compact.is_empty() {
        return Err("No nodes to compact".to_string());
    }

    let mut file_tracker = FileOperations::new();
    for node in nodes_to_compact {
        file_tracker.extract_from_node(node);
    }

    let conversation_text = serialize_nodes_for_summary(nodes_to_compact);
    let mut prompt_body = format!(
        "<conversation_to_compact>\n{}\n</conversation_to_compact>\n\n",
        conversation_text
    );
    if let Some(prev) = previous_summary {
        prompt_body.push_str(&format!(
            "<prior_summary>\n{}\n</prior_summary>\n\nMerge the above prior summary with the new conversation delta.\n\n",
            prev
        ));
    }
    for ctx in extra_context {
        prompt_body.push_str(&format!(
            "<additional_context>\n{}\n</additional_context>\n\n",
            ctx
        ));
    }
    prompt_body.push_str(COMPACTION_USER_INSTRUCTIONS);

    let instructions = muta_contracts::InstructionBundle::from_single(
        "compaction.split",
        muta_contracts::InstructionTier::Task,
        COMPACTION_SYSTEM_PROMPT,
    );
    let messages = vec![Message::new(Role::User, prompt_body)];
    let request = ModelRequest::ephemeral(messages).with_instructions(instructions);

    let response = match tokio::time::timeout(COMPACTION_TIMEOUT, provider.chat(request)).await {
        Ok(Ok(msg)) => msg,
        Ok(Err(err)) => return Err(format!("Compaction LLM call failed: {}", err)),
        Err(_) => return Err("Compaction LLM call timed out".to_string()),
    };

    let mut summary = response.message.content.trim().to_string();
    let file_section = file_tracker.format_markdown();
    summary.push_str(&file_section);

    let read_files: Vec<String> = file_tracker.read.into_iter().collect();
    let modified_files: Vec<String> = file_tracker.modified.into_iter().collect();

    Ok(NodePayload::Compaction {
        summary,
        first_kept_node_id: cut_point.first_kept_entry_id.clone(),
        tokens_before: cut_point.tokens_before,
        read_files,
        modified_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_cut_point_nodes_never_cuts_on_tool_result() {
        let n1 = CausalNode {
            id: "1".into(),
            parent_id: None,
            seq: 1,
            timestamp_ms: 100,
            kind: muta_contracts::NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::User, "run test"),
            },
        };
        let n2 = CausalNode {
            id: "2".into(),
            parent_id: Some("1".into()),
            seq: 2,
            timestamp_ms: 101,
            kind: muta_contracts::NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::Assistant, "calling bash"),
            },
        };
        let n3 = CausalNode {
            id: "3".into(),
            parent_id: Some("2".into()),
            seq: 3,
            timestamp_ms: 102,
            kind: muta_contracts::NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::Tool, "output of bash"),
            },
        };
        let n4 = CausalNode {
            id: "4".into(),
            parent_id: Some("3".into()),
            seq: 4,
            timestamp_ms: 103,
            kind: muta_contracts::NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::Assistant, "all done"),
            },
        };
        let nodes = vec![&n1, &n2, &n3, &n4];

        let cut = find_cut_point_nodes(&nodes, 5).unwrap();
        assert_ne!(cut.first_kept_index, 2);
    }
}
