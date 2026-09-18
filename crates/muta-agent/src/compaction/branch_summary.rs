use super::file_tracker::FileOperations;
use super::split_compaction::serialize_nodes_for_summary;
use muta_contracts::{CausalNode, Message, ModelRequest, NodePayload, Provider, Role};
use std::sync::Arc;
use tokio::time::Duration;

const BRANCH_SUMMARY_TIMEOUT: Duration = Duration::from_secs(45);

const BRANCH_SUMMARY_SYSTEM_PROMPT: &str = "\
You are a session branch summarization assistant. \
Your job is to summarize work done on a side branch or abandoned timeline into a concise summary \
so that another branch or future rounds can understand what was tried, what was completed, and why it was left.";

const BRANCH_SUMMARY_USER_INSTRUCTIONS: &str = "\
Summarize the work done on this timeline into a clear, structured summary.

Format:

## Branch Objective
[What was this branch attempting to accomplish?]

## Work Done
### Done
- [x] [Completed tasks or changes]

### In Progress
- [ ] [Work that was started but not completed]

### Blocked
- [Issues or roadblocks encountered, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise and factual. Preserve exact file paths, function names, and error messages.";

/// Generate a structured branch summary for abandoned nodes when transitioning branches (ADR-0255).
pub async fn generate_branch_summary(
    provider: Arc<dyn Provider>,
    from_leaf_id: &str,
    abandoned_nodes: &[&CausalNode],
    custom_instructions: Option<&str>,
) -> Result<Option<NodePayload>, String> {
    if abandoned_nodes.is_empty() {
        return Ok(None);
    }

    let mut file_tracker = FileOperations::new();
    for node in abandoned_nodes {
        file_tracker.extract_from_node(node);
    }

    let conversation_text = serialize_nodes_for_summary(abandoned_nodes);
    if conversation_text.trim().is_empty() {
        return Ok(None);
    }

    let prompt_body = format!(
        "<conversation>\n{}\n</conversation>\n\n{}{}",
        conversation_text,
        BRANCH_SUMMARY_USER_INSTRUCTIONS,
        custom_instructions
            .map(|inst| format!("\n\nAdditional focus: {}", inst))
            .unwrap_or_default()
    );

    let instructions = muta_contracts::InstructionBundle::from_single(
        "compaction.branch_summary",
        muta_contracts::InstructionTier::Task,
        BRANCH_SUMMARY_SYSTEM_PROMPT,
    );
    let messages = vec![Message::new(Role::User, prompt_body)];
    let request = ModelRequest::ephemeral(messages).with_instructions(instructions);

    let response = match tokio::time::timeout(BRANCH_SUMMARY_TIMEOUT, provider.chat(request)).await
    {
        Ok(Ok(msg)) => msg,
        Ok(Err(err)) => return Err(format!("Branch summarization failed: {}", err)),
        Err(_) => return Err("Branch summarization timed out".to_string()),
    };

    let mut summary = response.message.content;
    let file_section = file_tracker.format_markdown();
    summary.push_str(&file_section);

    let read_files: Vec<String> = file_tracker.read.into_iter().collect();
    let modified_files: Vec<String> = file_tracker.modified.into_iter().collect();

    Ok(Some(NodePayload::Compaction {
        summary,
        first_kept_node_id: from_leaf_id.to_string(),
        tokens_before: 0,
        read_files,
        modified_files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_preserves_dialogue() {
        let n1 = CausalNode {
            id: "1".into(),
            parent_id: None,
            seq: 1,
            timestamp_ms: 100,
            kind: muta_contracts::NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::User, "Hello"),
            },
        };
        let n2 = CausalNode {
            id: "2".into(),
            parent_id: Some("1".into()),
            seq: 2,
            timestamp_ms: 101,
            kind: muta_contracts::NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::Assistant, "Hi there"),
            },
        };
        let nodes = vec![&n1, &n2];
        let text = serialize_nodes_for_summary(&nodes);
        assert!(text.contains("[User]\nHello"));
        assert!(text.contains("[Assistant]\nHi there"));
    }
}
