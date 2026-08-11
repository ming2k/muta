//! The main TUI event/render loop and the helpers that only it needs.
//!
//! [`run_app_loop`] owns the per-frame work: sync shared runtime state into
//! [`App`], draw the chrome via the `render` modules, drain pending input
//! events through [`input::process_event`], and dispatch each
//! [`input::InputAction`] to its handler. State mutations almost always land
//! back on `App`; the few standalone helpers here cover status-text
//! formatting, message-tree navigation, and selection → clipboard extraction.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{self, Event};
use neenee_tui_engine::Terminal;
use tokio::sync::mpsc;

use neenee_core::{
    AgentRequest, HarnessSnapshot, LoopStatus, ParentStatus, PermissionDecision, PermissionRequest,
    ProviderPickerSnapshot, Role, SessionOverview, TodoList, UserQuestionRequest,
};

use crate::tui::clipboard;
use crate::tui::clipboard_ops;
use crate::tui::completion::{CompletionKind, completion_anchor_x, resolved_slash_command_len};
use crate::tui::composer_attachments;
use crate::tui::input::{self};
use crate::tui::interaction::{self, ClickTarget};
use crate::tui::model::document::{
    MessageKind, NoticeSeverity, TranscriptMessage, UserMessageOrigin,
};
use crate::tui::model::layout::{InteractiveTarget, InteractiveTargetKind, LayoutMap};
use crate::tui::model::selection::{
    CellDragInfo, SelectionState, floor_grapheme_boundary, get_selected_text,
    inclusive_grapheme_end,
};
use crate::tui::step_interaction::StepKind;
use crate::tui::versioned::{HeightInvalidation, TranscriptPatch, TranscriptUpdate, Versioned};
use crate::tui::view;
use crate::tui::view::Theme;
use crate::tui::{ActivityTab, App, CaretOwner, Modal, ProviderDeleteChoice, Recess};

use neenee_core::AgentResponse;
use tokio::sync::{Mutex, broadcast};

/// Shared runtime state crossing the response-listener / event-loop boundary.
/// Each field is the single source of truth for one piece of live harness
/// state; the listener writes, the loop reads (after acquiring the per-field
/// mutex for one frame'snapshot).
pub(super) fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Apply only the cache invalidation actually caused by the most recent
/// transcript mutation. A streaming assistant tail is the common long-session
/// case: invalidating that one id preserves the measured heights of every
/// earlier message. Structural/unknown writes remain conservatively global.
fn apply_height_invalidation(cache: &mut view::HeightCache, invalidation: HeightInvalidation) {
    match invalidation {
        HeightInvalidation::None => {}
        HeightInvalidation::Messages(ids) => cache.invalidate_messages(ids),
        HeightInvalidation::All => cache.clear(),
    }
}

/// Whether the transcript slice painted this frame changed shape.
///
/// Primary and `/btw` side transcripts advance independently. Bottom-follow
/// staging must track the one currently on screen: staging every background
/// side update while viewing the primary transcript would add a needless
/// second layout pass, while ignoring side updates reproduces the one-frame
/// stale-scroll flash that staging exists to prevent. A view transition also
/// replaces the painted slice without requiring either transcript version to
/// advance, so it always invalidates the displayed geometry.
fn displayed_transcript_did_change(
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

#[cfg(test)]
mod displayed_transcript_change_tests {
    use super::displayed_transcript_did_change;

    #[test]
    fn tracks_only_the_transcript_currently_on_screen() {
        assert!(displayed_transcript_did_change(false, true, false, false));
        assert!(!displayed_transcript_did_change(false, false, true, false));
        assert!(displayed_transcript_did_change(true, false, true, false));
        assert!(!displayed_transcript_did_change(true, true, false, false));
    }

    #[test]
    fn view_transition_always_changes_displayed_geometry() {
        assert!(displayed_transcript_did_change(false, false, false, true));
        assert!(displayed_transcript_did_change(true, false, false, true));
    }
}

/// Replay high-frequency stream changes into the app-owned transcript. Returns
/// `false` when the local copy cannot safely apply the patch, making the caller
/// fall back to a full snapshot. The fallback preserves correctness across a
/// session replacement, missed update, or unexpected event ordering; ordinary
/// text/tool streaming stays on this cheap path.
pub(super) fn apply_transcript_patch(
    messages: &mut [TranscriptMessage],
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
            TranscriptUpdate::EnvoyEvent {
                parent_call_id,
                event,
            } => messages
                .iter_mut()
                .find(|message| message.tool_step_call_id() == Some(parent_call_id.as_str()))
                .is_some_and(|message| message.push_envoy_event(&event)),
        };
        if !applied {
            return false;
        }
    }
    true
}

pub(super) struct UiRuntime {
    pub current_provider: Arc<Mutex<String>>,
    pub current_model: Arc<Mutex<String>>,
    /// Latest AI-visible context size for the primary session.
    pub context_tokens: Arc<Mutex<HashMap<String, neenee_core::ContextTokenSnapshot>>>,
    /// Latest per-round throughput summary (keyed by session id), surfaced in
    /// the TokenReport modal as an honest tokens/sec that excludes the time
    /// the round spent parked on human decisions.
    pub round_tps: Arc<Mutex<HashMap<String, neenee_core::RoundSummary>>>,
    pub harness: Arc<Mutex<HarnessSnapshot>>,
    pub activity_status: Arc<Mutex<String>>,
    pub pending_permission: Arc<Mutex<VecDeque<PermissionRequest>>>,
    pub pending_question: Arc<Mutex<VecDeque<UserQuestionRequest>>>,
    pub pending_input: Arc<Mutex<VecDeque<neenee_core::InputRequest>>>,
    pub is_responding: Arc<AtomicBool>,
    /// Stage 3 redraw signal. The response listener sets this on every handled
    /// response (the only off-loop source of shared-state change), so the event
    /// loop can skip the per-frame draw entirely while nothing has changed —
    /// turning an idle session from ~10 full relayouts/second into zero. The
    /// loop also draws on input, background clipboard results, and active
    /// animation; this flag covers everything the listener mutates.
    pub dirty: Arc<AtomicBool>,
    /// Stage 4 wakeup. Companion to [`Self::dirty`]: the listener notifies this
    /// after a response so the event loop's `select!` wakes *immediately* to
    /// redraw, instead of waiting out a fixed poll. `notify_one` keeps one
    /// permit, so a notification raised while the loop is mid-render is not
    /// lost — the next `notified()` returns at once.
    pub dirty_notify: Arc<tokio::sync::Notify>,
    /// Full-duplex (ADR-0029): request_id → the parent tool-call id of the
    /// envoy that surfaced a permission or `ask_user` request (carried up
    /// as a `RoundEvent::Envoy`). When the user answers in the modal, the
    /// loop looks the id up here to tag the reply with `parent_call_id` so the
    /// harness routes it down into the live child via the envoy registry.
    /// Top-level requests are absent here → `None` → legacy path. Kept as a
    /// side-table so the modal queue and rendering stay unchanged.
    pub envoy_permission_parent: Arc<Mutex<HashMap<String, String>>>,
    /// Companion to [`Self::envoy_permission_parent`] for `ask_user` replies.
    pub envoy_question_parent: Arc<Mutex<HashMap<String, String>>>,
    pub messages: Arc<Versioned<Vec<TranscriptMessage>>>,
    /// Side-conversation transcript buffer (ADR-0017). The listener appends
    /// per-turn events tagged with the side `session_id` here; the loop
    /// clones it into [`App::side_messages`] each frame while the side view
    /// is active.
    pub side_messages: Arc<Versioned<Vec<TranscriptMessage>>>,
    /// Coarse primary-session status, written by the listener from
    /// [`AgentResponse::ParentStatus`] and read into [`App::parent_status`]
    /// for the side banner (ADR-0017).
    pub parent_status: Arc<Mutex<ParentStatus>>,
    /// One-shot side-view transition (ADR-0017): `Opened` when the harness
    /// emits [`AgentResponse::SideViewOpened`] (the loop calls
    /// [`App::enter_side_view`]), `Closed` on [`AgentResponse::SideViewClosed`]
    /// ([`App::exit_side_view`]). Drained each frame.
    pub side_view_signal: Arc<Mutex<Option<SideViewSignal>>>,
    pub key_status: Arc<Mutex<HashMap<String, bool>>>,
    /// Model-picker snapshot shared with the response listener.
    pub provider_picker: Arc<Mutex<ProviderPickerSnapshot>>,
    /// Sessions picker rows + a one-shot request to open the picker modal.
    pub sessions_overview: Arc<Mutex<Vec<SessionOverview>>>,
    /// Monotonic revision bumped by the response listener every time it
    /// replaces `sessions_overview`. The event loop reads it to skip the deep
    /// `Vec` clone (`~2 strings × every row`) when nothing changed — with a
    /// large project (hundreds of sessions) the unconditional per-iteration
    /// clone dominated the loop and made the picker hitch on every redraw.
    pub sessions_overview_rev: Arc<std::sync::atomic::AtomicU64>,
    /// Latest full detail for one session, written by the listener from
    /// [`AgentResponse::SessionDetail`] and read into [`App::session_detail`]
    /// for the session-info sub-view.
    pub session_detail: Arc<Mutex<Option<neenee_core::SessionDetail>>>,
    /// Latest token-source report fetched from the harness for the viewed
    /// session (attach mode: the ledger is daemon-side). Written by the
    /// listener from [`AgentResponse::TokenUsageReport`] and read into
    /// [`App::token_report`]. In the standalone path the local ledger
    /// ([`App::token_ledger`]) is the source instead and this stays `None`.
    pub token_report: Arc<Mutex<Option<neenee_core::TokenSourceReport>>>,
    pub open_sessions: Arc<AtomicBool>,
    /// Live daemon monitor snapshot for the `/host` control panel
    /// (ADR-0096), maintained by a dedicated monitor client task.
    pub host_sessions: Arc<Mutex<Vec<neenee_core::MonitoredSession>>>,
    /// Revision for `host_sessions` (same skip-unchanged pattern as
    /// `sessions_overview_rev`).
    pub host_sessions_rev: Arc<std::sync::atomic::AtomicU64>,
    /// Set by the response listener on `AgentResponse::OpenHostPanel`.
    pub open_host: Arc<AtomicBool>,
    /// Live OAuth-add UI updates from the response listener.
    pub oauth_add_signal: Arc<Mutex<Option<OauthAddSignal>>>,
    /// Mirror of `App::awaiting_oauth_add`, written by the loop each frame so
    /// the response listener can suppress a duplicate transcript URL during an
    /// add flow (the modal is the surface there).
    pub awaiting_oauth_add: Arc<AtomicBool>,
    /// Latest session-context snapshot for the Tools / Mcp / Skills /
    /// Permissions managers, or `None` before the first `QuerySessionContext`
    /// round-trip completes. Each manager renders a lightweight placeholder
    /// while this is `None`.
    pub session_context: Arc<Mutex<Option<neenee_core::SessionContextSnapshot>>>,
    /// Unified task list, mirrored from `AgentResponse::TodosUpdated`. The
    /// render loop copies it into `App::todos` each frame so the Activity
    /// modal stays in sync with the agent's state.
    pub todos: Arc<Mutex<Option<TodoList>>>,
    /// Live harness round counter, mirrored from the harness snapshot so the
    /// task panel can reference the current round.
    pub round_count: Arc<Mutex<u64>>,
    /// Current ReAct turn within the active round (1-indexed for display).
    /// Set from `RoundEvent::TurnStarted`; reset to 0 at the round
    /// boundary so the pre-request phase does not show a stale turn.
    pub current_turn: Arc<Mutex<u64>>,
    /// Session-review alert (ADR-0016), or empty when inactive. Mirrored into
    /// `App::review_alert` each frame; while non-empty the activity bar appends
    /// a `⚠ <alert>` segment.
    pub review_alert: Arc<Mutex<String>>,
    /// Wall-clock instant the current round started, or `None` between rounds.
    /// Set by the response listener on a "running" `HarnessState` and cleared
    /// on idle; drives the muted `<elapsed>` segment in the activity bar.
    pub round_started_at: Arc<Mutex<Option<std::time::Instant>>>,
    /// Pending "unsend" from a Phase-1 interrupt: the response listener sets
    /// this when the harness reports the user's message was unsent (interrupted
    /// before any model output arrived), and the event loop drains it each
    /// frame to restore the prompt into the input box for re-editing. Carried
    /// as a signal (not direct `App` mutation) because the listener and the
    /// loop own disjoint halves of `App`'s state.
    pub unsent_input_signal: Arc<Mutex<Option<UnsentInput>>>,
    /// A toast-surfaced notice (`NoticeSurface::Toast`) the listener forwards
    /// instead of appending it to the transcript. The loop drains it each frame
    /// and shows it as a transient top-right bubble (command acknowledgments
    /// such as `/autopilot on`), mirroring the copy-toast. Latest wins; a
    /// pending older toast is replaced.
    pub notice_toast_signal: Arc<Mutex<Option<NoticeToastSignal>>>,
    /// Ordered protocol acknowledgements for the compact outbox. The response
    /// listener cannot mutate `App`, so it forwards only these small signals.
    pub outbox_signals: Arc<Mutex<VecDeque<OutboxSignal>>>,
}

/// A toast-surfaced notice forwarded across the listener → loop boundary.
/// `severity` drives the bubble accent color; `text` is the rendered body.
pub(super) struct NoticeToastSignal {
    pub severity: NoticeSeverity,
    pub text: String,
}

pub(super) enum OutboxSignal {
    /// A staged next-round item failed to start its round (e.g. no provider
    /// configured, or the addressed round is no longer accepting inserts).
    /// Re-queue the dispatch so the user can recall it or let it retry.
    Unavailable {
        session_id: String,
        input_id: String,
    },
    NextRoundStarted {
        session_id: String,
        input_id: String,
    },
    RoundCompleted {
        session_id: String,
    },
    HarnessState {
        session_id: String,
        idle: bool,
    },
}

/// A pending `/btw` side-view transition queued by the response listener and
/// drained by the event loop (ADR-0017). `Opened` carries the side routing
/// key the listener needs to direct subsequent per-turn events to the side
/// buffer.
pub(super) enum SideViewSignal {
    Opened { side_id: String },
    Closed,
}

/// Progress of the "+ Add provider → OAuth" browser flow.
pub(super) enum OauthAddSignal {
    Pending {
        url: String,
        user_code: String,
        message: String,
    },
    Done,
    Failed {
        message: String,
    },
}

/// A user message unsent by a Phase-1 interrupt (the round was cancelled before
/// any model output reached the client). The event loop drains this to restore
/// the prompt and images into the input box and pop the user message out of the
/// transcript, mirroring `App::recall_queued`'s composer restore.
pub(super) struct UnsentInput {
    pub prompt: String,
    pub images: Vec<neenee_core::ImagePart>,
}

/// Probe a raw input event against the provider-delete confirm overlay.
///
/// Returns `Some(action)` when the overlay is open (it owns every key in that
/// state): ←/→/Tab/`h`/`l` move focus between Cancel (the default) and Delete,
/// Enter activates the focused button, and Esc / Ctrl+C cancel. Returns `None`
/// when the overlay is closed so the caller proceeds with normal
/// [`input::process_event`] handling. The returned action — if any — is
/// dispatched by the standard `match action` block; `DeleteProviderConfirm`
/// and `DeleteProviderCancel` are the overlay-specific arms.
fn probe_delete_overlay(app: &mut App, event: &Event) -> Option<input::InputAction> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

    if app.pending_provider_delete.is_none() || app.active_modal != Modal::Connections {
        return None;
    }

    let Event::Key(k) = event else {
        // Mouse and resize events do not drive the overlay's keyboard UI;
        // outside-click dismissal is handled in the mouse branch instead.
        // Still consume them so nothing reaches the composer behind the panel.
        return Some(input::InputAction::None);
    };

    // Only act on Press (crossterm sends Release on some terminals); ignore
    // repeats so a held key does not spam focus or fire Delete repeatedly.
    if !matches!(k.kind, KeyEventKind::Press) {
        return Some(input::InputAction::None);
    }

    // Any other key is swallowed so it never edits the composer / moves the
    // provider list selection behind the panel.
    match (k.modifiers, k.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            Some(input::InputAction::DeleteProviderCancel)
        }
        (KeyModifiers::NONE, KeyCode::Esc) => Some(input::InputAction::DeleteProviderCancel),
        (KeyModifiers::NONE, KeyCode::Enter) => {
            if app.provider_delete_focus == ProviderDeleteChoice::Delete {
                Some(input::InputAction::DeleteProviderConfirm)
            } else {
                Some(input::InputAction::DeleteProviderCancel)
            }
        }
        // Focus cycling between Cancel and Delete.
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Left)
        | (KeyModifiers::CONTROL, KeyCode::Char('b'))
        | (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('h')) => {
            app.provider_delete_focus = ProviderDeleteChoice::Cancel;
            Some(input::InputAction::None)
        }
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Right)
        | (KeyModifiers::CONTROL, KeyCode::Char('f'))
        | (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('l')) => {
            app.provider_delete_focus = ProviderDeleteChoice::Delete;
            Some(input::InputAction::None)
        }
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Tab)
        | (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Down)
        | (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Up) => {
            // Tab/↑/↓ toggle the two buttons.
            app.provider_delete_focus = match app.provider_delete_focus {
                ProviderDeleteChoice::Cancel => ProviderDeleteChoice::Delete,
                ProviderDeleteChoice::Delete => ProviderDeleteChoice::Cancel,
            };
            Some(input::InputAction::None)
        }
        // Any other key is swallowed so it never edits the composer / moves the
        // provider list selection behind the panel.
        _ => Some(input::InputAction::None),
    }
}

/// Activate a (provider, model) pair picked in the Connections or Models
/// picker. Key-ready → `AgentRequest::SwitchProvider` + restore the parked
/// draft + close; a no-key OAuth provider → `AgentRequest::ConnectProvider` +
/// close; no key otherwise → open the key editor prefilled with this model so
/// the user can enter a key before activating. Shared by both pickers so the
/// two surfaces can never drift on activation semantics.
fn activate_picked_model(app: &mut App, id: String, model: String, key_ready: bool) {
    if key_ready {
        // SwitchProvider routes through build_provider_for_model so the
        // per-model transport is selected correctly.
        let _ = app.tx.send(AgentRequest::SwitchProvider {
            provider_type: id,
            model,
            api_key: None,
            base_url: None,
        });
        app.restore_model_draft();
        app.active_modal = Modal::None;
    } else if app.provider_row_auth(&id).is_oauth() {
        let auth = app.provider_row_auth(&id);
        let _ = app.tx.send(AgentRequest::ConnectProvider {
            id,
            method: auth
                .default_login_method()
                .unwrap_or(neenee_core::LoginMethod::Device),
        });
        app.restore_model_draft();
        app.active_modal = Modal::None;
    } else {
        // No key configured: open the key editor prefilled with this model so
        // the user can enter a key before activating. Esc returns to the
        // picker the editor was opened from (`editor_return_to`).
        app.editor_return_to = app.active_modal;
        app.editor_target = Some(id);
        app.editor_field = 0;
        app.editor_key.clear();
        app.editor_model = model;
        app.editor_model_settings_only = false;
        app.editor_target_is_builtin = false;
        app.editor_effort = "high".to_string();
        app.editor_thinking = true;
        app.input.clear();
        app.set_cursor(0);
        app.model_search = false;
        app.active_modal = Modal::ModelEditor;
    }
}

async fn handle_permission_submit(app: &mut App, runtime: &UiRuntime) {
    // The footer button layout depends on the request: an ordinary prompt is
    // [Allow once, Always allow, Reject, Details] (indices 0..3), but a
    // one-off prompt (the bash dangerous-command confirm) suppresses the
    // "Always allow" option, collapsing to [Allow once, Reject, Details]
    // (indices 0..2). Resolve the index→action mapping accordingly so the
    // Removed/Always/Reject/Details semantics stay correct in both layouts.
    let one_off = app.pending_permission.as_ref().is_some_and(|r| r.one_off);
    let reject_idx = if one_off { 1 } else { 2 };
    let details_idx = if one_off { 2 } else { 3 };
    if app.permission_confirm_always {
        // Confirm-always sub-step: index 0 = Confirm, 1 = Cancel. Reachable
        // only for non-one_off prompts (the Always option is suppressed
        // otherwise), so the `one_off` guard above keeps this branch honest.
        if app.modal_index == 1 {
            app.permission_confirm_always = false;
            app.modal_index = 1;
            return;
        }
        // index 0: fall through to send Always.
    } else {
        // "Details": expand/collapse the body without deciding, so the user
        // can review before acting.
        if app.modal_index == details_idx {
            app.permission_show_details = !app.permission_show_details;
            app.permission_scroll = 0;
            return;
        }
        // "Always allow" (only present for non-one_off prompts): gate behind a
        // confirm step. For one_off prompts this index is Reject (handled
        // below), so this branch is skipped.
        if !one_off && app.modal_index == 1 {
            app.permission_confirm_always = true;
            app.permission_show_details = false;
            app.modal_index = 0;
            return;
        }
    }
    if let Some(request) = app.pending_permission.take() {
        let decision = if app.permission_confirm_always {
            PermissionDecision::Always
        } else {
            // index 0 = Allow once; reject_idx (1 or 2) = Reject; anything
            // else also resolves to Reject as a safe default.
            match app.modal_index {
                0 => PermissionDecision::Once,
                i if i == reject_idx => PermissionDecision::Reject,
                _ => PermissionDecision::Reject,
            }
        };
        let request_id = request.id;
        let parent_call_id = runtime
            .envoy_permission_parent
            .lock()
            .await
            .remove(&request_id);
        let _ = app.tx.send(AgentRequest::PermissionReply {
            request_id: request_id.clone(),
            decision,
            parent_call_id,
        });
        if decision == PermissionDecision::Reject {
            // A rejection settles the whole concurrent permission batch:
            // resolve every other queued request too, otherwise their tool
            // futures stay blocked and the batch deadlocks.
            let queued: Vec<PermissionRequest> =
                runtime.pending_permission.lock().await.drain(..).collect();
            let mut parents = runtime.envoy_permission_parent.lock().await;
            for pending in queued {
                let parent_call_id = parents.remove(&pending.id);
                let _ = app.tx.send(AgentRequest::PermissionReply {
                    request_id: pending.id,
                    decision: PermissionDecision::Reject,
                    parent_call_id,
                });
            }
            app.pending_permission = None;
            app.active_modal = Modal::None;
        } else {
            // Drop the request we just answered and surface the next one (if
            // any) so the sheet hands off without flashing the composer for a
            // frame.
            let mut queue = runtime.pending_permission.lock().await;
            queue.retain(|r| r.id != request_id);
            app.pending_permission = queue.front().cloned();
            drop(queue);
            if app.pending_permission.is_none() {
                app.active_modal = Modal::None;
            }
        }
        app.modal_index = 0;
        app.permission_scroll = 0;
        app.permission_max_scroll = 0;
        app.permission_confirm_always = false;
        app.permission_show_details = false;
    }
}

/// Page step (rows) for a modal-body page scroll (`PageUp` / `PageDown` /
/// `Ctrl+Up` / `Ctrl+Down`). Uses the last-rendered [`App::modal_body_height`]
/// when known, falling back to the transcript `view_height` before the first
/// render after a modal opens. Always at least 1 so a page key never no-ops on
/// a zero-height capture. This is a free function (not a method) so it can be
/// evaluated before the mutable borrow of the modal's scroll field without
/// tripping the borrow checker.
pub(crate) fn modal_page_step(app: &App) -> usize {
    let h = if app.modal_body_height > 0 {
        app.modal_body_height
    } else {
        app.view_height
    };
    h.saturating_sub(1).max(1) as usize
}

/// Owns the dedicated terminal-input reader thread used by [`run_app_loop`].
///
/// The thread blocks in `crossterm::event::poll`/`read` and forwards each event
/// over an unbounded channel, so the async loop awaits/drains a plain tokio
/// channel instead of crossterm's `EventStream`. This is deliberate, not
/// incidental: `EventStream::poll_next` registers the *calling task's* waker
/// exactly once (guarded by an internal `executed` flag), so draining queued
/// events with `next().now_or_never()` registered a **no-op** waker — after
/// which a parked `select!` could no longer be woken by a real keystroke and
/// input was only serviced on the heartbeat tick (up to ~1s while idle). A
/// channel registers the real task waker on every `recv`/`try_recv`, so input
/// wakes the loop immediately and coalescing drains stay waker-safe.
///
/// Dropping the guard signals the thread to stop; it observes the flag within
/// one poll timeout and exits on its own. We deliberately do not join, so an
/// exiting loop is never blocked waiting on the next poll cycle.
struct InputReader {
    shutdown: Arc<AtomicBool>,
}

impl InputReader {
    fn spawn(tx: mpsc::UnboundedSender<Event>) -> io::Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = shutdown.clone();
        std::thread::Builder::new()
            .name("neenee-tui-engine-input".into())
            .spawn(move || {
                // The SGR reassembly sink. crossterm occasionally hands back a
                // split mouse report as a run of spurious `Char` events (issue
                // #854/#668) — worst on resize, fast trackpad scrolling, and
                // inside multiplexers. The sink keeps those fragments from
                // ever reaching the composer by re-draining the tail of the
                // sequence within a short window whenever the guard flags a
                // suspicious prefix.
                let mut sink = SgrReassemblySink::new(tx);
                // Poll with a bounded timeout instead of blocking forever in
                // `read()`: a ready event still returns immediately (zero added
                // input latency), while the timeout lets the thread notice the
                // shutdown flag and exit promptly once the loop ends.
                while !thread_shutdown.load(Ordering::Relaxed) {
                    match event::poll(std::time::Duration::from_millis(200)) {
                        Ok(true) => match event::read() {
                            // Receiver gone → the loop has exited; stop reading.
                            Ok(ev) => {
                                if !sink.handle(ev) {
                                    break;
                                }
                            }
                            // Terminal read error → nothing more to read; stop.
                            Err(_) => break,
                        },
                        Ok(false) => {}  // timeout: loop back to re-check shutdown
                        Err(_) => break, // poll error → stop
                    }
                }
            })?;
        Ok(Self { shutdown })
    }
}

/// Reader-thread SGR reassembly sink.
///
/// Wraps the event channel so that a mouse report crossterm split across two
/// `event::read()` calls is re-drained and dropped *here*, at the source,
/// instead of leaking to the composer as stray `Char` events (see
/// [`input::SgrLeakGuard`] for the full background). When a freshly read event
/// trips the symbol-layer guard — i.e. it looks like the `ESC [ < …` prefix of
/// an SGR mouse sequence — the sink keeps reading within a short deadline,
/// feeding every subsequent event to the same guard and discarding it, until
/// the guard returns to idle or the deadline elapses.
///
/// The deadline is intentionally tiny (a few ms): the tail of a split sequence
/// is already in the kernel TTY buffer and arrives on the very next `poll`, so
/// a real `Esc` key that the guard tentatively flagged pays at most one extra
/// short poll before being delivered normally. The symbol-layer guard in the
/// event loop is the backstop for any fragment that still escapes this window.
struct SgrReassemblySink {
    tx: mpsc::UnboundedSender<Event>,
    guard: input::SgrLeakGuard,
}

/// How long to keep draining a suspected split SGR sequence before giving up
/// and delivering the buffered event normally. The tail bytes are normally
/// already queued in the TTY, so this is only paid in pathological cases.
const SGR_REASSEMBLY_WINDOW: std::time::Duration = std::time::Duration::from_millis(40);

