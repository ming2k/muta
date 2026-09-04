//! Transcript stream mutations, patch application, and selection extraction.

use crate::model::document::{MessageKind, TranscriptMessage};
use crate::model::selection::{CellDragInfo, SelectionState, get_selected_text};
use crate::versioned::{HeightInvalidation, TranscriptPatch, TranscriptUpdate};
use crate::view;

/// Apply only the cache invalidation actually caused by the most recent transcript mutation.
pub(crate) fn apply_height_invalidation(
    cache: &mut view::HeightCache,
    invalidation: HeightInvalidation,
) {
    match invalidation {
        HeightInvalidation::None => {}
        HeightInvalidation::Messages(ids) => cache.invalidate_messages(ids),
        HeightInvalidation::All => cache.clear(),
    }
}

/// Whether the transcript slice painted this frame changed shape.
pub(crate) fn displayed_transcript_did_change(
    in_side_view: bool,
    primary_changed: bool,
    side_changed: bool,
    view_transitioned: bool,
) -> bool {
    view_transitioned
        || if in_side_view {
            side_changed
        } else {
            primary_changed
        }
}

/// Replay high-frequency stream changes into the app-owned transcript.
pub(crate) fn apply_transcript_patch(
    messages: &mut Vec<TranscriptMessage>,
    patch: TranscriptPatch,
) -> bool {
    let updates = match patch {
        TranscriptPatch::None => return true,
        TranscriptPatch::Replace => return false,
        TranscriptPatch::Updates(updates) => updates,
    };

    for update in updates {
        let applied = match update {
            TranscriptUpdate::TextDelta { message_id, delta } => {
                let Some(message) = messages
                    .iter_mut()
                    .rfind(|message| message.id == message_id)
                    .filter(|message| matches!(message.kind, MessageKind::Text))
                else {
                    return false;
                };
                message.push_stream(&delta);
                true
            }
            TranscriptUpdate::ReasoningDelta { message_id, delta } => {
                let Some(message) = messages
                    .iter_mut()
                    .rfind(|message| message.id == message_id)
                    .filter(|message| message.is_thinking())
                else {
                    return false;
                };
                message.push_stream(&delta);
                if let MessageKind::Thinking { content, .. } = &mut message.kind {
                    content.push_str(&delta);
                    true
                } else {
                    false
                }
            }
            TranscriptUpdate::ToolStream { id, stream } => messages
                .iter_mut()
                .any(|message| message.push_tool_stream(&id, &stream)),
            TranscriptUpdate::RunnerEvent {
                parent_call_id,
                event,
            } => messages
                .iter_mut()
                .find(|message| message.tool_step_call_id() == Some(parent_call_id.as_str()))
                .is_some_and(|message| message.push_runner_event(&event)),
            TranscriptUpdate::ReplaceMessage {
                message_id,
                message,
            } => {
                let Some(existing) = messages
                    .iter_mut()
                    .rfind(|message| message.id == message_id)
                else {
                    return false;
                };
                *existing = message;
                true
            }
            TranscriptUpdate::AppendMessage {
                pre_append_tail,
                message,
            } => {
                let local_tail = messages.last().map(|tail| tail.id);
                if local_tail != pre_append_tail {
                    return false;
                }
                messages.push(message);
                true
            }
        };
        if !applied {
            return false;
        }
    }
    true
}

/// Resolve a mutable reference to a message by semantic index.
pub(crate) fn resolve_focused_mut<'a>(
    messages: &'a mut [TranscriptMessage],
    focus_stack: &[crate::app::ZoomFrame],
    mi: usize,
) -> Option<&'a mut TranscriptMessage> {
    let Some(current) = focus_stack.last() else {
        return messages.get_mut(mi);
    };
    let task_idx = messages.iter().position(|message| {
        message.is_runner_task() && message.tool_step_call_id() == Some(current.call_id.as_str())
    })?;
    messages[task_idx].runner_children_mut()?.get_mut(mi)
}

