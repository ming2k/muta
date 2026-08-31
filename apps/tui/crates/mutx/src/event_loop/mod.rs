//! The main TUI event/render loop and its modular subsystems.

pub(crate) mod actions;
pub(crate) mod input_reader;
pub(crate) mod probes;
pub(crate) mod render;
pub(crate) mod runtime;
pub(crate) mod sync;
pub(crate) mod transcript;

#[allow(unused_imports)]
pub(crate) use actions::{effective_reasoning_effort, modal_page_step};
pub(crate) use probes::probe_input_selection_relay;
pub(crate) use runtime::{
    CompletionSignal, NoticeToastSignal, OauthAddSignal, OutboxSignal, SideViewSignal, UiRuntime,
    UnsentInput, now_epoch_ms,
};
#[cfg(test)]
pub(crate) use transcript::focused_messages_mut;
#[allow(unused_imports)]
pub(crate) use transcript::{apply_transcript_patch, display_status, resolve_focused_mut};

#[cfg(test)]
pub(crate) use actions::host_test_shims;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use actions::{
    handle_close_modal, handle_ctrl_c, handle_esc_interrupt, handle_modal_down, handle_modal_up,
    handle_send_slash,
};

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossterm::event::Event;
use mutx_engine::Terminal;
use tokio::sync::{Mutex, mpsc};

use muta_contracts::{AgentRequest, LoopStatus, ProviderPickerSnapshot};

use crate::App;
use crate::clipboard;
use crate::clipboard_ops;
use crate::input::{self};
use crate::modal::Modal;
use crate::model::document::TranscriptMessage;

use input_reader::InputReader;
use probes::{probe_config_dropdown, probe_delete_overlay};
use sync::{
    drain_outbox_signals, sync_runtime_state_to_app, sync_transcripts_and_session,
    tick_toast_timers,
};

pub(crate) fn tool_verb_for(name: &str) -> crate::phase::ToolVerb {
    match name {
        "find_files" | "list_dir" | "read_image" | "read_text" | "use_skill" | "fetch_url" => {
            crate::phase::ToolVerb::Exploring
        }
        "search_text" => crate::phase::ToolVerb::Searching,
        "search_web" => crate::phase::ToolVerb::WebSearching,
        "write_file" | "edit_file" => crate::phase::ToolVerb::Editing,
        "run_command" | "execute_command" | "bash" => crate::phase::ToolVerb::Running,
        "write_todos" | "update_todo" | "todo" | "todo_update" => {
            crate::phase::ToolVerb::UpdatingTasks
        }
        "spawn_runner" | "runner" | "runner_code" | "runner_mcp" => {
            crate::phase::ToolVerb::Delegating
        }
        n if n.starts_with("mcp__") => crate::phase::ToolVerb::Mcp,
        _ => crate::phase::ToolVerb::Generic,
    }
}

pub(crate) async fn attribution(
    provider: &Arc<Mutex<String>>,
    model: &Arc<Mutex<String>>,
) -> (String, String) {
    (provider.lock().await.clone(), model.lock().await.clone())
}

pub(crate) async fn picker_effort(
    picker: &Arc<Mutex<ProviderPickerSnapshot>>,
    provider: &Arc<Mutex<String>>,
    model: &Arc<Mutex<String>>,
) -> Option<String> {
    let provider = provider.lock().await.clone();
    let model = model.lock().await.clone();
    let picker = picker.lock().await;
    picker
        .rows
        .iter()
        .find(|row| row.id == provider)
        .and_then(|row| row.model_info.iter().find(|m| m.model == model))
        .and_then(|m| {
            let show = match m.protocol.as_str() {
                "anthropic" => m.thinking == Some(true),
                _ => m.effort.is_some(),
            };
            show.then(|| m.effort.clone()).flatten()
        })
}

async fn drain_unsent_input(app: &mut App, runtime: &UiRuntime) {
    if let Some(unsent) = runtime.unsent_input_signal.lock().await.take() {
        app.input = unsent.prompt;
        app.pending_images = unsent.images;
        app.cursor_position = app.input.chars().count();
    }
}