impl SgrReassemblySink {
    fn new(tx: mpsc::UnboundedSender<Event>) -> Self {
        Self {
            tx,
            guard: input::SgrLeakGuard::default(),
        }
    }

    /// Handle one freshly-read event. Returns `false` if the channel is closed
    /// and the reader thread should stop.
    fn handle(&mut self, ev: Event) -> bool {
        use input::Feed;
        match self.guard.feed(&ev) {
            Feed::Accept => {
                // Not (yet) part of a leaked sequence. If the guard is now idle
                // this is a clean, deliverable event; if it just entered a
                // tracking state we still deliver the *current* event and let
                // the next `read()` advance the guard — the symbol-layer guard
                // in the loop is the final backstop.
                self.deliver(ev)
            }
            Feed::Drop => {
                // Looks like SGR leakage. Drain the rest of the sequence within
                // a short window so the fragments never reach the composer.
                self.drain_split_sequence();
                true
            }
        }
    }

    /// Deliver an event, reporting channel closure to the caller.
    fn deliver(&self, ev: Event) -> bool {
        self.tx.send(ev).is_ok()
    }

    /// Best-effort drain of the tail of a split SGR mouse sequence. Each event
    /// read here is fed to the guard and discarded; the loop ends when the guard
    /// returns to idle, the channel closes, or the deadline passes.
    fn drain_split_sequence(&mut self) {
        let deadline = std::time::Instant::now() + SGR_REASSEMBLY_WINDOW;
        while std::time::Instant::now() < deadline {
            match event::poll(deadline.saturating_duration_since(std::time::Instant::now())) {
                Ok(true) => match event::read() {
                    Ok(ev) => {
                        use input::Feed;
                        match self.guard.feed(&ev) {
                            // Guard back to idle: sequence reassembled (or it
                            // turned out not to be a mouse report). Stop draining.
                            Feed::Accept if self.guard.is_idle() => break,
                            Feed::Accept => {} // keep draining the tail
                            Feed::Drop => {}   // expected: more fragments
                        }
                    }
                    Err(_) => break,
                },
                _ => break, // timeout or error → give up, guard resets next event
            }
        }
        // Ensure a fresh state for the next top-level event regardless of how
        // this drain ended (timeout mid-sequence, parse error, etc.).
        self.guard.reset();
    }
}