/// Iterate mutable messages in the currently focused view for tests.
#[cfg(test)]
pub(crate) fn focused_messages_mut<'a>(
    messages: &'a mut [TranscriptMessage],
    focus_stack: &[crate::app::ZoomFrame],
) -> Box<dyn Iterator<Item = &'a mut TranscriptMessage> + 'a> {
    match focus_stack.last() {
        None => Box::new(messages.iter_mut()),
        Some(current) => {
            let task_idx = messages.iter().position(|message| {
                message.is_runner_task()
                    && message.tool_step_call_id() == Some(current.call_id.as_str())
            });
            match task_idx {
                Some(idx) => match messages[idx].runner_children_mut() {
                    Some(children) => Box::new(children.iter_mut()),
                    None => Box::new(std::iter::empty()),
                },
                None => Box::new(std::iter::empty()),
            }
        }
    }
}

/// Extract selected text from either transcript messages or the live input box.
pub(crate) fn extract_selection_text(
    sel: &SelectionState,
    messages: &[crate::model::document::TranscriptMessage],
    input: &str,
    layout_map: &crate::model::layout::LayoutMap,
    cell_info: Option<&CellDragInfo>,
) -> Option<String> {
    if let Some((start, end)) = sel.active_normalized_range() {
        if start.message_idx == crate::view::INPUT_MSG_IDX {
            let s = start.byte_offset;
            let e = end.byte_offset;
            if s <= e && e <= input.len() {
                let start_idx = input
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|&i| i <= s)
                    .last()
                    .unwrap_or(0);
                let end_idx = input
                    .char_indices()
                    .map(|(i, _)| i)
                    .find(|&i| i >= e)
                    .unwrap_or(input.len());
                return Some(input[start_idx..end_idx].to_string());
            }
            return None;
        }
        if start.message_idx == crate::model::layout::MODAL_DOC_MSG_IDX {
            return layout_map.extract_text_for_range(sel);
        }
    } else if let SelectionState::Block { message_idx, .. } = sel
        && *message_idx == crate::view::INPUT_MSG_IDX
    {
        return Some(input.to_string());
    } else if let SelectionState::Block { message_idx, .. } = sel
        && *message_idx == crate::model::layout::MODAL_DOC_MSG_IDX
    {
        return layout_map.extract_text_for_range(sel);
    }

    let grid = |mi, bi| layout_map.table_grid(mi, bi);
    get_selected_text(sel, messages, &grid, cell_info)
}

/// Extract readable text content from an interactive focused target (for component copy).
pub(crate) fn extract_focused_target_text(
    messages: &[crate::model::document::TranscriptMessage],
    target: crate::model::layout::InteractiveTarget,
) -> Option<String> {
    let msg = messages.get(target.message_idx)?;
    let text = match &msg.kind {
        crate::model::document::MessageKind::ToolStep {
            output,
            arguments,
            name,
            ..
        } => output
            .as_ref()
            .cloned()
            .unwrap_or_else(|| format!("{name} {arguments}")),
        crate::model::document::MessageKind::Thinking { content, .. } => content.clone(),
        crate::model::document::MessageKind::CommandResult {
            invocation, result, ..
        } => {
            let inv_str = format!("{} {}", invocation.name, invocation.args)
                .trim()
                .to_string();
            result
                .as_ref()
                .map(|r| format!("{inv_str}: {r:?}"))
                .unwrap_or(inv_str)
        }
        crate::model::document::MessageKind::Notice { parts, .. } => parts
            .as_ref()
            .map(|p| {
                if let Some(detail) = &p.detail {
                    format!("{}: {detail}", p.title)
                } else {
                    p.title.clone()
                }
            })
            .unwrap_or_else(|| msg.raw.clone()),
        _ => msg.raw.clone(),
    };
    Some(text)
}

/// Format the current loop status into human-readable text.
pub(crate) fn display_status(
    loop_status: muta_contracts::LoopStatus,
    phase: Option<&crate::phase::Phase>,
) -> String {
    match (loop_status, phase) {
        (muta_contracts::LoopStatus::Idle, None) => "idle".to_string(),
        (muta_contracts::LoopStatus::Running, None) => "preparing".to_string(),
        (muta_contracts::LoopStatus::Idle, Some(phase))
        | (muta_contracts::LoopStatus::Running, Some(phase)) => phase.label().into_owned(),
    }
}
