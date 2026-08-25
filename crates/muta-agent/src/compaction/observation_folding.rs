//! Micro-compaction: folding oversized older tool observation outputs.
//!
//! Replaces verbose older tool outputs in early turns with concise structural markers
//! while leaving recent turns and the OpenAI `tool_call_id` chain completely intact.

use muta_contracts::{Message, Role};

/// Fold verbose historical tool results in `messages` to reduce context pressure.
///
/// Leaves the most recent `keep_recent_turns` dialogue turns untouched.
/// For older turns, tool messages with content larger than `min_fold_bytes`
/// are truncated to their leading lines plus a structured fold marker.
///
/// Returns the total number of bytes saved.
pub fn fold_historical_observations(
    messages: &mut [Message],
    keep_recent_turns: usize,
    min_fold_bytes: usize,
) -> usize {
    if messages.is_empty() {
        return 0;
    }

    // Identify turn boundaries (Role::User messages)
    let mut user_turn_indices = Vec::new();
    for (i, msg) in messages.iter().enumerate() {
        if msg.role == Role::User {
            user_turn_indices.push(i);
        }
    }

    // Determine cutoff index: messages after this index are considered "recent"
    let cutoff_idx = if user_turn_indices.len() > keep_recent_turns {
        user_turn_indices[user_turn_indices.len() - keep_recent_turns]
    } else {
        return 0; // Not enough turns to warrant folding
    };

    let mut bytes_saved = 0;

    for msg in &mut messages[..cutoff_idx] {
        if msg.role == Role::Tool && msg.content.len() > min_fold_bytes {
            let original_len = msg.content.len();

            // Extract the first 3 non-empty lines as header signal
            let mut head_lines = Vec::new();
            for line in msg.content.lines().take(3) {
                head_lines.push(line);
            }
            let head = head_lines.join("\n");

            let folded_text = format!(
                "{}\n\n[... Folded {} bytes of tool observation. Full content preserved in session history ...]",
                head,
                original_len.saturating_sub(head.len())
            );

            if folded_text.len() < original_len {
                bytes_saved += original_len - folded_text.len();
                msg.content = folded_text;
            }
        }
    }

    bytes_saved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_folding_preserves_recent_turns_and_folds_older() {
        let mut messages = vec![
            // Turn 1 (Old)
            Message::new(Role::User, "Please find all rust files"),
            Message::new(Role::Assistant, "Running find command..."),
            Message::new(
                Role::Tool,
                "src/main.rs\nsrc/lib.rs\nsrc/actor.rs\nsrc/worktree.rs\n".repeat(50),
            ), // > 2KB
            Message::new(Role::Assistant, "Found 200 files."),
            // Turn 2 (Old)
            Message::new(Role::User, "Now grep for ActorHandle"),
            Message::new(Role::Assistant, "Running grep..."),
            Message::new(
                Role::Tool,
                "src/actor.rs: pub struct ActorHandle;\n".repeat(40),
            ), // > 1.5KB
            Message::new(Role::Assistant, "Found definition in actor.rs."),
            // Turn 3 (Recent)
            Message::new(Role::User, "Please read actor.rs"),
            Message::new(Role::Assistant, "Reading actor.rs..."),
            Message::new(Role::Tool, "use std::sync::Arc;\npub struct ActorHandle {}"),
            Message::new(Role::Assistant, "Here is the code."),
            // Turn 4 (Recent)
            Message::new(Role::User, "What does ActorHandle do?"),
        ];

        let saved = fold_historical_observations(&mut messages, 2, 200);
        assert!(saved > 2000, "Should have saved over 2KB, saved: {}", saved);

        // Turn 1 tool result should be folded
        assert!(messages[2].content.contains("[... Folded"));
        assert!(messages[2].content.starts_with("src/main.rs"));

        // Turn 2 tool result should be folded
        assert!(messages[6].content.contains("[... Folded"));

        // Turn 3 tool result (recent) must NOT be folded
        assert!(!messages[10].content.contains("[... Folded"));
        assert_eq!(
            messages[10].content,
            "use std::sync::Arc;\npub struct ActorHandle {}"
        );
    }
}