impl Drop for InputReader {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

pub(super) async fn run_app_loop(
    terminal: &mut Terminal<std::io::Stdout>,
    app: &mut App,
    runtime: UiRuntime,
    session: crate::tui::SessionSource,
) -> io::Result<()> {
    let mut _copy_toast_timer: u8 = 0;
    // Clipboard copies run in background tasks so a slow/hanging system
    // clipboard (arboard/wl-copy) can never freeze the event loop.
    let (copy_tx, mut copy_rx) =
        mpsc::unbounded_channel::<Result<clipboard::CopyOutcome, String>>();
    // Number of clipboard copies still in flight. While this is non-zero the
    // event loop uses a short poll interval so the "copied" toast appears
    // within ~16ms of completion instead of waiting up to the full idle tick.
    let copy_pending = Arc::new(AtomicUsize::new(0));

    // Clipboard paste reads (Ctrl+V) run in background tasks for the same
    // reason: arboard/wl-paste must never block the event loop.
    let (paste_tx, mut paste_rx) = mpsc::unbounded_channel::<clipboard::ClipboardRead>();

    // Stage 4: terminal input intake. A dedicated reader thread blocks on
    // `event::read()` and forwards each event over this channel; the loop awaits
    // it in the `select!` below (and drains queued events with `try_recv`). Both
    // register the real task waker, so input intake stays decoupled from
    // rendering *and* always wakes the loop immediately — a slow frame never
    // starves input, and a keystroke never waits out the heartbeat. See
    // [`InputReader`] for why this replaces crossterm's `EventStream`.
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Event>();
    // Held until the loop returns; its `Drop` signals the reader thread to stop.
    let _input_reader = InputReader::spawn(input_tx)?;

    // Symbol-layer SGR leakage backstop. The reader-thread reassembler
    // (`SgrReassemblySink`) catches split mouse sequences at the source, but a
    // fragment can still escape it on some terminals (browser xterm.js, very
    // high-frequency trackpad inertia). This second guard drops any such stray
    // `Char` event *before* `process_event` can insert it into the input line.
    let mut sgr_guard = input::SgrLeakGuard::default();

    // Stage 3: carries "an input event was handled last iteration, so a frame
    // is due" across the loop boundary, since input is drained at the *end* of
    // an iteration but rendered at the *start* of the next.
    let mut input_redraw_pending = true;
    // Whether the previous frame was animating. When animation stops (spinner
    // ends, a toast/armed timer expires) we still owe one final draw to clear
    // its last visual, so a true→false transition forces exactly one more frame.
    let mut was_animating = true;
    // Last `sessions_overview_rev` the loop mirrored into `App`. The listener
    // bumps the revision whenever it replaces the shared overview, so the loop
    // can skip the deep `Vec<SessionOverview>` clone (two `String`s per row)
    // on the vast majority of iterations where the picker data is unchanged.
    // Without this gate a large project (hundreds of sessions) re-cloned the
    // whole list every frame, a major contributor to picker hitches.
    let mut sessions_overview_rev_seen: u64 = 0;
    let mut host_sessions_rev_seen: u64 = 0;

    loop {
        if app.should_quit.load(Ordering::SeqCst) {
            tracing::info!(reason = "should_quit_flag", "app exiting");
            return Ok(());
        }

        // Stage 3 redraw bookkeeping. Start from any input handled last
        // iteration; background results and active animation are folded in
        // below. When nothing here is true, the per-frame draw is skipped
        // entirely so an idle session does no rendering work.
        let mut frame_dirty = input_redraw_pending;
        input_redraw_pending = false;

        // Apply any completed background clipboard copies.
        while let Ok(result) = copy_rx.try_recv() {
            clipboard_ops::set_copy_feedback(app, result);
            app.copy_toast_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(1800));
            frame_dirty = true;
        }

        // Apply any completed clipboard paste reads.
        while let Ok(read) = paste_rx.try_recv() {
            clipboard_ops::apply_clipboard_paste(app, read);
            frame_dirty = true;
        }

        // Sync provider/model from listener
        {
            app.current_provider = runtime.current_provider.lock().await.clone();
            app.current_model = runtime.current_model.lock().await.clone();
            let harness = runtime.harness.lock().await.clone();
            app.loop_status = harness.loop_status;
            app.autopilot = harness.autopilot;
            app.activity_status = runtime.activity_status.lock().await.clone();
            app.session_context = runtime.session_context.lock().await.clone();
            app.todos = runtime.todos.lock().await.clone();
            app.round_count = *runtime.round_count.lock().await;
            app.current_turn = *runtime.current_turn.lock().await;
            app.review_alert = runtime.review_alert.lock().await.clone();
            app.round_started_at = *runtime.round_started_at.lock().await;
            app.pending_permission = runtime.pending_permission.lock().await.front().cloned();
            app.key_status = runtime.key_status.lock().await.clone();
            app.provider_picker = runtime.provider_picker.lock().await.clone();
            if app.pending_permission.is_some() && app.active_modal == Modal::None {
                app.active_modal = Modal::Permission;
                app.modal_index = 0;
                app.permission_scroll = 0;
                app.permission_show_details = false;
                // A permission prompt is urgent: clear any focused transcript
                // step so the next keypress decides the sheet, not the step.
                app.focused_target = None;
            } else if app.pending_permission.is_none() && app.active_modal == Modal::Permission {
                app.active_modal = Modal::None;
                app.modal_index = 0;
                app.permission_confirm_always = false;
                app.permission_scroll = 0;
                app.permission_max_scroll = 0;
                app.permission_show_details = false;
            }
            // Question modal: mirror the pending-request queue front into the
            // App-level model. A new front (arriving request) opens a fresh
            // QuestionModel with default selections; an emptied front (after a
            // submit/cancel drained the queue) clears the model and closes the
            // modal. The model is the single source of truth for the modal's
            // interaction state once open.
            {
                let front = runtime.pending_question.lock().await.front().cloned();
                let model_matches_front = match (&app.question, &front) {
                    (Some(m), Some(req)) => m.request().id == req.id,
                    (None, None) => true,
                    _ => false,
                };
                if !model_matches_front {
                    if let Some(req) = front {
                        app.question = Some(crate::tui::question_model::QuestionModel::open(req));
                        app.question_scroll = 0;
                        app.question_modal_follow = true;
                        app.active_modal = Modal::Question;
                        app.modal_index = 0;
                        app.focused_target = None;
                    } else {
                        app.question = None;
                        if app.active_modal == Modal::Question {
                            app.active_modal = Modal::None;
                            app.modal_index = 0;
                        }
                    }
                }
            }
            // Input-injection modal (L3.5 β): mirror the pending-input queue
            // front. A new front opens the modal and parks the composer draft;
            // an emptied front closes it and restores the draft.
            {
                let front = runtime.pending_input.lock().await.front().cloned();
                let matches_front = match (&app.pending_input, &front) {
                    (Some(cur), Some(req)) => cur.id == req.id,
                    (None, None) => true,
                    _ => false,
                };
                if !matches_front {
                    if let Some(req) = front {
                        // Park the composer draft so Enter submits the injected
                        // input, not a chat message (mirrors Provider/ModelEditor).
                        app.park_input_draft();
                        app.pending_input = Some(req);
                        app.active_modal = Modal::InputInjection;
                        app.modal_index = 0;
                        app.focused_target = None;
                    } else {
                        app.pending_input = None;
                        if app.active_modal == Modal::InputInjection {
                            app.active_modal = Modal::None;
                            app.modal_index = 0;
                            app.restore_input_draft();
                        }
                    }
                }
            }
            // Sessions picker: refresh rows and open the modal on request.
            // Sessions picker: refresh rows (only when the listener actually
            // changed them) and open the modal on request. The revision gate
            // avoids cloning the full overview Vec every iteration — the
            // listener bumps `sessions_overview_rev` on every replacement.
            {
                let rev = runtime.sessions_overview_rev.load(Ordering::Acquire);
                if rev != sessions_overview_rev_seen {
                    app.sessions_overview = runtime.sessions_overview.lock().await.clone();
                    sessions_overview_rev_seen = rev;
                }
            }
            if runtime.open_sessions.swap(false, Ordering::SeqCst)
                && app.active_modal != Modal::Permission
            {
                // Only reset the selection/scroll when the modal is actually
                // being *opened* — transitioning from some other state into the
                // sessions picker. When the picker is already open this signal
                // is just a data refresh (e.g. the overview the backend pushes
                // right after a delete), and resetting the cursor there would
                // snap the selection back to the top on every delete, fighting
                // the optimistic local removal the delete handler already did.
                let opening = app.active_modal != Modal::Sessions;
                app.active_modal = Modal::Sessions;
                if opening {
                    app.modal_index = 0;
                    // Reuse the Tools/Mcp/Skills body scroll slot so the picker is
                    // scrollable (PageUp/PageDown/Ctrl+↑↓/wheel) like the other
                    // list modals. Reset on open so a long list starts at the top.
                    app.session_scroll = 0;
                    app.session_modal_follow = true;
                }
            }
            // Mirror the daemon monitor snapshot for the `/host` panel.
            {
                let rev = runtime.host_sessions_rev.load(Ordering::Acquire);
                if rev != host_sessions_rev_seen {
                    app.host_sessions = runtime.host_sessions.lock().await.clone();
                    host_sessions_rev_seen = rev;
                }
            }
            if runtime.open_host.swap(false, Ordering::SeqCst)
                && app.active_modal != Modal::Permission
            {
                let opening = app.active_modal != Modal::Host;
                app.active_modal = Modal::Host;
                if opening {
                    app.modal_index = 0;
                    app.host_scroll = 0;
                    app.host_modal_follow = true;
                    // Default focus is the console/input region (ADR-0097
                    // §3): typing lands there; the dock is entered with Tab.
                    app.host_focus = crate::tui::overlays::DashboardFocus::Detail;
                    app.host_detail_scroll = 0;
                    app.host_preview = None;
                    app.host_preview_scroll = 0;
                    app.host_prompting = false;
                    app.modal_keymap_open = false;
                }
            }
            // Mirror the on-demand session detail (info sub-view) when the
            // listener has a fresh one. Replacing `None` with `None` is a
            // no-op, so this is cheap when the sub-view is closed.
            if let Some(detail) = runtime.session_detail.lock().await.take() {
                app.session_detail = Some(detail);
                app.session_info_scroll = 0;
            }
            // Mirror the on-demand token-source report (attach mode) when the
            // listener has a fresh one for the viewed session.
            if let Some(report) = runtime.token_report.lock().await.take() {
                app.token_report = Some(report);
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
                            app.active_modal = Modal::OauthPending;
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
                            app.active_modal = Modal::OauthPending;
                        }
                    }
                }
            }
            // Mirror the add-flow flag so the response listener can suppress a
            // duplicate transcript URL during an add (the modal owns it).
            runtime
                .awaiting_oauth_add
                .store(app.awaiting_oauth_add, Ordering::SeqCst);
        }

        // Decrement toast timers
        if let Some(until) = app.copy_toast_until
            && std::time::Instant::now() >= until
        {
            app.copy_toast_until = None;
        }
        // Drain a forwarded toast-surfaced notice (command acknowledgment).
        // Latest wins: a newer toast replaces an in-flight older one. The
        // duration is wall-clock consistent regardless of the loop cadence.
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
        // While images are staged for the next message, keep a persistent
        // indicator visible so the user knows Enter will send them. Skipped
        // while the Ctrl+C quit window is armed so a freshly-shown
        // "input cleared — Ctrl+C again to exit" toast keeps the floor and
        // is not immediately overwritten by the per-frame image reminder.
        // The armed window is wall-clock (`ctrl_c_armed_until`), so it
        // lapses on its own — there is no per-tick counter to decrement
        // here (the old tick counter stretched the intended ~2s window to
        // ~20s under the 1s idle heartbeat).
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
        // The Esc armed toast only makes sense while a task is running; once
        // the turn finishes there is nothing left to interrupt, so let it
        // expire immediately rather than mislead the user.
        if app.esc_armed_ticks > 0 {
            if runtime.is_responding.load(Ordering::SeqCst) {
                app.esc_armed_ticks -= 1;
            } else {
                app.esc_armed_ticks = 0;
            }
        }

        // Pull messages from the shared buffer into app state for rendering,
        // but only when they actually changed. `Versioned` advances a counter
        // on every mutation, so an unchanged transcript — the common case while
        // the user is typing into a long session — skips the O(n) deep clone
        // entirely. This is the single biggest source of the "slows down the
        // longer you use it" sluggishness.
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
        // Mirror the side buffer for the `/btw` banner (ADR-0017), likewise
        // gated on its version so it is cloned only when it changes — even
        // while the side view is open and the user briefly returns to the
        // primary transcript.
        let side_messages_version = runtime.side_messages.version();
        let side_transcript_changed = side_messages_version != app.side_messages_version;
        if side_transcript_changed {
            let patch = runtime.side_messages.take_transcript_patch();
            if !apply_transcript_patch(&mut app.side_messages, patch) {
                app.side_messages = runtime.side_messages.read().await.clone();
            }
            app.side_messages_version = side_messages_version;
            // The side view shares the same height cache (keyed by message id),
            // so consume its targeted invalidations too.
            apply_height_invalidation(
                &mut app.layout_height_cache,
                runtime.side_messages.take_height_invalidation(),
            );
        }
        app.parent_status = *runtime.parent_status.lock().await;
        // Drain a pending side-view transition (enter/leave `/btw`).
        let side_view_transitioned = match runtime.side_view_signal.lock().await.take() {
            Some(crate::tui::event_loop::SideViewSignal::Opened { side_id, .. }) => {
                app.enter_side_view(side_id);
                true
            }
            Some(crate::tui::event_loop::SideViewSignal::Closed) => {
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
        // The primary session id: the local store's when standalone, the
        // handshake-learned id when attached to a server (SessionSource).
        let primary_session_id = session.session_id().await;
        let viewed_session_id = if app.in_side_view {
            app.side_session_id
                .as_deref()
                .unwrap_or(primary_session_id.as_str())
        } else {
            primary_session_id.as_str()
        }
        .to_string();
        // Keep the origin stampers in sync with whatever session the user is
        // currently composing into: `record_input_history` tags each entry
        // with this id + workspace so the inline ↑/↓ recall can walk only
        // this session's prompts (while Ctrl+R searches the whole history).
        if app.current_session_id != viewed_session_id {
            app.current_session_id = viewed_session_id.clone();
        }
        let workspace = crate::tui::chrome::tilde_home(&app.cwd);
        if app.current_workspace != workspace {
            app.current_workspace = workspace;
        }
        app.context_tokens = runtime
            .context_tokens
            .lock()
            .await
            .get(&viewed_session_id)
            .copied();
        app.round_tps = runtime
            .round_tps
            .lock()
            .await
            .get(&viewed_session_id)
            .copied();

        // Apply protocol acknowledgements before handling the next key. The
        // transcript listener has already committed admitted/started messages;
        // this side owns only compact outbox and composer state.
        while let Some(signal) = runtime.outbox_signals.lock().await.pop_front() {
            match signal {
                OutboxSignal::NextRoundStarted {
                    session_id,
                    input_id,
                } => {
                    app.remove_dispatch(&session_id, &input_id);
                }
                OutboxSignal::Unavailable {
                    session_id,
                    input_id,
                } => app.requeue_dispatch(&session_id, &input_id),
                OutboxSignal::RoundCompleted { session_id } => {
                    app.naturally_completed_sessions.insert(session_id);
                }
                OutboxSignal::HarnessState { session_id, idle } => {
                    if idle {
                        app.running_sessions.remove(&session_id);
                        app.idle_sessions.insert(session_id);
                    } else {
                        app.idle_sessions.remove(&session_id);
                        // A fresh round invalidates any older success token.
                        // Its own next-round items must wait for this round's
                        // terminal result; if it errors or is interrupted they
                        // remain paused.
                        app.naturally_completed_sessions.remove(&session_id);
                        app.running_sessions.insert(session_id);
                    }
                }
            }
        }

        // Drain a pending Phase-1 unsend: the user interrupted before any model
        // output arrived, the harness reverted the conversation context, and the
        // listener already popped the user message from the transcript. Restore
        // the prompt + images into the composer so the user can edit and resend
        // — the same restore `App::recall_queued` performs for a queued message.
        if let Some(unsent) = runtime.unsent_input_signal.lock().await.take() {
            // Phase-1 unsend: the interrupted input becomes the new draft —
            // the newest *unsent* slot. `adopt_as_draft` enters draft mode,
            // replaces any stale remembered draft, and mirrors the staged
            // attachments into both the pending slots and the draft stash.
            app.adopt_as_draft(unsent.prompt, unsent.images, Vec::new());
        }

        // A next-round item auto-runs only after both a natural-completion
        // event and the matching session's idle snapshot. Error, interrupt,
        // blocked-hook and vanished-session paths leave it visibly paused.
        // A user block (`F3` / queue-modal-open) holds items back even from a
        // ready session — the block is the explicit "don't send anything"
        // override.
        let ready_session = app
            .naturally_completed_sessions
            .iter()
            .find(|session_id| {
                app.idle_sessions.contains(*session_id)
                    && !app.queue_blocked_sessions.contains(session_id.as_str())
                    && app.pending_dispatch.iter().any(|item| {
                        item.session_id == session_id.as_str()
                            && item.state == crate::tui::app::QueuedDispatchState::Waiting
                    })
            })
            .cloned();
        if let Some(session_id) = ready_session
            && let Some(dispatch) = app.begin_next_round_dispatch(&session_id)
        {
            let sent_at_ms = now_epoch_ms();
            let expanded_text =
                composer_attachments::expand_paste_chips(&dispatch.text, &dispatch.text_pastes);
            // Drop orphaned image labels (no staged payload) so a queued
            // message recalled from history never ships a phantom chip.
            let expanded_text = composer_attachments::strip_orphan_image_chips(
                &expanded_text,
                dispatch.images.len(),
            );
            app.naturally_completed_sessions.remove(&session_id);
            app.idle_sessions.remove(&session_id);
            app.running_sessions.insert(session_id.clone());
            let _ = app.tx.send(AgentRequest::ChatToSession {
                session_id,
                input: neenee_core::QueuedUserInput {
                    id: dispatch.id,
                    text: expanded_text,
                    display_text: Some(dispatch.text),
                    images: dispatch.images,
                    sent_at_ms: Some(sent_at_ms),
                },
            });
        }

        // While following, keep the newest content in view using the previous
        // frame's measurement (max_scroll is recomputed after each draw).
        if app.follow_bottom {
            app.scroll = app.max_scroll;
        }

        // Stage 3: decide whether this frame needs drawing at all. An idle
        // session — no input handled, no streaming, no active animation — skips
        // the draw entirely and just blocks on the input poll below, doing zero
        // rendering work instead of ~10 full relayouts per second. While a turn
        // runs (or a toast/armed timer is live) `animating` keeps the spinner
        // and timers advancing at the existing poll cadence.
        let animating = runtime.is_responding.load(Ordering::SeqCst)
            || app.round_started_at.is_some()
            || app.copy_toast_until.is_some()
            || app.notice_toast_until.is_some()
            || app.ctrl_c_armed()
            || app.esc_armed_ticks > 0
            || !app.pending_images.is_empty()
            || copy_pending.load(Ordering::SeqCst) > 0;
        // `swap` consumes the listener's signal exactly once. Folded in: input
        // handled last iteration, background clipboard results this one, and one
        // trailing frame after animation stops (`was_animating`) so the spinner
        // and expiring toasts are actually cleared from the screen.
        let needs_draw = frame_dirty
            || animating
            || was_animating
            || runtime.dirty.swap(false, Ordering::AcqRel);
        was_animating = animating;

        // The breathing indicator's phase is derived from wall-clock time at
        // the draw site (see `spinner_epoch`), not advanced per frame: the loop
        // wakes at irregular intervals (mouse-move/hover floods, streaming,
        // paste), so a per-frame counter would make the breathing speed up and
        // stutter with input activity instead of holding a steady cadence.

        // ── Cursor ownership & IME anchor ───────────────────────────────────
        // The terminal cursor is what the host terminal's IME anchors its
        // composition window to. `App::caret_owner` / `App::caret_visible` are
        // the single source of truth for which surface holds it and whether it
        // is visible; every cursor decision below derives from them. No site
        // re-derives visibility from raw fields — that is what previously let
        // the cursor stay visible (and the IME anchored to a stale coordinate)
        // on states that own no caret at all: a focused transcript step (e.g.
        // clicking a disclosure mid-composition), an envoy zoom, or a
        // read-only / decision modal.
        let caret_owner = app.caret_owner();
        let caret_visible = app.caret_visible();

        // Immediate cursor sync (IME correctness): the IME samples the cursor
        // the instant a keystroke arrives — *before* the next frame is
        // rendered. If we only reposition as a per-frame side effect of
        // drawing, there is a one-frame window in which the caret's logical
        // and physical positions disagree, so the IME window drifts. Whenever
        // input handling moved the caret, sync the backend's cursor *now*, in
        // the same iteration, using the last-known composer rect (refreshed
        // every draw). This closes the lag window to zero; the coordinates
        // come from the same pure function the draw path uses, so the
        // immediate value and the rendered value can never diverge.
        //
        // Only the composer's caret is repositioned here, and only when it is
        // actually visible (a selection hides it). A caret-owning modal places
        // its own cursor via the draw closure's `set_cursor_position`, so a
        // pending flag set while the composer didn't own the caret is consumed
        // without action (it is stale by definition once ownership returns —
        // the next real keystroke re-arms it).
        if app.cursor_sync_pending {
            app.cursor_sync_pending = false;
            if caret_visible
                && caret_owner == CaretOwner::Composer
                && let Some((x, y)) = view::cursor_screen_pos(
                    app.last_input_rect,
                    &app.input,
                    app.byte_cursor(),
                    &mut app.input_scroll,
                )
            {
                let _ = terminal.backend().show_cursor_at(x, y);
                let _ = std::io::Write::flush(terminal.backend().writer());
            }
        }

        // Hide/show is a state transition, not a per-frame guess: emit the
        // escape codes only when the desired visibility differs from what we
        // last told the terminal, so there is no per-frame flicker. Resolving
        // it here — at the same moment we (re)position — means the IME never
        // samples a hide↔show edge at a coordinate that no longer matches a
        // visible caret.
        if caret_visible != app.cursor_visible {
            if caret_visible {
                let _ = terminal.show_cursor();
            } else {
                let _ = terminal.hide_cursor();
            }
            app.cursor_visible = caret_visible;
        }

        // A mutation of the transcript currently on screen (or a transition to
        // a different transcript view) can change the measured bottom after
        // layout. While following that bottom, stage the measurement frame in
        // the retained grid without flushing it; the immediate next pass paints
        // at the final scroll offset and is the only frame the terminal sees.
        let stage_bottom_follow = displayed_transcript_changed && app.follow_bottom;

        // Draw frame (skipped when nothing changed — see `needs_draw`).
        if needs_draw {
            let render_frame = |f: &mut neenee_tui_engine::Frame<'_>| {
                let mut layout_map = LayoutMap::new();

                if app.startup_overlay == crate::tui::StartupOverlay::SessionsPicker
                    && app.active_modal == Modal::Sessions
                {
                    // `neenee resume` (no id): initial launch opens ONLY the sessions picker
                    // on a clean background. Do not open/render the chat interface, empty state,
                    // composer input box, status bar, or header components until a session is selected.
                    f.render_widget(
                        neenee_tui_engine::widgets::Block::default()
                            .style(neenee_tui_engine::Style::default().bg(app.theme.app_bg)),
                        f.area(),
                    );

                    let spinner_phase = (app.spinner_epoch.elapsed().as_millis() / 100) as usize;
                    let drawn_modal_rect = view::draw_sessions_modal(
                        f,
                        &app.sessions_overview,
                        app.modal_index
                            .min(app.sessions_overview.len().saturating_sub(1)),
                        app.modal_keymap_open,
                        &mut app.session_scroll,
                        app.session_modal_follow,
                        &app.theme,
                        app.startup_overlay == crate::tui::StartupOverlay::SessionsPicker,
                        spinner_phase,
                        app.session_info_detail,
                        app.session_detail.as_ref(),
                        &mut app.session_info_scroll,
                    );

                    app.layout_map = layout_map;
                    app.modal_body_height = drawn_modal_rect.height.saturating_sub(
                        crate::tui::primitives::modal_chrome_rows(
                            crate::tui::primitives::ModalSpec {
                                width_percent: 0,
                                header: true,
                                footer: true,
                            },
                        ),
                    );
                    app.modal_rect = if app.active_modal.dismissable_by_outside_click() {
                        Some(drawn_modal_rect)
                    } else {
                        None
                    };
                    return;
                }

                app.modal_hit_map.clear();
                // Borrow the height cache out of `app` for the duration of the draw:
                // `view_messages` borrows `app` immutably below, so the cache cannot
                // also be reached through `app` at the same time. It is restored once
                // `view_messages` is no longer borrowed (see below).
                let mut height_cache = std::mem::take(&mut app.layout_height_cache);
                let activity_for_display = app.activity_status.as_str();
                let status = display_status(
                    app.loop_status,
                    activity_for_display,
                    app.pending_permission.is_some(),
                );

                // Compute the displayed input text first so the transcript layout can
                // reserve the right height for a wrapping, growing input box.
                let masked_input =
                    if app.active_modal == Modal::ModelEditor && app.editor_field == 0 {
                        // Mask the API key everywhere it could be rendered (the editor
                        // field itself, and any layout pass that inspects the input).
                        "•".repeat(app.input.chars().count())
                    } else {
                        app.input.clone()
                    };

                // Modal recess policy (single source of truth: `Modal::recess`).
                // A terminal cannot alpha-blend, so a modal either floats, darkens
                // the live surface in place, or fully occludes it:
                // - Takeover (Sessions): the footer collapses to zero height and
                //   the surface is occluded — opening a different session is a full
                //   context switch, so a clean slate is the intent.
                // - Dim (every other centered modal): the footer keeps its height
                //   so layout is stable, and the whole surface is darkened in place
                //   by the recess pass just before the modal is drawn. Context
                //   (transcript, input, hint bar, activity bar, state bar) stays visible for
                //   focus while the centered panel reads as the focal layer.
                // - None (Question / Permission): floats on the fully-live surface.
                // Provider / ModelEditor / HistorySearch borrow the input line as
                // their own field, so the composer is suppressed for them (its rect
                // stays as recessed surface) — no duplicate field, and no
                // masked-cursor panic in the editor.
                let recess = app.active_modal.recess();
                let chrome_hidden = recess == Recess::Takeover;

                // When zoomed into an Envoy, render its child messages and
                // show a contextual first-row header; otherwise render the
                // root conversation.
                let view_messages = app.focused_messages();
                // `/btw` page-header context (ADR-0017): shown only while the
                // side view is active. Envoy zoom and the side view are
                // mutually exclusive, so the two modes never coexist.
                let side_banner = app.in_side_view.then_some(app.parent_status);
                let envoy_bar = app.focus_stack.last().and_then(|current| {
                    let tasks: Vec<&TranscriptMessage> = app
                        .messages
                        .iter()
                        .filter(|message| message.is_envoy_task())
                        .collect();
                    let idx = tasks.iter().position(|message| {
                        message.tool_step_call_id() == Some(current.call_id.as_str())
                    })?;
                    Some(view::EnvoyBarInfo {
                        label: tasks.get(idx)?.envoy_label(),
                        index: idx + 1,
                        total: tasks.len(),
                    })
                });

                // Suppress the hover affordance whenever a full-overlay modal is
                // open so no stale highlight bleeds through. The permission sheet
                // keeps the transcript interactive, so it is exempted.
                let chrome_interactive =
                    matches!(app.active_modal, Modal::None | Modal::Permission);

                // Project the viewed session's outbox into the small view the
                // persistent queue bar renders. Dispatch order (front pops
                // first) is preserved, so the bar previews the genuine next
                // item to ship. The items are owned snapshots so the bar/modal
                // do not borrow `app` (which is mutated again right after the
                // draw closure).
                let queue_items: Vec<view::QueueItemView> = app
                    .pending_dispatch
                    .iter()
                    .filter(|item| item.session_id == viewed_session_id)
                    .map(|item| view::QueueItemView {
                        queued_at_ms: item.queued_at_ms,
                        text: item.text.clone(),
                    })
                    .collect();

                let transcript_render = view::draw_transcript(
                    f,
                    &mut layout_map,
                    view::TranscriptView {
                        messages: view_messages,
                        scroll: app.scroll,
                        selection: &app.selection,
                        cell_selection: app.drag.cell_info.as_ref(),
                        activity: &status,
                        // A pending permission request forces the activity bar
                        // on (and tints it warning) so it stays the visible
                        // anchor above the permission sheet even if the loop
                        // has gone idle.
                        awaiting_permission: app.pending_permission.is_some(),
                        // ~100ms per phase keeps one breathing cycle near 1.2s
                        // (SPINNER_PHASES steps); `breathing_color` wraps modulo.
                        spinner_phase: (app.spinner_epoch.elapsed().as_millis() / 100) as usize,
                        input: &masked_input,
                        byte_cursor: app.byte_cursor(),
                        chrome_hidden,
                        queue_bar: view::QueueBarView {
                            items: &queue_items,
                            paused: app.pending_count(&viewed_session_id) > 0
                                && app.idle_sessions.contains(&viewed_session_id)
                                && !app
                                    .naturally_completed_sessions
                                    .contains(&viewed_session_id),
                            blocked: app.pending_count(&viewed_session_id) > 0
                                && app.is_queue_blocked(&viewed_session_id),
                        },
                        envoy_bar,
                        side_banner,
                        session_head: Some(view::SessionHead {
                            session_id: &viewed_session_id,
                            workspace: &app.current_workspace,
                            autopilot: app.autopilot,
                        }),
                        todos: app.todos.as_ref(),
                        review_alert: app.review_alert.clone(),
                        round_started_at: app.round_started_at,
                        hovered_step: chrome_interactive.then_some(app.hovered_step).flatten(),
                        focused_target: chrome_interactive.then_some(app.focused_target).flatten(),
                        logo: app.logo.as_deref(),
                        guidance: Default::default(),
                        theme: &app.theme,
                        layout: app.transcript_layout,
                        height_cache: Some(&mut height_cache),
                    },
                );
                let input_rect = transcript_render.input_rect;
                let hint_rect = transcript_render.hint_rect;
                let activity_rect = transcript_render.activity_rect;
                let todos_rect = transcript_render.todos_rect;
                let queue_rect = transcript_render.queue_rect;
                let content_lines = transcript_render.content_lines;
                let view_height = transcript_render.view_height;
                let sticky = transcript_render.sticky;

                // The input-action hint bar (with model/context metadata on
                // the right) lives directly below the input box. It is drawn
                // before the composer because it borrows `view_messages` (an
                // immutable borrow of `app`) while `draw_composer` needs a
                // mutable borrow of `app.input_scroll`.
                // The permission sheet takes over the hint line as well as the
                // input box, so suppress the hint bar while it is open.
                if !chrome_hidden && hint_rect.height > 0 && app.active_modal != Modal::Permission {
                    // Resolve the active model's effective reasoning effort for
                    // the hint bar's `◆ {effort}` tag. Reads the same per-model
                    // channel info the `/models` picker uses
                    // (`ProviderModelInfo { effort, thinking }`), then applies
                    // the ADR-0046 per-protocol gating: Anthropic effort shows
                    // only while thinking is opted in; OpenAI effort (a
                    // standalone knob with no separate thinking field) shows
                    // whenever the model exposes one; Google never. `None`
                    // otherwise — non-reasoning models keep the bar quiet.
                    let active_provider_row = app
                        .provider_picker
                        .rows
                        .iter()
                        .find(|row| row.id == app.current_provider);
                    // The `@<instance>` suffix after the model name — the
                    // instance's display name, so identical models served by
                    // different instances stay attributable.
                    let hint_instance = active_provider_row.map(|row| row.name.as_str());
                    let hint_reasoning = active_provider_row
                        .and_then(|row| {
                            row.model_info.iter().find(|m| m.model == app.current_model)
                        })
                        .and_then(|m| {
                            let show = match m.protocol.as_str() {
                                "anthropic" => m.thinking == Some(true),
                                "openai" => m.effort.is_some(),
                                _ => false,
                            };
                            if show { m.effort.as_deref() } else { None }
                        });
                    app.hint_context_rect = view::draw_hint_bar(
                        f,
                        hint_rect,
                        view::HintBarView {
                            current_model: &app.current_model,
                            provider_name: hint_instance,
                            messages: view_messages,
                            reasoning_effort: hint_reasoning,
                            shell_active: app.focused_target.is_none()
                                && app.active_modal == Modal::None
                                && app.input.starts_with('!'),
                            busy: app.running_sessions.contains(&viewed_session_id),
                            context_tokens: app.context_tokens.map(|snapshot| snapshot.tokens),
                        },
                        &app.theme,
                    );
                } else {
                    app.hint_context_rect = None;
                }

                // The input box is only shown when no overlay modal is open. The
                // `focused` flag drops the panel to its dim "blurred" palette and
                // hides the caret whenever keyboard focus is on the conversation
                // stream (Browse zone), so the user can see at a glance which
                // surface the next keypress will land on. A pending permission
                // request replaces the composer with the inline permission sheet.
                if !chrome_hidden {
                    if app.active_modal == Modal::Permission {
                        if let Some(request) = app.pending_permission.as_ref() {
                            // Extend the slot down by the composer/hint gap plus
                            // the hint-line height so the sheet also covers
                            // (replaces) the bar below the input.
                            let permission_rect = neenee_tui_engine::Rect::new(
                                input_rect.x,
                                input_rect.y,
                                input_rect.width,
                                input_rect.height
                                    + crate::tui::design::COMPOSER_HINT_GAP_ROWS
                                    + hint_rect.height,
                            );
                            let max_scroll = view::draw_permission_sheet(
                                f,
                                &mut app.modal_hit_map,
                                request,
                                app.modal_index,
                                app.permission_confirm_always,
                                app.permission_show_details,
                                app.permission_scroll,
                                permission_rect,
                                &app.theme,
                            );
                            app.permission_max_scroll = max_scroll;
                            app.permission_scroll =
                                app.permission_scroll.min(app.permission_max_scroll);
                        }
                    } else if matches!(
                        app.active_modal,
                        Modal::Connections
                            | Modal::Models
                            | Modal::ModelEditor
                            | Modal::CustomProvider
                    ) {
                        // These modals borrow the input line as their own field
                        // (filter / key+model / history-query), so the composer
                        // underneath would only duplicate the same `app.input` the
                        // modal already shows — and, since both are bound to the
                        // one buffer, would read as a second live input field
                        // accepting the same keystrokes. Its rect stays mounted
                        // (so the footer layout is stable) but is left as recessed
                        // surface — the dim pass darkens it like the rest of the
                        // background. For the editor's key field the composer would
                        // also panic: the masked key's byte cursor is computed
                        // against the unmasked string.
                    } else if !app.in_envoy_view() {
                        // The composer stays mounted for the dim-recess modals
                        // (Help / Session /
                        // Activity) so the footer layout doesn't shift when the
                        // overlay opens or closes; the recess pass darkens it in
                        // place with the rest of the surface. When a transcript
                        // step carries keyboard focus (Ctrl+↑/↓), the composer drops
                        // to its dim "blurred" palette and hides the caret so the
                        // user can see at a glance that the next keypress targets
                        // the step, not the input box. Typing into the box clears
                        // the focus and re-brightens it immediately.
                        //
                        // `show_caret` comes straight from the single source of
                        // truth (`App::caret_visible`): in this branch the composer
                        // is the only possible caret surface (the caret-owning
                        // modals are handled by the `skip` branch above, and envoy
                        // zoom is excluded by the `!in_envoy_view` gate), so
                        // `caret_visible` reduces to "no step focus, no selection"
                        // — exactly the old hand-rolled condition, without the risk
                        // of drifting from the hide/show state machine.
                        let step_focused = app.focused_target.is_some();
                        let show_caret = app.caret_visible();
                        // A fully-typed known `/command` is painted in bold +
                        // accent color so it reads as a resolved command
                        // rather than prose; an unmatched `/`-prefix keeps
                        // the normal text color.
                        let slash_len =
                            resolved_slash_command_len(&app.input, &app.custom_commands);
                        match slash_len {
                            Some(len) => view::draw_composer_highlighted(
                                f,
                                input_rect,
                                &app.input,
                                app.byte_cursor(),
                                !step_focused,
                                show_caret,
                                &app.theme,
                                &mut layout_map,
                                true,
                                &mut app.input_scroll,
                                &app.selection,
                                len,
                                app.pending_images.len(),
                                app.pending_text_pastes.len(),
                            ),
                            None => view::draw_composer(
                                f,
                                input_rect,
                                &app.input,
                                app.byte_cursor(),
                                !step_focused,
                                show_caret,
                                &app.theme,
                                &mut layout_map,
                                true,
                                &mut app.input_scroll,
                                &app.selection,
                                app.pending_images.len(),
                                app.pending_text_pastes.len(),
                            ),
                        }
                    }
                }

                // Now that `view_messages` is no longer borrowed, persist the
                // per-frame layout state back onto `app` for the next iteration
                // and for click routing.
                // Restore the height cache (populated/refreshed during this draw)
                // so the next frame can reuse it.
                app.layout_height_cache = height_cache;
                app.content_lines = content_lines;
                app.view_height = view_height;
                app.activity_rect = activity_rect;
                app.todos_rect = todos_rect;
                app.queue_rect = queue_rect;
                // Feed the observed composer rect back so the *next* iteration's
                // immediate cursor flush (which runs before this draw closure
                // re-runs) places the caret against the geometry the user is
                // actually looking at.
                app.observe_input_rect(input_rect);
                match sticky {
                    Some(info) => {
                        app.sticky_step = Some(info.message_idx);
                        app.sticky_rect = Some(info.rect);
                        app.sticky_summary_line = Some(info.summary_line);
                    }
                    None => {
                        app.sticky_step = None;
                        app.sticky_rect = None;
                        app.sticky_summary_line = None;
                    }
                }

                // Completion menu: slash commands or `@path` file mentions.
                // Honors `completion_dismissed` so Esc / Enter-commit keep the
                // popup hidden until the next edit clears the latch. Also
                // suppressed for a fully-typed command whose exact match is the
                // text already in the box — that is a *resolved* state (the
                // composer paints it bold + accent), the popup has nothing left
                // to offer, and ↑/↓ keep walking history instead of cycling a
                // single pinned row.
                if app.active_modal == Modal::None
                    && !app.completion_dismissed
                    && app.completion_kind() != CompletionKind::None
                {
                    let completions = app.completions();
                    let exact_match = completions.iter().any(|c| {
                        c.replace_start == 0
                            && c.replace_end == app.input.len()
                            && c.label == app.input
                    });
                    if !completions.is_empty() && !exact_match {
                        // Hang the popup's leading edge off the trigger token
                        // it completes — column 0 of the composer text area
                        // for a `/command`, the `@`'s column for a path
                        // mention — so the menu aligns with what was typed
                        // even after the line wraps.
                        let anchor_x = completion_anchor_x(
                            &app.input,
                            app.byte_cursor(),
                            input_rect,
                            app.completion_kind(),
                        );
                        view::draw_completion_menu(
                            f,
                            &mut layout_map,
                            &completions,
                            app.suggestion_index,
                            input_rect,
                            anchor_x,
                            &app.theme,
                        );
                    }
                }

                // Recess the live surface for the open modal: darken it in place
                // (Dim), occlude it fully (Takeover), or leave it untouched (None).
                // Done after the transcript + chrome are drawn and before the modal
                // panel so the panel overpaints its own crisp area on top of the
                // recessed background.
                view::recess_backdrop(f, recess, &app.theme);

                let spinner_phase = (app.spinner_epoch.elapsed().as_millis() / 100) as usize;

                // The dashboard reports its true list-body height through this
                // slot (its body is not the centered panel-minus-chrome the
                // shared post-match math assumes). Reset each frame; only the
                // `Modal::Host` arm sets it.
                let mut dashboard_list_body_height: Option<u16> = None;

                // Modals
                let drawn_modal_rect = match app.active_modal {
                    Modal::Connections => {
                        let providers = app.providers_filtered();
                        Some(view::draw_connections_modal(
                            f,
                            &mut layout_map,
                            &providers,
                            &app.current_provider,
                            app.modal_index,
                            &app.input,
                            app.cursor_position,
                            &mut app.model_scroll,
                            app.model_modal_follow,
                            app.model_search,
                            app.modal_keymap_open,
                            &app.theme,
                        ))
                    }
                    Modal::Models => {
                        let models = app.models_flat_filtered();
                        Some(view::draw_models_modal(
                            f,
                            &mut layout_map,
                            &models,
                            &app.current_provider,
                            &app.current_model,
                            app.modal_index,
                            &app.input,
                            app.cursor_position,
                            &mut app.model_scroll,
                            app.model_modal_follow,
                            app.model_search,
                            app.modal_keymap_open,
                            &app.theme,
                        ))
                    }
                    Modal::HistorySearch => {
                        let ranked = app.history_rows();
                        // The activity bar sits directly above the composer, so
                        // reserve its rows: the dropdown must never paint over
                        // the live status bar above it. `activity_rect` carries
                        // the bar's exact footprint this frame (None when idle,
                        // height 0).
                        let activity_height = activity_rect.map_or(0, |r| r.height);
                        view::draw_history_panel(
                            f,
                            &app.input_history,
                            &ranked,
                            app.modal_index,
                            &mut app.history_scroll,
                            app.history_modal_follow,
                            app.history_preview,
                            app.modal_keymap_open,
                            input_rect,
                            activity_height,
                            &app.theme,
                        )
                    }
                    Modal::Permission => None,
                    Modal::InputInjection => {
                        if let Some(ref req) = app.pending_input {
                            Some(view::draw_input_injection(
                                f,
                                req,
                                &app.input,
                                app.cursor_position,
                                input_rect,
                                &app.theme,
                            ))
                        } else {
                            None
                        }
                    }
                    Modal::Question => {
                        if let Some(ref qmodel) = app.question {
                            Some(view::draw_question_modal(
                                f,
                                &mut app.modal_hit_map,
                                qmodel.request(),
                                qmodel.current(),
                                qmodel.selected(),
                                qmodel.other_text(),
                                qmodel.highlight(),
                                &mut app.question_scroll,
                                app.question_modal_follow,
                                &app.theme,
                            ))
                        } else {
                            None
                        }
                    }
                    Modal::ModelEditor => {
                        let title = if app.editor_model_settings_only {
                            crate::tui::model_display_name(&app.editor_model)
                        } else {
                            app.editor_target
                                .as_deref()
                                .and_then(|id| app.provider_picker.rows.iter().find(|r| r.id == id))
                                .map(|r| r.name.clone())
                                .unwrap_or_else(|| "model".to_string())
                        };
                        // ADR-0046: the effort/thinking rows belong ONLY to the
                        // per-model settings editor (`editor_model_settings_only`,
                        // opened from the Models picker). The provider key editor
                        // never shows them — reasoning is set per model, not per
                        // provider.
                        let effort = app
                            .editor_model_settings_only
                            .then_some(app.editor_effort.as_str());
                        let thinking = app
                            .editor_model_settings_only
                            .then_some(app.editor_thinking)
                            .filter(|_| app.editor_thinking_available);
                        Some(view::draw_model_editor(
                            f,
                            &title,
                            &app.input,
                            app.cursor_position,
                            !app.editor_model_settings_only,
                            app.editor_field,
                            effort,
                            thinking,
                            &app.theme,
                        ))
                    }
                    Modal::ProviderTemplate => Some(view::draw_provider_template_chooser(
                        app.template_choice,
                        f,
                        &app.theme,
                        &mut app.template_scroll,
                    )),
                    Modal::OauthPending => {
                        let title: &'static str = match app.custom_auth {
                            neenee_core::ChannelAuth::ChatGptOAuth => "ChatGPT",
                            neenee_core::ChannelAuth::CopilotOAuth => "Copilot",
                            neenee_core::ChannelAuth::XaiOAuth => "xAI",
                            neenee_core::ChannelAuth::ApiKey => "OAuth",
                        };
                        Some(view::draw_oauth_pending(
                            title,
                            &app.oauth_pending_message,
                            &app.oauth_pending_url,
                            &app.oauth_pending_user_code,
                            app.oauth_pending_error.as_deref(),
                            f,
                            &app.theme,
                            &mut app.oauth_scroll,
                        ))
                    }
                    Modal::CustomProvider => {
                        let editing = app.custom_is_editing();
                        let title = if editing {
                            format!("Edit · {}", app.custom_name)
                        } else {
                            crate::tui::provider_template_label_for(&app.custom_protocol_wire)
                        };
                        let model_display = if app.custom_model.is_empty() {
                            "—".to_string()
                        } else {
                            crate::tui::model_display_name(&app.custom_model)
                        };
                        // Suggestion dropdown for the Model filter field.
                        let suggestions: Vec<String> =
                            if app.current_custom_field() == Some(crate::tui::CustomField::Model) {
                                app.custom_model_suggestions()
                                    .iter()
                                    .map(|v| crate::tui::model_display_name(v))
                                    .collect()
                            } else {
                                Vec::new()
                            };
                        Some(view::draw_custom_provider_editor(
                            view::CustomEditorView {
                                fields: &app.custom_fields,
                                field: app.custom_field,
                                editing,
                                title: &title,
                                name_buf: &app.custom_name,
                                base_url_buf: &app.custom_base_url,
                                token_buf: &app.custom_token,
                                model_display: &model_display,
                                url_hint: &app.custom_url_hint,
                                suggestions: &suggestions,
                                suggest_index: app.custom_suggest_index,
                                input: &app.input,
                                cursor_position: app.cursor_position,
                            },
                            f,
                            &app.theme,
                            &mut app.custom_scroll,
                        ))
                    }
                    Modal::Help => {
                        // Project the global-keybinding registry into the rows
                        // the Help modal renders. Help and the live input
                        // resolver share the same registry, so the keys shown
                        // here can never drift from the keys that actually fire.
                        let bindings: Vec<view::HelpBinding> = crate::tui::keymap::Registry::new()
                            .bindings()
                            .iter()
                            .map(|b| view::HelpBinding {
                                // Help prose rows use the compact lowercase
                                // chord form (`ctrl+t`), sourced from the same
                                // vocabulary the footers' capitalized form
                                // (`Ctrl+T`) derives from.
                                key: b.key.chord(),
                                description: b.description,
                            })
                            .collect();
                        Some(view::draw_help_modal(
                            f,
                            &mut app.help_scroll,
                            &bindings,
                            &app.theme,
                        ))
                    }
                    Modal::Sessions => Some(view::draw_sessions_modal(
                        f,
                        &app.sessions_overview,
                        app.modal_index
                            .min(app.sessions_overview.len().saturating_sub(1)),
                        app.modal_keymap_open,
                        &mut app.session_scroll,
                        app.session_modal_follow,
                        &app.theme,
                        app.startup_overlay == crate::tui::StartupOverlay::SessionsPicker,
                        spinner_phase,
                        app.session_info_detail,
                        app.session_detail.as_ref(),
                        &mut app.session_info_scroll,
                    )),
                    Modal::Host => {
                        // The session dashboard is a first-class, full-screen
                        // surface (Recess::Takeover already occluded the
                        // conversation): lay it out over the whole viewport
                        // instead of a centered modal rect.
                        let rects = view::draw_dashboard(
                            f,
                            &app.host_sessions,
                            app.modal_index
                                .min(app.host_sessions.len().saturating_sub(1)),
                            app.host_focus,
                            app.modal_keymap_open,
                            &mut app.host_scroll,
                            app.host_modal_follow,
                            &mut app.host_detail_scroll,
                            app.host_prompting,
                            app.host_prompt_new,
                            &app.input,
                            &app.theme,
                            spinner_phase,
                            &viewed_session_id,
                        );
                        // Stash the list-body height so the page-scroll step
                        // (computed after this match from `drawn_modal_rect`)
                        // can use the real body height, not panel-minus-chrome.
                        dashboard_list_body_height = Some(rects.list_body.height);
                        // The session preview overlays the dashboard (Enter on
                        // a dock selection). Rendered after the dashboard so
                        // it floats on top.
                        if let Some(preview_id) = &app.host_preview {
                            let row = app.host_sessions.iter().find(|r| &r.id == preview_id);
                            view::draw_session_preview(
                                f,
                                row,
                                &mut app.host_preview_scroll,
                                &app.theme,
                            );
                        }
                        Some(rects.area)
                    }
                    Modal::TokenReport => {
                        // Snapshot the shared ledger (standalone path) or the
                        // on-demand harness reply (attach path); the attach
                        // path renders a loading placeholder until the reply
                        // lands.
                        let report = app.token_source_report(&viewed_session_id);
                        let loading = app.token_ledger.is_none() && report.is_none();
                        let report = report.unwrap_or_default();
                        Some(view::draw_token_report_modal(
                            f,
                            &report,
                            view::ContextUsageView {
                                snapshot: app.context_tokens,
                                window_tokens: crate::tui::providers::model_context_window(
                                    &app.current_model,
                                ),
                                round_summary: app.round_tps,
                            },
                            app.modal_index
                                .min(view::token_report_round_count(&report).saturating_sub(1)),
                            app.token_report_detail,
                            loading,
                            &mut app.token_report_scroll,
                            &app.theme,
                        ))
                    }
                    Modal::Tools => Some(view::draw_tools_modal(
                        f,
                        app.session_context.as_ref(),
                        app.modal_index,
                        &mut app.session_scroll,
                        app.session_modal_follow,
                        &app.theme,
                    )),
                    Modal::Mcp => Some(view::draw_mcp_modal(
                        f,
                        app.session_context.as_ref(),
                        app.modal_index,
                        &mut app.session_scroll,
                        app.session_modal_follow,
                        &app.theme,
                    )),
                    Modal::Skills => Some(view::draw_skills_modal(
                        f,
                        app.session_context.as_ref(),
                        app.modal_index,
                        app.skills_expanded,
                        &mut app.session_scroll,
                        &app.theme,
                    )),
                    Modal::Permissions => Some(view::draw_permissions_manager(
                        f,
                        app.session_context.as_ref(),
                        app.modal_index,
                        &mut app.permissions_scroll,
                        &app.theme,
                    )),
                    Modal::Config => Some(view::draw_config_modal(
                        f,
                        app.modal_index,
                        &mut app.config_scroll,
                        view::ConfigOverview {
                            color_scheme: &app.color_scheme,
                            layout: app.transcript_layout,
                        },
                        app.modal_keymap_open,
                        &app.theme,
                    )),
                    Modal::ConfigTheme => Some(view::draw_config_theme_modal(
                        f,
                        &app.color_scheme,
                        &app.custom_color_scheme,
                        app.modal_index
                            .min(crate::tui::view::overlays::config_theme::ROW_COUNT - 1),
                        &mut app.config_scroll,
                        app.modal_keymap_open,
                        &app.theme,
                    )),
                    Modal::ConfigThemeCustom => Some(view::draw_config_theme_custom_modal(
                        f,
                        &app.custom_color_draft,
                        app.modal_index
                            .min(crate::tui::view::overlays::config_theme_custom::ROW_COUNT - 1),
                        &app.input,
                        app.cursor_position,
                        &mut app.config_scroll,
                        &app.theme,
                    )),
                    Modal::ConfigLayout => Some(view::draw_config_layout_modal(
                        f,
                        app.transcript_layout,
                        app.modal_index
                            .min(crate::tui::view::overlays::config_layout::ROW_COUNT - 1),
                        &mut app.config_scroll,
                        app.modal_keymap_open,
                        &app.theme,
                    )),
                    Modal::Activity => {
                        let user_prompt: Option<String> = app
                            .focused_messages()
                            .iter()
                            .rev()
                            // Only a genuine chat prompt is the round's driving
                            // prompt. Slash commands (`/review …`) and shell
                            // passthroughs (`!ls`) are surfaced as `Role::User`
                            // in the transcript but are handled by the harness /
                            // bash tool, never seen by the model — so they must
                            // not be shown as the Activity modal's "Prompt".
                            .find(|m| {
                                m.role == neenee_core::Role::User
                                    && m.origin
                                        == crate::tui::model::document::UserMessageOrigin::Chat
                            })
                            .map(|m| m.raw.clone());
                        Some(view::draw_activity_modal(
                            f,
                            view::ActivityModalView {
                                active_tab: app.activity_tab,
                                todos: app.todos.as_ref(),
                                user_prompt: user_prompt.as_deref(),
                                round_count: app.round_count,
                                current_turn: app.current_turn,
                                review_alert: &app.review_alert,
                                current_model: app.current_model.as_str(),
                                round_started_at: app.round_started_at,
                                activity: &status,
                            },
                            &mut app.activity_scroll,
                            &app.theme,
                        ))
                    }
                    Modal::Queue => Some(view::draw_queue_modal(
                        f,
                        view::QueueModalView {
                            items: &queue_items,
                            paused: app.pending_count(&viewed_session_id) > 0
                                && app.idle_sessions.contains(&viewed_session_id)
                                && !app
                                    .naturally_completed_sessions
                                    .contains(&viewed_session_id),
                            blocked: app.pending_count(&viewed_session_id) > 0
                                && app.is_queue_blocked(&viewed_session_id),
                        },
                        app.modal_index,
                        &mut app.queue_scroll,
                        app.queue_modal_follow,
                        &app.theme,
                    )),
                    Modal::None => None,
                };

                // Provider-delete confirm overlay: a sub-layer painted *on top
                // of* the Connections list. Drawn after the picker so it
                // overpaints its own dimmed backdrop + centered panel, leaving
                // the list visible (dimmed) behind it. Only present while a
                // deletion is staged from `Shift+D`.
                if app.active_modal == Modal::Connections
                    && let Some(ref pending_id) = app.pending_provider_delete
                {
                    let provider_name = app
                        .provider_picker
                        .rows
                        .iter()
                        .find(|r| &r.id == pending_id)
                        .map(|r| r.name.clone())
                        .unwrap_or_else(|| pending_id.clone());
                    app.provider_delete_rect = Some(view::draw_provider_delete_confirm(
                        f,
                        &provider_name,
                        match app.provider_delete_focus {
                            ProviderDeleteChoice::Cancel => view::ProviderDeleteChoiceView::Cancel,
                            ProviderDeleteChoice::Delete => view::ProviderDeleteChoiceView::Delete,
                        },
                        &app.theme,
                    ));
                } else {
                    app.provider_delete_rect = None;
                }

                // Copy toast
                if app.copy_toast_until.is_some() {
                    view::draw_copy_toast(
                        f,
                        &app.copy_toast_message,
                        app.copy_toast_failed,
                        &app.theme,
                    );
                } else if app.notice_toast_until.is_some() {
                    // A toast-surfaced command acknowledgment (e.g.
                    // `/autopilot on`). Rendered only when no copy toast is
                    // showing, since the two share the same top-right slot.
                    view::draw_notice_toast(
                        f,
                        &app.notice_toast_message,
                        app.notice_toast_severity,
                        &app.theme,
                    );
                } else if app.ctrl_c_armed() {
                    // The copy toast and the armed toast render at the same
                    // screen position, so only one shows at a time. The
                    // clearing-input path surfaces the armed state through the
                    // copy toast itself ("input cleared — Ctrl+C again to
                    // exit"); once it expires, the standalone armed toast
                    // takes over for the remainder of the quit window.
                    view::draw_armed_toast(f, "press Ctrl+C again to exit", &app.theme);
                }
                if app.esc_armed_ticks > 0 {
                    view::draw_armed_toast(f, "press Esc again to interrupt", &app.theme);
                }

                app.layout_map = layout_map;

                // Capture the open modal's body height for page-scroll step
                // sizing. The renderer returns the full panel rect; the body is
                // that rect minus the header/footer/padding chrome. All
                // centered modals that paint a scrollable body use the same
                // `modal_frame(header, footer)` chrome, so the row count is the
                // shared `modal_chrome_rows` for a header+footer spec. Stays 0
                // for modals that return no rect (Permission sheet, which
                // scrolls the transcript behind it via `view_height` instead),
                // so the page step falls back to the transcript height there.
                app.modal_body_height = match dashboard_list_body_height {
                    // The dashboard's scroll body is its list pane, whose height
                    // was reported directly by the renderer.
                    Some(h) => h,
                    None => drawn_modal_rect
                        .map(|r| {
                            r.height
                                .saturating_sub(crate::tui::primitives::modal_chrome_rows(
                                    crate::tui::primitives::ModalSpec {
                                        width_percent: 0,
                                        header: true,
                                        footer: true,
                                    },
                                ))
                        })
                        .unwrap_or(0),
                };

                // Record the open modal's actual panel rect (when one is
                // dismissable) so a click on the backdrop outside it can close it.
                // The rect comes from the renderer that just painted the panel, so
                // dynamic-height modals and click hit-tests cannot drift apart.
                app.modal_rect = if app.active_modal.dismissable_by_outside_click() {
                    drawn_modal_rect
                } else {
                    None
                };
            };
            if stage_bottom_follow {
                terminal.stage(render_frame)?;
            } else {
                terminal.draw(render_frame)?;
            }
        } // end `if needs_draw`

        // Recompute the bottom scroll offset for the next frame and keep the
        // manual scroll position within bounds when not following.
        let natural_max = app.content_lines.saturating_sub(app.view_height as usize) as u16;
        // `app.max_scroll` stays at the natural bottom so scroll shortcuts
        // (ScrollBottom / wheel down) still land on the real last page.
        app.max_scroll = natural_max;
        if !app.follow_bottom {
            // A collapsed sticky header may leave too little content below it
            // for `natural_max` to reach the header line; while a pin is
            // active, allow scrolling up to that line so the header stays at
            // the top of the viewport instead of being dragged back down.
            let limit = app
                .pin_summary_line
                .map(|line| natural_max.max(line.min(u16::MAX as usize) as u16))
                .unwrap_or(natural_max);
            app.scroll = app.scroll.min(limit);
        }

        // The staged pass above measured the new content but emitted no bytes.
        // If the bottom moved, redraw immediately at the final offset. If it
        // did not, commit the already-final staged grid without a second layout.
        if stage_bottom_follow && needs_draw {
            if app.scroll != app.max_scroll {
                app.scroll = app.max_scroll;
                input_redraw_pending = true;
                continue;
            }
            terminal.commit_staged()?;
        }
        app.retain_visible_focused_target();

        // Drain all currently-ready input events before redrawing. The first
        // event is awaited (alongside non-input wakeups); any further events the
        // terminal has already queued are coalesced with non-blocking polls so
        // they share a single redraw. Without this, pasting text triggers one
        // full screen redraw per pasted character.
        let mut events_drained = false;
        loop {
            let event = if events_drained {
                // Coalesce already-queued events into this same redraw. A
                // non-blocking `try_recv` takes any event the reader thread has
                // already queued; an empty channel ends the drain. Unlike the
                // old `EventStream::next().now_or_never()`, this registers no
                // stray waker, so the parked `select!` above stays wakeable by
                // the next real keystroke.
                match input_rx.try_recv() {
                    Ok(ev) => ev,
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => return Ok(()),
                }
            } else {
                // First wakeup of the iteration: await an input event OR a
                // non-input signal that warrants a redraw — a listener state
                // change (immediate, via `Notify`), the animation tick (only
                // while something is animating), or a completed background
                // clipboard read/copy. Anything other than an input event just
                // breaks out so the top of the loop re-renders.
                tokio::select! {
                    biased;
                    maybe = input_rx.recv() => match maybe {
                        Some(ev) => ev,
                        // Sender dropped: the reader thread stopped (terminal
                        // read error or shutdown) — exit.
                        None => return Ok(()),
                    },
                    _ = runtime.dirty_notify.notified() => break,
                    // Adaptive heartbeat: ~10fps while animating (advances the
                    // spinner / expires toasts), and a slow 1s idle tick that
                    // re-checks `should_quit` and any state a wakeup might have
                    // missed. The idle tick is cheap — with nothing dirty the
                    // top of the loop skips the draw entirely.
                    _ = tokio::time::sleep(std::time::Duration::from_millis(
                        if animating { 100 } else { 1000 },
                    )) => break,
                    Some(result) = copy_rx.recv() => {
                        clipboard_ops::set_copy_feedback(app, result);
                        app.copy_toast_until = Some(
                            std::time::Instant::now()
                                + std::time::Duration::from_millis(1800),
                        );
                        input_redraw_pending = true;
                        break;
                    }
                    Some(read) = paste_rx.recv() => {
                        clipboard_ops::apply_clipboard_paste(app, read);
                        input_redraw_pending = true;
                        break;
                    }
                }
            };
            events_drained = true;
            // Any input this iteration means the next frame must redraw (input
            // is drained here, at the end of the iteration, but rendered at the
            // start of the next one).
            input_redraw_pending = true;
            // The Ctrl+R history modal's search sub-layer borrows the input line
            // as its fuzzy query, so a literal `/foo` query must NOT trigger the
            // slash completion popup (or `@path` mentions); browse mode keeps the
            // line empty. Either way, suppress completions while the modal is
            // open. The same suppression applies right after an Enter-driven
            // commit: the user just finished a completion, so the popup should
            // stay hidden until the next edit.
            let suppress_completions =
                app.active_modal == Modal::HistorySearch || app.completion_dismissed;
            // Pre-compute completion data to avoid borrow conflicts with process_event.
            let completions = if suppress_completions {
                Vec::new()
            } else {
                app.completions()
            };
            let suggestion_count = completions.len();
            // The "exact match" auto-accept on Enter only makes sense for slash
            // commands: there, typing an unambiguous prefix and pressing Enter
            // should expand to the unique command rather than send `/go` as a
            // (rejected) command. Path mentions are accepted only via Tab so
            // plain Enter still ships the message as the user typed it.
            let has_exact_suggestion = completions.iter().any(|c| {
                c.replace_start == 0 && c.replace_end == app.input.len() && c.label == app.input
            });
            // Whether the composer's first `/token` is a recognized command.
            // Used by the Enter handler to route a `/`-leading input as a
            // command (only when known) or fall back to chat (so a stray `/`
            // doesn't trip the backend's "Unknown command" error). Covers
            // built-ins, the app's discovered custom commands, and the
            // frontend-only `/serve`.
            let recognized_command = app
                .input
                .split_whitespace()
                .next()
                .map(|first| {
                    crate::startup::BuiltinCmd::from_slash(first).is_some()
                        || app.custom_commands.iter().any(|(name, _)| name == first)
                        || first == "/serve"
                })
                .unwrap_or(false);
            let completion_kind = if suppress_completions {
                crate::tui::CompletionKind::None
            } else {
                app.completion_kind()
            };
            let in_envoy_view = app.in_envoy_view();
            // SGR leakage backstop: drop any stray mouse-sequence fragment
            // before it can reach `process_event` and be inserted as text.
            // `continue` keeps the drain loop alive (events_drained stays set)
            // without costing a redraw beyond the input-pending flag already
            // raised for this iteration.
            if matches!(sgr_guard.feed(&event), input::Feed::Drop) {
                continue;
            }
            // Text-triggered modal commands (`/models`, `/tools`, …) consume
            // the composer text the same way `SendSlash` does, but the action
            // they resolve to is data-less (e.g. `OpenModels`), so the typed
            // `/cmd` — which should still be recallable from input history —
            // would be lost. Snapshot the composer before `process_event`
            // mutates it, then record it after dispatch if the resulting action
            // is one of those commands. Keybinding-driven modals (Ctrl+R, F1,
            // …) open with an empty composer and are excluded by the predicate.
            let modal_cmd_history = (!app.input.is_empty())
                .then(|| app.input.clone())
                .filter(|_| !app.input.starts_with('!') && matches!(app.active_modal, Modal::None));
            // The provider-delete confirm overlay is a sub-layer over the
            // stage-1 Connections list: when it is open it owns every key, so
            // probe the raw event before the general input mapper and skip
            // `process_event` entirely (the latter would otherwise edit the
            // composer or move the list selection behind the panel). The
            // returned action flows through the normal `match action`
            // dispatch below (`DeleteProviderConfirm` / `DeleteProviderCancel`
            // are the overlay-specific arms).
            let action = if let Some(overlay_action) = probe_delete_overlay(app, &event) {
                overlay_action
            } else {
                input::process_event(
                    event,
                    &mut app.input,
                    &mut app.cursor_position,
                    input::InputContext {
                        active_modal: app.active_modal,
                        session_info_detail: app.session_info_detail,
                        is_responding: app.running_sessions.contains(&viewed_session_id),
                        completion_kind,
                        suggestion_count,
                        has_exact_suggestion,
                        suggestion_index: app.suggestion_index,
                        permission_confirm_always: app.permission_confirm_always,
                        permission_show_details: app.permission_show_details,
                        in_envoy_view,
                        in_side_view: app.in_side_view,
                        has_focused_target: app.focused_target.is_some(),
                        has_queued: app.pending_dispatch.iter().any(|item| {
                            item.session_id == viewed_session_id
                                && item.state == crate::tui::app::QueuedDispatchState::Waiting
                        }),
                        history_searching: app.history_search,
                        model_searching: app.model_search,
                        modal_keymap_open: app.modal_keymap_open,
                        custom_provider_field: (app.active_modal == Modal::CustomProvider)
                            .then_some(app.custom_field),
                        editor_field: (app.active_modal == Modal::ModelEditor)
                            .then_some(app.editor_field),
                        question_other_highlighted: app
                            .question
                            .as_ref()
                            .is_some_and(|q| q.is_other_highlighted()),
                        history_clear_confirm: app.history_clear_confirm,
                        host_prompting: app.host_prompting,
                    },
                    &mut app.drag,
                )
            };

            // `process_event` mutates `cursor_position` in place (it cannot go
            // through `App::set_cursor`), so any keystroke that moved the caret
            // must still mark the terminal cursor for an immediate re-sync.
            app.note_cursor_moved();

            // A `/`-leading input whose first token is NOT a recognized command
            // (built-in, discovered custom command, or the frontend-only
            // `/serve`) is ordinary prose, not a command invocation: the `/` is
            // just a character the user typed. Convert the SendSlash the input
            // layer produced into a SendChat so the message ships normally
            // instead of tripping the backend's "Unknown command" error. The
            // recognition is computed here (not in the input layer) because
            // only the loop has access to both the built-in vocabulary and the
            // app's discovered custom commands.
            let action =
                if matches!(action, input::InputAction::SendSlash(_)) && !recognized_command {
                    if let input::InputAction::SendSlash(text) = action {
                        input::InputAction::SendChat(text)
                    } else {
                        action
                    }
                } else {
                    action
                };

            // Record a text-triggered modal command (`/models`, `/tools`, …)
            // in BOTH the transcript and input history, exactly like the
            // notification-style slash commands routed through `SendSlash`.
            //
            // This is the consistency point between the two command families:
            // whether a command surfaces as a modal or as an inline reply, the
            // user's invocation is recorded the same way — recallable from
            // Ctrl+R history and visible in the scrollback. Modal *outcomes*
            // (e.g. a provider switch) are emitted separately by the harness
            // listener as follow-up notices, so the transcript reads as a
            // natural pair: `> /models` then `↳ Provider switched to …`.
            //
            // The composer text was consumed by `process_event` (the action is
            // data-less), so we replay it from the pre-dispatch snapshot.
            if action.is_text_modal_command()
                && let Some(entry) = modal_cmd_history
            {
                runtime.messages.write().await.push(
                    TranscriptMessage::new(Role::User, entry.clone())
                        .with_origin(UserMessageOrigin::Slash),
                );
                app.record_input_history(entry, Vec::new(), Vec::new());
            }

            match action {
                input::InputAction::None => {}
                input::InputAction::TerminalResized => {
                    // A resize is the prime trigger for crossterm splitting an
                    // in-flight SGR mouse sequence across reads (issue #854).
                    // Re-arm mouse capture so both crossterm's parser and the
                    // terminal's mouse-tracking state start from a clean slate,
                    // and force an immediate redraw to replace the stale frame
                    // at the old geometry. The re-arm is best-effort: if the
                    // terminal is mid-shutdown the write is ignored.
                    use crossterm::event::EnableMouseCapture;
                    let _ = crossterm::execute!(terminal.backend().writer(), EnableMouseCapture);
                    sgr_guard.reset();
                    // No need to set `frame_dirty` here: every drained event
                    // already raised `input_redraw_pending`, which forces a
                    // redraw on the very next frame at the new geometry.
                }
                input::InputAction::Quit => {
                    // Now reachable only via the `/exit` slash command (the bare
                    // `q` shortcut was removed to stop accidental first-key exits).
                    tracing::info!(reason = "slash_exit", "app exiting");
                    return Ok(());
                }
                input::InputAction::SendChat(text) => {
                    // Note: history-search selection no longer flows through
                    // here — Enter in `Modal::HistorySearch` emits the dedicated
                    // `HistoryInsert` action so the chosen entry lands in the
                    // input box for editing instead of being sent immediately.
                    app.active_modal = Modal::None;
                    app.suggestion_index = None;
                    app.input_scroll = 0;

                    // Stage the chips' backing payloads so they ship with
                    // this message. The text is expanded into the real paste
                    // contents at the moment of dispatch — either inline
                    // (immediate send) or when the queue drains (queued
                    // send). For queue recall, the raw chip text and the
                    // staged vectors are restored verbatim so the user can
                    // keep editing the placeholder.
                    let images = std::mem::take(&mut app.pending_images);
                    let text_pastes = std::mem::take(&mut app.pending_text_pastes);
                    let has_images = !images.is_empty();

                    if !text.is_empty() || has_images {
                        if app.running_sessions.contains(&viewed_session_id) {
                            // Busy sends live in the fixed outbox, not the
                            // scrollback. A staged message always waits for the
                            // running round to finish naturally before starting a
                            // new one (next-round only); there is no mid-round
                            // insert path.
                            let id = uuid::Uuid::new_v4().to_string();
                            let queued_at_ms = now_epoch_ms();
                            app.pending_dispatch
                                .push_back(crate::tui::app::QueuedDispatch {
                                    id: id.clone(),
                                    session_id: viewed_session_id.clone(),
                                    state: crate::tui::app::QueuedDispatchState::Waiting,
                                    text: text.clone(),
                                    queued_at_ms,
                                    images: images.clone(),
                                    text_pastes: text_pastes.clone(),
                                });
                            app.record_input_history(
                                text.clone(),
                                images.clone(),
                                text_pastes.clone(),
                            );
                            // The draft's content has been taken into the
                            // outbox — it is no longer the unsent slot.
                            app.clear_history_draft();
                            app.follow_bottom = true;
                            app.pin_summary_line = None;
                        } else {
                            // Expand `[Pasted text #N +M lines]` chips into
                            // their full staged text right before dispatch so
                            // the model receives the real paste contents
                            // rather than the chip label. Image chips stay
                            // in the text as positional labels.
                            let expanded =
                                composer_attachments::expand_paste_chips(&text, &text_pastes);
                            // An image chip with no staged payload (e.g.
                            // recalled from a history entry recorded before
                            // attachment staging) is a bare label — drop it
                            // so the model never receives a phantom
                            // `[Image #N …]` it cannot see.
                            let expanded = composer_attachments::strip_orphan_image_chips(
                                &expanded,
                                images.len(),
                            );
                            if !app.in_side_view {
                                runtime.is_responding.store(true, Ordering::SeqCst);
                                *runtime.activity_status.lock().await = "queued".to_string();
                            }
                            app.idle_sessions.remove(&viewed_session_id);
                            app.running_sessions.insert(viewed_session_id.clone());
                            let sent_at_ms = now_epoch_ms();
                            let sent = TranscriptMessage::new(Role::User, text.clone())
                                .with_sent_at_ms(sent_at_ms);
                            if !app.in_side_view {
                                runtime.messages.write().await.push(sent);
                            } else {
                                runtime.side_messages.write().await.push(sent);
                            }
                            app.record_input_history(
                                text.clone(),
                                images.clone(),
                                text_pastes.clone(),
                            );
                            // The draft's content has been sent — it is now a
                            // history row, not the unsent slot.
                            app.clear_history_draft();
                            app.follow_bottom = true;
                            app.pin_summary_line = None;
                            let _ = app.tx.send(AgentRequest::Chat {
                                text: expanded,
                                images,
                                sent_at_ms: Some(sent_at_ms),
                            });
                        }
                    } else if let Some((start, end)) = app.selection.active_normalized_range() {
                        // Enter on a selected step: navigate into an envoy
                        // task, otherwise toggle that step's expansion.
                        if start.message_idx == end.message_idx {
                            let mi = start.message_idx;
                            let mut messages = runtime.messages.write().await;
                            // An envoy task navigates into its view instead
                            // of expanding.
                            let enter_id = resolve_focused_mut(&mut messages, &app.focus_stack, mi)
                                .and_then(|message| {
                                    if message.is_envoy_task() {
                                        message.tool_step_call_id().map(String::from)
                                    } else {
                                        None
                                    }
                                });
                            if let Some(id) = enter_id {
                                drop(messages);
                                app.enter_envoy(id);
                            } else {
                                let toggled = app.toggle_step_pinned(&mut messages, mi);
                                drop(messages);
                                if toggled {
                                    app.selection = SelectionState::None;
                                }
                            }
                        }
                    }
                }
                input::InputAction::SendSlash(cmd) => {
                    app.suggestion_index = None;
                    app.input_scroll = 0;
                    // A running round owns the activity surface. Do not paint
                    // an optimistic "queued" over its live label, and do not
                    // arm the responding flag for a control-plane command the
                    // round did not ask for: the round's own events keep the
                    // bar truthful, and the command's reply must not be able
                    // to leave a fabricated "queued" behind.
                    let session_busy = app.running_sessions.contains(&viewed_session_id);
                    if !session_busy {
                        runtime.is_responding.store(true, Ordering::SeqCst);
                        *runtime.activity_status.lock().await = "queued".to_string();
                    }
                    app.follow_bottom = true;
                    app.pin_summary_line = None;
                    runtime
                        .messages
                        .write()
                        .await
                        // A slash command is surfaced as a user message in the
                        // transcript (so history recall shows the `/cmd`), but
                        // it is NOT the prompt driving the model — the harness
                        // handles it directly. Tag it so the Activity modal
                        // does not mistake it for the round's prompt.
                        .push(
                            TranscriptMessage::new(Role::User, cmd.clone())
                                .with_origin(UserMessageOrigin::Slash),
                        );
                    app.record_input_history(cmd.clone(), Vec::new(), Vec::new());
                    // `/serve` is a pure frontend concern (hot-attach a
                    // WebSocket listener to the running session). Intercept
                    // it here rather than routing through SessionDriver.
                    if cmd == "/serve" || cmd.starts_with("/serve ") {
                        // Serving needs the in-process session store. In
                        // attach mode the session already lives on a server,
                        // so there is nothing local to serve — say so instead
                        // of forwarding the command to the harness (where it
                        // would surface as an unknown command).
                        let store = match session.local_store() {
                            Some(store) => store,
                            None => {
                                runtime.messages.write().await.push(
                                    TranscriptMessage::new(
                                        Role::Assistant,
                                        "Already attached to a session server; \
                                         /serve is only available in standalone mode."
                                            .to_string(),
                                    )
                                    .with_origin(UserMessageOrigin::Slash),
                                );
                                // `/serve` resolves inline (never reaches the
                                // driver), so retire the optimistic "queued"
                                // state ourselves — and only when we painted
                                // it (a live round owns the flag otherwise).
                                if !session_busy {
                                    runtime.is_responding.store(false, Ordering::SeqCst);
                                    runtime.activity_status.lock().await.clear();
                                }
                                continue;
                            }
                        };
                        let tokens = cmd.split_whitespace().collect::<Vec<_>>();
                        let mut port: u16 = 0;
                        let mut expose_public = false;
                        // Parse `/serve [port] [--public]` in any order.
                        for arg in &tokens[1..] {
                            if *arg == "--public" {
                                expose_public = true;
                            } else if let Ok(p) = arg.parse::<u16>() {
                                port = p;
                            }
                        }
                        let mut tap = app.serve_tap.lock().await;
                        if tap.is_some() {
                            // `/serve` with no arg while active = stop.
                            if cmd == "/serve" {
                                *tap = None;
                                if let Some(ct) = app.serve_cancel.take() {
                                    ct.cancel();
                                }
                                runtime.messages.write().await.push(
                                    TranscriptMessage::new(
                                        Role::Assistant,
                                        "Serve mode stopped.".to_string(),
                                    )
                                    .with_origin(UserMessageOrigin::Slash),
                                );
                            } else {
                                runtime.messages.write().await.push(
                                    TranscriptMessage::new(
                                        Role::Assistant,
                                        "Serve already active. Use /serve (no args) to stop."
                                            .to_string(),
                                    )
                                    .with_origin(UserMessageOrigin::Slash),
                                );
                            }
                        } else {
                            let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
                            *tap = Some(bc_tx.clone());
                            // Local (loopback, no token) is the default; `--public`
                            // binds all interfaces and forces a bearer token that
                            // the client must send as `Authorization: Bearer <t>`.
                            let expose = if expose_public {
                                neenee_transport::serve::ServeExpose::Public
                            } else {
                                neenee_transport::serve::ServeExpose::Local
                            };
                            let opts = neenee_transport::serve::ServeOptions {
                                port,
                                expose,
                                token: None,
                                #[cfg(unix)]
                                uds_path: None,
                            };
                            // `/serve` exposes the single live TUI session as a one-entry
                            // prehost registry (ADR-0089).
                            let registry = Arc::new(
                                neenee_transport::registry::SessionRegistry::prehost_only(),
                            );
                            registry
                                .host(neenee_transport::registry::HostedSession {
                                    project_root: std::env::current_dir()
                                        .unwrap_or_else(|_| std::path::PathBuf::from(".")),
                                    session: store.clone(),
                                    req_tx: app.tx.clone(),
                                    events: bc_tx.clone(),
                                    cancel: tokio_util::sync::CancellationToken::new(),
                                    // A `/serve` prehost has no separate
                                    // attach-sync buffer to drain — its single
                                    // client is this live TUI, which already
                                    // holds the state. An empty buffer is a
                                    // no-op for any hypothetical attacher.
                                    sync_buffer: Arc::new(tokio::sync::Mutex::new(
                                        std::collections::VecDeque::new(),
                                    )),
                                    // A `/serve` prehost is observed over the same
                                    // monitor protocol (ADR-0093); seed the tracker
                                    // from the session header like the host does.
                                    tracker: Arc::new(tokio::sync::Mutex::new(
                                        neenee_transport::monitor::MonitorTracker::bootstrap(
                                            neenee_core::MonitoredSession {
                                                id: store.id().await,
                                                overview: String::new(),
                                                created_at: 0,
                                                updated_at: 0,
                                                message_count: 0,
                                                status: neenee_core::SessionStatus::Idle,
                                                // A `/serve` prehost session is driven
                                                // by this process — the host.
                                                hosting: neenee_core::SessionHosting::Hosted,
                                                round: 0,
                                                turn: None,
                                                output_tokens: 0,
                                                elapsed_ms: 0,
                                                current_tool: None,
                                                activity: None,
                                                context_tokens: None,
                                                note: None,
                                                // A `/serve` prehost lives in the
                                                // current project by construction.
                                                project_root: String::new(),
                                                wip: None,
                                            },
                                            neenee_core::SessionStatus::Idle,
                                        ),
                                    )),
                                })
                                .await;
                            let handle = neenee_transport::serve::start_server(opts, registry);
                            // Stash the cancel token so `/serve` (stop) can
                            // shut the listener down.
                            app.serve_cancel = Some(handle.cancel);
                            // Wait for the listener to report the actual bound
                            // port (resolves port=0 to the OS-assigned value).
                            let actual_port = handle.port.await.unwrap_or(port);
                            let msg = if let Some(token) = &handle.token {
                                format!(
                                    "Serve mode started on port {} (public). \
                                     Connect with `Authorization: Bearer {token}` — \
                                     ws://localhost:{}",
                                    actual_port, actual_port,
                                )
                            } else {
                                format!(
                                    "Serve mode started on port {} (loopback only). \
                                     Open ws://localhost:{} in a WebSocket client. \
                                     Add `--public` to expose on all interfaces.",
                                    actual_port, actual_port,
                                )
                            };
                            runtime.messages.write().await.push(
                                TranscriptMessage::new(Role::Assistant, msg)
                                    .with_origin(UserMessageOrigin::Slash),
                            );
                        }
                        // `/serve` resolves inline (never reaches the driver),
                        // so retire the optimistic "queued" state ourselves —
                        // and only when we painted it (a live round owns the
                        // responding flag otherwise).
                        if !session_busy {
                            runtime.is_responding.store(false, Ordering::SeqCst);
                            runtime.activity_status.lock().await.clear();
                        }
                        return Ok(());
                    }
                    let _ = app.tx.send(AgentRequest::SlashCommand(cmd));
                }
                input::InputAction::SendShell(command) => {
                    // `!<command>` runs directly through the bash tool. We
                    // surface the literal `!command` the user typed as the
                    // transcript entry (so history recall shows the bang) but
                    // ship only the stripped command to the harness.
                    app.active_modal = Modal::None;
                    app.suggestion_index = None;
                    app.input_scroll = 0;
                    // The shell path begins its own round (which emits its own
                    // `HarnessState` + ToolCall events). When a round is
                    // already live, that round owns the activity surface — do
                    // not paint an optimistic "queued" over it.
                    let session_busy = app.running_sessions.contains(&viewed_session_id);
                    if !session_busy {
                        runtime.is_responding.store(true, Ordering::SeqCst);
                        *runtime.activity_status.lock().await = "queued".to_string();
                    }
                    app.follow_bottom = true;
                    app.pin_summary_line = None;
                    let display = format!("!{}", command);
                    runtime
                        .messages
                        .write()
                        .await
                        // A `!command` shell passthrough runs directly through
                        // the bash tool, bypassing the model entirely — it is
                        // not the round's driving prompt. Tag it so the
                        // Activity modal does not mistake it for one.
                        .push(
                            TranscriptMessage::new(Role::User, display.clone())
                                .with_origin(UserMessageOrigin::Shell),
                        );
                    app.record_input_history(display, Vec::new(), Vec::new());
                    let _ = app.tx.send(AgentRequest::ShellCommand { command });
                }
                input::InputAction::ProviderPickerActivate => {
                    // Activate is a Models-only action: the flat (provider,
                    // model) pair under the highlight. The Connections list has
                    // no activate concept — it only manages instances, so Enter
                    // never produces this action there. Both the Models picker
                    // and the key editor share one activation path (key-ready /
                    // OAuth / key editor) via `activate_picked_model`.
                    let key_ready =
                        |app: &App, id: &str| app.key_status.get(id).copied().unwrap_or(true);
                    let target = if app.active_modal == Modal::Models {
                        let rows = app.models_flat_filtered();
                        rows.get(app.modal_index)
                            .or_else(|| rows.first())
                            .map(|row| (row.provider_id.clone(), row.model.clone()))
                    } else {
                        None
                    };
                    if let Some((id, model)) = target {
                        let ready = key_ready(app, &id);
                        activate_picked_model(app, id, model, ready);
                    }
                }
                input::InputAction::CustomProviderNextField => {
                    if app.active_modal == Modal::CustomProvider {
                        app.cycle_custom_field(true);
                    }
                }
                input::InputAction::CustomProviderPrevField => {
                    if app.active_modal == Modal::CustomProvider {
                        app.cycle_custom_field(false);
                    }
                }
                input::InputAction::MoveCustomSuggestion { forward } => {
                    if app.active_modal == Modal::CustomProvider {
                        app.move_custom_suggestion(forward);
                    }
                }
                input::InputAction::MoveProviderTemplate { forward } => {
                    if app.active_modal == Modal::ProviderTemplate {
                        app.move_template_choice(forward);
                    }
                }
                input::InputAction::SelectProviderTemplate => {
                    if app.active_modal == Modal::ProviderTemplate
                        && let Some(template) =
                            crate::tui::PROVIDER_TEMPLATES.get(app.template_choice)
                    {
                        if template.oauth_first() {
                            app.begin_oauth_add(template);
                            let _ = app.tx.send(AgentRequest::AuthorizeOAuth {
                                method: template
                                    .auth
                                    .default_login_method()
                                    .unwrap_or(neenee_core::LoginMethod::Device),
                                auth: template.auth,
                            });
                        } else {
                            app.open_custom_provider_editor(template);
                        }
                    }
                }
                input::InputAction::CancelOauthPending => {
                    if app.active_modal == Modal::OauthPending {
                        app.awaiting_oauth_add = false;
                        app.oauth_pending_url.clear();
                        app.oauth_pending_user_code.clear();
                        app.oauth_pending_message.clear();
                        app.oauth_pending_error = None;
                        app.open_provider_template_chooser();
                    }
                }
                input::InputAction::CopyOauthContent { target } => {
                    // Copy the OAuth pending sheet's primary field to the
                    // system clipboard. Mouse drag-select does not reach modal
                    // body text (mouse events are captured), so these are the
                    // in-app copy affordances. Nothing else changes: the sheet
                    // stays open and keeps waiting for authorization.
                    let text = match target {
                        input::OauthCopyTarget::UserCode => app.oauth_pending_user_code.clone(),
                        input::OauthCopyTarget::Url => app.oauth_pending_url.clone(),
                    };
                    if !text.is_empty() {
                        clipboard_ops::spawn_clipboard_copy(&copy_tx, copy_pending.clone(), text);
                    }
                }
                input::InputAction::CancelProviderTemplate => {
                    // Return to the Connections list the chooser was opened
                    // from; the chat draft stays parked in stashed_input.
                    if app.active_modal == Modal::ProviderTemplate {
                        app.input.clear();
                        app.set_cursor(0);
                        app.active_modal = Modal::Connections;
                        app.model_search = false;
                        app.model_scroll = 0;
                        app.model_modal_follow = true;
                        app.modal_index = 0;
                    }
                }
                input::InputAction::DeleteProvider => {
                    // Connections `Shift+D`: stage the highlighted custom
                    // provider for deletion and open the confirm overlay over
                    // the list (dimmed backdrop + centered panel). The actual
                    // `AgentRequest::DeleteProvider` only fires once the user
                    // confirms inside the overlay. Built-in providers and the
                    // synthetic "＋ Add connection" row are ignored by the
                    // helper.
                    app.stage_provider_delete();
                }
                input::InputAction::DeleteProviderConfirm => {
                    // The confirm overlay's Enter-on-Delete: dispatch the
                    // staged deletion and tear the overlay down.
                    if let Some(req) = app.confirm_provider_delete() {
                        let _ = app.tx.send(req);
                    }
                }
                input::InputAction::DeleteProviderCancel => {
                    // Esc / Ctrl+C / Enter-on-Cancel inside the confirm
                    // overlay: drop the staged provider id and return keyboard
                    // focus to the Connections list. The modal itself stays
                    // open.
                    app.cancel_provider_delete();
                }
                input::InputAction::CancelCustomProvider => {
                    // Return to the Connections list the editor was opened
                    // from; the chat draft stays parked in stashed_input.
                    if app.active_modal == Modal::CustomProvider {
                        app.input.clear();
                        app.set_cursor(0);
                        app.custom_field = 0;
                        app.custom_edit_id = None;
                        app.active_modal = Modal::Connections;
                        app.model_search = false;
                        app.model_scroll = 0;
                        app.model_modal_follow = true;
                        app.modal_index = 0;
                    }
                }
                input::InputAction::SubmitCustomProvider => {
                    if app.active_modal == Modal::CustomProvider {
                        // Commit the focused text field's live value first.
                        app.stash_custom_field();
                        let name = app.custom_name.trim().to_string();
                        let protocol = app.custom_protocol_wire.clone();
                        let base_url = app.custom_base_url.trim().to_string();
                        let api_key = neenee_core::SecretString::from(app.custom_token.trim());
                        if let Some(id) = app.custom_edit_id.clone() {
                            // Edit mode: update meta (models stay managed in
                            // the Models picker). A name is still required.
                            // ADR-0046: effort/thinking are no longer
                            // provider-level.
                            if name.is_empty() {
                                app.load_custom_field();
                            } else {
                                let _ = app.tx.send(AgentRequest::EditProvider {
                                    id,
                                    name,
                                    protocol,
                                    base_url,
                                    api_key,
                                });
                                app.input = std::mem::take(&mut app.stashed_input);
                                app.set_cursor_end();
                                app.custom_field = 0;
                                app.custom_edit_id = None;
                                app.active_modal = Modal::None;
                            }
                        } else {
                            // Create mode: the model list comes from the template's
                            // seeded models, or the single typed Model field when
                            // the template exposes one.
                            // ADR-0046: new channels start with thinking off;
                            // reasoning is opted in per model from the Models
                            // picker.
                            let models: Vec<String> =
                                if app.custom_fields.contains(&crate::tui::CustomField::Model) {
                                    vec![app.custom_model.trim().to_string()]
                                } else {
                                    app.custom_models.clone()
                                };
                            let usable = models.iter().any(|m| !m.trim().is_empty());
                            if name.is_empty() || !usable {
                                app.load_custom_field();
                            } else {
                                let _ = app.tx.send(AgentRequest::AddProvider {
                                    name,
                                    protocol,
                                    base_url,
                                    api_key,
                                    user_agent: app.custom_user_agent.clone(),
                                    models,
                                    auth: app.custom_auth,
                                    template_id: app.custom_template_id.take(),
                                });
                                app.input = std::mem::take(&mut app.stashed_input);
                                app.set_cursor_end();
                                app.custom_field = 0;
                                app.active_modal = Modal::None;
                            }
                        }
                    }
                }
                input::InputAction::ModelEnterSearch => {
                    // `/` in browse mode: enter the search sub-layer. The input
                    // line is already empty (held in `stashed_input`); typing now
                    // builds the fuzzy query and re-ranks the active picker's
                    // rows. Shared by the Connections and Models pickers.
                    if matches!(app.active_modal, Modal::Connections | Modal::Models) {
                        app.model_search = true;
                        app.modal_keymap_open = false;
                        app.modal_index = 0;
                        app.model_scroll = 0;
                        app.model_modal_follow = true;
                    }
                }
                input::InputAction::ModelExitSearch => {
                    // First Esc while searching: drop the query and return to the
                    // full browse list. The chat draft stays parked in
                    // `stashed_input` until the modal closes for real.
                    if matches!(app.active_modal, Modal::Connections | Modal::Models) {
                        app.model_search = false;
                        app.modal_keymap_open = false;
                        app.input.clear();
                        app.set_cursor(0);
                        app.input_scroll = 0;
                        app.suggestion_index = None;
                        app.modal_index = 0;
                        app.model_scroll = 0;
                        app.model_modal_follow = true;
                    }
                }
                input::InputAction::ProviderPickerToggleFavorite => {
                    // Models only (gated in input): toggle the favorite on the
                    // highlighted MODEL (falling back to the first visible row).
                    // Favorite is model-level (ADR-0046), so the id is the
                    // model wire id. Sending the request is enough; the backend
                    // pushes a fresh snapshot that flips the ★ next frame.
                    if app.active_modal == Modal::Models {
                        let ranked = app.models_flat_filtered();
                        if let Some(row) = ranked.get(app.modal_index).or_else(|| ranked.first()) {
                            let _ = app.tx.send(AgentRequest::ToggleFavorite {
                                id: row.model.clone(),
                            });
                        }
                    }
                }
                input::InputAction::OpenModelEditor => {
                    if app.active_modal == Modal::Models {
                        // `e` on a flat model row. The per-model settings popup
                        // opens for any model that exposes effort and/or a
                        // separate thinking switch.
                        let rows = app.models_flat_filtered();
                        if let Some(row) = rows.get(app.modal_index).or_else(|| rows.first())
                            && (row.effort.is_some() || row.thinking.is_some())
                        {
                            let is_builtin = !app.provider_is_custom(&row.provider_id);
                            app.editor_return_to = Modal::Models;
                            app.editor_target = Some(row.provider_id.clone());
                            app.editor_model = row.model.clone();
                            app.editor_model_settings_only = true;
                            app.editor_target_is_builtin = is_builtin;
                            app.editor_key.clear();
                            app.editor_effort =
                                row.effort.clone().unwrap_or_else(|| "medium".to_string());
                            app.editor_thinking_available = row.thinking.is_some();
                            // ADR-0046: reasoning is opt-in where a separate
                            // thinking switch exists. OpenAI GPT effort has no
                            // thinking switch, so this value is ignored there.
                            app.editor_thinking = row.thinking.unwrap_or(false);
                            app.editor_field = 1;
                            app.input = app.editor_effort.clone();
                            app.set_cursor_end();
                            app.model_search = false;
                            app.active_modal = Modal::ModelEditor;
                        }
                    } else if app.active_modal == Modal::Connections {
                        // `e` in the Connections list. A built-in provider opens
                        // the API-key editor (only its auth changes; the model is
                        // chosen from the Models picker). A user-defined provider
                        // opens the full meta edit form (Name/Protocol/Base
                        // URL/Token); its models stay managed in the Models
                        // picker.
                        let ranked = app.providers_filtered();
                        let target = ranked
                            .get(app.modal_index)
                            .or_else(|| ranked.first())
                            .map(|row| (row.id.clone(), row.model.clone(), row.builtin));
                        if let Some((id, model, builtin)) = target {
                            if builtin {
                                app.editor_return_to = Modal::Connections;
                                app.editor_target = Some(id);
                                app.editor_field = 0;
                                app.editor_key.clear();
                                app.editor_model = model;
                                app.editor_model_settings_only = false;
                                app.editor_target_is_builtin = false;
                                app.editor_effort = "high".to_string();
                                app.editor_thinking_available = false;
                                app.editor_thinking = true;
                                app.input.clear();
                                app.set_cursor(0);
                                app.model_search = false;
                                app.active_modal = Modal::ModelEditor;
                            } else {
                                // Pre-fill the edit form from the snapshot row.
                                let row = app
                                    .provider_picker
                                    .rows
                                    .iter()
                                    .find(|r| r.id == id)
                                    .cloned();
                                let (name, protocol, base_url, auth) = row
                                    .map(|r| (r.name, r.protocol, r.base_url, r.auth))
                                    .unwrap_or_default();
                                app.model_search = false;
                                app.open_edit_provider_editor(id, name, protocol, base_url, auth);
                            }
                        }
                    }
                }
                input::InputAction::ModelEditorNextField => {
                    // Cycle focus through the per-model editor's fields: effort
                    // (1) ↔ thinking (2). ADR-0046: the provider key editor has
                    // only an API-key field, so Tab is a no-op there
                    // (it never reaches this branch — `editor_model_settings_only`
                    // gates it). The focused text field owns the composer line;
                    // the thinking field is a toggle (no text), so it clears the
                    // line while focused.
                    if app.editor_model_settings_only {
                        match app.editor_field {
                            1 if app.editor_thinking_available => {
                                app.editor_effort = app.input.clone();
                                app.input.clear();
                                app.set_cursor(0);
                                app.editor_field = 2;
                            }
                            2 if app.editor_thinking_available => {
                                app.input = app.editor_effort.clone();
                                app.set_cursor_end();
                                app.editor_field = 1;
                            }
                            _ => {
                                app.input = app.editor_effort.clone();
                                app.set_cursor_end();
                                app.editor_field = 1;
                            }
                        }
                    }
                }
                input::InputAction::ModelEditorEffortCycle { delta } => {
                    // Cycle the effort selector through the selected model's
                    // supported wire levels, wrapping at both ends. Mirrored
                    // into app.input so the renderer shows the live value.
                    let model = neenee_core::resolve_model(&app.editor_model);
                    let levels: Vec<&str> = model
                        .effort_levels
                        .iter()
                        .map(|level| level.as_str())
                        .collect();
                    if levels.is_empty() {
                        continue;
                    }
                    let cur = levels
                        .iter()
                        .position(|l| *l == app.editor_effort)
                        .unwrap_or_else(|| {
                            levels
                                .iter()
                                .position(|l| *l == "medium")
                                .or_else(|| levels.iter().position(|l| *l == "high"))
                                .unwrap_or(0)
                        }) as isize;
                    let n = levels.len() as isize;
                    let next = ((cur + delta as isize).rem_euclid(n)) as usize;
                    app.editor_effort = levels[next].to_string();
                    app.input = app.editor_effort.clone();
                    app.set_cursor_end();
                }
                input::InputAction::ModelEditorThinkingToggle => {
                    // Toggle extended thinking on/off (Space). Orthogonal to
                    // effort — the two knobs are independent on the wire.
                    if app.editor_thinking_available {
                        app.editor_thinking = !app.editor_thinking;
                    }
                }
                input::InputAction::SubmitModelEditor => {
                    if app.active_modal == Modal::ModelEditor
                        && let Some(id) = app.editor_target.clone()
                    {
                        let model = if app.editor_model.trim().is_empty() {
                            app.provider_picker
                                .rows
                                .iter()
                                .find(|r| r.id == id)
                                .map(|r| r.model.clone())
                                .unwrap_or_default()
                        } else {
                            app.editor_model.trim().to_string()
                        };
                        if app.editor_model_settings_only {
                            // Per-model settings editor (opened from the Models
                            // picker). Flush the focused field's
                            // live text into its buffer before reading, so a
                            // submit while effort is focused captures the value.
                            // Field 2 (thinking) is a toggle with no text.
                            if app.editor_field == 1 {
                                app.editor_effort = app.input.clone();
                            }
                            let effort = app.editor_effort.clone();
                            // Built-in models persist to `[model_reasoning]` (no
                            // user-editable channel); user-defined models persist
                            // to their channel. ADR-0045.
                            if app.editor_target_is_builtin {
                                let _ = app.tx.send(AgentRequest::EditModelReasoning {
                                    model,
                                    effort: Some(effort),
                                    thinking: app
                                        .editor_thinking_available
                                        .then_some(app.editor_thinking),
                                });
                            } else {
                                let _ = app.tx.send(AgentRequest::EditProviderModel {
                                    provider_id: id,
                                    model,
                                    effort: Some(effort),
                                    thinking: app
                                        .editor_thinking_available
                                        .then_some(app.editor_thinking),
                                });
                            }
                            app.input.clear();
                            app.set_cursor(0);
                            app.editor_target = None;
                            app.editor_model_settings_only = false;
                            app.editor_target_is_builtin = false;
                            app.editor_thinking_available = false;
                            app.model_search = false;
                            app.model_modal_follow = true;
                            app.active_modal = app.editor_return_to;
                            continue;
                        }
                        // Key editor (not model-settings-only): this is a
                        // built-in provider's API-key edit or a first-key entry.
                        // ADR-0046 removed effort/thinking from the provider
                        // level, so switching now carries only the key
                        // (effort/thinking are set per model from the Models
                        // picker `e` editor).
                        let key = app.input.trim().to_string();
                        let _ = app.tx.send(AgentRequest::SwitchProvider {
                            provider_type: id,
                            model,
                            api_key: if key.is_empty() {
                                None
                            } else {
                                Some(key.into())
                            },
                            base_url: None,
                        });
                        // Close to chat: restore the original draft.
                        app.input = std::mem::take(&mut app.stashed_input);
                        app.set_cursor_end();
                        app.editor_target = None;
                        app.editor_model_settings_only = false;
                        app.editor_target_is_builtin = false;
                        app.active_modal = Modal::None;
                    }
                }
                input::InputAction::Interrupt => {
                    // Mirror Ctrl+C's quit pattern: the first Esc only arms a
                    // ~2s window (and shows a toast); the second Esc within
                    // that window actually interrupts the running task.
                    if app.esc_armed_ticks > 0 {
                        app.esc_armed_ticks = 0;
                        let _ = app.tx.send(AgentRequest::Interrupt);
                    } else {
                        app.esc_armed_ticks = 20;
                    }
                }
                input::InputAction::OpenModels => {
                    // Stash whatever the user was composing so Esc restores it
                    // unchanged. The picker opens in browse mode, so the input
                    // line stays empty until `/` enters search and borrows it as
                    // the fuzzy query (same pattern as the history modal).
                    app.stashed_input = std::mem::take(&mut app.input);
                    app.set_cursor(0);
                    app.input_scroll = 0;
                    app.active_modal = Modal::Models;
                    app.modal_keymap_open = false;
                    app.model_search = false;
                    app.model_scroll = 0;
                    app.model_modal_follow = true;
                    // Land the cursor on the live (provider, model) pair, so
                    // "open picker + Enter" re-activates the current selection.
                    let rows = app.models_flat_filtered();
                    app.modal_index = rows
                        .iter()
                        .position(|row| {
                            row.provider_id == app.current_provider
                                && row.model == app.current_model
                        })
                        .unwrap_or(0);
                    app.suggestion_index = None;
                }
                input::InputAction::OpenConnections => {
                    // Same stash + browse-mode open as `OpenModels`.
                    app.stashed_input = std::mem::take(&mut app.input);
                    app.set_cursor(0);
                    app.input_scroll = 0;
                    app.active_modal = Modal::Connections;
                    app.modal_keymap_open = false;
                    app.model_search = false;
                    app.model_scroll = 0;
                    app.model_modal_follow = true;
                    // Land the cursor on the currently-active provider (falling
                    // back to the default), so "open picker + Enter"
                    // re-activates it.
                    let ranked = app.providers_filtered();
                    app.modal_index = ranked
                        .iter()
                        .position(|row| row.id == app.current_provider)
                        .or_else(|| {
                            ranked
                                .iter()
                                .position(|row| row.id == app.provider_picker.default_id)
                        })
                        .unwrap_or(0);
                    app.suggestion_index = None;
                }
                input::InputAction::OpenProviderTemplate => {
                    // `a` in the Connections modal: open the add-provider
                    // template chooser (the first step of adding a connection).
                    // Only meaningful from Connections; ignored otherwise.
                    if app.active_modal == Modal::Connections {
                        app.open_provider_template_chooser();
                    }
                }
                input::InputAction::OpenHistory => {
                    // The history panel floats above the composer, and the
                    // composer itself is the live filter field: typing narrows
                    // the list immediately (no separate browse/search mode).
                    // Stash whatever the user was composing so Esc restores it
                    // unchanged, and start with an empty query (show all, newest
                    // first) — they type to narrow.
                    app.stashed_input = std::mem::take(&mut app.input);
                    app.set_cursor(0);
                    app.input_scroll = 0;
                    app.suggestion_index = None;
                    app.active_modal = Modal::HistorySearch;
                    app.modal_keymap_open = false;
                    // The composer is permanently the filter while this panel
                    // is open, so `history_search` is latched true.
                    app.history_search = true;
                    app.history_clear_confirm = false;
                    // Rows are newest-first, so index 0 is the most-recent entry
                    // — focus the top so an immediate Enter re-inserts it.
                    app.modal_index = 0;
                    app.history_scroll = 0;
                    app.history_modal_follow = true;
                    app.history_preview = false;
                }
                input::InputAction::HistoryInsert => {
                    // Enter inside the Ctrl+R panel: pull the focused entry out
                    // of `history_rows` (the filtered matches) and drop it into
                    // the input box for further editing / sending. The message
                    // is not shipped here — the user hits Enter again to send.
                    let ranked = app.history_rows();
                    let pick = ranked.get(app.modal_index).or_else(|| ranked.first());
                    if let Some((orig_idx, _)) = pick {
                        let original = *orig_idx;
                        let text = app.input_history[original].text.clone();
                        // Restore the attachments cached behind this entry (if
                        // any) so a re-send ships the real image / paste
                        // payloads rather than a bare chip label; with no
                        // cache the staged vectors are cleared.
                        app.restore_history_attachments(original);
                        // The inserted entry becomes the new draft: it is the
                        // newest *unsent* input, so ↓ past the newest history
                        // row restores it, never a stale remembered draft.
                        app.adopt_as_draft(
                            text,
                            app.pending_images.clone(),
                            app.pending_text_pastes.clone(),
                        );
                    }
                    // The selection replaces the in-progress draft, so the
                    // stash is dropped (not restored).
                    app.stashed_input.clear();
                    app.input_scroll = 0;
                    app.suggestion_index = None;
                    // A programmatic input replacement — latch the dismissal so
                    // a slash-command selection doesn't flash its completion
                    // popup until the next real edit.
                    app.completion_dismissed = true;
                    app.modal_index = 0;
                    app.active_modal = Modal::None;
                }
                input::InputAction::HistoryTogglePreview => {
                    // Tab inside the Ctrl+R modal: flip between the fuzzy list
                    // and a full-text view of the selected entry. Reusing
                    // `history_scroll` as the per-entry scroll means entering
                    // preview or moving to another entry starts from the top.
                    app.history_preview = !app.history_preview;
                    app.history_scroll = 0;
                    app.history_modal_follow = true;
                }
                input::InputAction::HistoryClearAll => {
                    // Ctrl+X inside the Ctrl+R modal: arm the clear-history
                    // confirmation. Nothing is deleted yet — the next `y`
                    // (or Enter) wipes, any other key cancels.
                    app.history_clear_confirm = true;
                    let n = app.input_history.len();
                    show_local_toast(
                        app,
                        format!(
                            "Press y to clear all {n} history entr{} (any other key cancels)",
                            if n == 1 { "y" } else { "ies" }
                        ),
                        false,
                        std::time::Duration::from_millis(2600),
                    );
                }
                input::InputAction::HistoryClearConfirm => {
                    let n = app.input_history.len();
                    app.clear_input_history();
                    show_local_toast(
                        app,
                        if n == 0 {
                            "Input history is already empty."
                        } else {
                            "Input history cleared."
                        },
                        false,
                        std::time::Duration::from_millis(2600),
                    );
                }
                input::InputAction::HistoryClearCancel => {
                    app.history_clear_confirm = false;
                }
                input::InputAction::OpenHelp => {
                    app.active_modal = Modal::Help;
                    app.modal_keymap_open = false;
                    app.modal_index = 0;
                    app.help_scroll = 0;
                }
                input::InputAction::OpenPermissions => {
                    // The permissions manager modal. Reached via the
                    // `/permissions` slash command (intercepted locally, never
                    // sent to the backend). Kick off a snapshot request so the
                    // rule list populates; `/permissions clear` still goes to
                    // the backend via SendSlash.
                    app.active_modal = Modal::Permissions;
                    app.modal_keymap_open = false;
                    app.modal_index = 0;
                    app.permissions_scroll = 0;
                    let _ = app.tx.send(AgentRequest::QuerySessionContext);
                }
                input::InputAction::OpenTools => {
                    // The tools manager modal. Reached via `/tools`
                    // (intercepted locally). It shares the session-context
                    // snapshot, so (re)kick a query so the list is fresh.
                    app.active_modal = Modal::Tools;
                    app.modal_keymap_open = false;
                    app.modal_index = 0;
                    app.session_scroll = 0;
                    app.session_modal_follow = true;
                    let _ = app.tx.send(AgentRequest::QuerySessionContext);
                }
                input::InputAction::OpenMcp => {
                    // The MCP manager modal. Reached via `/mcp` (intercepted
                    // locally). Shares the session-context snapshot, so kick a
                    // fresh query and let the modal populate from its `mcp` pane.
                    app.active_modal = Modal::Mcp;
                    app.modal_keymap_open = false;
                    app.modal_index = 0;
                    app.session_scroll = 0;
                    app.session_modal_follow = true;
                    let _ = app.tx.send(AgentRequest::QuerySessionContext);
                }
                input::InputAction::OpenSkills => {
                    // The skills modal. Reached via `/skills` (intercepted
                    // locally). Shares the session-context snapshot, so kick a
                    // fresh query and let the modal populate from its `skills`
                    // pane. Detail expansions start collapsed.
                    app.active_modal = Modal::Skills;
                    app.modal_keymap_open = false;
                    app.modal_index = 0;
                    app.session_scroll = 0;
                    app.session_modal_follow = true;
                    app.skills_expanded = None;
                    let _ = app.tx.send(AgentRequest::QuerySessionContext);
                }
                input::InputAction::SkillsToggleDetail => {
                    // Toggle the detail block of the selected skill row. Re-pressing
                    // Enter on an already-expanded row collapses it.
                    app.skills_expanded = if app.skills_expanded == Some(app.modal_index) {
                        None
                    } else {
                        Some(app.modal_index)
                    };
                    app.session_modal_follow = true;
                }
                input::InputAction::SkillsReload => {
                    // Reload the skill registry. The harness replies with a fresh
                    // snapshot reflecting the reloaded skills.
                    let _ = app
                        .tx
                        .send(AgentRequest::SlashCommand("/skills reload".to_string()));
                    let _ = app.tx.send(AgentRequest::QuerySessionContext);
                }
                input::InputAction::OpenConfig => {
                    // The config manager modal. Reached via `/config`
                    // (intercepted locally, never sent to the backend). Lists
                    // the configurable categories; selecting one drills into
                    // its sub-page.
                    app.active_modal = Modal::Config;
                    app.modal_keymap_open = false;
                    app.modal_index = 0;
                    app.config_scroll = 0;
                }
                input::InputAction::ConfigActivate => {
                    // Drill into the selected config category's sub-page.
                    // Index matches `categories()` order in config.rs
                    // (0 = Appearance, 1 = Layout).
                    match app.modal_index {
                        0 => {
                            app.active_modal = Modal::ConfigTheme;
                            app.modal_keymap_open = false;
                            app.modal_index = Theme::color_scheme_index(&app.color_scheme);
                            app.config_scroll = 0;
                        }
                        1 => {
                            app.active_modal = Modal::ConfigLayout;
                            app.modal_keymap_open = false;
                            app.modal_index = match app.transcript_layout {
                                crate::tui::view::layout::Strategy::Default => 0,
                                crate::tui::view::layout::Strategy::Legacy => 1,
                            };
                            app.config_scroll = 0;
                        }
                        _ => {}
                    }
                }
                input::InputAction::ConfigBack => {
                    // The custom editor is one level deeper than the other
                    // pages. Esc cancels its preview and returns to Appearance;
                    // the other pages return to the settings index.
                    app.modal_keymap_open = false;
                    app.config_scroll = 0;
                    if app.active_modal == Modal::ConfigThemeCustom {
                        app.theme =
                            Theme::from_color_scheme(&app.color_scheme, &app.custom_color_scheme);
                        app.custom_color_draft = app.custom_color_scheme.clone();
                        app.input.clear();
                        app.set_cursor(0);
                        app.active_modal = Modal::ConfigTheme;
                        app.modal_index = Theme::color_scheme_index("custom");
                    } else {
                        app.modal_index = match app.active_modal {
                            Modal::ConfigTheme => 0,
                            Modal::ConfigLayout => 1,
                            _ => 0,
                        };
                        app.active_modal = Modal::Config;
                    }
                }
                input::InputAction::ConfigThemeActivate => {
                    if let Some(name) =
                        crate::tui::view::overlays::config_theme::scheme_id_at(app.modal_index)
                    {
                        if name == "custom" {
                            app.custom_color_draft = app.custom_color_scheme.clone();
                            app.active_modal = Modal::ConfigThemeCustom;
                            app.modal_keymap_open = false;
                            app.modal_index = 0;
                            app.config_scroll = 0;
                            app.input = Theme::custom_color_value(&app.custom_color_draft, 0)
                                .unwrap_or("#000000")
                                .to_string();
                            app.set_cursor_end();
                            app.theme = Theme::from_color_scheme("custom", &app.custom_color_draft);
                        } else {
                            app.color_scheme = name.to_string();
                            app.theme = Theme::from_color_scheme(name, &app.custom_color_scheme);
                            let _ = app.tx.send(AgentRequest::UpdateTuiColorScheme {
                                name: app.color_scheme.clone(),
                                custom: app.custom_color_scheme.clone(),
                            });
                        }
                    }
                }
                input::InputAction::ConfigThemeField { delta } => {
                    if app.active_modal == Modal::ConfigThemeCustom
                        && Theme::set_custom_color_value(
                            &mut app.custom_color_draft,
                            app.modal_index,
                            &app.input,
                        )
                    {
                        app.theme = Theme::from_color_scheme("custom", &app.custom_color_draft);
                        let count =
                            crate::tui::view::overlays::config_theme_custom::ROW_COUNT as i32;
                        let next = (app.modal_index as i32 + delta).rem_euclid(count) as usize;
                        app.modal_index = next;
                        app.input = Theme::custom_color_value(&app.custom_color_draft, next)
                            .unwrap_or("#000000")
                            .to_string();
                        app.set_cursor_end();
                    }
                }
                input::InputAction::ConfigThemeCustomSave => {
                    if app.active_modal == Modal::ConfigThemeCustom
                        && Theme::set_custom_color_value(
                            &mut app.custom_color_draft,
                            app.modal_index,
                            &app.input,
                        )
                    {
                        app.custom_color_scheme = app.custom_color_draft.clone();
                        app.color_scheme = "custom".to_string();
                        app.theme = Theme::from_color_scheme("custom", &app.custom_color_scheme);
                        let _ = app.tx.send(AgentRequest::UpdateTuiColorScheme {
                            name: app.color_scheme.clone(),
                            custom: app.custom_color_scheme.clone(),
                        });
                        app.input.clear();
                        app.set_cursor(0);
                        app.active_modal = Modal::ConfigTheme;
                        app.modal_index = Theme::color_scheme_index("custom");
                        app.config_scroll = 0;
                    }
                }
                input::InputAction::ConfigLayoutApply => {
                    // Apply the selected layout strategy. Persisted to
                    // config.toml by the harness; the optimistic local update
                    // makes the live transcript switch immediately, and the
                    // `TuiLayoutUpdated` reply re-seeds the authoritative value.
                    if let Some(value) =
                        crate::tui::view::overlays::config_layout::config_value_at(app.modal_index)
                    {
                        app.transcript_layout =
                            crate::tui::view::layout::Strategy::from_config(value);
                        let _ = app
                            .tx
                            .send(AgentRequest::UpdateTuiLayout(value.to_string()));
                    }
                }
                input::InputAction::McpToggle => {
                    // Connect/disconnect the selected server for the session.
                    // The "enabled intent" is the inverse of its disabled flag;
                    // the harness replies with a fresh snapshot.
                    if let Some(server) = app
                        .session_context
                        .as_ref()
                        .and_then(|s| s.mcp.get(app.modal_index))
                    {
                        let _ = app.tx.send(AgentRequest::ToggleMcpServer {
                            name: server.name.clone(),
                            enabled: server.disabled,
                        });
                    }
                }
                input::InputAction::McpReconnect => {
                    // Reconnect the selected server on demand. The harness
                    // replies with a fresh snapshot reflecting the new status.
                    if let Some(server) = app
                        .session_context
                        .as_ref()
                        .and_then(|s| s.mcp.get(app.modal_index))
                    {
                        let _ = app.tx.send(AgentRequest::ReconnectMcpServer {
                            name: server.name.clone(),
                        });
                    }
                }
                input::InputAction::PermissionsActivate => {
                    // Revoke the selected "always allow" rule. The harness
                    // replies with a fresh snapshot so the list re-renders.
                    if let Some(snapshot) = app.session_context.as_ref()
                        && let Some(rule) = snapshot.permissions.get(app.modal_index)
                    {
                        let _ = app.tx.send(AgentRequest::RevokePermission {
                            tool: rule.tool.clone(),
                            scope: rule.scope.clone(),
                        });
                    }
                }
                input::InputAction::PermissionsClearAll => {
                    // Clear every cached rule. The harness replies with a fresh
                    // (empty) snapshot.
                    let _ = app.tx.send(AgentRequest::ClearAllPermissions);
                    app.modal_index = 0;
                }
                input::InputAction::SessionSelect { forward } => {
                    // Move the selection cursor (the body scroll follows it).
                    // The list is the tools list, except in the MCP manager
                    // where it is the configured-server list. When empty (still
                    // loading / none), Up/Down scrolls the body directly so the
                    // other content stays reachable.
                    let list_len = if app.active_modal == Modal::Mcp {
                        app.session_context
                            .as_ref()
                            .map(|s| s.mcp.len())
                            .unwrap_or(0)
                    } else if app.active_modal == Modal::Skills {
                        app.session_context
                            .as_ref()
                            .map(|s| s.skills.len())
                            .unwrap_or(0)
                    } else if app.active_modal == Modal::Queue {
                        app.pending_dispatch
                            .iter()
                            .filter(|item| item.session_id == viewed_session_id)
                            .count()
                    } else {
                        app.session_tools_len()
                    };
                    if list_len > 0 {
                        app.modal_index = if forward {
                            (app.modal_index + 1) % list_len
                        } else if app.modal_index == 0 {
                            list_len - 1
                        } else {
                            app.modal_index - 1
                        };
                        // The queue modal tracks its own follow flag so it can
                        // be scrolled independently of the shared session
                        // scroll the other list modals reuse.
                        if app.active_modal == Modal::Queue {
                            app.queue_modal_follow = true;
                        } else {
                            app.session_modal_follow = true;
                        }
                    } else if app.active_modal == Modal::Queue {
                        // Empty queue: Up/Down is inert.
                    } else {
                        app.session_scroll = if forward {
                            app.session_scroll.saturating_add(1)
                        } else {
                            app.session_scroll.saturating_sub(1)
                        };
                    }
                }
                input::InputAction::SessionActivate => {
                    // Toggle the selected tool. The request is sent through the
                    // normal agent channel; the harness replies with a fresh
                    // snapshot that re-renders the dashboard.
                    if let Some(req) = app.session_activate_request() {
                        let _ = app.tx.send(req);
                    }
                }
                input::InputAction::OpenSelectedSession => {
                    if let Some(session) = app.sessions_overview.get(
                        app.modal_index
                            .min(app.sessions_overview.len().saturating_sub(1)),
                    ) {
                        let id = session.id.clone();
                        app.active_modal = Modal::None;
                        app.modal_index = 0;
                        // A session was chosen from the startup picker, so a
                        // real conversation now backs the view: subsequent
                        // `/sessions` modals should behave as ordinary
                        // transient overlays (Esc = dismiss, not quit).
                        app.startup_overlay = crate::tui::StartupOverlay::None;
                        let _ = app
                            .tx
                            .send(AgentRequest::SlashCommand(format!("/session open {}", id)));
                    }
                }
                input::InputAction::HostPreviewSelected => {
                    // Enter on a dock selection opens the read-only preview
                    // modal. Selection alone never opens it; Esc closes.
                    let idx = app
                        .modal_index
                        .min(app.host_sessions.len().saturating_sub(1));
                    let order = crate::tui::overlays::creation_order(&app.host_sessions);
                    if let Some(row) = order.get(idx).map(|&i| &app.host_sessions[i]) {
                        app.host_preview = Some(row.id.clone());
                        app.host_preview_scroll = 0;
                    }
                }
                input::InputAction::HostSwitchSelected => {
                    let idx = app
                        .modal_index
                        .min(app.host_sessions.len().saturating_sub(1));
                    // The dock renders sessions in creation order (`#seq`);
                    // the selection indexes that sequence, not the raw
                    // newest-first snapshot.
                    let order = crate::tui::overlays::creation_order(&app.host_sessions);
                    if let Some(row) = order.get(idx).map(|&i| &app.host_sessions[i]) {
                        // Only hosted sessions can be switched to — a mirrored
                        // row belongs to another TUI (ADR-0095). Current
                        // session is a no-op.
                        let switchable = row.hosting == neenee_core::SessionHosting::Hosted
                            && row.id != viewed_session_id;
                        if switchable {
                            app.switch_to_target = Some(row.id.clone());
                            app.should_quit.store(true, Ordering::SeqCst);
                        }
                        app.active_modal = Modal::None;
                        app.modal_index = 0;
                        app.host_prompting = false;
                    }
                }
                input::InputAction::HostFocusToggle => {
                    app.host_focus = match app.host_focus {
                        crate::tui::overlays::DashboardFocus::List => {
                            crate::tui::overlays::DashboardFocus::Detail
                        }
                        crate::tui::overlays::DashboardFocus::Detail => {
                            crate::tui::overlays::DashboardFocus::List
                        }
                    };
                }
                input::InputAction::HostInterruptSelected => {
                    let idx = app
                        .modal_index
                        .min(app.host_sessions.len().saturating_sub(1));
                    // Creation-order selection, mirroring the dock (see
                    // HostSwitchSelected).
                    let order = crate::tui::overlays::creation_order(&app.host_sessions);
                    if let Some(row) = order.get(idx).map(|&i| &app.host_sessions[i]) {
                        if row.hosting == neenee_core::SessionHosting::Hosted {
                            let id = row.id.clone();
                            tokio::spawn(async move {
                                let project_root = std::env::current_dir()
                                    .unwrap_or_else(|_| std::path::PathBuf::from("."));
                                let Some(info) = crate::remote::discover(&project_root) else {
                                    return;
                                };
                                let req = neenee_transport::serve::ControlRequest::Interrupt {
                                    session_id: id.clone(),
                                };
                                if let Err(e) = crate::remote::control(&info, req).await {
                                    tracing::warn!(%e, session=%id, "dashboard interrupt failed");
                                }
                            });
                            app.notice_toast_message = "interrupt sent".to_string();
                            app.notice_toast_severity = NoticeSeverity::Info;
                            app.notice_toast_until = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(1600),
                            );
                        } else {
                            app.notice_toast_message = "mirrored session is view-only".to_string();
                            app.notice_toast_severity = NoticeSeverity::Warning;
                            app.notice_toast_until = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(2000),
                            );
                        }
                    }
                }
                input::InputAction::HostPromptOpen => {
                    // `p`: prompt the selected session. The composer buffer
                    // becomes the task text.
                    app.host_prompting = true;
                    app.host_prompt_new = false;
                    app.input.clear();
                    app.set_cursor(0);
                }
                input::InputAction::HostNewSession => {
                    // `n`: create a new session with the text as opening task.
                    app.host_prompting = true;
                    app.host_prompt_new = true;
                    app.input.clear();
                    app.set_cursor(0);
                }
                input::InputAction::HostPromptSubmit => {
                    let text = app.input.trim().to_string();
                    let create_new = app.host_prompt_new;
                    app.host_prompting = false;
                    app.host_prompt_new = false;
                    app.input.clear();
                    app.set_cursor(0);
                    if !text.is_empty() {
                        let project_root = std::env::current_dir()
                            .unwrap_or_else(|_| std::path::PathBuf::from("."));
                        let idx = app
                            .modal_index
                            .min(app.host_sessions.len().saturating_sub(1));
                        // Creation-order selection, mirroring the dock.
                        let order = crate::tui::overlays::creation_order(&app.host_sessions);
                        let selected = order.get(idx).map(|&i| app.host_sessions[i].clone());
                        tokio::spawn(async move {
                            let Some(info) = crate::remote::discover(&project_root) else {
                                tracing::warn!("dashboard control: no daemon discovered");
                                return;
                            };
                            // `n` always creates; `p` prompts the selected
                            // hosted session (creating is impossible without a
                            // selection, so fall back to create).
                            let req = if !create_new
                                && let Some(row) = &selected
                                && row.hosting == neenee_core::SessionHosting::Hosted
                            {
                                neenee_transport::serve::ControlRequest::SendPrompt {
                                    session_id: row.id.clone(),
                                    text,
                                }
                            } else {
                                neenee_transport::serve::ControlRequest::CreateSession {
                                    project: project_root.display().to_string(),
                                    prompt: Some(text),
                                }
                            };
                            if let Err(e) = crate::remote::control(&info, req).await {
                                tracing::warn!(%e, "dashboard prompt/create failed");
                            }
                        });
                        app.notice_toast_message = if create_new {
                            "session created".to_string()
                        } else {
                            "task sent".to_string()
                        };
                        app.notice_toast_severity = NoticeSeverity::Info;
                        app.notice_toast_until = Some(
                            std::time::Instant::now() + std::time::Duration::from_millis(1600),
                        );
                    }
                }
                input::InputAction::DeleteSelectedSession => {
                    let idx = app
                        .modal_index
                        .min(app.sessions_overview.len().saturating_sub(1));
                    if idx < app.sessions_overview.len() {
                        let deleted = app.sessions_overview.remove(idx);
                        app.modal_index = app
                            .modal_index
                            .min(app.sessions_overview.len().saturating_sub(1));
                        let _ = app.tx.send(AgentRequest::DeleteSession { id: deleted.id });
                    }
                }
                input::InputAction::CreateNewSession => {
                    app.startup_overlay = crate::tui::StartupOverlay::None;
                    app.active_modal = Modal::None;
                    let _ = app
                        .tx
                        .send(AgentRequest::SlashCommand("/session new".to_string()));
                }
                input::InputAction::OpenSessionInfo => {
                    // Drill into the session-info sub-view for the highlighted
                    // row. Request the full detail (complete last prompt,
                    // timestamps) on demand — the picker rows only carry a
                    // truncated preview. While the round-trip is in flight the
                    // body shows a loading state.
                    if let Some(session) = app.sessions_overview.get(
                        app.modal_index
                            .min(app.sessions_overview.len().saturating_sub(1)),
                    ) {
                        app.session_info_detail = true;
                        app.session_detail = None;
                        app.session_info_scroll = 0;
                        let _ = app.tx.send(AgentRequest::QuerySessionDetail {
                            id: session.id.clone(),
                        });
                    }
                }
                input::InputAction::CloseModal => {
                    // Sub-page back-out is checked FIRST (deepest level wins),
                    // so Esc from a drill-in always returns to its parent view
                    // before any close/quit logic runs — otherwise pressing Esc
                    // in e.g. the Sessions › Info sub-view at startup would quit
                    // the program instead of dropping back to the sessions list.
                    if app.active_modal == Modal::Host && app.host_preview.is_some() {
                        // Deepest dashboard layer: first Esc closes the
                        // session preview, returning to the dashboard; a
                        // second Esc closes the dashboard itself.
                        app.host_preview = None;
                        app.host_preview_scroll = 0;
                    } else if app.active_modal == Modal::Host && app.host_prompting {
                        // First Esc cancels the dashboard's inline prompt,
                        // returning to the list; a second Esc closes the
                        // dashboard. Mirrors the two-stage Esc of the other
                        // drill-in sub-layers below.
                        app.host_prompting = false;
                        app.host_prompt_new = false;
                        app.input.clear();
                        app.set_cursor(0);
                    } else if app.active_modal == Modal::TokenReport && app.token_report_detail {
                        // First Esc returns from the turn breakdown to the round list;
                        // a second Esc closes the modal.
                        app.token_report_detail = false;
                        app.token_report_scroll = 0;
                    } else if app.active_modal == Modal::Sessions && app.session_info_detail {
                        // First Esc returns from the session-info sub-view to
                        // the sessions list; a second Esc closes the modal.
                        app.session_info_detail = false;
                        app.session_detail = None;
                        app.session_info_scroll = 0;
                    } else if app.startup_overlay == crate::tui::StartupOverlay::SessionsPicker
                        && app.active_modal == Modal::Sessions
                    {
                        // `neenee resume` (no id) opened the picker at startup
                        // instead of loading any session: there is no real
                        // conversation behind the modal, so closing the *list*
                        // (not a sub-view — those are handled above) must quit
                        // the program rather than drop into an empty chat.
                        tracing::info!(reason = "startup_picker_cancelled", "app exiting");
                        app.should_quit.store(true, Ordering::SeqCst);
                    } else if app.startup_overlay == crate::tui::StartupOverlay::Dashboard
                        && app.active_modal == Modal::Host
                    {
                        // `neenee dashboard` opened the dashboard over a carrier
                        // session the user never asked to converse with: Esc
                        // here quits rather than dropping into that chat. Enter
                        // on a row (HostSwitchSelected) re-attaches as usual.
                        tracing::info!(reason = "startup_dashboard_cancelled", "app exiting");
                        app.should_quit.store(true, Ordering::SeqCst);
                    } else {
                        // Most modals close straight to chat. The model editor
                        // and the custom-provider editor instead step back to
                        // the picker they were opened from, so a key entry is
                        // recoverable with Esc.
                        let mut return_to: Option<Modal> = None;
                        if app.active_modal == Modal::HistorySearch {
                            // Closing from either browse or search: hand the parked
                            // draft back so Esc is a true cancel, and clear the
                            // search sub-layer / preview flags for the next open.
                            app.restore_history_draft();
                            app.history_clear_confirm = false;
                        } else if matches!(app.active_modal, Modal::Connections | Modal::Models) {
                            // The input box may have been borrowed as the fuzzy
                            // filter (search sub-layer); hand the parked draft back
                            // and clear the search/scroll flags so Esc cancels
                            // cleanly. (The two-stage Esc inside search is handled
                            // earlier by `ModelExitSearch`; this path is the
                            // browse-mode close.)
                            app.restore_model_draft();
                        } else if app.active_modal == Modal::ModelEditor {
                            // Cancel the editor: discard its fields and return to
                            // the picker it was opened from in browse mode. The
                            // original chat draft stays in stashed_input for when
                            // that picker itself closes.
                            app.editor_target = None;
                            app.editor_model_settings_only = false;
                            app.editor_target_is_builtin = false;
                            app.input.clear();
                            app.set_cursor(0);
                            app.model_search = false;
                            app.model_modal_follow = true;
                            return_to = Some(app.editor_return_to);
                        } else if app.active_modal == Modal::CustomProvider {
                            // Same as Esc: discard the editor fields and step back
                            // to the Connections list; the chat draft stays parked
                            // in stashed_input.
                            app.input.clear();
                            app.set_cursor(0);
                            app.custom_field = 0;
                            app.model_search = false;
                            app.model_modal_follow = true;
                            app.modal_index = 0;
                            return_to = Some(Modal::Connections);
                        } else if app.active_modal == Modal::ConfigThemeCustom {
                            // Click-outside closes the settings stack. Discard
                            // the transactional custom preview before leaving.
                            app.theme = Theme::from_color_scheme(
                                &app.color_scheme,
                                &app.custom_color_scheme,
                            );
                            app.custom_color_draft = app.custom_color_scheme.clone();
                            app.input.clear();
                            app.set_cursor(0);
                        }
                        // The queue modal auto-blocked the outbox on open so
                        // items could be managed safely; closing it resumes
                        // normal auto-drain. (A persistent block set via `F3`
                        // at the top level is unaffected, since the modal's
                        // own open/close latch is what's being released here —
                        // but to keep this simple and predictable we always
                        // resume on close; the user can re-block with F3.)
                        if app.active_modal == Modal::Queue {
                            app.resume_queue(&viewed_session_id);
                        }
                        app.modal_keymap_open = false;
                        app.active_modal = return_to.unwrap_or(Modal::None);
                    }
                }
                input::InputAction::ToggleModalKeymap => {
                    // In-modal `?` expand: swap the body for the full keymap
                    // page (or close it). Not a nested modal.
                    app.modal_keymap_open = !app.modal_keymap_open;
                    // Reset the body scroll so the keymap starts at the top.
                    match app.active_modal {
                        Modal::Connections | Modal::Models => {
                            app.model_scroll = 0;
                            app.model_modal_follow = true;
                        }
                        Modal::HistorySearch => {
                            app.history_scroll = 0;
                            app.history_modal_follow = true;
                        }
                        Modal::Help => app.help_scroll = 0,
                        Modal::Activity => app.activity_scroll = 0,
                        Modal::Permissions => app.permissions_scroll = 0,
                        Modal::Tools | Modal::Mcp | Modal::Skills => {
                            app.session_scroll = 0;
                            app.session_modal_follow = true;
                        }
                        Modal::Config | Modal::ConfigTheme | Modal::ConfigLayout => {
                            app.config_scroll = 0;
                        }
                        Modal::TokenReport => app.token_report_scroll = 0,
                        _ => {}
                    }
                }
                input::InputAction::TokenReportActivate => {
                    if app.active_modal == Modal::TokenReport && !app.token_report_detail {
                        let has_turns = app
                            .token_source_report(&viewed_session_id)
                            .map(|report| view::token_report_round_count(&report) > 0)
                            .unwrap_or(false);
                        if has_turns {
                            app.token_report_detail = true;
                            app.token_report_scroll = 0;
                        }
                    }
                }
                input::InputAction::ScrollUp => {
                    if let Some((scroll, follow)) = app.modal_scroll_field() {
                        if let Some(f) = follow {
                            *f = false;
                        }
                        *scroll = scroll.saturating_sub(1);
                    } else {
                        // While a permission sheet is open the transcript stays
                        // scrollable, so the wheel / page keys drive the
                        // conversation behind it, not the sheet's own body.
                        app.follow_bottom = false;
                        app.pin_summary_line = None;
                        // Mouse wheel tick = 4 lines, not 1, so scrolling feels fast
                        // and responsive instead of crawling line-by-line.
                        app.scroll = app.scroll.saturating_sub(4);
                    }
                }
                input::InputAction::ScrollDown => {
                    if let Some((scroll, follow)) = app.modal_scroll_field() {
                        if let Some(f) = follow {
                            *f = false;
                        }
                        *scroll = scroll.saturating_add(1);
                    } else {
                        app.pin_summary_line = None;
                        app.scroll = app.scroll.saturating_add(4).min(app.max_scroll);
                        if app.scroll >= app.max_scroll {
                            app.follow_bottom = true;
                        }
                    }
                }
                input::InputAction::ScrollPageUp => {
                    // Read the (Copy) page step up front so the subsequent
                    // mutable borrow of the scroll field doesn't conflict.
                    let step = modal_page_step(app);
                    if let Some((scroll, follow)) = app.modal_scroll_field() {
                        if let Some(f) = follow {
                            *f = false;
                        }
                        *scroll = scroll.saturating_sub(step);
                    } else {
                        let step = app.view_height.saturating_sub(1).max(1);
                        app.follow_bottom = false;
                        app.pin_summary_line = None;
                        app.scroll = app.scroll.saturating_sub(step);
                    }
                }
                input::InputAction::ScrollPageDown => {
                    // Read the (Copy) page step up front so the subsequent
                    // mutable borrow of the scroll field doesn't conflict.
                    let step = modal_page_step(app);
                    if let Some((scroll, follow)) = app.modal_scroll_field() {
                        if let Some(f) = follow {
                            *f = false;
                        }
                        *scroll = scroll.saturating_add(step);
                    } else {
                        let step = app.view_height.saturating_sub(1).max(1);
                        app.pin_summary_line = None;
                        app.scroll = app.scroll.saturating_add(step).min(app.max_scroll);
                        if app.scroll >= app.max_scroll {
                            app.follow_bottom = true;
                        }
                    }
                }
                input::InputAction::ScrollTop => {
                    if let Some((scroll, follow)) = app.modal_scroll_field() {
                        if let Some(f) = follow {
                            *f = false;
                        }
                        *scroll = 0;
                    } else {
                        app.follow_bottom = false;
                        app.pin_summary_line = None;
                        app.scroll = 0;
                    }
                }
                input::InputAction::ScrollBottom => {
                    // Modal scroll bounds are clamped by render_body each
                    // frame, so a large number here just means "go to end".
                    if let Some((scroll, follow)) = app.modal_scroll_field() {
                        if let Some(f) = follow {
                            *f = false;
                        }
                        *scroll = usize::MAX;
                    } else {
                        app.pin_summary_line = None;
                        app.scroll = app.max_scroll;
                        app.follow_bottom = true;
                    }
                }
                input::InputAction::PermissionDetailsUp => {
                    app.permission_scroll = app.permission_scroll.saturating_sub(1);
                }
                input::InputAction::PermissionDetailsDown => {
                    app.permission_scroll = app
                        .permission_scroll
                        .saturating_add(1)
                        .min(app.permission_max_scroll);
                }
                input::InputAction::CopySelection => {
                    if let Some(text) = extract_selection_text(
                        &app.selection,
                        app.focused_messages(),
                        &app.input,
                        &app.layout_map,
                        app.drag.cell_info.as_ref(),
                    ) {
                        clipboard_ops::spawn_clipboard_copy(&copy_tx, copy_pending.clone(), text);
                    }
                }
                input::InputAction::CtrlC => {
                    if let Some(text) = extract_selection_text(
                        &app.selection,
                        app.focused_messages(),
                        &app.input,
                        &app.layout_map,
                        app.drag.cell_info.as_ref(),
                    ) {
                        clipboard_ops::spawn_clipboard_copy(&copy_tx, copy_pending.clone(), text);
                    } else if app.active_modal == Modal::HistorySearch {
                        // Cancel the history modal: restore the in-progress draft
                        // the user was composing before Ctrl+R (clears the search
                        // query and sub-flags too).
                        app.restore_history_draft();
                        app.active_modal = Modal::None;
                    } else if app.startup_overlay == crate::tui::StartupOverlay::SessionsPicker
                        && app.active_modal == Modal::Sessions
                    {
                        // `neenee resume` (no id) opened the picker at startup:
                        // there is no conversation behind it, so Ctrl+C — like
                        // Esc and an outside click — quits the program rather
                        // than dropping into an empty session. Without this,
                        // Ctrl+C used to close the modal and land the user in a
                        // bare empty chat (which a stray /models then persisted
                        // as an empty-session file).
                        tracing::info!(reason = "startup_picker_cancelled", "app exiting");
                        app.should_quit.store(true, Ordering::SeqCst);
                    } else if app.active_modal != Modal::None
                        && app.active_modal != Modal::Permission
                    {
                        app.active_modal = Modal::None;
                    } else if app.in_side_view {
                        // `/btw` side view: Ctrl+C leaves the side
                        // conversation (ADR-0017), mirroring Esc. Slotted
                        // after modal-close so an open overlay still wins.
                        app.exit_side_view();
                        let _ = app.tx.send(AgentRequest::ExitSideView);
                    } else if !app.input.is_empty() {
                        // Ctrl+C is purely a compose-level action: copy,
                        // close overlay, clear, or quit. It never interrupts a
                        // running turn — only double-Esc does — so a task in
                        // flight is left untouched here and the input is
                        // cleared instead. Clearing the input also arms the
                        // quit window so
                        // the chain is exactly two presses total (clear,
                        // then quit). The combined toast says both what
                        // just happened and what the next Ctrl+C will do,
                        // removing the old "silent clear → user can't tell
                        // if the next press will quit or do something else"
                        // ambiguity. Pending-image reminders skip their
                        // per-frame refresh while the quit window is armed
                        // so this toast keeps the floor.
                        app.input.clear();
                        app.set_cursor(0);
                        app.input_scroll = 0;
                        show_local_toast(
                            app,
                            "input cleared — Ctrl+C again to exit",
                            false,
                            std::time::Duration::from_millis(2000),
                        );
                        app.arm_ctrl_c(Some(
                            std::time::Instant::now() + std::time::Duration::from_secs(2),
                        ));
                    } else if app.ctrl_c_armed() {
                        tracing::info!(reason = "ctrl_c_double_press", "app exiting");
                        return Ok(());
                    } else {
                        // Arm a real 2s window (wall-clock) in which a second
                        // Ctrl+C quits.
                        app.arm_ctrl_c(Some(
                            std::time::Instant::now() + std::time::Duration::from_secs(2),
                        ));
                    }
                }
                input::InputAction::OpenTodos => {
                    // Ctrl+T opens the Todos modal — the agent's live task
                    // list surfaced on its own overlay. The list is
                    // agent-owned and read-only in the TUI; this simply opens
                    // the Activity modal pinned to the Todos section, exactly
                    // like clicking the todo bar.
                    app.active_modal = Modal::Activity;
                    app.activity_tab = ActivityTab::Todos;
                    app.modal_keymap_open = false;
                    app.modal_index = 0;
                    app.activity_scroll = 0;
                    app.selection = SelectionState::None;
                    app.focused_target = None;
                    app.drag.cancel();
                }
                input::InputAction::OpenQueue => {
                    // F2 opens the queue overview — the full outbox list that
                    // the persistent queue bar previews. The selection starts
                    // at the front (the next item to pop). This mirrors a
                    // click on the queue bar.
                    //
                    // Opening the modal auto-blocks the viewed session's
                    // outbox so items can be managed safely (delete / reorder
                    // / re-edit) without one auto-draining mid-edit. Closing
                    // the modal (Esc / outside-click) resumes auto-drain —
                    // the block here is an editing safety latch, not a
                    // persistent user choice (that's `F3`). See the
                    // `CloseModal` / outside-click paths for the matching
                    // resume.
                    app.active_modal = Modal::Queue;
                    app.modal_keymap_open = false;
                    app.modal_index = 0;
                    app.queue_scroll = 0;
                    app.queue_modal_follow = true;
                    app.selection = SelectionState::None;
                    app.focused_target = None;
                    app.drag.cancel();
                    app.block_queue(&viewed_session_id);
                }
                input::InputAction::FocusNextTarget => {
                    // Ctrl+↓ (or ↓ while focused): advance to the next step.
                    // From no focus this lands on the first (oldest) step.
                    app.focus_interactive_target(1);
                }
                input::InputAction::FocusPrevTarget => {
                    // Ctrl+↑ (or ↑ while focused): step back. From no focus this
                    // lands on the last (nearest-to-prompt) step.
                    app.focus_interactive_target(-1);
                }
                input::InputAction::ClearFocusedTarget => {
                    // Esc: drop the focus highlight, returning every key to its
                    // ordinary input-box meaning.
                    app.focused_target = None;
                }
                input::InputAction::ActivateFocusedTarget => {
                    if let Some(target) = app.focused_target {
                        match target.kind {
                            InteractiveTargetKind::ToolStep => {
                                let mut messages = runtime.messages.write().await;
                                let enter_id = resolve_focused_mut(
                                    &mut messages,
                                    &app.focus_stack,
                                    target.message_idx,
                                )
                                .and_then(|message| {
                                    if message.is_envoy_task() {
                                        message.tool_step_call_id().map(String::from)
                                    } else {
                                        None
                                    }
                                });
                                if let Some(id) = enter_id {
                                    drop(messages);
                                    app.enter_envoy(id);
                                } else {
                                    // Enter mirrors the mouse click on a tool
                                    // step's summary: toggle its inline
                                    // disclosure (expand/collapse) rather than
                                    // popping a modal. Keeping keyboard and
                                    // pointer parity is the expected behavior
                                    // for the disclosure affordance.
                                    app.toggle_step_pinned(&mut messages, target.message_idx);
                                    drop(messages);
                                }
                            }
                            InteractiveTargetKind::Thinking => {
                                let mut messages = runtime.messages.write().await;
                                let toggled =
                                    app.toggle_step_pinned(&mut messages, target.message_idx);
                                drop(messages);
                                if toggled {
                                    app.selection = SelectionState::None;
                                }
                            }
                            InteractiveTargetKind::ProviderRetry => {
                                let mut messages = runtime.messages.write().await;
                                let toggled =
                                    app.toggle_step_pinned(&mut messages, target.message_idx);
                                drop(messages);
                                if toggled {
                                    app.selection = SelectionState::None;
                                }
                            }
                            InteractiveTargetKind::CommandResult => {
                                // Enter mirrors the mouse click on a command
                                // row: toggle its expandable result body.
                                let mut messages = runtime.messages.write().await;
                                let toggled =
                                    app.toggle_step_pinned(&mut messages, target.message_idx);
                                drop(messages);
                                if toggled {
                                    app.selection = SelectionState::None;
                                }
                            }
                        }
                    }
                }
                input::InputAction::Paste => {
                    // Ctrl+V: read the system clipboard off the event loop.
                    // The result is delivered back through `paste_rx` and
                    // applied on a later frame (image -> attach, text ->
                    // insert on the main prompt, or inline splice into the
                    // focused modal field). `apply_clipboard_paste` branches
                    // on the active modal at apply time, so a paste spawned
                    // inside a modal that the user closed before the read
                    // returned lands in the main prompt rather than being
                    // dropped.
                    clipboard_ops::spawn_clipboard_paste(&paste_tx);
                }
                input::InputAction::BracketedPaste(text) => {
                    // Terminal-level paste (bracketed paste mode). The payload
                    // is already in hand, so route it directly through the same
                    // chip-or-inline logic as Ctrl+V without an async hop.
                    clipboard_ops::apply_clipboard_paste(app, clipboard::ClipboardRead::Text(text));
                }
                input::InputAction::ExitEnvoy => {
                    app.exit_envoy();
                }
                input::InputAction::ExitSideView => {
                    // `/btw`: return to the primary transcript (ADR-0017).
                    // Optimistically flip the view for snappiness and tell the
                    // harness to tear down the live side session; its
                    // `SideViewClosed` reply is a backstop in case this fires
                    // twice (Esc then Ctrl+C).
                    if app.in_side_view {
                        app.exit_side_view();
                        let _ = app.tx.send(AgentRequest::ExitSideView);
                    }
                }
                input::InputAction::PrevSibling => {
                    app.cycle_sibling(-1);
                }
                input::InputAction::NextSibling => {
                    app.cycle_sibling(1);
                }
                input::InputAction::InsertChar(c) => {
                    // Already handled by process_event mutating app.input
                    let _ = c;
                    // The custom-provider filter field re-ranks its suggestion
                    // list as the query changes.
                    if app.active_modal == Modal::CustomProvider {
                        app.on_custom_filter_changed();
                    } else if app.active_modal == Modal::ConfigThemeCustom
                        && Theme::set_custom_color_value(
                            &mut app.custom_color_draft,
                            app.modal_index,
                            &app.input,
                        )
                    {
                        app.theme = Theme::from_color_scheme("custom", &app.custom_color_draft);
                    }
                    app.suggestion_index = None;
                    // The user is editing again, so live completions are
                    // once again useful — clear the Enter-commit dismissal.
                    app.completion_dismissed = false;
                    // Typing into the input box reclaims it as the active
                    // surface: drop any transcript-step focus so the composer
                    // re-brightens and the next arrow key resumes caret movement
                    // rather than step navigation.
                    app.focused_target = None;
                    // Reconcile attachments: if the user typed inside a chip
                    // (breaking its syntax) the backing staged entry must be
                    // dropped, and surviving chips relabeled.
                    app.reconcile_attachments();
                }
                input::InputAction::Backspace => {
                    if app.active_modal == Modal::CustomProvider {
                        app.on_custom_filter_changed();
                    } else if app.active_modal == Modal::ConfigThemeCustom
                        && Theme::set_custom_color_value(
                            &mut app.custom_color_draft,
                            app.modal_index,
                            &app.input,
                        )
                    {
                        app.theme = Theme::from_color_scheme("custom", &app.custom_color_draft);
                    }
                    app.suggestion_index = None;
                    app.completion_dismissed = false;
                    // Same as InsertChar: editing the input box reclaims focus
                    // from any transcript step.
                    app.focused_target = None;
                    // Reconcile attachments: a chip-aware backspace has
                    // already spliced the chip out of `app.input`; this
                    // drops the orphaned entry from `pending_images` /
                    // `pending_text_pastes` and relabels survivors.
                    app.reconcile_attachments();
                }
                input::InputAction::SuggestNext => {
                    let count = app.completions().len();
                    if count > 0 {
                        let next = match app.suggestion_index {
                            Some(i) => (i + 1) % count,
                            None => 0,
                        };
                        app.suggestion_index = Some(next);
                    }
                }
                input::InputAction::SuggestPrev => {
                    let count = app.completions().len();
                    if count > 0 {
                        let prev = match app.suggestion_index {
                            Some(i) => {
                                if i == 0 {
                                    count - 1
                                } else {
                                    i - 1
                                }
                            }
                            None => count - 1,
                        };
                        app.suggestion_index = Some(prev);
                    }
                }
                input::InputAction::AcceptSuggestion(idx_str) => {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        app.accept_completion(idx);
                    }
                    // Note: slash-command accepts latch the dismissal flag
                    // inside accept_completion (terminal accept), so Tab on
                    // `/pursue` exits completion just like Enter. `@path`
                    // accepts stay live so Tab keeps cycling candidates.
                }
                input::InputAction::CommitSuggestion(idx_str) => {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        app.accept_completion(idx);
                    }
                    // Enter always "finishes" the completion regardless of
                    // kind: drop the highlight and latch the dismissal flag
                    // so the popup stays hidden until the next edit. For
                    // slash commands this mirrors what accept_completion
                    // already did; for `@path` it is Enter-specific (Tab on
                    // a path stays live so the user can keep cycling).
                    app.suggestion_index = None;
                    app.completion_dismissed = true;
                }
                input::InputAction::CloseCompletion => {
                    // Esc dismisses the popup without accepting anything.
                    // Same latch as Enter-commit so the popup stays hidden
                    // until the next edit clears `completion_dismissed`.
                    app.suggestion_index = None;
                    app.completion_dismissed = true;
                }
                input::InputAction::HistoryPrev => {
                    // Inline ↑ walks the **current session's** history only
                    // (newest-first), not the whole cross-session log — Ctrl+R
                    // is the global search surface. We recompute the session
                    // slice each press so newly-recorded entries appear
                    // without a restart; `history_index` is a position into
                    // that slice. `App::history_prev` advances toward older
                    // entries and stashes the in-progress draft on the first
                    // press (so ↓ can restore it).
                    let session_rows = app.current_session_history();
                    app.history_prev(&session_rows);
                }
                input::InputAction::RecallQueued => {
                    // Top-level `↑` (in an empty composer while the queue is
                    // non-empty) recalls the newest staged item into the
                    // composer for editing. Purely local — no modal is open,
                    // no block state changes.
                    match app.recall_queued(&viewed_session_id) {
                        Some(crate::tui::app::RecallQueued::Restored(dispatch)) => {
                            app.restore_dispatch(dispatch);
                        }
                        None => {}
                    }
                }
                input::InputAction::RecallQueuedSelected => {
                    // The queue modal's `Enter` recalls the *selected* item
                    // (the `↑/↓` highlight, not always the newest) into the
                    // composer and closes the modal. Closing resumes the
                    // auto-block the modal set on open.
                    let idx = app.modal_index;
                    app.active_modal = Modal::None;
                    app.resume_queue(&viewed_session_id);
                    match app.recall_queued_at(&viewed_session_id, idx) {
                        Some(crate::tui::app::RecallQueued::Restored(dispatch)) => {
                            app.restore_dispatch(dispatch);
                        }
                        None => {}
                    }
                }
                input::InputAction::QueueToggleBlock => {
                    // `F3` (top-level or inside the queue modal): toggle the
                    // hard block on the viewed session's outbox. While blocked
                    // no queued message auto-drains, even after the round
                    // completes. This is the persistent user choice, distinct
                    // from the modal's editing-safety auto-block.
                    app.toggle_queue_block(&viewed_session_id);
                }
                input::InputAction::QueueDelete => {
                    // `Shift+D` in the queue modal: remove the highlighted
                    // item outright. The queue is auto-blocked on open, so the
                    // index can't drift under us. Clamp the selection to the
                    // now-shorter list.
                    if app.active_modal == Modal::Queue {
                        let idx = app.modal_index;
                        app.remove_queued_at(&viewed_session_id, idx);
                        let count = app.pending_count(&viewed_session_id);
                        if count == 0 {
                            app.modal_index = 0;
                        } else if app.modal_index >= count {
                            app.modal_index = count - 1;
                        }
                        app.queue_modal_follow = true;
                    }
                }
                input::InputAction::QueueMoveItem { delta } => {
                    // `K`/`J` in the queue modal: reorder the highlighted item
                    // toward the front (next to pop) or the tail. Clamp at the
                    // session slice boundaries so it can't escape into another
                    // session's items.
                    if app.active_modal == Modal::Queue {
                        let idx = app.modal_index;
                        app.move_queued(&viewed_session_id, idx, delta);
                        // Follow the moved item if it changed position.
                        let count = app.pending_count(&viewed_session_id);
                        if count > 0 {
                            app.modal_index =
                                (idx as i32 + delta).clamp(0, count as i32 - 1) as usize;
                            app.queue_modal_follow = true;
                        }
                    }
                }
                input::InputAction::HistoryNext => {
                    // Inline ↓ walks the current session's history forward
                    // (toward the newest), mirroring HistoryPrev. Walking past
                    // the newest entry restores the stashed draft.
                    let session_rows = app.current_session_history();
                    app.history_next(&session_rows);
                }
                input::InputAction::ModalUp => match app.active_modal {
                    Modal::Connections | Modal::Models => {
                        // Walk the fuzzy-filtered rows of the *active picker*
                        // (providers in Connections, flat (provider, model)
                        // pairs in Models), so the cursor never lands on a
                        // hidden row (same rule as the history-search modal).
                        let count = app.picker_row_count();
                        app.modal_index = if count == 0 {
                            0
                        } else if app.modal_index == 0 {
                            count - 1
                        } else {
                            app.modal_index - 1
                        };
                        app.model_modal_follow = true;
                    }
                    Modal::HistorySearch => {
                        // Up/Down walk the fuzzy-filtered list, not the raw
                        // history, so the cursor never lands on an entry the
                        // user cannot actually see or select.
                        let count = app.history_rows().len();
                        app.modal_index = if count == 0 {
                            0
                        } else if app.modal_index == 0 {
                            count - 1
                        } else {
                            app.modal_index - 1
                        };
                        app.history_modal_follow = true;
                        // In preview mode the body shows the focused entry's
                        // full text, so moving to another entry re-anchors it
                        // to the top.
                        if app.history_preview {
                            app.history_scroll = 0;
                        }
                    }
                    Modal::Permission => {
                        let count = if app.permission_confirm_always { 2 } else { 4 };
                        app.modal_index = if app.modal_index == 0 {
                            count - 1
                        } else {
                            app.modal_index - 1
                        };
                    }
                    Modal::Sessions => {
                        let count = app.sessions_overview.len();
                        app.modal_index = if count == 0 {
                            0
                        } else if app.modal_index == 0 {
                            count - 1
                        } else {
                            app.modal_index - 1
                        };
                        app.session_modal_follow = true;
                    }
                    Modal::Host => {
                        if app.host_focus == crate::tui::overlays::DashboardFocus::List {
                            let count = app.host_sessions.len();
                            app.modal_index = if count == 0 {
                                0
                            } else if app.modal_index == 0 {
                                count - 1
                            } else {
                                app.modal_index - 1
                            };
                            app.host_modal_follow = true;
                            // Re-engage body-follow so the moved selection stays on
                            // screen (cleared again on manual page/wheel scroll).
                            app.session_modal_follow = true;
                        } else {
                            app.host_detail_scroll = app.host_detail_scroll.saturating_sub(1);
                        }
                    }
                    Modal::Permissions => {
                        let count = app
                            .session_context
                            .as_ref()
                            .map(|s| s.permissions.len())
                            .unwrap_or(0);
                        app.modal_index = if count == 0 {
                            0
                        } else if app.modal_index == 0 {
                            count - 1
                        } else {
                            app.modal_index - 1
                        };
                    }
                    Modal::Config => {
                        // Config root: cycle up through the category list.
                        // Count matches `categories()` in config.rs.
                        let count = 2usize;
                        app.modal_index = (app.modal_index + count - 1) % count;
                    }
                    Modal::ConfigTheme => {
                        let count = crate::tui::view::overlays::config_theme::ROW_COUNT;
                        app.modal_index = (app.modal_index + count - 1) % count;
                    }
                    Modal::ConfigLayout => {
                        let count = crate::tui::view::overlays::config_layout::ROW_COUNT;
                        app.modal_index = (app.modal_index + 1) % count;
                    }
                    Modal::TokenReport => {
                        if app.token_report_detail {
                            app.token_report_scroll = app.token_report_scroll.saturating_sub(1);
                        } else {
                            let count = app
                                .token_source_report(&viewed_session_id)
                                .map(|report| view::token_report_round_count(&report))
                                .unwrap_or(0)
                                .max(1);
                            app.modal_index = (app.modal_index + count - 1) % count;
                        }
                    }
                    Modal::Queue => {
                        // Wheel/PageUp: scroll the queue body. Clearing the
                        // follow flag lets the user browse freely until they
                        // navigate with ↑/↓ again.
                        app.queue_scroll = app.queue_scroll.saturating_sub(1);
                        app.queue_modal_follow = false;
                    }
                    Modal::Help
                    | Modal::Question
                    | Modal::ModelEditor
                    | Modal::ProviderTemplate
                    | Modal::OauthPending
                    | Modal::CustomProvider
                    | Modal::ConfigThemeCustom
                    | Modal::InputInjection
                    | Modal::Tools
                    | Modal::Mcp
                    | Modal::Skills
                    | Modal::Activity
                    | Modal::None => {}
                },
                input::InputAction::ModalDown => match app.active_modal {
                    Modal::Connections | Modal::Models => {
                        let count = app.picker_row_count().max(1);
                        app.modal_index = (app.modal_index + 1) % count;
                        app.model_modal_follow = true;
                    }
                    Modal::HistorySearch => {
                        let count = app.history_rows().len().max(1);
                        app.modal_index = (app.modal_index + 1) % count;
                        app.history_modal_follow = true;
                        if app.history_preview {
                            app.history_scroll = 0;
                        }
                    }
                    Modal::Permission => {
                        let count = if app.permission_confirm_always { 2 } else { 4 };
                        app.modal_index = (app.modal_index + 1) % count;
                    }
                    Modal::Sessions => {
                        let count = app.sessions_overview.len().max(1);
                        app.modal_index = (app.modal_index + 1) % count;
                        // Re-engage body-follow so the moved selection stays on
                        // screen (cleared again on manual page/wheel scroll).
                        app.session_modal_follow = true;
                    }
                    Modal::Host => {
                        if app.host_focus == crate::tui::overlays::DashboardFocus::List {
                            let count = app.host_sessions.len().max(1);
                            app.modal_index = (app.modal_index + 1) % count;
                            app.host_modal_follow = true;
                        } else {
                            app.host_detail_scroll = app.host_detail_scroll.saturating_add(1);
                        }
                    }
                    Modal::Permissions => {
                        let count = app
                            .session_context
                            .as_ref()
                            .map(|s| s.permissions.len())
                            .unwrap_or(0)
                            .max(1);
                        app.modal_index = (app.modal_index + 1) % count;
                    }
                    Modal::Config => {
                        // Config root: cycle down through the category list.
                        // Count matches `categories()` in config.rs.
                        let count = 2usize;
                        app.modal_index = (app.modal_index + 1) % count;
                    }
                    Modal::ConfigTheme => {
                        let count = crate::tui::view::overlays::config_theme::ROW_COUNT;
                        app.modal_index = (app.modal_index + 1) % count;
                    }
                    Modal::ConfigLayout => {
                        let count = crate::tui::view::overlays::config_layout::ROW_COUNT;
                        app.modal_index = (app.modal_index + 1) % count;
                    }
                    Modal::TokenReport => {
                        if app.token_report_detail {
                            app.token_report_scroll = app.token_report_scroll.saturating_add(1);
                        } else {
                            let count = app
                                .token_source_report(&viewed_session_id)
                                .map(|report| view::token_report_round_count(&report))
                                .unwrap_or(0)
                                .max(1);
                            app.modal_index = (app.modal_index + 1) % count;
                        }
                    }
                    Modal::Queue => {
                        // Wheel/PageDown: scroll the queue body. Clearing the
                        // follow flag lets the user browse freely until they
                        // navigate with ↑/↓ again.
                        app.queue_scroll = app.queue_scroll.saturating_add(1);
                        app.queue_modal_follow = false;
                    }
                    Modal::Help
                    | Modal::Question
                    | Modal::ModelEditor
                    | Modal::ProviderTemplate
                    | Modal::OauthPending
                    | Modal::CustomProvider
                    | Modal::ConfigThemeCustom
                    | Modal::InputInjection
                    | Modal::Tools
                    | Modal::Mcp
                    | Modal::Skills
                    | Modal::Activity
                    | Modal::None => {}
                },
                input::InputAction::QuestionUp => {
                    if app.active_modal == Modal::Question
                        && let Some(qm) = app.question.take()
                    {
                        app.question =
                            Some(qm.update(crate::tui::question_model::QuestionAction::Up).0);
                        // Moving the highlight re-enables follow so the body
                        // scrolls to keep the cursor visible.
                        app.question_modal_follow = true;
                    }
                }
                input::InputAction::QuestionDown => {
                    if app.active_modal == Modal::Question
                        && let Some(qm) = app.question.take()
                    {
                        app.question = Some(
                            qm.update(crate::tui::question_model::QuestionAction::Down)
                                .0,
                        );
                        app.question_modal_follow = true;
                    }
                }
                input::InputAction::QuestionToggle => {
                    if app.active_modal == Modal::Question
                        && let Some(qm) = app.question.take()
                    {
                        app.question = Some(
                            qm.update(crate::tui::question_model::QuestionAction::Toggle)
                                .0,
                        );
                    }
                }
                input::InputAction::QuestionSelect(n) => {
                    if app.active_modal == Modal::Question
                        && let Some(qm) = app.question.take()
                    {
                        app.question = Some(
                            qm.update(crate::tui::question_model::QuestionAction::Select(n))
                                .0,
                        );
                        // A digit jump moves the highlight, so follow it.
                        app.question_modal_follow = true;
                    }
                }
                input::InputAction::QuestionSubmit => {
                    if app.active_modal == Modal::Question
                        && let Some(qm) = app.question.take()
                    {
                        let (qm, effects) =
                            qm.update(crate::tui::question_model::QuestionAction::Submit);
                        // Keep the model until the per-frame queue sync clears
                        // it; the Closed effect drives the channel reply + drain.
                        app.question = Some(qm);
                        question_effects::apply(&effects, app, &runtime).await;
                        app.question_scroll = 0;
                        app.question_modal_follow = true;
                    }
                }
                input::InputAction::QuestionPrevious => {
                    if app.active_modal == Modal::Question
                        && let Some(qm) = app.question.take()
                    {
                        app.question = Some(
                            qm.update(crate::tui::question_model::QuestionAction::Previous)
                                .0,
                        );
                        app.question_scroll = 0;
                        app.question_modal_follow = true;
                    }
                }
                input::InputAction::QuestionCancel => {
                    if app.active_modal == Modal::Question
                        && let Some(qm) = app.question.take()
                    {
                        let (_qm, effects) =
                            qm.update(crate::tui::question_model::QuestionAction::Cancel);
                        // Cancel discards the model immediately; the Closed
                        // effect drives the (empty-answers) reply + drain.
                        question_effects::apply(&effects, app, &runtime).await;
                    }
                }
                input::InputAction::InputSubmit => {
                    if app.active_modal == Modal::InputInjection {
                        let text = std::mem::take(&mut app.input);
                        if let Some(req) = app.pending_input.take() {
                            // Drain the matching front so the per-frame sync
                            // closes the modal and restores the composer draft.
                            runtime.pending_input.lock().await.pop_front();
                            let parent_call_id =
                                runtime.envoy_question_parent.lock().await.remove(&req.id);
                            let _ = app.tx.send(AgentRequest::InputReply {
                                request_id: req.id.clone(),
                                text,
                                parent_call_id,
                            });
                        }
                        app.restore_input_draft();
                        app.active_modal = Modal::None;
                    }
                }
                input::InputAction::InputCancel => {
                    if app.active_modal == Modal::InputInjection
                        && let Some(req) = app.pending_input.take()
                    {
                        // Empty reply = cancel → the command runs with closed
                        // stdin and fails fast with a non-interactive remedy.
                        runtime.pending_input.lock().await.pop_front();
                        let parent_call_id =
                            runtime.envoy_question_parent.lock().await.remove(&req.id);
                        let _ = app.tx.send(AgentRequest::InputReply {
                            request_id: req.id.clone(),
                            text: String::new(),
                            parent_call_id,
                        });
                        app.restore_input_draft();
                        app.active_modal = Modal::None;
                    }
                }
                input::InputAction::QuestionInsertChar(c) => {
                    if app.active_modal == Modal::Question
                        && let Some(qm) = app.question.take()
                    {
                        app.question = Some(
                            qm.update(crate::tui::question_model::QuestionAction::InsertChar(c))
                                .0,
                        );
                        // Typing into the "Other" field may grow it onto a new
                        // wrapped line, pushing the caret below the viewport.
                        // Re-arm follow so the body scrolls to track the
                        // caret (not just the "Other" label row).
                        app.question_modal_follow = true;
                    }
                }
                input::InputAction::QuestionBackspace => {
                    if app.active_modal == Modal::Question
                        && let Some(qm) = app.question.take()
                    {
                        app.question = Some(
                            qm.update(crate::tui::question_model::QuestionAction::Backspace)
                                .0,
                        );
                        // Backspace can collapse the field back up a line;
                        // re-arm follow so the caret stays on screen.
                        app.question_modal_follow = true;
                    }
                }
                input::InputAction::PermissionSubmit => {
                    handle_permission_submit(app, &runtime).await;
                }
                input::InputAction::PermissionReject => {
                    // Rejecting settles the whole concurrent permission batch;
                    // resolve every queued request so its tool futures finish.
                    let queued: Vec<PermissionRequest> =
                        runtime.pending_permission.lock().await.drain(..).collect();
                    app.pending_permission = None;
                    app.active_modal = Modal::None;
                    app.modal_index = 0;
                    app.permission_confirm_always = false;
                    app.permission_show_details = false;
                    let mut parents = runtime.envoy_permission_parent.lock().await;
                    for pending in queued {
                        let parent_call_id = parents.remove(&pending.id);
                        let _ = app.tx.send(AgentRequest::PermissionReply {
                            request_id: pending.id,
                            decision: PermissionDecision::Reject,
                            parent_call_id,
                        });
                    }
                }
                input::InputAction::PermissionBack => {
                    app.permission_confirm_always = false;
                    app.modal_index = 1;
                }
                input::InputAction::SelectionStart { x, y } => {
                    // Provider-delete confirm overlay owns clicks while open: a
                    // press outside the panel cancels the staged deletion
                    // (mirrors Esc) but leaves the provider picker open, and a
                    // press inside is a no-op (the buttons are keyboard-only).
                    // Either way the click is consumed so it never reaches the
                    // picker or transcript behind the backdrop.
                    if app.pending_provider_delete.is_some()
                        && let Some(r) = app.provider_delete_rect
                    {
                        let inside =
                            r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height;
                        if !inside {
                            app.pending_provider_delete = None;
                            app.provider_delete_focus = ProviderDeleteChoice::default();
                        }
                        app.selection = SelectionState::None;
                        app.focused_target = None;
                        app.drag.cancel();
                    } else if app.active_modal == Modal::Question {
                        if let Some(hit) = app.modal_hit_map.question_option_at(x, y)
                            && let Some(qm) = app.question.take()
                        {
                            app.question = Some(
                                qm.update(crate::tui::question_model::QuestionAction::Select(
                                    hit.option_index + 1,
                                ))
                                .0,
                            );
                            app.question_modal_follow = true;
                        }
                        app.selection = SelectionState::None;
                        app.focused_target = None;
                        app.drag.cancel();
                    } else if app.active_modal == Modal::Permission
                        && let Some(hit) = app.modal_hit_map.permission_action_at(x, y)
                    {
                        app.modal_index = hit.action_index;
                        handle_permission_submit(app, &runtime).await;
                        app.selection = SelectionState::None;
                        app.focused_target = None;
                        app.drag.cancel();
                    } else if app.active_modal == Modal::Permission
                        && app.modal_hit_map.permission_sheet_contains(x, y)
                    {
                        app.selection = SelectionState::None;
                        app.focused_target = None;
                        app.drag.cancel();
                    } else if app.active_modal.dismissable_by_outside_click() {
                        // Click-to-dismiss: while a dismissable overlay modal is
                        // open, the full-screen backdrop owns the click — a press
                        // outside the panel closes the modal (mirroring Esc), and a
                        // press inside is a no-op (these info modals have no click
                        // targets yet). Either way the click is consumed so it does
                        // not also fall through to the transcript behind the
                        // backdrop. Modals that hold precious input and need their
                        // own restore path (Provider / ModelEditor) report no rect
                        // and are skipped here, so a stray click never discards an
                        // API key. HistorySearch *is* dismissable: its filter is
                        // ephemeral and the draft is parked, so an outside click
                        // restores the draft (mirroring Esc / CloseModal).
                        //
                        // The close decision mirrors the `CloseModal` arm
                        // exactly, *including the deepest-level-first ordering*:
                        // an outside click while inside a drill-in sub-view (e.g.
                        // Sessions › Info) backs out to the parent view, not out
                        // to chat / quit — so the hierarchy is consistent between
                        // Esc and outside-click.
                        let inside = app.modal_rect.map_or(false, |r| {
                            r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height
                        });
                        if !inside && app.click_outside_dismiss {
                            // Dismiss when `[tui] click_outside_dismiss` is on
                            // (default on): a click outside the panel closes
                            // a dismissable overlay like Esc. The dismissable
                            // set excludes modals holding precious in-progress
                            // input (they report no rect and are skipped above),
                            // so a stray click never discards an API key. When
                            // the flag is off the click is still consumed (this
                            // whole branch owns it) so it does not fall through
                            // to the transcript behind the backdrop.
                            //
                            // The close decision mirrors the `CloseModal` arm
                            // exactly, including the deepest-level-first
                            // ordering: an outside click while inside a drill-in
                            // sub-view (e.g. Sessions › Info) backs out to the
                            // parent view, not out to chat / quit — so the
                            // hierarchy is consistent between Esc and outside-
                            // click.
                            if app.active_modal == Modal::Sessions && app.session_info_detail {
                                // Outside-click from the info sub-view → back to
                                // the sessions list (mirrors Esc).
                                app.session_info_detail = false;
                                app.session_detail = None;
                                app.session_info_scroll = 0;
                            } else if app.active_modal == Modal::TokenReport
                                && app.token_report_detail
                            {
                                // Outside-click from the turn breakdown → back to
                                // the round list (mirrors Esc).
                                app.token_report_detail = false;
                                app.token_report_scroll = 0;
                            } else {
                                if app.active_modal == Modal::HistorySearch {
                                    app.restore_history_draft();
                                }
                                // The queue modal auto-blocked on open; an
                                // outside-click closes it like Esc, so resume
                                // auto-drain to match.
                                if app.active_modal == Modal::Queue {
                                    app.resume_queue(&viewed_session_id);
                                }
                                // `neenee resume` (no id): the startup picker has
                                // no conversation behind it, so a click-outside
                                // (mirroring Esc) quits instead of landing in an
                                // empty chat.
                                if app.startup_overlay == crate::tui::StartupOverlay::SessionsPicker
                                {
                                    tracing::info!(
                                        reason = "startup_picker_cancelled",
                                        "app exiting"
                                    );
                                    app.should_quit.store(true, Ordering::SeqCst);
                                }
                                app.active_modal = Modal::None;
                            }
                        }
                        app.selection = SelectionState::None;
                        app.focused_target = None;
                        app.drag.cancel();
                    } else if app.active_modal == Modal::None
                        && app.todos_rect.is_some_and(|r| {
                            // Todo bar: open the Activity modal on the Todos
                            // section directly. Checked before the activity-bar
                            // rect in case the two bars ever overlap.
                            r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height
                        })
                    {
                        // The activity bar may still be painted while a modal
                        // owns the surface — especially the pending Permission
                        // sheet, whose expanded body grows up over this row.
                        // Gate on `Modal::None` so a click never stacks an
                        // Activity modal on top of an in-progress decision.
                        app.active_modal = Modal::Activity;
                        app.activity_tab = ActivityTab::Todos;
                        app.modal_index = 0;
                        app.activity_scroll = 0;
                        app.selection = SelectionState::None;
                        app.focused_target = None;
                        app.drag.cancel();
                    } else if app.active_modal == Modal::None
                        && app.activity_rect.is_some_and(|r| {
                            r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height
                        })
                    {
                        app.active_modal = Modal::Activity;
                        app.activity_tab = ActivityTab::Activity;
                        app.modal_index = 0;
                        app.activity_scroll = 0;
                        app.selection = SelectionState::None;
                        app.focused_target = None;
                        app.drag.cancel();
                    } else if app.active_modal == Modal::None
                        && app.queue_rect.is_some_and(|r| {
                            r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height
                        })
                    {
                        // Click anywhere on the persistent queue bar → expand
                        // the full Queue modal. Selection starts at the front
                        // (the next item to pop). Auto-blocks the outbox for
                        // safe editing (mirrors the F2 open path); closing the
                        // modal resumes.
                        app.active_modal = Modal::Queue;
                        app.modal_keymap_open = false;
                        app.modal_index = 0;
                        app.queue_scroll = 0;
                        app.queue_modal_follow = true;
                        app.selection = SelectionState::None;
                        app.focused_target = None;
                        app.drag.cancel();
                        app.block_queue(&viewed_session_id);
                    } else if app.active_modal == Modal::None
                        && app.hint_context_rect.is_some_and(|r| {
                            r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height
                        })
                    {
                        // Click on the context meter in the hint bar → token
                        // source report modal. In attach mode there is no
                        // local ledger (token accounting lives server-side),
                        // so fetch the report from the harness on demand and
                        // render a loading placeholder until the reply lands.
                        app.active_modal = Modal::TokenReport;
                        app.modal_index = 0;
                        app.token_report_scroll = 0;
                        app.token_report_detail = false;
                        if app.token_ledger.is_none() {
                            app.token_report = None;
                            let _ = app.tx.send(AgentRequest::QueryTokenUsage {
                                session_id: viewed_session_id.clone(),
                            });
                        }
                        app.selection = SelectionState::None;
                        app.focused_target = None;
                        app.drag.cancel();
                    } else if app.sticky_rect.is_some_and(|r| {
                        // Sticky pinned step header: collapse it on click.
                        r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height
                    }) {
                        if let Some(mi) = app.sticky_step {
                            let mut messages = runtime.messages.write().await;
                            app.focused_target =
                                app.focused_messages().get(mi).and_then(|message| {
                                    if message.is_thinking() {
                                        Some(InteractiveTarget::thinking(mi))
                                    } else if message.is_tool_step() || message.is_envoy_task() {
                                        Some(InteractiveTarget::tool_step(mi))
                                    } else {
                                        None
                                    }
                                });
                            app.toggle_step_pinned(&mut messages, mi);
                            drop(messages);
                        }
                        // Clicking the sticky header focuses that step (set
                        // above), so keyboard navigation can continue from it.
                        app.selection = SelectionState::None;
                        app.drag.cancel();
                    } else {
                        // ── Unified content hit-test cascade ──
                        // interaction::classify_click runs the full priority
                        // chain (input box → step summary → table cell →
                        // generic content → gap → dead) so the event loop
                        // only needs a single match.
                        match interaction::classify_click(&app.layout_map, x, y) {
                            ClickTarget::InputBox { cursor } => {
                                // Click inside the live input box: clear any
                                // focused step so the next keypress edits rather
                                // than acting on a step.
                                app.focused_target = None;
                                app.drag.begin_range(&mut app.selection, cursor);
                            }
                            ClickTarget::StepSummary { message_idx, kind } => {
                                // Clicked a step summary: navigate into an envoy
                                // task, otherwise toggle that step's disclosure.
                                let mi = message_idx;
                                app.focused_target = Some(kind.focus_target(mi));
                                let mut messages = runtime.messages.write().await;
                                match kind {
                                    StepKind::ToolStep => {
                                        let enter_id = resolve_focused_mut(
                                            &mut messages,
                                            &app.focus_stack,
                                            mi,
                                        )
                                        .and_then(|message| {
                                            if message.is_envoy_task() {
                                                message.tool_step_call_id().map(String::from)
                                            } else {
                                                None
                                            }
                                        });
                                        if let Some(id) = enter_id {
                                            drop(messages);
                                            app.enter_envoy(id);
                                        } else {
                                            app.toggle_step_pinned(&mut messages, mi);
                                            drop(messages);
                                        }
                                    }
                                    StepKind::Thinking => {
                                        app.toggle_step_pinned(&mut messages, mi);
                                        drop(messages);
                                    }
                                    StepKind::ProviderRetry => {
                                        app.toggle_step_pinned(&mut messages, mi);
                                        drop(messages);
                                    }
                                }
                                app.selection = SelectionState::None;
                                app.drag.cancel();
                            }
                            ClickTarget::TableCell {
                                message_idx,
                                block_idx,
                                cursor,
                                cell_text,
                                cell_segments,
                                ..
                            } => {
                                // A cell drag is clamped to `│` boundaries: the
                                // pointer may wander anywhere but the selection
                                // can never cross a `│` border into an adjacent
                                // cell.  Within the cell the user has free
                                // substring selection — no auto-full-select.
                                app.drag.begin_cell(
                                    &mut app.selection,
                                    cursor,
                                    CellDragInfo {
                                        message_idx,
                                        block_idx,
                                        cell_text,
                                        segments: cell_segments,
                                    },
                                );
                                app.focused_target = None;
                            }
                            ClickTarget::Link { url, .. } => {
                                app.selection = SelectionState::None;
                                app.focused_target = None;
                                app.drag.cancel();
                                if let Err(err) = webbrowser::open(&url) {
                                    runtime
                                        .messages
                                        .write()
                                        .await
                                        .push(TranscriptMessage::notice(
                                            NoticeSeverity::Warning,
                                            format!("Failed to open link {url}: {err}"),
                                        ));
                                }
                            }
                            ClickTarget::Content { cursor } => {
                                // A plain click does NOT select — it only arms a
                                // drag. A zero-length range is created so an
                                // immediate drag extends it normally.
                                app.drag.begin_range(&mut app.selection, cursor);
                                app.focused_target = None;
                            }
                            ClickTarget::ContentGap => {
                                // Click inside the content band but not on a
                                // region: clear any step focus and selection
                                // without starting a text selection.
                                app.selection = SelectionState::None;
                                app.focused_target = None;
                                app.drag.cancel();
                            }
                            ClickTarget::Dead => {
                                // Click outside all known areas (outer gutters,
                                // below content). Fully inert.
                                app.selection = SelectionState::None;
                                app.focused_target = None;
                                app.drag.cancel();
                            }
                        }
                    }
                }
                input::InputAction::RightClick { x, y } => {
                    // Right-click on a tool-step summary toggles its inline
                    // disclosure (same as left-click / Enter). For
                    // permission-denied steps the inline body surfaces the
                    // "Permission denied" message directly.
                    if let ClickTarget::StepSummary {
                        message_idx,
                        kind: StepKind::ToolStep,
                    } = interaction::classify_click(&app.layout_map, x, y)
                    {
                        app.focused_target = Some(InteractiveTarget::tool_step(message_idx));
                        let mut messages = runtime.messages.write().await;
                        app.toggle_step_pinned(&mut messages, message_idx);
                        drop(messages);
                    }
                    app.selection = SelectionState::None;
                    app.drag.cancel();
                }
                input::InputAction::SelectionUpdate { x, y } => {
                    app.drag
                        .update_from_point(&mut app.selection, &app.layout_map, x, y);
                }
                input::InputAction::SelectionEnd => {
                    app.drag.finish(&mut app.selection);
                }
                input::InputAction::SelectBlock { x, y } => {
                    if let Some((mi, bi)) = input::resolve_block(&app.layout_map, x, y) {
                        app.selection = SelectionState::Block {
                            message_idx: mi,
                            block_idx: bi,
                        };
                    }
                }
                input::InputAction::Hover { x, y } => {
                    // Every step summary (tool step, envoy task, reasoning
                    // trace) carries the same hover affordance. When the pointer
                    // rests on one — either the inline summary or the sticky
                    // pinned variant — record its message index so the next draw
                    // lights it up to the intermediate hover tone; otherwise
                    // clear it.
                    if app.sticky_rect.is_some_and(|r| {
                        r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height
                    }) {
                        if let Some(mi) = app.sticky_step {
                            let is_step = runtime
                                .messages
                                .read()
                                .await
                                .get(mi)
                                .map(|m| m.is_thinking() || m.is_tool_step() || m.is_envoy_task())
                                .unwrap_or(false);
                            app.hovered_step = is_step.then_some(mi);
                        }
                    } else {
                        app.hovered_step = match interaction::classify_click(&app.layout_map, x, y)
                        {
                            ClickTarget::StepSummary { message_idx, .. } => Some(message_idx),
                            _ => None,
                        };
                    }
                }
            }
        }
    }
}