fn auto_dispatch_ready_round(app: &mut App, session_id: &str) {
    if app.loop_status != LoopStatus::Idle {
        return;
    }
    if let Some(dispatch) = app.begin_next_round_dispatch(session_id) {
        let sent_at_ms = now_epoch_ms();
        let _ = app.tx.send(AgentRequest::Prompt {
            text: dispatch.text,
            images: dispatch.images,
            sent_at_ms: Some(sent_at_ms),
        });
    }
}

pub async fn run_app_loop(
    terminal: &mut Terminal<std::io::Stdout>,
    app: &mut App,
    runtime: UiRuntime,
    session: crate::SessionSource,
) -> io::Result<()> {
    let (copy_tx, mut copy_rx) =
        mpsc::unbounded_channel::<Result<clipboard::CopyOutcome, String>>();
    let copy_pending = Arc::new(AtomicUsize::new(0));

    let (paste_tx, mut paste_rx) = mpsc::unbounded_channel::<clipboard::ClipboardRead>();

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Event>();
    let _input_reader = InputReader::spawn(input_tx)?;

    let mut sgr_guard = input::SgrLeakGuard::default();

    let mut input_redraw_pending = true;
    let mut was_animating = true;
    let mut sessions_overview_rev_seen: u64 = 0;
    let mut host_sessions_rev_seen: u64 = 0;

    loop {
        if app.should_quit.load(Ordering::SeqCst) {
            tracing::info!(reason = "should_quit_flag", "app exiting");
            return Ok(());
        }

        let mut frame_dirty = input_redraw_pending;
        input_redraw_pending = false;

        while let Ok(result) = copy_rx.try_recv() {
            clipboard_ops::set_copy_feedback(app, result);
            app.copy_toast_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(1800));
            frame_dirty = true;
        }

        while let Ok(read) = paste_rx.try_recv() {
            clipboard_ops::apply_clipboard_paste(app, read);
            frame_dirty = true;
        }

        sync_runtime_state_to_app(
            app,
            &runtime,
            &mut sessions_overview_rev_seen,
            &mut host_sessions_rev_seen,
        )
        .await;

        tick_toast_timers(app, &runtime).await;

        if app.step_input_drag_scroll() {
            frame_dirty = true;
        }

        let (displayed_transcript_changed, viewed_session_id) =
            sync_transcripts_and_session(app, &runtime).await;

        drain_outbox_signals(app, &runtime).await;
        drain_unsent_input(app, &runtime).await;
        auto_dispatch_ready_round(app, &viewed_session_id);

        if let Some(signal) = runtime.completion_signal.lock().await.take() {
            app.apply_backend_completions(
                signal.request_id,
                signal.input,
                signal.cursor,
                signal.items,
            );
            frame_dirty = true;
        }
        app.refresh_backend_completion_request();

        if app.follow_bottom {
            app.scroll = app.max_scroll;
        }

        if let Some(epoch) = app.effort_ignition_epoch
            && crate::effort_ignition::ignition_finished(epoch.elapsed().as_millis())
        {
            app.effort_ignition_epoch = None;
        }

        let empty_state_showing =
            app.focused_messages().is_empty() && app.focus_stack.is_empty() && !app.in_side_view;
        let viewed_animating = app.viewed_chrome().responding;
        let animating = viewed_animating
            || app.provider_retry.is_some()
            || app.copy_toast_until.is_some()
            || app.notice_toast_until.is_some()
            || app.ctrl_c_armed()
            || app.esc_armed()
            || !app.pending_images.is_empty()
            || app.effort_ignition_epoch.is_some()
            || empty_state_showing
            || app.input_drag_scroll.is_some()
            || copy_pending.load(Ordering::SeqCst) > 0;

        let is_typing_active = app.last_key_press.elapsed() < std::time::Duration::from_millis(150);
        let animation_draw = animating && !is_typing_active;

        let needs_draw = frame_dirty
            || animation_draw
            || was_animating
            || runtime.dirty.swap(false, Ordering::AcqRel);
        was_animating = animation_draw;

        let stage_bottom_follow = displayed_transcript_changed && app.follow_bottom;
        let stage_settle = app.scroll_settle_pending && !stage_bottom_follow;

        let painted_scroll = app.scroll;
        if needs_draw {
            if stage_bottom_follow || stage_settle {
                terminal.stage(|f| render::render_frame(app, f, &viewed_session_id))?;
            } else {
                terminal.draw(|f| render::render_frame(app, f, &viewed_session_id))?;
            }
        }

        if displayed_transcript_changed {
            if app.follow_bottom {
                app.scroll = app.max_scroll;
                if stage_bottom_follow {
                    terminal.draw(|f| render::render_frame(app, f, &viewed_session_id))?;
                }
            } else {
                app.scroll = app.scroll.min(app.max_scroll);
            }
        }

        if app.scroll_settle_pending {
            let max = app.max_scroll;
            let current = app.scroll;
            let clamped = current.min(max);
            app.scroll = clamped;
            let clamp_moved_viewport = clamped != current || clamped != painted_scroll;
            if clamp_moved_viewport {
                terminal.draw(|f| render::render_frame(app, f, &viewed_session_id))?;
            }
            app.scroll_settle_pending = false;
        }

        let poll_interval = if animating {
            if copy_pending.load(Ordering::SeqCst) > 0 {
                std::time::Duration::from_millis(16)
            } else if viewed_animating {
                std::time::Duration::from_millis(50)
            } else {
                std::time::Duration::from_millis(100)
            }
        } else {
            std::time::Duration::from_millis(1000)
        };

        tokio::select! {
            biased;
            Some(first_event) = input_rx.recv() => {
                let mut batch = Vec::with_capacity(8);
                batch.push(first_event);
                while let Ok(ev) = input_rx.try_recv() {
                    batch.push(ev);
                }
                for event in batch {
                    let flow = process_one_event(
                        &event,
                        app,
                        terminal,
                        &runtime,
                        &session,
                        &viewed_session_id,
                        &copy_tx,
                        &copy_pending,
                        &paste_tx,
                        &mut sgr_guard,
                        &mut input_redraw_pending,
                    ).await?;
                    if matches!(flow, actions::ActionFlow::Exit) {
                        return Ok(());
                    }
                }
            }
            _ = runtime.dirty_notify.notified() => {
                input_redraw_pending = true;
            }
            _ = tokio::time::sleep(poll_interval) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)] // Keeps event-loop resources borrowed without a second state bundle.
