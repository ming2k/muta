//! Translation between the harness's persistent [`Message`] stream and the
//! TUI's semantic [`TranscriptMessage`] document model. Also hosts the small
//! parsing helpers for the textual `` `[<tool> result]`: `` envelope and the
//! matching `` `Calling \`<tool>\`` `` formatter used when no display content
//! is available for a restored assistant turn.
//!
//! [`Message`]: neenee_core::Message

use std::collections::HashMap;
use std::collections::VecDeque;

use neenee_core::{Message, Role};

use crate::tui::config::{self, TuiConfig};
use crate::tui::document::{TranscriptMessage, UserMessageOrigin};
use crate::tui::step_interaction;

pub(super) fn transcript_message_from_core(message: Message) -> Option<TranscriptMessage> {
    if message.hidden || message.role == Role::System {
        return None;
    }
    let provider = message.provider.clone();
    let model = message.model.clone();
    let sent_at_ms = message.sent_at_ms.or_else(|| {
        message
            .timestamp
            .map(|seconds| seconds.saturating_mul(1000))
    });
    // Whether the harness carried a curated `display_content` for this user
    // message — slash commands set this to the literal `/cmd` (their `content`
    // is the harness-expanded form), so its presence is the signal that the
    // turn was a slash command rather than a genuine chat prompt.
    let had_display_content = message.display_content.is_some();
    // Capture non-driving provenance before `content` is moved (ADR-0050): a
    // durable `CommandEcho` origin unambiguously marks the message as a
    // non-driving command regardless of text shape.
    let is_echo = message.is_command_echo();
    let is_insert = message
        .origin
        .as_ref()
        .is_some_and(|origin| origin.kind == neenee_core::InjectionKind::UserSteer);
    let content = if let Some(display_content) = message.display_content {
        display_content
    } else if message.content.is_empty() {
        message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|call| format_tool_call(&call.name, &call.arguments))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        message.content
    };
    if content.is_empty() {
        None
    } else {
        let mut msg = TranscriptMessage::new(message.role, content);
        msg.provider = provider;
        msg.model = model;
        if msg.role == Role::User {
            msg.sent_at_ms = sent_at_ms;
        }
        // Infer the turn origin for restored user messages so a resumed
        // session's Activity modal still skips slash/shell turns. The durable
        // `origin` field is consulted first (ADR-0050): a `CommandEcho`
        // provenance unambiguously marks the message as a non-driving command,
        // regardless of its text shape. When `origin` is `None` (legacy
        // sessions predating the field, or genuine chat prompts) the shape
        // heuristic below applies: slash commands carry a `display_content`
        // that is the literal `/cmd` (the `content` is the harness-expanded
        // form); shell passthroughs persist as the `!command` the user typed.
        // Only a real prompt surfaces a leading `/` or `!` *not* followed by a
        // word char would still be misclassified — but in practice both are
        // followed by a command word, so the heuristic is exact for the shapes
        // the harness produces.
        if message.role == Role::User {
            // Slash origin: a durable `CommandEcho` provenance (ADR-0050, which
            // unambiguously marks the message as a non-driving command
            // regardless of text shape) OR the legacy shape signal of a
            // `display_content` whose text is the literal `/cmd`. Both fold to
            // Slash; the echo check runs first so a durable echo without
            // `display_content` is still classified correctly.
            let is_slash = is_echo
                || (had_display_content
                    && msg.raw.strip_prefix('/').is_some_and(|rest| {
                        rest.chars().next().is_some_and(|c| c.is_alphanumeric())
                    }));
            if is_slash {
                msg.origin = UserMessageOrigin::Slash;
            } else if is_insert {
                msg.origin = UserMessageOrigin::Insert;
            } else if msg
                .raw
                .strip_prefix('!')
                .is_some_and(|rest| !rest.is_empty())
            {
                msg.origin = UserMessageOrigin::Shell;
            }
        }
        Some(msg)
    }
}