pub(super) fn tool_activity_status(name: &str) -> &'static str {
    match name {
        "read_text" | "read_image" | "list_dir" | "use_skill" => "exploring",
        "grep" => "searching codebase",
        "write_file" | "edit_file" => "making edits",
        "bash" => "running command",
        name if name.starts_with("mcp__") => "using MCP",
        _ => "using tool",
    }
}

/// Snapshot the currently active provider id and model so a freshly created
/// message can be attributed to the model that produced it. The listener keeps
/// these in sync with the harness via `ProviderSwitched` and the initial
/// selection, so live messages stay traceable just like restored ones.
pub(super) async fn attribution(
    provider: &Arc<Mutex<String>>,
    model: &Arc<Mutex<String>>,
) -> (String, String) {
    (provider.lock().await.clone(), model.lock().await.clone())
}

/// Resolve a mutable reference to the message at index `mi` within the
/// currently focused view: the root conversation when the focus stack is empty,
/// or the focused envoy task's child stream otherwise. Selection and layout
/// indices are recorded against whichever slice was rendered, so mutations must
/// resolve through the same context.
pub(super) fn resolve_focused_mut<'a>(
    messages: &'a mut [TranscriptMessage],
    focus_stack: &[crate::tui::app::ZoomFrame],
    mi: usize,
) -> Option<&'a mut TranscriptMessage> {
    let Some(current) = focus_stack.last() else {
        return messages.get_mut(mi);
    };
    let task_idx = messages.iter().position(|message| {
        message.is_envoy_task() && message.tool_step_call_id() == Some(current.call_id.as_str())
    })?;
    messages[task_idx].envoy_children_mut()?.get_mut(mi)
}

