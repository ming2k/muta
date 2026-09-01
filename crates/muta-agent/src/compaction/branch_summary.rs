use super::file_tracker::FileOperations;
use muta_contracts::{Message, ModelRequest, Provider, Role, SessionEntry, SessionEntryKind};
use std::sync::Arc;
use tokio::time::Duration;

const BRANCH_SUMMARY_TIMEOUT: Duration = Duration::from_secs(30);

const BRANCH_SUMMARY_SYSTEM_PROMPT: &str = "\
You are a technical context summarizer for an AI software engineering assistant. \
Your task is to summarize the work, exploration, and key findings of a conversation branch \
so that the user and assistant can seamlessly continue work on another branch without losing context.";

const BRANCH_SUMMARY_USER_INSTRUCTIONS: &str = "\
Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned, or (none)]

## Progress
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

/// Serialize session entries to text representation for LLM summarization.
pub fn serialize_entries_for_summary(entries: &[SessionEntry]) -> String {
    let mut out = String::new();
    for entry in entries {
        if let Some(msg) = entry.to_context_message() {
            let role_label = match msg.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::Tool => "Tool Result",
                Role::System => "System",
            };
            out.push_str(&format!("[{}]\n{}\n\n", role_label, msg.content));
        }
    }
    out
}

/// Generate a structured branch summary for abandoned entries when transitioning branches.
pub async fn generate_branch_summary(
    provider: Arc<dyn Provider>,
    from_leaf_id: &str,
    abandoned_entries: &[SessionEntry],
    custom_instructions: Option<&str>,
) -> Result<Option<SessionEntryKind>, String> {
    if abandoned_entries.is_empty() {
        return Ok(None);
    }

    let mut file_tracker = FileOperations::new();
    for entry in abandoned_entries {
        file_tracker.extract_from_entry(entry);
    }

    let conversation_text = serialize_entries_for_summary(abandoned_entries);
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

    Ok(Some(SessionEntryKind::BranchSummary {
        summary,
        from_id: from_leaf_id.to_string(),
        read_files,
        modified_files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_preserves_dialogue() {
        let entries = vec![
            SessionEntry::new_message("1", None, 100, Message::new(Role::User, "Hello")),
            SessionEntry::new_message(
                "2",
                Some("1".into()),
                101,
                Message::new(Role::Assistant, "Hi there"),
            ),
        ];
        let text = serialize_entries_for_summary(&entries);
        assert!(text.contains("[User]\nHello"));
        assert!(text.contains("[Assistant]\nHi there"));
    }
}