async fn process_one_event(
    event: &Event,
    app: &mut App,
    terminal: &mut Terminal<std::io::Stdout>,
    runtime: &UiRuntime,
    session: &crate::SessionSource,
    viewed_session_id: &str,
    copy_tx: &mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    copy_pending: &Arc<AtomicUsize>,
    paste_tx: &mpsc::UnboundedSender<clipboard::ClipboardRead>,
    sgr_guard: &mut input::SgrLeakGuard,
    input_redraw_pending: &mut bool,
) -> io::Result<actions::ActionFlow> {
    if let Event::Key(_) = event {
        app.last_key_press = std::time::Instant::now();
        if matches!(sgr_guard.feed(event), input::Feed::Drop) {
            return Ok(actions::ActionFlow::Handled);
        }
    }

    if let Event::Key(_) | Event::Paste(_) = event {
        if app.copy_toast_until.is_some() {
            app.copy_toast_until = None;
        }
        if app.notice_toast_until.is_some() {
            app.notice_toast_until = None;
        }
    }

    let active_modal = app.active_modal();
    let is_responding = app.viewed_chrome().responding;
    let completion_kind = app.completion_kind();
    let suppress_completions = matches!(
        active_modal,
        Modal::Help | Modal::Question | Modal::InputInjection | Modal::ViewSwitcher
    );
    let completions = if suppress_completions {
        Vec::new()
    } else {
        app.completions()
    };
    let suggestion_count = completions.len();
    let has_exact_suggestion = completions
        .iter()
        .any(|c| c.insert_text == app.input || c.label == app.input);
    let suggestion_index = app.suggestion_index;
    let completion_dismissed = app.completion_dismissed;
    let has_trigger_text = app.completion_trigger_text_present();
    let permission_confirm_always = app.permission_confirm_always;
    let permission_show_details = app.permission_show_details;
    let in_runner_view = app.in_runner_view();
    let in_side_view = app.in_side_view;
    let has_focused_target = app.focused_target.is_some();
    let has_queued = app.pending_count(viewed_session_id) > 0;
    let queue_pointer_armed = app.queue_pointer.is_some();
    let history_searching = app.history_search;
    let model_searching = app.model_search;
    let modal_keymap_open = app.modal_keymap_open;
    let custom_provider_field =
        if active_modal == Modal::CustomProvider && app.custom_text_field_focused() {
            Some(app.custom_field)
        } else {
            None
        };
    let editor_field = if active_modal == Modal::ModelEditor {
        Some(app.editor_field)
    } else {
        None
    };
    let question_other_highlighted = app
        .question
        .as_ref()
        .is_some_and(|q| q.is_other_highlighted());
    let history_clear_confirm = app.history_clear_confirm;
    let host_prompting = app.host_prompting;
    let session_info_detail = app.session_info_detail;
    let connection_info_detail = app.connection_info_detail;

    let modal_cmd_history: Option<String> = if matches!(event, Event::Key(k) if k.code == crossterm::event::KeyCode::Enter)
        && active_modal == Modal::None
        && app.input.starts_with('/')
    {
        Some(app.input.clone())
    } else {
        None
    };

    let recognized_command = app.input.starts_with('/')
        && crate::completion::resolved_slash_command_len(&app.input, &app.command_catalog)
            .is_some();

    let action = if let Some(dropdown_action) = probe_config_dropdown(app, event) {
        dropdown_action
    } else if let Some(delete_action) = probe_delete_overlay(app, event) {
        delete_action
    } else if let Some(relay) = probe_input_selection_relay(app, event) {
        relay
    } else {
        input::process_event(
            event.clone(),
            &mut app.input,
            &mut app.cursor_position,
            input::InputContext {
                active_modal,
                session_info_detail,
                connection_info_detail,
                is_responding,
                completion_kind,
                suggestion_count,
                has_exact_suggestion,
                suggestion_index,
                completion_dismissed,
                has_trigger_text,
                permission_confirm_always,
                permission_show_details,
                in_runner_view,
                in_side_view,
                has_focused_target,
                has_queued,
                queue_pointer_armed,
                history_searching,
                model_searching,
                modal_keymap_open,
                custom_provider_field,
                editor_field,
                question_other_highlighted,
                history_clear_confirm,
                host_prompting,

                config_focus: app.config_focus,
                leader_chord: app.leader_chord,
            },
            &mut app.drag,
        )
    };

    let action = if matches!(action, input::InputAction::SendSlash(_)) && !recognized_command {
        if let input::InputAction::SendSlash(text) = action {
            input::InputAction::SendChat(text)
        } else {
            action
        }
    } else {
        action
    };

    if action.is_text_modal_command()
        && let Some(entry) = modal_cmd_history
    {
        let (name, args) = actions::split_command_word(&entry);
        runtime
            .messages
            .write()
            .await
            .push(TranscriptMessage::pending_command(name, args).with_sent_at_ms(now_epoch_ms()));
        app.record_input_history(entry, Vec::new(), Vec::new());
    }

    let flow = actions::dispatch_action(
        app,
        terminal,
        action,
        &mut actions::ActionContext {
            runtime,
            session,
            viewed_session_id,
            copy_tx,
            copy_pending,
            paste_tx,
            sgr_guard,
        },
    )
    .await;

    if matches!(event, Event::Key(_) | Event::Mouse(_) | Event::Paste(_)) {
        *input_redraw_pending = true;
    }

    let completions = if suppress_completions {
        Vec::new()
    } else {
        app.completions()
    };
    app.anchor_completion_selection(&completions);

    Ok(flow)
}
