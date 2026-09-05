//! Per-frame runtime state synchronization (hydrating App from UiRuntime).

use std::sync::atomic::Ordering;

use muta_contracts::Role;

use crate::App;
use crate::event_loop::runtime::{OauthAddSignal, OutboxSignal, UiRuntime, now_epoch_ms};
use crate::event_loop::transcript::{
    apply_height_invalidation, apply_transcript_patch, displayed_transcript_did_change,
};
use crate::modal::Modal;
use crate::model::document::{TranscriptMessage, UserMessageOrigin};

/// Mirror shared runtime state into `App` each frame.
pub(crate) async fn sync_runtime_state_to_app(
    app: &mut App,
    runtime: &UiRuntime,
    sessions_overview_rev_seen: &mut u64,
    host_sessions_rev_seen: &mut u64,
) {
    app.current_provider = runtime.current_provider.lock().await.clone();
    app.current_model = runtime.current_model.lock().await.clone();
    let harness = runtime.harness.lock().await.clone();
    app.loop_status = harness.loop_status;
    app.delegated = harness.delegated;
    app.unconfined = harness.unconfined;
    app.harness_retry_pending = harness.retry_pending;
    app.provider_retry = runtime.provider_retry.lock().await.clone();
    app.phase = runtime.phase.lock().await.clone();
    app.todos = runtime.todos.lock().await.clone();
    app.round_count = *runtime.round_count.lock().await;
    app.current_turn = *runtime.current_turn.lock().await;
    app.round_started_at = *runtime.round_started_at.lock().await;
    {
        let pending = runtime.pending_permission.lock().await;
        app.pending_permission = pending.front().cloned();
        app.pending_permission_depth = pending.len();
    }
    app.key_status = runtime.key_status.lock().await.clone();
    app.websearch_config = runtime.websearch_config.lock().await.clone();
    app.provider_picker = runtime.provider_picker.lock().await.clone();

    let request_sheet_open = app.active_sheet().is_some();

    if app.pending_permission.is_some() && !request_sheet_open {
        app.push_sheet_surface(crate::sheet::Permission);
        app.modal_index = 0;
        app.permission_scroll = 0;
        app.permission_show_details = false;
        app.focused_target = None;
    } else if app.pending_permission.is_none()
        && app.active_sheet() == Some(crate::sheet::Permission)
    {
        app.dismiss_sheet();
        app.modal_index = 0;
        app.permission_confirm_always = false;
        app.permission_scroll = 0;
        app.permission_max_scroll = 0;
        app.permission_show_details = false;
    }

    // Question modal sync
    {
        let pending = runtime.pending_question.lock().await;
        let front = pending.front().cloned();
        // ADR-0175 §7: the badge reports items queued *behind* the one
        // already mounted on the sheet — not counting the mounted one
        // itself. The permission sheet's predicate is `> 1` for the
        // same reason; the question sheet's renderer uses `> 0`, so the
        // depth must subtract the front item here.
        app.pending_question_depth = pending.len().saturating_sub(1);
        drop(pending);
        let model_matches_front = match (&app.question, &front) {
            (Some(m), Some(req)) => m.request().id == req.id,
            (None, None) => true,
            _ => false,
        };
        if !model_matches_front {
            if let Some(req) = front {
                app.question = Some(crate::question_model::QuestionModel::open(req));
                app.question_scroll = 0;
                app.question_modal_follow = true;
                app.modal_index = 0;
                app.focused_target = None;
            } else {
                app.question = None;
                if app.active_sheet() == Some(crate::sheet::Question) {
                    app.dismiss_sheet();
                    app.modal_index = 0;
                }
            }
        }
        if app.question.is_some() && !request_sheet_open {
            app.push_sheet_surface(crate::sheet::Question);
            app.modal_index = 0;
            app.focused_target = None;
        }
    }

    // ADR-0175: PreAttach interstitial sync. Two flows:
    //
    // 1. **Mount.** The listener task published a fresh quarantined
    //    snapshot through `pre_attach_signal`. Drain the cell and
    //    build a `PreAttachState` from it. Mounting is suppressed
    //    when the per-run `trust_gate_dismissed` latch is set (the
    //    operator dismissed earlier in this run) or when `pre_attach`
    //    is already mounted — the listener only republishes on
    //    snapshot change, so a duplicate publication is a no-op.
    //
    // 2. **Unmount.** When the snapshot transitions to `Trusted`
    //    (the operator chose "Trust all" → daemon persisted via
    //    `/trust` → republished `HarnessState`), clear `pre_attach`
    //    so the chat surface mounts on the next frame. The harness
    //    snapshot lives on `UiRuntime::harness` and was already
    //    mirrored into `app` higher in this function via
    //    `runtime.harness`; reading it through the runtime cell
    //    avoids racing the listener.
    {
        let mut signal = runtime.pre_attach_signal.lock().await;
        if let Some(pub_) = signal.take() {
            let dismissed = runtime.trust_gate_dismissed.load(Ordering::SeqCst);
            if !dismissed
                && let Some(state) = crate::PreAttachState::from_snapshot(&pub_.snapshot)
                && app.pre_attach.is_none()
            {
                tracing::info!("mutx: mounting PreAttach interstitial");
                app.pre_attach = Some(state);
                // PreAttach claims the keyboard; reset the
                // composer/sheet state the way an ordinary
                // sheet mount does, so nothing downstream
                // assumes it owns input.
                app.modal_index = 0;
                app.focused_target = None;
            }
        }
        drop(signal);

        // Unmount when the snapshot turns trusted or the workspace
        // is no longer quarantined. The harness snapshot on
        // `UiRuntime::harness` is the freshest one the listener has
        // observed; reading it through the runtime cell avoids racing
        // the listener's own `harness_clone` update.
        if app.pre_attach.is_some() {
            let harness = runtime.harness.lock().await;
            let trusted = harness.workspace_security.aggregate()
                == muta_contracts::WorkspaceTrustState::Trusted
                || harness.workspace_security.aggregate()
                    == muta_contracts::WorkspaceTrustState::Absent;
            if trusted {
                tracing::info!("mutx: clearing PreAttach interstitial (workspace trusted)");
                app.pre_attach = None;
                // The latch is set so a subsequent periodic republish
                // of a still-quarantined snapshot (race with the
                // `/trust` reload) does not re-mount within this run.
                runtime.trust_gate_dismissed.store(true, Ordering::SeqCst);
            }
        }
    }

    // Input-injection modal sync
    {
        let front = runtime.pending_input.lock().await.front().cloned();
        let matches_front = match (&app.pending_input, &front) {
            (Some(cur), Some(req)) => cur.id == req.id,
            (None, None) => true,
            _ => false,
        };
        if !matches_front {
            if let Some(req) = front {
                app.pending_input = Some(req);
                app.modal_index = 0;
                app.focused_target = None;
            } else {
                app.pending_input = None;
                if app.active_sheet() == Some(crate::sheet::InputInjection) {
                    app.restore_input_draft();
                    app.dismiss_sheet();
                    app.modal_index = 0;
                }
            }
        }
        if app.pending_input.is_some() && !request_sheet_open {
            app.park_input_draft();
            app.push_sheet_surface(crate::sheet::InputInjection);
            app.modal_index = 0;
            app.focused_target = None;
        }
    }

    // Sessions overview revision check
    {
        let rev = runtime.sessions_overview_rev.load(Ordering::Acquire);
        if rev != *sessions_overview_rev_seen {
            app.sessions_overview = runtime.sessions_overview.lock().await.clone();
            app.sessions_loading = false;
            *sessions_overview_rev_seen = rev;
        }
    }

    let can_apply_backend_navigation = app.can_accept_navigation_signal();
    let view_session_id = app.current_session_id.clone();
    if can_apply_backend_navigation && runtime.open_sessions.swap(false, Ordering::SeqCst) {
        crate::event_loop::actions::enter_panel(
            app,
            crate::surfaces::PanelId::Sessions,
            runtime,
            &view_session_id,
        );
    }
    if can_apply_backend_navigation && runtime.open_tree.swap(false, Ordering::SeqCst) {
        crate::event_loop::actions::enter_panel(
            app,
            crate::surfaces::PanelId::Tree,
            runtime,
            &view_session_id,
        );
    }

    // Host sessions revision check
    {
        let rev = runtime.host_sessions_rev.load(Ordering::Acquire);
        if rev != *host_sessions_rev_seen {
            app.host_sessions = runtime.host_sessions.lock().await.clone();
            *host_sessions_rev_seen = rev;
        }
    }

    // Console logs
    {
        let mut queue = runtime.host_console_signal.lock().await;
        while let Some(line) = queue.pop_front() {
            app.host_console_log.push(line);
        }
    }

    if can_apply_backend_navigation && runtime.open_host.swap(false, Ordering::SeqCst) {
        crate::event_loop::actions::enter_view(app, crate::surfaces::View::Dashboard, runtime);
    }

    if let Some(detail) = runtime.session_detail.lock().await.take() {
        let same_id = app.session_detail.as_ref().map(|s| &s.id) == Some(&detail.id);
        app.session_detail = Some(detail);
        if !same_id {
            app.session_info_scroll = 0;
        }
    }
    if let Some(snapshot) = runtime.session_context.lock().await.take() {
        app.session_context = Some(snapshot);
    }
    if let Some(detail) = runtime.connection_detail.lock().await.take() {
        let same_id = app.connection_detail.as_ref().map(|c| &c.id) == Some(&detail.id);
        app.connection_detail = Some(detail);
        if !same_id {
            app.connection_info_scroll = 0;
        }
    }
    if let Some(report) = runtime.token_report.lock().await.take() {
        app.token_report = Some(report);
    }
    if let Some(report) = runtime.usage_stats.lock().await.take() {
        app.usage_stats = Some(report);
    }

    if let Some(sig) = runtime.oauth_add_signal.lock().await.take() {
        match sig {
            OauthAddSignal::Pending {
                url,
                user_code,
                message,
            } => {
                if app.awaiting_oauth_add {
                    app.oauth_pending_url = url;
                    app.oauth_pending_user_code = user_code;
                    app.oauth_pending_message = message;
                    app.oauth_pending_error = None;
                    app.replace_transient_surface(Modal::OauthPending);
                }
            }
            OauthAddSignal::Done => {
                if app.awaiting_oauth_add {
                    app.open_oauth_instance_name_editor();
                }
            }
            OauthAddSignal::Failed { message } => {
                if app.awaiting_oauth_add {
                    app.oauth_pending_error = Some(message);
                    app.replace_transient_surface(Modal::OauthPending);
                }
            }
        }
    }

    runtime
        .awaiting_oauth_add
        .store(app.awaiting_oauth_add, Ordering::SeqCst);
}

