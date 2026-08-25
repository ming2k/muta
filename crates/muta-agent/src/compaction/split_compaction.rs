use super::file_tracker::FileOperations;
use muta_contracts::{Message, ModelRequest, Provider, Role, SessionEntry, SessionEntryKind};
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
    /// Index in entries slice of the first entry to KEEP in active context.
    pub first_kept_index: usize,
    /// ID of the first entry kept.
    pub first_kept_entry_id: String,
    /// Whether the cut splits an in-flight multi-step turn.
    pub is_split_turn: bool,
    /// Estimated tokens in the messages before the cut point.
    pub tokens_before: usize,
}

/// Conservative token estimation for an entry (chars / 4 heuristic).
pub fn estimate_entry_tokens(entry: &SessionEntry) -> usize {
    match &entry.kind {
        SessionEntryKind::Message { message } => {
            let mut chars = message.content.len();
            if let Some(ref calls) = message.tool_calls {
                for c in calls {
                    chars += c.name.len() + c.arguments.len() + 16;
                }
            }
            chars.div_ceil(4)
        }
        SessionEntryKind::Compaction { summary, .. } => summary.len().div_ceil(4),
        SessionEntryKind::BranchSummary { summary, .. } => summary.len().div_ceil(4),
        SessionEntryKind::Custom { content, .. } => content.len().div_ceil(4),
    }
}

/// Find a turn-aware cut point that preserves approximately `keep_recent_tokens` of newest context.
/// Never cuts on a Tool Result — always keeps tool calls and their results together.
pub fn find_cut_point(
    entries: &[SessionEntry],
    keep_recent_tokens: usize,
) -> Option<CutPointResult> {
    if entries.is_empty() {
        return None;
    }

    let mut total_tokens = 0;
    let token_counts: Vec<usize> = entries.iter().map(estimate_entry_tokens).collect();
    for &tokens in &token_counts {
        total_tokens += tokens;
    }

    if total_tokens <= keep_recent_tokens {
        return None; // No compaction needed yet
    }

    let mut accumulated_tokens = 0;
    let mut cut_index = 0;

    // Scan backwards from newest entry
    for i in (0..entries.len()).rev() {
        accumulated_tokens += token_counts[i];
        if accumulated_tokens >= keep_recent_tokens {
            cut_index = i;
            break;
        }
    }

    // Adjust cut_index so we never cut in the middle of a ToolResult
    // If the entry at cut_index is a ToolResult, walk backward to find the Assistant call that triggered it
    while cut_index > 0 {
        if let SessionEntryKind::Message { ref message } = entries[cut_index].kind {
            if message.role == Role::Tool {
                cut_index -= 1;
                continue;
            }
        }
        break;
    }

    if cut_index == 0 || cut_index >= entries.len() {
        return None;
    }

    let first_kept_entry_id = entries[cut_index].id.clone();
    let is_split_turn = if let SessionEntryKind::Message { ref message } = entries[cut_index].kind {
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

/// Execute turn-aware split compaction on session entries.
pub async fn compact_entries(
    provider: Arc<dyn Provider>,
    entries: &[SessionEntry],
    cut_point: &CutPointResult,
) -> Result<SessionEntryKind, String> {
    let entries_to_compact = &entries[..cut_point.first_kept_index];
    if entries_to_compact.is_empty() {
        return Err("No entries to compact".to_string());
    }

    let mut file_tracker = FileOperations::new();
    for entry in entries_to_compact {
        file_tracker.extract_from_entry(entry);
    }

    let conversation_text =
        super::branch_summary::serialize_entries_for_summary(entries_to_compact);
    let prompt_body = format!(
        "<conversation_to_compact>\n{}\n</conversation_to_compact>\n\n{}",
        conversation_text, COMPACTION_USER_INSTRUCTIONS
    );

    let messages = vec![
        Message::new(Role::System, COMPACTION_SYSTEM_PROMPT),
        Message::new(Role::User, prompt_body),
    ];

    let request = ModelRequest::ephemeral(messages);

    let response = match tokio::time::timeout(COMPACTION_TIMEOUT, provider.chat(request)).await {
        Ok(Ok(msg)) => msg,
        Ok(Err(err)) => return Err(format!("Compaction LLM call failed: {}", err)),
        Err(_) => return Err("Compaction LLM call timed out".to_string()),
    };

    let mut summary = response.content;
    let file_section = file_tracker.format_markdown();
    summary.push_str(&file_section);

    let read_files: Vec<String> = file_tracker.read.into_iter().collect();
    let modified_files: Vec<String> = file_tracker.modified.into_iter().collect();

    Ok(SessionEntryKind::Compaction {
        summary,
        first_kept_entry_id: cut_point.first_kept_entry_id.clone(),
        tokens_before: cut_point.tokens_before,
        read_files,
        modified_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_cut_point_never_cuts_on_tool_result() {
        let entries = vec![
            SessionEntry::new_message("1", None, 100, Message::new(Role::User, "run test")),
            SessionEntry::new_message(
                "2",
                Some("1".into()),
                101,
                Message::new(Role::Assistant, "calling bash"),
            ),
            SessionEntry::new_message(
                "3",
                Some("2".into()),
                102,
                Message::new(Role::Tool, "output of bash"),
            ),
            SessionEntry::new_message(
                "4",
                Some("3".into()),
                103,
                Message::new(Role::Assistant, "all done"),
            ),
        ];

        // If budget demands cutting near entry 3 (the tool result), it must safely backtrack to entry 2 (the assistant call)
        let cut = find_cut_point(&entries, 5).unwrap();
        assert_ne!(cut.first_kept_index, 2); // Cannot cut on entry 2 (index 2 is Tool message '3')
    }
}