/// Iterate mutable messages in the currently focused view (the root
/// conversation, or the focused envoy task's child stream) for bulk
/// expand/collapse operations. Callers filter by kind as needed.
#[cfg(test)]
pub(super) fn focused_messages_mut<'a>(
    messages: &'a mut [TranscriptMessage],
    focus_stack: &[crate::tui::app::ZoomFrame],
) -> Box<dyn Iterator<Item = &'a mut TranscriptMessage> + 'a> {
    match focus_stack.last() {
        None => Box::new(messages.iter_mut()),
        Some(current) => {
            let task_idx = messages.iter().position(|message| {
                message.is_envoy_task()
                    && message.tool_step_call_id() == Some(current.call_id.as_str())
            });
            match task_idx {
                Some(idx) => match messages[idx].envoy_children_mut() {
                    Some(children) => Box::new(children.iter_mut()),
                    None => Box::new(std::iter::empty()),
                },
                None => Box::new(std::iter::empty()),
            }
        }
    }
}

/// Extract selected text from either transcript messages or the live input box,
/// depending on which the semantic selection covers. `cell_info` supplies the
/// cell context when the selection is a `Range` bounded inside a table cell.
pub(super) fn extract_selection_text(
    sel: &SelectionState,
    messages: &[crate::tui::model::document::TranscriptMessage],
    input: &str,
    layout_map: &crate::tui::model::layout::LayoutMap,
    cell_info: Option<&CellDragInfo>,
) -> Option<String> {
    let on_input = match sel {
        SelectionState::None => false,
        SelectionState::Block { message_idx, .. } => {
            *message_idx == crate::tui::view::INPUT_MSG_IDX
        }
        SelectionState::TableCell { message_idx, .. } => {
            *message_idx == crate::tui::view::INPUT_MSG_IDX
        }
        SelectionState::Range { anchor, head } => {
            anchor.message_idx == crate::tui::view::INPUT_MSG_IDX
                && head.message_idx == crate::tui::view::INPUT_MSG_IDX
        }
    };
    if !on_input {
        return get_selected_text(
            sel,
            messages,
            &|mi, bi| layout_map.table_grid(mi, bi),
            cell_info,
        );
    }
    match sel {
        SelectionState::Block { .. } => Some(input.to_string()),
        SelectionState::Range { .. } => {
            let (start, end) = sel.active_normalized_range()?;
            let start = floor_grapheme_boundary(input, start.byte_offset);
            let end = inclusive_grapheme_end(input, end.byte_offset);
            (start < end).then(|| input[start..end].to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod selection_text_tests {
    use super::*;
    use crate::tui::model::layout::{LayoutMap, SemanticCursor};
    use crate::tui::model::selection::SelectionState;
    use crate::tui::view::INPUT_MSG_IDX;

    #[test]
    fn input_collapsed_selection_copies_nothing() {
        let cursor = SemanticCursor::new(INPUT_MSG_IDX, 0, 0);
        let sel = SelectionState::Range {
            anchor: cursor,
            head: cursor,
        };

        assert_eq!(
            extract_selection_text(&sel, &[], "中文", &LayoutMap::new(), None),
            None
        );
    }

    #[test]
    fn input_wide_glyph_drag_copies_one_grapheme() {
        let sel = SelectionState::Range {
            anchor: SemanticCursor::new(INPUT_MSG_IDX, 0, 0),
            head: SemanticCursor::new(INPUT_MSG_IDX, 0, 1),
        };

        assert_eq!(
            extract_selection_text(&sel, &[], "中文", &LayoutMap::new(), None),
            Some("中".to_string())
        );
    }
}

fn show_local_toast(
    app: &mut App,
    message: impl Into<String>,
    failed: bool,
    duration: std::time::Duration,
) {
    app.copy_toast_message = message.into();
    app.copy_toast_failed = failed;
    app.copy_toast_until = Some(std::time::Instant::now() + duration);
}

pub(super) fn display_status(
    loop_status: LoopStatus,
    activity: &str,
    awaiting_permission: bool,
) -> String {
    let activity = if awaiting_permission {
        "awaiting permission"
    } else {
        activity
    };
    match (loop_status, activity) {
        (LoopStatus::Idle, "") => "idle".to_string(),
        (LoopStatus::Idle, activity) => activity.to_string(),
        // "running" is implied by the activity bar's spinner + live status,
        // so it would be redundant noise ahead of the status. Drop
        // it and show the activity alone — but fall back to "preparing" when
        // no specific activity has landed yet (the gap between turn start
        // and the first `AgentResponse::Activity`), so the activity bar
        // always has a non-empty label to anchor the breathing dot against.
        (LoopStatus::Running, "") => "preparing".to_string(),
        (LoopStatus::Running, activity) => activity.to_string(),
    }
}

/// Execute the side effects that the pure `QuestionModel::update` described.
///
/// This is the effect interpreter — the *only* place the question modal touches
/// the agent channel, the pending-request queue, or the modal/queue sync. The
/// `Reply` effect looks up the envoy parent routing key (so an envoy's
/// answer routes back down to it), sends the reply, and removes the request
/// from the queue; `Cancelled` sends the cancellation sentinel; `Closed`
/// removes the settled request from the TUI queue. The per-frame queue sync
/// then opens the next queued question or closes the modal.
mod question_effects {
    use super::{AgentRequest, App, Modal, UiRuntime};

    pub(super) async fn apply(
        effects: &[crate::tui::question_model::QuestionEffect],
        app: &mut App,
        runtime: &UiRuntime,
    ) {
        for effect in effects {
            match effect {
                crate::tui::question_model::QuestionEffect::Reply {
                    request_id,
                    answers,
                } => {
                    let parent_call_id = runtime
                        .envoy_question_parent
                        .lock()
                        .await
                        .remove(request_id);
                    let _ = app.tx.send(AgentRequest::UserQuestionReply {
                        request_id: request_id.clone(),
                        answers: answers.clone(),
                        parent_call_id,
                    });
                }
                crate::tui::question_model::QuestionEffect::Cancelled { request_id } => {
                    let parent_call_id = runtime
                        .envoy_question_parent
                        .lock()
                        .await
                        .remove(request_id);
                    let _ = app.tx.send(AgentRequest::UserQuestionReply {
                        request_id: request_id.clone(),
                        answers: Vec::new(),
                        parent_call_id,
                    });
                }
                // Draining the queue + settling the modal is shared by both
                // Close-causing effects (Submit → Reply+Closed, Cancel → Closed).
                // The per-frame sync re-derives the model from the queue front,
                // so here we only need to drop the answered/cancelled request
                // and clear the stale modal state.
                crate::tui::question_model::QuestionEffect::Closed { request_id } => {
                    let mut queue = runtime.pending_question.lock().await;
                    queue.retain(|r| r.id != *request_id);
                    // If the queue is now empty the modal closes; the sync block
                    // will also clear `app.question`, but clearing it here keeps
                    // the very next render (same frame) consistent.
                    if queue.is_empty() {
                        app.question = None;
                        app.active_modal = Modal::None;
                        app.modal_index = 0;
                    }
                }
            }
        }
    }
}