pub(crate) async fn tick_toast_timers(app: &mut App, runtime: &UiRuntime) {
    if let Some(until) = app.copy_toast_until
        && std::time::Instant::now() >= until
    {
        app.copy_toast_until = None;
    }

    if let Some(signal) = runtime.notice_toast_signal.lock().await.take() {
        app.notice_toast_message = signal.text;
        app.notice_toast_severity = signal.severity;
        app.notice_toast_until =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(2600));
    }
    if let Some(until) = app.notice_toast_until
        && std::time::Instant::now() >= until
    {
        app.notice_toast_until = None;
    }

    if !app.pending_images.is_empty() && !app.ctrl_c_armed() {
        let n = app.pending_images.len();
        show_local_toast(
            app,
            format!(
                "{n} image{} attached — enter to send",
                if n == 1 { "" } else { "s" }
            ),
            false,
            std::time::Duration::from_millis(600),
        );
    }

    app.tick_esc_arm();
}

pub(crate) fn show_local_toast(
    app: &mut App,
    message: impl Into<String>,
    failed: bool,
    duration: std::time::Duration,
) {
    app.copy_toast_message = message.into();
    app.copy_toast_failed = failed;
    app.copy_toast_until = Some(std::time::Instant::now() + duration);
}

fn user_prompt_tail(messages: &[TranscriptMessage]) -> Vec<(String, bool, u64)> {
    messages
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| {
            (
                m.raw.clone(),
                m.origin == UserMessageOrigin::Chat,
                m.sent_at_ms.unwrap_or(0),
            )
        })
        .collect()
}