pub(super) fn transcript_messages_from_core(
    messages: Vec<Message>,
    config: &TuiConfig,
) -> Vec<TranscriptMessage> {
    let mut restored = Vec::new();
    // Model-request counter for restored assistant turns. Each
    // `Role::Assistant` message is one round, so the counter increments per
    // assistant message and stamps its thinking, tools, and text — mirroring
    // the live `TurnStarted` path so restored spacing has identical boundaries.
    let mut restored_round: u64 = 0;
    // Index of every still-unfinished tool step, queued per tool name. A tool
    // result pairs with the *earliest* unfinished step of the same name (live
    // order), so a per-name FIFO reproduces the old forward-scan semantics in
    // O(1) per result instead of rescanning the whole `restored` vec — turning
    // the restore of a tool-heavy session from O(n²) into O(n).
    let mut pending_steps: HashMap<String, VecDeque<usize>> = HashMap::new();
    for mut message in messages {
        if message.hidden || message.role == Role::System {
            continue;
        }
        // Attribution travels on every part so a resumed session that mixed
        // models still shows which model produced each turn.
        let provider = message.provider.clone();
        let model = message.model.clone();
        let message_sent_at_ms = message.sent_at_ms.or_else(|| {
            message
                .timestamp
                .map(|seconds| seconds.saturating_mul(1000))
        });
        if message.role == Role::Assistant {
            restored_round = restored_round.saturating_add(1);
            // Mirrors the live path's `StreamReasoningDelta` gate: a hidden-chain
            // model (`ReasoningSummary`, e.g. GPT-5.x) never disclosed its full
            // reasoning chain, so its persisted `reasoning_content` is only a
            // summary. Restoring it as a `MessageKind::Thinking` message would
            // resurrect a phantom entry the live stream never created — leaking
            // into layout counts, selection math, and scroll state. Skip it.
            // `model` is the persisted `Option<String>` attribution. Use
            // `model_by_id` (not `resolve`): `resolve` falls back to a model
            // with `ThinkingSupport::None` for unrecognized ids, whose
            // `chain_disclosed()` is `false`, which would suppress restoration
            // of reasoning traces for local/user-defined models that DO reason
            // — a regression for legacy transcripts. `model_by_id` returns
            // `None` for unknown ids, and we default to `true` (disclosed) so
            // only known hidden-chain models (`ReasoningSummary`, GPT-5.x) are
            // gated.
            let chain_disclosed = model
                .as_deref()
                .and_then(neenee_core::model_by_id)
                .map(|m| m.thinking.chain_disclosed())
                .unwrap_or(true);
            if chain_disclosed && let Some(reasoning) = message.reasoning_content.take() {
                let mut thinking = TranscriptMessage::thinking(reasoning);
                thinking.provider = provider.clone();
                thinking.model = model.clone();
                thinking.turn = Some(restored_round);
                thinking.set_thinking_duration(0);
                // Honor the configured default expand state for reasoning
                // traces so resumed sessions match live behavior.
                if config::thinking_default_expanded(config) {
                    thinking.set_thinking_expanded(true);
                }
                restored.push(thinking);
            }
            if let Some(calls) = message.tool_calls.take() {
                for call in calls {
                    // Historical results match by tool name, so use it as the id.
                    // Disclosure is applied when the matching result finishes
                    // the step below (lifecycle-aware default), mirroring live.
                    let mut step = TranscriptMessage::tool_step(
                        call.name.clone(),
                        call.name.clone(),
                        call.arguments,
                    );
                    step.provider = provider.clone();
                    step.model = model.clone();
                    step.turn = Some(restored_round);
                    step.sent_at_ms = message_sent_at_ms;
                    pending_steps
                        .entry(call.name)
                        .or_default()
                        .push_back(restored.len());
                    restored.push(step);
                }
                if message.content.is_empty() {
                    continue;
                }
            }
        }
        if message.role == Role::Tool
            && let Some((name, output)) = parse_tool_result(&message.content)
        {
            // Pair with the earliest unfinished step of this name (O(1) via the
            // per-name queue). Fall back to nothing if no step is pending — an
            // orphan result is then rendered as a plain message below.
            if let Some(idx) = pending_steps.get_mut(name).and_then(|q| q.pop_front()) {
                let item = &mut restored[idx];
                if item.finish_tool_step(name, output, neenee_core::ToolOutput::text(output), 0) {
                    // Apply the lifecycle-aware default disclosure so
                    // restored steps match live (Failed/Denied expand,
                    // Ok follows per-tool config).
                    if let Some(status) = item.tool_step_status() {
                        let default =
                            step_interaction::default_tool_expanded(status, name, config, false);
                        item.set_tool_step_expanded(default);
                    }
                    continue;
                }
            }
        }
        if let Some(mut transcript_message) = transcript_message_from_core(message) {
            if transcript_message.role == Role::Assistant {
                transcript_message.turn = Some(restored_round);
            } else if transcript_message.role == Role::User
                && transcript_message.origin == UserMessageOrigin::Insert
            {
                transcript_message.turn = Some(restored_round.saturating_add(1));
            }
            restored.push(transcript_message);
        }
    }
    restored
}

/// Freeze any in-flight reasoning traces in `messages`.
///
/// A reasoning trace is rendered as "running" (breathing spinner) for as
/// long as its `duration_ms` is `None`. The trace normally reaches that
/// terminal state when `StreamReasoningEnd` arrives. But when a turn ends
/// first — the user interrupts, the provider errors mid-stream, or a fresh
/// turn supersedes a still-streaming one — that event never arrives and the
/// spinner would breathe forever. This sweep stamps `duration_ms` on every
/// still-streaming trace so the marker freezes on its last token.
///
/// `duration_ms` is the elapsed reasoning time if known (e.g. captured from
/// the stream start); `None` means the start instant was already consumed
/// or never recorded, in which case `0` is used so the trace still leaves
/// the streaming state.
pub(super) fn finalize_streaming_reasoning(
    messages: &mut [TranscriptMessage],
    duration_ms: Option<u64>,
) {
    let stamped = duration_ms.unwrap_or(0);
    for message in messages.iter_mut() {
        if message.is_thinking_streaming() {
            message.set_thinking_duration(stamped);
        }
    }
}

pub(super) fn parse_tool_result(content: &str) -> Option<(&str, &str)> {
    let content = content.strip_prefix('[')?;
    let (name, output) = content.split_once(" result]:")?;
    Some((name, output.trim_start_matches('\n')))
}

pub(super) fn format_tool_call(name: &str, arguments: &str) -> String {
    let arguments = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| arguments.to_string());
    format!("Calling `{}`\n\n```json\n{}\n```", name, arguments)
}
