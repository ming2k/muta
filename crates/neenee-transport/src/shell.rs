//! The `!`-prefix shell-command path, extracted from `main.rs`. Executes a
//! command directly through the `bash` tool, bypassing the LLM, and emits the
//! same lifecycle events as a normal tool step (`ToolCall` → live
//! `ToolStream` → `ToolResult` / `ToolCancelled`) so the existing render path
//! picks it up unchanged.

use neenee_agent::Agent;
use neenee_agent::orchestration::{round_response, send_harness_state, stop_superseded_pursuit};
use neenee_agent::tools::BashTool;
use neenee_agent::{RoundBegin, RoundLifecycle};
use neenee_core::{AgentResponse, LoopStatus, Message, RoundEvent, Tool, ToolOutput, ToolStream};
use neenee_persistence::session::SessionStore;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Execute a `!`-prefixed shell command directly through the `bash` tool,
/// bypassing the LLM. Emits the same lifecycle events as a normal tool step
/// (`ToolCall` → live `ToolStream` → `ToolResult` or `ToolCancelled`) so the
/// existing render path picks it up unchanged.
///
/// Cancellation mirrors `start_interactive_round`: the round lifecycle's
/// `begin` installs a fresh token (any previous round is cancelled through
/// the returned predecessor) and bumps the generation so a later round
/// supersedes a still-running shell command and its tail-end events do not
/// race with the new round.
pub async fn run_shell_command(
    command: String,
    tx: mpsc::UnboundedSender<AgentResponse>,
    session_id: String,
    lifecycle: Arc<RoundLifecycle>,
    agent: Arc<Agent>,
    session: Arc<SessionStore>,
) {
    // Record the shell invocation as a durable, non-driving echo so it
    // survives resume/export/audit (ADR-0050). Persist the literal `!command`
    // (matching the TUI's live display), never sent to the model. The tool
    // result stays ephemeral — it surfaces live via the ToolResult event and
    // mirroring it durably would duplicate the model-driven bash path.
    let echo_text = format!("!{}", command);
    if let Err(error) = session
        .mutate_messages(|w| w.push(Message::command_echo(&echo_text)))
        .await
    {
        tracing::warn!(?error, command = %echo_text, "could not persist shell echo");
    }
    let call_id = format!("shell_{}", uuid::Uuid::new_v4());
    let arguments = serde_json::json!({ "command": command }).to_string();

    // Mirror start_interactive_round: begin a new round, cancelling any
    // in-flight predecessor, so we can tell on exit whether we are still the
    // active round. Every parked request owned by the predecessor is settled
    // before its cancellation token is released.
    let RoundBegin {
        token,
        generation,
        previous,
    } = lifecycle.begin().await;
    if let Some(previous) = previous {
        agent.reject_pending_permissions();
        agent.reject_pending_user_questions();
        agent.reject_pending_inputs();
        let _ = tx.send(AgentResponse::PermissionsCleared);
        previous.cancel();
    }
    stop_superseded_pursuit(
        &agent,
        &session,
        &tx,
        &session_id,
        "superseded by a direct shell command",
    )
    .await;
    let is_current = || lifecycle.is_current(generation);

    // Surface the synthetic tool step starting. The response listener maps
    // `name: "bash"` to the "running command" activity status.
    let _ = tx.send(round_response(
        &session_id,
        RoundEvent::ToolCall {
            id: call_id.clone(),
            name: "bash".to_string(),
            arguments: arguments.clone(),
        },
    ));

    let bash = BashTool;
    let tx_for_stream = tx.clone();
    let session_id_for_stream = session_id.clone();
    let call_id_for_stream = call_id.clone();
    let mut on_stream = move |stream: ToolStream| {
        if !is_current() {
            return;
        }
        let _ = tx_for_stream.send(round_response(
            &session_id_for_stream,
            RoundEvent::ToolStream {
                id: call_id_for_stream.clone(),
                stream,
            },
        ));
    };

    // The `!` passthrough is a user-direct shell invocation. We use the safe
    // Closed default here too (consistent with the model-driven path); a
    // future enhancement may let the `!` channel opt into a PTY or human
    // input injection for truly interactive commands, but that is a separate
    // UX decision from the autonomous-agent stdin contract.
    let run = bash.call_structured_with_events(
        "",
        &arguments,
        Box::new(|_| {}),
        &mut on_stream,
        neenee_core::StdinPolicy::default(),
    );

    tokio::select! {
        biased;
        _ = token.cancelled() => {
            // Ctrl+C (or a newer round replacing us): dropping `run` kills
            // the child via `kill_on_drop`. Only emit the cancellation
            // event if we are still the active round — a newer round's
            // ToolCall events must not be flattened by our exit.
            if is_current() {
                let _ = tx.send(round_response(
                    &session_id,
                    RoundEvent::ToolCancelled {
                        id: call_id,
                        name: "bash".to_string(),
                    },
                ));
            }
        }
        result = run => if is_current() {
            match result {
                Ok(structured) => {
                    let output = structured.to_text();
                    let _ = tx.send(round_response(
                        &session_id,
                        RoundEvent::ToolResult {
                            id: call_id,
                            name: "bash".to_string(),
                            output,
                            structured,
                            duration_ms: 0,
                        },
                    ));
                }
                Err(error) => {
                    let structured = ToolOutput::Text(error.clone());
                    let _ = tx.send(round_response(
                        &session_id,
                        RoundEvent::ToolResult {
                            id: call_id,
                            name: "bash".to_string(),
                            output: error,
                            structured,
                            duration_ms: 0,
                        },
                    ));
                }
            }
        },
    }

    // Release the round and flip the harness to idle, matching
    // start_interactive_round's cleanup. Guarded by the generation check
    // (`finish` returns false for a superseded round) so a newer round is not
    // reset by our exit.
    if lifecycle.finish(generation).await {
        send_harness_state(&tx, &session_id, &agent, LoopStatus::Idle);
        let _ = tx.send(round_response(
            &session_id,
            RoundEvent::Activity(String::new()),
        ));
    }
}
