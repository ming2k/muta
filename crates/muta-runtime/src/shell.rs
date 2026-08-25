//! The `!`-prefix shell-command path, extracted from `main.rs`. Executes a
//! command directly through the `bash` tool, bypassing the LLM, and emits the
//! same lifecycle events as a normal tool step (`ToolCall` → live
//! `ToolStream` → `ToolResult` / `ToolCancelled`) so the existing render path
//! picks it up unchanged.

use muta_agent::Agent;
use muta_agent::orchestration::{round_response, send_harness_state};
use muta_agent::tools::BashTool;
use muta_agent::{RoundBegin, RoundLifecycle};
use muta_contracts::{AgentResponse, LoopStatus, RoundEvent, Tool, ToolOutput, ToolStream};
use muta_persistence::session::SessionStore;
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
    project_root: std::path::PathBuf,
) {
    // Record the shell invocation in the durable command ledger so it
    // survives resume/export/audit (ADR-0091, revising ADR-0050's echo-in-
    // message-stream mechanism). Persist the literal `!command` (matching the
    // TUI's live display) as a `CommandRecord` under the `"shell"` name with
    // `result: None` — the invocation is durable, the reply is not persisted.
    // The tool result stays ephemeral: it surfaces live via the ToolResult
    // event and mirroring it durably would duplicate the model-driven bash
    // path (ADR-0050's boundary, retained).
    let echo_text = format!("!{}", command);
    if let Err(error) = session
        .mutate_commands(|records| {
            records.push(muta_contracts::CommandRecord::new(
                "shell",
                echo_text.clone(),
            ));
        })
        .await
    {
        tracing::warn!(?error, command = %echo_text, "could not persist shell command record");
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
        // Park the superseded reason before cancelling (C11): the `!` command
        // replacing a live round is the same supersede semantics as a new
        // chat message.
        lifecycle.record_interrupt(muta_contracts::RoundInterruptReason::Superseded);
        agent.reject_pending_permissions();
        agent.reject_pending_user_questions();
        agent.reject_pending_inputs();
        let _ = tx.send(AgentResponse::PermissionsCleared);
        previous.cancel();
    }
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

    // Run in the session's workspace root, not the daemon process's cwd
    // (ADR-0096): the `!` path must land in the same project the model-driven
    // bash tool does.
    let shell_env: Arc<dyn muta_contracts::ExecutionEnvironment> = Arc::new(
        muta_agent::execution::WorkspaceExecutionEnvironment::new(project_root),
    );
    let bash = BashTool::with_env(shell_env);
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
        muta_contracts::StdinPolicy::default(),
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