pub(crate) async fn sync_transcripts_and_session(
    app: &mut App,
    runtime: &UiRuntime,
) -> (bool, String) {
    let messages_version = runtime.messages.version();
    let transcript_changed = messages_version != app.messages_version;
    if transcript_changed {
        let patch = runtime.messages.take_transcript_patch();
        if !apply_transcript_patch(&mut app.messages, patch) {
            app.messages = runtime.messages.read().await.clone();
        }
        app.messages_version = messages_version;
        apply_height_invalidation(
            &mut app.layout_height_cache,
            runtime.messages.take_height_invalidation(),
        );
    }

    let side_messages_version = runtime.side_messages.version();
    let side_transcript_changed = side_messages_version != app.side_messages_version;
    if side_transcript_changed {
        let patch = runtime.side_messages.take_transcript_patch();
        if !apply_transcript_patch(&mut app.side_messages, patch) {
            app.side_messages = runtime.side_messages.read().await.clone();
        }
        app.side_messages_version = side_messages_version;
        apply_height_invalidation(
            &mut app.layout_height_cache,
            runtime.side_messages.take_height_invalidation(),
        );
    }

    app.parent_status = *runtime.parent_status.lock().await;
    app.btw_list = runtime.btw_list.lock().await.clone();
    app.session_chrome = runtime
        .session_chrome
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    let side_view_transitioned = match runtime.side_view_signal.lock().await.take() {
        Some(crate::event_loop::runtime::SideViewSignal::Opened { side_id, .. }) => {
            app.enter_side_view(side_id);
            true
        }
        Some(crate::event_loop::runtime::SideViewSignal::Closed) => {
            app.exit_side_view();
            true
        }
        None => false,
    };

    let displayed_transcript_changed = displayed_transcript_did_change(
        app.in_side_view,
        transcript_changed,
        side_transcript_changed,
        side_view_transitioned,
    );

    let primary_session_id = runtime.live_session_id.lock().await.clone();
    let viewed_session_id = if app.in_side_view {
        app.side_session_id
            .as_deref()
            .unwrap_or(primary_session_id.as_str())
    } else {
        primary_session_id.as_str()
    }
    .to_string();

    if app.current_session_id != viewed_session_id {
        app.current_session_id = viewed_session_id.clone();
        app.on_viewed_session_changed();
        app.switching_session = None;
        *runtime.switching_session.lock().await = None;
    } else {
        app.switching_session = runtime.switching_session.lock().await.clone();
    }

    let backfill_from = app.session_history_backfill_cursor;
    let viewed_len = if app.in_side_view {
        app.side_messages.len()
    } else {
        app.messages.len()
    };
    if backfill_from < viewed_len {
        let tail: Vec<(String, bool, u64)> = if app.in_side_view {
            user_prompt_tail(&app.side_messages[backfill_from..])
        } else {
            user_prompt_tail(&app.messages[backfill_from..])
        };
        app.session_history_backfill_cursor = viewed_len;
        app.backfill_session_history(&tail, now_epoch_ms());
    }

    {
        let mut cell = runtime.viewed_session_id.lock().await;
        *cell = Some(viewed_session_id.clone());
    }

    let workspace = crate::chrome::tilde_home(&app.cwd);
    if app.current_workspace != workspace {
        app.current_workspace = workspace;
    }

    app.context_tokens = runtime
        .context_tokens
        .lock()
        .await
        .get(&viewed_session_id)
        .copied();

    (displayed_transcript_changed, viewed_session_id)
}

