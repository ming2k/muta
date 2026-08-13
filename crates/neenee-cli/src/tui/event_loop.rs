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
use crate::tui::composer_attachments;
use crate::tui::input::{self};
use crate::tui::model::document::{
    MessageKind, NoticeSeverity, TranscriptMessage, UserMessageOrigin,
};
use crate::tui::model::selection::{
    CellDragInfo, SelectionState, floor_grapheme_boundary, get_selected_text,
    inclusive_grapheme_end,
};
use crate::tui::versioned::{HeightInvalidation, TranscriptPatch, TranscriptUpdate, Versioned};
use crate::tui::view;
use crate::tui::{App, CaretOwner, Modal, ProviderDeleteChoice};

use tokio::sync::Mutex;

mod actions;
mod render;

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
    /// [`neenee_core::AgentResponse::ParentStatus`] and read into [`App::parent_status`]
    /// for the side banner (ADR-0017).
    pub parent_status: Arc<Mutex<ParentStatus>>,
    /// One-shot side-view transition (ADR-0017): `Opened` when the harness
    /// emits [`neenee_core::AgentResponse::SideViewOpened`] (the loop calls
    /// [`App::enter_side_view`]), `Closed` on [`neenee_core::AgentResponse::SideViewClosed`]
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
    /// [`neenee_core::AgentResponse::SessionDetail`] and read into [`App::session_detail`]
    /// for the session-info sub-view.
    pub session_detail: Arc<Mutex<Option<neenee_core::SessionDetail>>>,
    /// Latest token-source report fetched from the harness for the viewed
    /// session (attach mode: the ledger is daemon-side). Written by the
    /// listener from [`neenee_core::AgentResponse::TokenUsageReport`] and read into
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
    /// A mid-round steer (`InsertUserInput`, `F4`) was admitted at a safe
    /// turn boundary; the transcript listener already appended the visible
    /// user message. The loop drops the shadow outbox item.
    Inserted {
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

/// The active model's effective reasoning effort, resolved the same way the
/// hint bar resolves it (per-protocol gating included — ADR-0046): Anthropic
/// effort counts only while thinking is opted in, OpenAI effort whenever the
/// channel reports one, Google never. Shared by the hint-bar render and the
/// effort-ignition triggers so both agree on whether `max` is live.
fn effective_reasoning_effort(app: &App) -> Option<&str> {
    app.provider_picker
        .rows
        .iter()
        .find(|row| row.id == app.current_provider)
        .and_then(|row| row.model_info.iter().find(|m| m.model == app.current_model))
        .and_then(|m| {
            let show = match m.protocol.as_str() {
                "anthropic" => m.thinking == Some(true),
                "openai" => m.effort.is_some(),
                _ => false,
            };
            if show { m.effort.as_deref() } else { None }
        })
}

/// Fire the effort-ignition celebration when the effective effort just
/// reached the model's top tier (`max`) — codex's Ultra-ignition port, timed
/// against a wall-clock epoch so the wave cadence is cadence-stable. A
/// no-op while an ignition is already running, so a redundant switch can't
/// restart a wave mid-flight.
fn arm_effort_ignition_if_max(app: &mut App) {
    if effective_reasoning_effort(app) == Some("max") && app.effort_ignition_epoch.is_none() {
        app.effort_ignition_epoch = Some(std::time::Instant::now());
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
        arm_effort_ignition_if_max(app);
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

/// Loop stage: mirror every listener-owned runtime field into `App` for this
/// frame (provider/model, harness snapshot, permission/question/input queues,
/// picker + monitor snapshots, OAuth add signals). Extracted verbatim from the
/// top of `run_app_loop`'s iteration; every lock guard is a statement-level
/// temporary, so acquisition order and drop timing are unchanged from the
/// inline block.
async fn sync_runtime_state(
    app: &mut App,
    runtime: &UiRuntime,
    sessions_overview_rev_seen: &mut u64,
    host_sessions_rev_seen: &mut u64,
) {
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
        if rev != *sessions_overview_rev_seen {
            app.sessions_overview = runtime.sessions_overview.lock().await.clone();
            *sessions_overview_rev_seen = rev;
        }
    }
    if runtime.open_sessions.swap(false, Ordering::SeqCst) && app.active_modal != Modal::Permission
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
        if rev != *host_sessions_rev_seen {
            app.host_sessions = runtime.host_sessions.lock().await.clone();
            *host_sessions_rev_seen = rev;
        }
    }
    if runtime.open_host.swap(false, Ordering::SeqCst) && app.active_modal != Modal::Permission {
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

/// Loop stage: per-frame timer bookkeeping — expire the copy and notice
/// toasts, drain a forwarded toast-surfaced notice, refresh the attached-
/// images reminder, and tick the Esc-interrupt armed window. Extracted
/// verbatim from `run_app_loop`.
async fn tick_toast_timers(app: &mut App, runtime: &UiRuntime) {
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
}

/// Loop stage: mirror the versioned transcript buffers (primary + `/btw`
/// side), drain the side-view transition signal, resolve the viewed session
/// id, keep the origin stampers in sync with it, and mirror the per-session
/// context/throughput snapshots. Returns whether the displayed transcript
/// changed (drives bottom-follow staging) and the viewed session id.
/// Extracted verbatim from `run_app_loop`; all lock/read guards stay
/// statement-level temporaries, as in the inline block.
async fn sync_transcripts_and_session(
    app: &mut App,
    runtime: &UiRuntime,
    session: &crate::tui::SessionSource,
) -> (bool, String) {
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
    (displayed_transcript_changed, viewed_session_id)
}

/// Loop stage: apply the protocol acknowledgements the response listener
/// forwarded (outbox dispatch state transitions). Extracted verbatim from
/// `run_app_loop`; the queue lock guard stays a per-pop temporary.
async fn drain_outbox_signals(app: &mut App, runtime: &UiRuntime) {
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
            } => {
                // The round closed before this insert could be admitted:
                // the item returns to `Waiting` as a paused next-round
                // entry (also the `ChatToSession` failure path).
                app.requeue_dispatch(&session_id, &input_id);
            }
            OutboxSignal::Inserted {
                session_id,
                input_id,
            } => {
                // The steer crossed a safe turn boundary and the listener
                // already committed it to the transcript — drop the shadow
                // outbox item.
                app.remove_dispatch(&session_id, &input_id);
            }
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
}

/// Loop stage: drain a pending Phase-1 unsend — restore the interrupted
/// prompt + images into the composer as the new draft. Extracted verbatim
/// from `run_app_loop`.
async fn drain_unsent_input(app: &mut App, runtime: &UiRuntime) {
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
}

/// Loop stage: auto-run the next-round dispatch for a session that both
/// completed naturally and is idle (and is not user-blocked). Extracted
/// verbatim from `run_app_loop`.
fn auto_dispatch_ready_round(app: &mut App) {
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
        let expanded_text =
            composer_attachments::strip_orphan_image_chips(&expanded_text, dispatch.images.len());
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
}

/// Loop stage: cursor ownership & IME anchor — sync the terminal cursor
/// (position immediately after caret-moving input, visibility only on a
/// state transition) from `App::caret_owner` / `App::caret_visible`.
/// Extracted verbatim from `run_app_loop`.
fn sync_caret_and_cursor(app: &mut App, terminal: &mut Terminal<std::io::Stdout>) {
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
        sync_runtime_state(
            app,
            &runtime,
            &mut sessions_overview_rev_seen,
            &mut host_sessions_rev_seen,
        )
        .await;

        tick_toast_timers(app, &runtime).await;

        let (displayed_transcript_changed, viewed_session_id) =
            sync_transcripts_and_session(app, &runtime, &session).await;

        // Apply protocol acknowledgements before handling the next key. The
        // transcript listener has already committed admitted/started messages;
        // this side owns only compact outbox and composer state.
        drain_outbox_signals(app, &runtime).await;

        drain_unsent_input(app, &runtime).await;

        auto_dispatch_ready_round(app);

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
        // Drop a finished ignition before the animating check so the final
        // frame (wave fully faded) is the last one the epoch drives.
        if let Some(epoch) = app.effort_ignition_epoch
            && crate::tui::effort_ignition::ignition_finished(epoch.elapsed().as_millis())
        {
            app.effort_ignition_epoch = None;
        }
        let animating = runtime.is_responding.load(Ordering::SeqCst)
            || app.round_started_at.is_some()
            || app.copy_toast_until.is_some()
            || app.notice_toast_until.is_some()
            || app.ctrl_c_armed()
            || app.esc_armed_ticks > 0
            || !app.pending_images.is_empty()
            || app.effort_ignition_epoch.is_some()
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
        sync_caret_and_cursor(app, terminal);

        // A mutation of the transcript currently on screen (or a transition to
        // a different transcript view) can change the measured bottom after
        // layout. While following that bottom, stage the measurement frame in
        // the retained grid without flushing it; the immediate next pass paints
        // at the final scroll offset and is the only frame the terminal sees.
        let stage_bottom_follow = displayed_transcript_changed && app.follow_bottom;

        // Draw frame (skipped when nothing changed — see `needs_draw`).
        if needs_draw {
            if stage_bottom_follow {
                terminal.stage(|f| render::render_frame(app, f, &viewed_session_id))?;
            } else {
                terminal.draw(|f| render::render_frame(app, f, &viewed_session_id))?;
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
            // returned action flows through the normal action dispatch below
            // (`DeleteProviderConfirm` / `DeleteProviderCancel` are the
            // overlay-specific arms).
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

            match actions::dispatch_action(
                app,
                &runtime,
                terminal,
                &session,
                action,
                &viewed_session_id,
                &copy_tx,
                &copy_pending,
                &paste_tx,
                &mut sgr_guard,
            )
            .await
            {
                actions::ActionFlow::Handled => {}
                actions::ActionFlow::NextEvent => continue,
                actions::ActionFlow::Exit => return Ok(()),
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
