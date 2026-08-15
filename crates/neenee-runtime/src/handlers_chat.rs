//! Chat-round and `!`-prefix shell-command handlers, extracted verbatim from
//! the agent background task's `match req { … }` dispatch.
//!
//! Each handler is one match arm, lifted unchanged. Parameters are named to
//! match the original loop locals (`active_view_side`, `side`, `agent`,
//! `history`, `session`, `lifecycle`, `resp_tx`, `pursuit_service`, `config`,
//! …) so the body reads exactly as it did inline.

use neenee_agent::orchestration::{RoundInput, round_response};
use neenee_agent::{Agent, RoundLifecycle};
use neenee_contracts::{AgentResponse, QueuedUserInput, RoundEvent};
use neenee_persistence::{config::Config, session::SessionStore};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::shell::run_shell_command;
use crate::side::{
    SideSession, refuse_if_no_provider, start_active_turn, start_session_turn, target_agent,
};

/// `AgentRequest::Chat` — start an interactive round against whichever session
/// the user is currently composing into (primary or `/btw` side).
#[allow(clippy::too_many_arguments)]
pub async fn chat(
    active_view_side: &AtomicBool,
    side: &Arc<AsyncRwLock<Option<SideSession>>>,
    agent: &Arc<Agent>,
    session: &Arc<SessionStore>,
    lifecycle: &Arc<RoundLifecycle>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    config: &Config,
    text: String,
    images: Vec<neenee_contracts::ImagePart>,
    sent_at_ms: Option<u64>,
) {
    // Refuse up-front when no real provider is configured: the shared holder
    // is parked on the `NoProvider` sentinel (catalog could not resolve a
    // channel at startup or the last `/models` switch). Failing here keeps
    // the user's text out of the transcript and surfaces a single notice
    // instead of letting the request reach a non-functional provider. Use the
    // same refusal contract as every other round-entry path (`RoundEvent::Error`
    // + idle `HarnessState`): the TUI has already optimistically painted
    // "queued" for this send, and a bare top-level `Error` would leave that
    // state stuck on the activity bar forever.
    if refuse_if_no_provider(resp_tx, agent, &session.id().await) {
        return;
    }
    start_active_turn(
        active_view_side,
        side,
        agent,
        session,
        lifecycle,
        resp_tx,
        config,
        RoundInput {
            prompt: text,
            hidden: false,
            display_prompt: None,
            sent_at_ms,
            images,
        },
    )
    .await;
}

/// Queue an input into the exact live round named by `session_id`. Failure is
/// returned as a scoped event so the frontend can retain the text as a paused
/// next-round item instead of dropping it.
pub async fn insert_user_input(
    side: &Arc<AsyncRwLock<Option<SideSession>>>,
    agent: &Arc<Agent>,
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: String,
    input: QueuedUserInput,
) {
    let accepted = match target_agent(side, agent, session, &session_id).await {
        Some(target) => target.submit_user_input(&session_id, input.clone()),
        None => false,
    };
    if !accepted {
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::UserInputUnavailable { input_id: input.id },
        ));
    }
}

/// Cancel an insert if it has not crossed the agent boundary yet. The agent's
/// queue mutex linearizes this against admission, so the response is final.
pub async fn cancel_inserted_input(
    side: &Arc<AsyncRwLock<Option<SideSession>>>,
    agent: &Arc<Agent>,
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: String,
    input_id: String,
) {
    let cancelled = match target_agent(side, agent, session, &session_id).await {
        Some(target) => target.cancel_user_input(&session_id, &input_id).is_some(),
        None => false,
    };
    let event = if cancelled {
        RoundEvent::UserInputCancelled { input_id }
    } else {
        RoundEvent::UserInputCancelFailed { input_id }
    };
    let _ = resp_tx.send(round_response(&session_id, event));
}

/// Dispatch a paused outbox item into a fresh round without consulting the
/// frontend's current view. If its side session vanished, hand ownership back
/// to the outbox through `UserInputUnavailable`.
#[allow(clippy::too_many_arguments)]
pub async fn chat_to_session(
    side: &Arc<AsyncRwLock<Option<SideSession>>>,
    agent: &Arc<Agent>,
    session: &Arc<SessionStore>,
    lifecycle: &Arc<RoundLifecycle>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    config: &Config,
    session_id: String,
    input: QueuedUserInput,
) {
    let started = start_session_turn(
        &session_id,
        side,
        agent,
        session,
        lifecycle,
        resp_tx,
        config,
        RoundInput {
            prompt: input.text.clone(),
            hidden: false,
            display_prompt: input.display_text.clone(),
            sent_at_ms: input.sent_at_ms,
            images: input.images.clone(),
        },
    )
    .await;
    if !started {
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::UserInputUnavailable { input_id: input.id },
        ));
    } else {
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::NextRoundStarted(input),
        ));
    }
}

/// `AgentRequest::ShellCommand` — the `!` prefix path: run the command
/// directly through the `bash` tool, bypassing the LLM. The lifecycle mirrors
/// a normal tool step (`ToolCall` → live `ToolStream` → `ToolResult`) so the
/// existing render path picks it up with no special-casing. Spawned onto its
/// own task so it runs concurrently with the dispatch loop.
pub async fn shell(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    lifecycle: &Arc<RoundLifecycle>,
    agent: &Arc<Agent>,
    session: &Arc<SessionStore>,
    command: String,
) {
    let shell_tx = resp_tx.clone();
    let shell_lifecycle = lifecycle.clone();
    let shell_agent = agent.clone();
    let shell_session = session.clone();
    let shell_session_id = session.id().await;
    tokio::spawn(async move {
        run_shell_command(
            command,
            shell_tx,
            shell_session_id,
            shell_lifecycle,
            shell_agent,
            shell_session,
        )
        .await;
    });
}