pub(crate) async fn drain_outbox_signals(app: &mut App, runtime: &UiRuntime) {
    while let Some(signal) = runtime.outbox_signals.lock().await.pop_front() {
        match signal {
            OutboxSignal::FollowUpStarted {
                session_id,
                input_id,
            } => {
                app.remove_dispatch(&session_id, &input_id);
            }
            OutboxSignal::Unavailable {
                session_id,
                input_id,
            } => {
                let held = app
                    .messages
                    .iter()
                    .chain(app.side_messages.iter())
                    .rev()
                    .find(|m| {
                        m.insert_id.as_deref() == Some(input_id.as_str()) && m.role == Role::User
                    })
                    .map(|m| (m.raw.clone(), Vec::new(), Vec::new()));
                app.requeue_dispatch(&session_id, &input_id, held);
            }
            OutboxSignal::SteerAdmitted {
                session_id,
                input_id,
            } => {
                app.remove_dispatch(&session_id, &input_id);
            }
            OutboxSignal::RoundCompleted { session_id } => {
                app.naturally_completed_sessions.insert(session_id);
            }
            OutboxSignal::RoundInterrupted { session_id } => {
                app.naturally_completed_sessions.remove(&session_id);
                app.block_queue(&session_id);
                for item in app.pending_dispatch.iter_mut() {
                    if item.session_id == session_id
                        && item.state == crate::app::QueuedDispatchState::Dispatching
                    {
                        item.state = crate::app::QueuedDispatchState::Waiting;
                    }
                }
            }
            OutboxSignal::HarnessState { session_id, idle } => {
                if idle {
                    app.running_sessions.remove(&session_id);
                    app.idle_sessions.insert(session_id);
                } else {
                    app.idle_sessions.remove(&session_id);
                    app.naturally_completed_sessions.remove(&session_id);
                    app.running_sessions.insert(session_id);
                }
            }
        }
    }
}
