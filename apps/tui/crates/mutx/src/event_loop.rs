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
use mutx_engine::Terminal;
use tokio::sync::mpsc;

use muta_contracts::{
    AgentRequest, HarnessSnapshot, LoopStatus, ParentStatus, PermissionDecision, PermissionRequest,
    ProviderPickerSnapshot, SessionOverview, TodoList, UserQuestionRequest,
};

use crate::clipboard;
use crate::clipboard_ops;
use crate::composer_attachments;
use crate::input::{self};
use crate::model::document::{MessageKind, NoticeSeverity, TranscriptMessage, UserMessageOrigin};
use crate::model::selection::{
    CellDragInfo, SelectionState, floor_grapheme_boundary, get_selected_text,
    inclusive_grapheme_end,
};
use crate::versioned::{HeightInvalidation, TranscriptPatch, TranscriptUpdate, Versioned};
use crate::view;
use crate::{App, Modal, ProviderDeleteChoice, SelectionEdge};

use tokio::sync::Mutex;

mod actions;
mod render;

#[cfg(test)]
pub(crate) use actions::handle_esc_interrupt;
/// Test-only bridge: the behavior-lock tests in `crate::tests` drive the
/// insert staging directly (ADR-0126). The production path dispatches it
/// inside `actions::process`; this re-export never leaves the test profile.
#[cfg(test)]
pub(crate) use actions::handle_insert_into_round;
#[cfg(test)]
pub(crate) use actions::handle_send_slash;
/// Same bridge for the dashboard console dispatcher: the tests in
/// `crate::tests` drive the grammar/kill-confirm logic directly, without
/// the terminal or clipboard plumbing of the full dispatch path.
#[cfg(test)]
pub(crate) use actions::host_test_shims;
/// Same bridge for the dashboard-exit behavior locks: the tests drive the
/// CtrlC / CloseModal arms directly (they are plain functions, but only the
/// event loop can reach them otherwise).
#[cfg(test)]
pub(crate) use actions::{handle_close_modal, handle_ctrl_c};

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

#[derive(Debug)]
pub(super) struct CompletionSignal {
    pub request_id: u64,
    pub input: String,
    pub cursor: usize,
    pub items: Vec<muta_contracts::InputCompletion>,
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
mod input_selection_relay_tests {
    //! The mouse half of the caret relay: `handle_selection_end` parks the
    //! hidden caret at the drag's release point, and an InputBox click breaks
    //! a whole-input selection at the click point. These run here (not in
    //! `crate::tests`) because the mouse handlers are `pub(super)` to the
    //! event loop.
    use super::*;
    use crate::model::selection::SelectionState;
    use crate::view::INPUT_MSG_IDX;

    fn app_with_input(input: &str) -> App {
        let mut app = crate::tests::new_app_for_relay_tests();
        app.input = input.to_string();
        app
    }

    #[test]
    fn drag_end_parks_caret_at_release_point() {
        let mut app = app_with_input("hello world");
        // Simulate a completed drag whose head (release point) is byte 5,
        // between "hello" and " world".
        app.selection = SelectionState::Range {
            anchor: crate::model::layout::SemanticCursor::new(INPUT_MSG_IDX, 0, 0),
            head: crate::model::layout::SemanticCursor::new(INPUT_MSG_IDX, 0, 5),
        };
        app.cursor_position = 0; // stale pre-drag caret
        super::actions::handle_selection_end_for_test(&mut app);
        assert_eq!(app.cursor_position, 5, "caret parks at the head byte");
        assert!(
            app.cursor_sync_pending,
            "the parked caret must arm the immediate sync"
        );
    }

    #[test]
    fn drag_end_snaps_to_grapheme_boundary() {
        // A head landing mid-CJK-glyph (byte 1 of 中) must snap to a
        // boundary so the char-indexed caret stays sliceable.
        let mut app = app_with_input("中文");
        app.selection = SelectionState::Range {
            anchor: crate::model::layout::SemanticCursor::new(INPUT_MSG_IDX, 0, 0),
            head: crate::model::layout::SemanticCursor::new(INPUT_MSG_IDX, 0, 1),
        };
        super::actions::handle_selection_end_for_test(&mut app);
        assert_eq!(app.cursor_position, 0, "byte 1 floors to the cluster start");
    }

    #[test]
    fn drag_end_ignores_transcript_selections() {
        let mut app = app_with_input("hello");
        app.selection = SelectionState::Range {
            anchor: crate::model::layout::SemanticCursor::new(0, 0, 0),
            head: crate::model::layout::SemanticCursor::new(0, 0, 3),
        };
        app.cursor_position = 2;
        super::actions::handle_selection_end_for_test(&mut app);
        assert_eq!(
            app.cursor_position, 2,
            "a transcript drag must not move the input caret"
        );
    }

    #[test]
    fn whole_block_select_parks_caret_at_end() {
        let mut app = app_with_input("abc");
        app.selection = SelectionState::Block {
            message_idx: INPUT_MSG_IDX,
            block_idx: 0,
        };
        super::actions::handle_selection_end_for_test(&mut app);
        assert_eq!(app.cursor_position, 3);
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
            TranscriptUpdate::EnvoyEvent {
                parent_call_id,
                event,
            } => messages
                .iter_mut()
                .find(|message| message.tool_step_call_id() == Some(parent_call_id.as_str()))
                .is_some_and(|message| message.push_envoy_event(&event)),
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
                // Append is only safe when the app-side tail is the exact
                // tail the append was computed against; any divergence (a
                // missed replace, a popped tail, a session switch) falls
                // back to the snapshot instead of building a fork.
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

pub(super) struct UiRuntime {
    pub current_provider: Arc<Mutex<String>>,
    pub current_model: Arc<Mutex<String>>,
    /// Latest AI-visible context size for the primary session.
    pub context_tokens: Arc<Mutex<HashMap<String, muta_contracts::ContextTokenSnapshot>>>,
    pub harness: Arc<Mutex<HarnessSnapshot>>,
    pub activity_status: Arc<Mutex<String>>,
    pub provider_retry: Arc<Mutex<Option<crate::app::ProviderRetryState>>>,
    pub pending_permission: Arc<Mutex<VecDeque<PermissionRequest>>>,
    pub pending_question: Arc<Mutex<VecDeque<UserQuestionRequest>>>,
    pub pending_input: Arc<Mutex<VecDeque<muta_contracts::InputRequest>>>,
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
    /// Latest daemon-produced composer completion response.
    pub completion_signal: Arc<Mutex<Option<CompletionSignal>>>,
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
    /// [`muta_contracts::AgentResponse::ParentStatus`] and read into [`App::parent_status`]
    /// for the side banner (ADR-0017).
    pub parent_status: Arc<Mutex<ParentStatus>>,
    /// One-shot side-view transition (ADR-0017): `Opened` when the harness
    /// emits [`muta_contracts::AgentResponse::SideViewOpened`] (the loop calls
    /// [`App::enter_side_view`]), `Closed` on [`muta_contracts::AgentResponse::SideViewClosed`]
    /// ([`App::exit_side_view`]). Drained each frame.
    pub side_view_signal: Arc<Mutex<Option<SideViewSignal>>>,
    /// `/btw` asides list (ADR-0103), written by the listener from
    /// [`muta_contracts::AgentResponse::BtwList`] and mirrored into
    /// [`App::btw_list`] for the asides modal and the main header count.
    pub btw_list: Arc<Mutex<Vec<muta_contracts::BtwAsideSummary>>>,
    /// Per-session chrome (view-scoped state): activity / responding /
    /// round / turn for the primary **and** every live aside, maintained by
    /// the response listener and mirrored into [`App::session_chrome`]
    /// each frame. A view renders only its own session's entry
    /// ([`App::viewed_chrome`]) so an aside view never shows the primary's
    /// activity bar.
    pub session_chrome:
        Arc<std::sync::Mutex<std::collections::HashMap<String, crate::app::SessionChrome>>>,
    /// Console receipts from the dashboard's dispatched control verbs
    /// (ADR-0097 §3): spawned one-shot control tasks push the daemon's
    /// answer here; the loop drains it into [`App::host_console_log`]
    /// each frame. A queue (not a slot) so concurrent fan-out dispatches
    /// (`@2 @3 …`) each land their receipt.
    pub host_console_signal: Arc<Mutex<VecDeque<crate::overlays::ConsoleLine>>>,
    /// Which session the frontend is currently viewing (primary id, or the
    /// focused aside's id), written by the loop's sync stage each frame and
    /// read by the listener to scope on-demand query replies
    /// (`TokenUsageReport`) so a reply that raced a view switch is dropped.
    pub viewed_session_id: Arc<Mutex<Option<String>>>,
    /// The **live primary session id**, updated by the response listener on
    /// every session switch (`ConversationCleared` for `/new`,
    /// `ConversationReplaced` for `/session open`, `/resume`, `/fork`).
    ///
    /// Distinct from [`Self::viewed_session_id`], which the *loop* writes for
    /// the listener: this one flows the other way, because the handshake-time
    /// [`crate::SessionSource`] is frozen for the process lifetime and goes
    /// stale the moment the harness repoints the shared store. Everything
    /// session-scoped that must follow a mid-run switch reads this instead —
    /// most importantly the origin tag the inline ↑/↓ prompt recall filters
    /// `input_history` by (ADR-0018 origin tracking), which would otherwise
    /// keep stamping (and recalling) the retired session's id after `/new` or
    /// `/session open`.
    pub live_session_id: Arc<Mutex<String>>,
    pub key_status: Arc<Mutex<HashMap<String, bool>>>,
    /// Effective `[websearch]` config view (presence-only), kept current by
    /// the response listener; mirrored into [`App::websearch_config`] each
    /// frame for the Settings view's Web Search pane.
    pub websearch_config: Arc<Mutex<Option<muta_contracts::WebSearchConfigView>>>,
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
    /// [`muta_contracts::AgentResponse::SessionDetail`] and read into [`App::session_detail`]
    /// for the session-info sub-view.
    pub session_detail: Arc<Mutex<Option<muta_contracts::SessionDetail>>>,
    /// Latest session DAG fetched on demand for the Tree view.
    pub session_tree: Arc<Mutex<Option<muta_contracts::SessionTree>>>,
    /// Latest token-source report fetched from the harness for the viewed
    /// session (attach mode: the ledger is daemon-side). Written by the
    /// listener from [`muta_contracts::AgentResponse::TokenUsageReport`] and read into
    /// [`App::token_report`]. In the standalone path the local ledger
    /// ([`App::token_ledger`]) is the source instead and this stays `None`.
    pub token_report: Arc<Mutex<Option<muta_contracts::TokenSourceReport>>>,
    /// Cross-session usage-statistics report (ADR-0122), written by the
    /// listener from [`muta_contracts::AgentResponse::UsageStatsReport`]
    /// and read into [`App::usage_stats`] for the `/usage` overlay.
    /// Session-independent by design — the durable store it aggregates
    /// survives session cleanup.
    pub usage_stats: Arc<Mutex<Option<muta_contracts::usage_stats::UsageStatsReport>>>,
    pub open_sessions: Arc<AtomicBool>,
    /// Presentation signal for the backend-owned `/tree` command.
    pub open_tree: Arc<AtomicBool>,
    /// Live daemon monitor snapshot for the `/host` control panel
    /// (ADR-0096), maintained by a dedicated monitor client task.
    pub host_sessions: Arc<Mutex<Vec<muta_contracts::MonitoredSession>>>,
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
    pub session_context: Arc<Mutex<Option<muta_contracts::SessionContextSnapshot>>>,
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

impl UiRuntime {
    /// Minimal runtime for unit tests: every cell starts empty/idle so a
    /// test can drive one action and assert on exactly the cells it cares
    /// about (e.g. that a slash dispatch leaves `activity_status` and
    /// `is_responding` untouched — ADR-0110).
    #[cfg(test)]
    pub(super) fn minimal_for_test() -> Self {
        Self {
            current_provider: Arc::new(Mutex::new(String::new())),
            current_model: Arc::new(Mutex::new(String::new())),
            context_tokens: Arc::new(Mutex::new(HashMap::new())),
            harness: Arc::new(Mutex::new(HarnessSnapshot {
                loop_status: LoopStatus::Idle,
                round_counter: 0,
                autopilot: false,
                retry_pending: false,
            })),
            activity_status: Arc::new(Mutex::new(String::new())),
            provider_retry: Arc::new(Mutex::new(None)),
            pending_permission: Arc::new(Mutex::new(VecDeque::new())),
            pending_question: Arc::new(Mutex::new(VecDeque::new())),
            pending_input: Arc::new(Mutex::new(VecDeque::new())),
            is_responding: Arc::new(AtomicBool::new(false)),
            dirty: Arc::new(AtomicBool::new(false)),
            dirty_notify: Arc::new(tokio::sync::Notify::new()),
            completion_signal: Arc::new(Mutex::new(None)),
            envoy_permission_parent: Arc::new(Mutex::new(HashMap::new())),
            envoy_question_parent: Arc::new(Mutex::new(HashMap::new())),
            messages: Arc::new(Versioned::new(Vec::new())),
            side_messages: Arc::new(Versioned::new(Vec::new())),
            parent_status: Arc::new(Mutex::new(ParentStatus::Idle)),
            side_view_signal: Arc::new(Mutex::new(None)),
            btw_list: Arc::new(Mutex::new(Vec::new())),
            session_chrome: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            host_console_signal: Arc::new(Mutex::new(VecDeque::new())),
            viewed_session_id: Arc::new(Mutex::new(None)),
            live_session_id: Arc::new(Mutex::new(String::new())),
            key_status: Arc::new(Mutex::new(HashMap::new())),
            websearch_config: Arc::new(Mutex::new(None)),
            provider_picker: Arc::new(Mutex::new(Default::default())),
            sessions_overview: Arc::new(Mutex::new(Vec::new())),
            sessions_overview_rev: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            session_detail: Arc::new(Mutex::new(None)),
            session_tree: Arc::new(Mutex::new(None)),
            token_report: Arc::new(Mutex::new(None)),
            usage_stats: Arc::new(Mutex::new(None)),
            open_sessions: Arc::new(AtomicBool::new(false)),
            open_tree: Arc::new(AtomicBool::new(false)),
            host_sessions: Arc::new(Mutex::new(Vec::new())),
            host_sessions_rev: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            open_host: Arc::new(AtomicBool::new(false)),
            oauth_add_signal: Arc::new(Mutex::new(None)),
            awaiting_oauth_add: Arc::new(AtomicBool::new(false)),
            session_context: Arc::new(Mutex::new(None)),
            todos: Arc::new(Mutex::new(None)),
            round_count: Arc::new(Mutex::new(0)),
            current_turn: Arc::new(Mutex::new(0)),
            round_started_at: Arc::new(Mutex::new(None)),
            unsent_input_signal: Arc::new(Mutex::new(None)),
            notice_toast_signal: Arc::new(Mutex::new(None)),
            outbox_signals: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
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
    /// A mid-round steer (`InsertUserInput`, `Ctrl+O`) was admitted at a safe
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
    pub images: Vec<muta_contracts::ImagePart>,
}

/// Probe a raw input event against an active **whole-input selection**.
///
/// While the composer's text is selected and the composer owns the caret,
/// the terminal block cursor is hidden (see [`App::caret_visible`]) — but its
/// remembered position is the selection's head, the point where the mouse
/// button was released. Every direction key must *relay* from that hidden
/// position instead of the stale visible `cursor_position`, and the keypress
/// must break the selection:
///
/// - `←`/`→` adopt the caret at the head edge, then step one character
///   (or one word, with Ctrl/Alt) in the pressed direction — so the first
///   press lands one past the release point, exactly like a desktop editor.
/// - `↑`/`↓` adopt the head edge only (the caret is restored where the mouse
///   released; the press itself keeps its normal meaning from there — the
///   event loop dispatches `None` and the next press behaves ordinarily).
/// - `Home`/`End` adopt the tail/head edge respectively.
/// - `Backspace`/`Delete`/`Ctrl+W`/`Alt+D`/… delete the whole selection in
///   one keystroke.
///
/// Returns `None` when no whole-input selection is active (the caller falls
/// through to ordinary input handling) or the event is not a key / has
/// modifiers beyond the relay's scope. The returned action — if any — flows
/// through the standard `match action` dispatch so its post-edit passes
/// (focus reclaim, attachment reconcile) still run.
pub(crate) fn probe_input_selection_relay(
    app: &mut App,
    event: &Event,
) -> Option<input::InputAction> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

    if !app.input_selection_relays_arrows() {
        return None;
    }
    let Event::Key(key) = event else {
        return None;
    };
    if !matches!(key.kind, KeyEventKind::Press) {
        return None;
    }
    // Only the plain relay family: no Shift (shift+arrows would mean
    // "extend selection", which the TUI does not offer), and of the
    // control-family only the delete/word-jump chords that edit or move by
    // word — everything else (Ctrl+T, Ctrl+M, …) keeps its global meaning
    // and must NOT be swallowed by the selection.
    let word_chord = key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);

    // Break the selection at the head (mouse-release) edge, then step the
    // caret one char or one whitespace-delimited word in the pressed
    // direction — so the first press lands one past the release point,
    // matching every desktop editor.
    let step_from_head = |app: &mut App, forward: bool, word: bool| {
        app.adopt_caret_from_input_selection(SelectionEdge::Head);
        let count = app.input.chars().count();
        let at = app.cursor_position.min(count);
        let target = if word {
            let chars: Vec<char> = app.input.chars().collect();
            let mut i = at;
            if forward {
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
            } else {
                while i > 0 && chars[i - 1].is_whitespace() {
                    i -= 1;
                }
                while i > 0 && !chars[i - 1].is_whitespace() {
                    i -= 1;
                }
            }
            i
        } else if forward {
            (at + 1).min(count)
        } else {
            at.saturating_sub(1)
        };
        app.set_cursor(target);
    };

    match (key.code, word_chord) {
        (KeyCode::Left, false) => {
            step_from_head(app, false, false);
            Some(input::InputAction::None)
        }
        (KeyCode::Right, false) => {
            step_from_head(app, true, false);
            Some(input::InputAction::None)
        }
        (KeyCode::Left, true) => {
            step_from_head(app, false, true);
            Some(input::InputAction::None)
        }
        (KeyCode::Right, true) => {
            step_from_head(app, true, true);
            Some(input::InputAction::None)
        }
        (KeyCode::Up | KeyCode::Down, _) => {
            // Vertical motion restores the hidden caret at the mouse-release
            // point and consumes the press; multi-line column-walking and
            // history recall resume from there on the next press.
            app.adopt_caret_from_input_selection(SelectionEdge::Head);
            Some(input::InputAction::None)
        }
        (KeyCode::Home, _) => {
            if let Some((start, _)) = app.selection.active_normalized_range() {
                let byte =
                    floor_grapheme_boundary(&app.input, start.byte_offset).min(app.input.len());
                let pos = app.input[..byte].chars().count();
                app.selection = SelectionState::None;
                app.drag.cancel();
                app.set_cursor(pos);
            } else {
                app.adopt_caret_from_input_selection(SelectionEdge::Tail);
            }
            Some(input::InputAction::None)
        }
        (KeyCode::End, _) => {
            if let Some((_, end)) = app.selection.active_normalized_range() {
                let byte = inclusive_grapheme_end(&app.input, end.byte_offset).min(app.input.len());
                let pos = app.input[..byte].chars().count();
                app.selection = SelectionState::None;
                app.drag.cancel();
                app.set_cursor(pos);
            } else {
                app.adopt_caret_from_input_selection(SelectionEdge::Head);
            }
            Some(input::InputAction::None)
        }
        (KeyCode::Backspace | KeyCode::Delete, _) => {
            // Plain delete over an active selection replaces it (the
            // standard editor contract): the whole selected text goes in
            // one stroke. Returns Backspace's post-edit signal so focus
            // reclaim and attachment reconcile run in the dispatch below.
            app.delete_input_selection();
            Some(input::InputAction::Backspace)
        }
        // The delete-family chords (Ctrl+W / Ctrl+U / Ctrl+K / Alt+D) behave
        // the same over a selection: they replace it rather than deleting
        // from the stale caret.
        (KeyCode::Char('w') | KeyCode::Char('u') | KeyCode::Char('k'), _)
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            app.delete_input_selection();
            Some(input::InputAction::Backspace)
        }
        (KeyCode::Char('d'), _) if key.modifiers.contains(KeyModifiers::ALT) => {
            app.delete_input_selection();
            Some(input::InputAction::Backspace)
        }
        _ => None,
    }
}

fn probe_delete_overlay(app: &mut App, event: &Event) -> Option<input::InputAction> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

    if app.pending_provider_delete.is_none() || app.active_modal() != Modal::Connections {
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
/// effort counts only while thinking is opted in, all other protocols (OpenAI,
/// Google, etc.) whenever the channel reports an effort level. Shared by the
/// hint-bar render and the effort-ignition triggers so both agree on whether
/// `max` is live.
fn effective_reasoning_effort(app: &App) -> Option<&str> {
    app.provider_picker
        .rows
        .iter()
        .find(|row| row.id == app.current_provider)
        .and_then(|row| row.model_info.iter().find(|m| m.model == app.current_model))
        .and_then(|m| {
            let show = match m.protocol.as_str() {
                "anthropic" => m.thinking == Some(true),
                _ => m.effort.is_some(),
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
        app.dismiss_surface();
    } else if app.provider_row_auth(&id).is_oauth() {
        let auth = app.provider_row_auth(&id);
        let _ = app.tx.send(AgentRequest::ConnectProvider {
            id,
            method: auth
                .default_login_method()
                .unwrap_or(muta_contracts::LoginMethod::Device),
        });
        app.dismiss_surface();
    } else {
        // No key configured: open the key editor prefilled with this model so
        // the user can enter a key before activating. Esc returns to the
        // picker the editor was opened from (phase 3: the nav stack).
        app.push_transient_surface(Modal::ModelEditor);
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
            app.pop_transient_surface();
        } else {
            // Drop the request we just answered and surface the next one (if
            // any) so the sheet hands off without flashing the composer for a
            // frame.
            let mut queue = runtime.pending_permission.lock().await;
            queue.retain(|r| r.id != request_id);
            app.pending_permission = queue.front().cloned();
            drop(queue);
            if app.pending_permission.is_none() {
                app.pop_transient_surface();
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
            .name("mutx-engine-input".into())
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
    app.harness_retry_pending = harness.retry_pending;
    app.autopilot = harness.autopilot;
    app.activity_status = runtime.activity_status.lock().await.clone();
    app.provider_retry = runtime.provider_retry.lock().await.clone();
    app.session_context = runtime.session_context.lock().await.clone();
    app.todos = runtime.todos.lock().await.clone();
    app.round_count = *runtime.round_count.lock().await;
    app.current_turn = *runtime.current_turn.lock().await;
    app.round_started_at = *runtime.round_started_at.lock().await;
    app.pending_permission = runtime.pending_permission.lock().await.front().cloned();
    app.key_status = runtime.key_status.lock().await.clone();
    app.websearch_config = runtime.websearch_config.lock().await.clone();
    app.provider_picker = runtime.provider_picker.lock().await.clone();
    let request_sheet_open = |modal: Modal| {
        matches!(
            modal,
            Modal::Permission | Modal::Question | Modal::InputInjection
        )
    };
    if app.pending_permission.is_some() && !request_sheet_open(app.active_modal()) {
        app.push_transient_surface(Modal::Permission);
        app.modal_index = 0;
        app.permission_scroll = 0;
        app.permission_show_details = false;
        // A permission prompt is urgent: clear any focused transcript
        // step so the next keypress decides the sheet, not the step.
        app.focused_target = None;
    } else if app.pending_permission.is_none() && app.active_modal() == Modal::Permission {
        app.pop_transient_surface();
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
                app.question = Some(crate::question_model::QuestionModel::open(req));
                app.question_scroll = 0;
                app.question_modal_follow = true;
                app.modal_index = 0;
                app.focused_target = None;
            } else {
                app.question = None;
                if app.active_modal() == Modal::Question {
                    app.pop_transient_surface();
                    app.modal_index = 0;
                }
            }
        }
        if app.question.is_some() && !request_sheet_open(app.active_modal()) {
            app.push_transient_surface(Modal::Question);
            app.modal_index = 0;
            app.focused_target = None;
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
                app.pending_input = Some(req);
                app.modal_index = 0;
                app.focused_target = None;
            } else {
                app.pending_input = None;
                if app.active_modal() == Modal::InputInjection {
                    app.restore_input_draft();
                    app.pop_transient_surface();
                    app.modal_index = 0;
                }
            }
        }
        if app.pending_input.is_some() && !request_sheet_open(app.active_modal()) {
            // Park the foreground's input before the injection sheet borrows
            // the composer; the pop path restores it, then returns to the
            // exact parent surface.
            app.park_input_draft();
            app.push_transient_surface(Modal::InputInjection);
            app.modal_index = 0;
            app.focused_target = None;
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
    let can_apply_backend_navigation = app.can_accept_navigation_signal();
    let view_session_id = app.current_session_id.clone();
    if can_apply_backend_navigation && runtime.open_sessions.swap(false, Ordering::SeqCst) {
        // A retained view (ADR-0139): first show initializes; when
        // the picker is already up this signal is just a data refresh —
        // `open_view` is a same-view re-focus that does not reset, so the
        // cursor never snaps back on a delete (the refresh-while-open
        // regression this branch used to guard with an `opening` flag).
        actions::enter_view(
            app,
            crate::views::ViewId::Sessions,
            runtime,
            &view_session_id,
        );
    }
    if let Some(tree) = runtime.session_tree.lock().await.take() {
        app.session_tree = tree;
    }
    if can_apply_backend_navigation && runtime.open_tree.swap(false, Ordering::SeqCst) {
        actions::enter_view(app, crate::views::ViewId::Tree, runtime, &view_session_id);
    }
    // Mirror the daemon monitor snapshot for the `/host` panel.
    {
        let rev = runtime.host_sessions_rev.load(Ordering::Acquire);
        if rev != *host_sessions_rev_seen {
            app.host_sessions = runtime.host_sessions.lock().await.clone();
            *host_sessions_rev_seen = rev;
        }
    }
    // Drain the dashboard console's receipts (ADR-0097 §3): spawned control
    // tasks push the daemon's answers here; each lands in the cockpit log
    // and wakes a redraw (the drain itself only needs to move the lines).
    {
        let mut queue = runtime.host_console_signal.lock().await;
        while let Some(line) = queue.pop_front() {
            app.host_console_log.push(line);
        }
    }
    if can_apply_backend_navigation && runtime.open_host.swap(false, Ordering::SeqCst) {
        // A retained view (ADR-0139): the dock selection, focus
        // pane, and detail scroll survive hide. First open runs the
        // entry-state ritual once; the cockpit log now lives for the
        // *view's* lifetime (cleared on first open, retained across
        // hide) — it is a session at the controls, not history.
        actions::enter_view(app, crate::views::ViewId::Host, runtime, &view_session_id);
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
    // Mirror the on-demand cross-session usage statistics (ADR-0122).
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
    // The Esc armed toast only makes sense while the viewed session's round
    // is still running; once it finishes there is nothing left to interrupt,
    // so let it expire immediately rather than mislead. View-scoped via
    // `App::tick_esc_arm` (never the runtime's primary-only
    // `is_responding` flag): an aside view armed from its own running round
    // must survive the primary being idle, and vice versa.
    app.tick_esc_arm();
}

/// Extract the transcript suffix's user rows as `(text, is_chat_prompt,
/// sent_at_ms)` triples for [`App::backfill_session_history`]. Assistant and
/// tool rows are dropped here — only user rows can be prompts — and `is_chat`
/// distinguishes genuine prompts from slash / shell / insert gestures.
fn user_prompt_tail(messages: &[TranscriptMessage]) -> Vec<(String, bool, u64)> {
    messages
        .iter()
        .filter(|m| m.role == muta_contracts::Role::User)
        .map(|m| {
            (
                m.raw.clone(),
                m.origin == UserMessageOrigin::Chat,
                m.sent_at_ms.unwrap_or(0),
            )
        })
        .collect()
}

/// Loop stage: mirror the versioned transcript buffers (primary + `/btw`
/// side), drain the side-view transition signal, resolve the viewed session
/// id, keep the origin stampers in sync with it, and mirror the per-session
/// context/throughput snapshots. Returns whether the displayed transcript
/// changed (drives bottom-follow staging) and the viewed session id.
/// Extracted verbatim from `run_app_loop`; all lock/read guards stay
/// statement-level temporaries, as in the inline block.
async fn sync_transcripts_and_session(app: &mut App, runtime: &UiRuntime) -> (bool, String) {
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
    // Mirror the `/btw` asides list (ADR-0103 §5) into the app each frame.
    // Cheap: it is a small Vec replaced only when the registry changed.
    app.btw_list = runtime.btw_list.lock().await.clone();
    // Mirror the per-session chrome map (view-scoped state): the listener
    // maintains one entry per observed session (primary + asides); the app
    // copy is what `App::viewed_chrome` reads. `enter_side_view` /
    // `exit_side_view` swap entries in/out of the display fields.
    app.session_chrome = runtime
        .session_chrome
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    // Drain a pending side-view transition (enter/leave `/btw`).
    let side_view_transitioned = match runtime.side_view_signal.lock().await.take() {
        Some(crate::event_loop::SideViewSignal::Opened { side_id, .. }) => {
            app.enter_side_view(side_id);
            true
        }
        Some(crate::event_loop::SideViewSignal::Closed) => {
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
    // The primary session id: the **live** id from the listener's switch
    // tracking, not the handshake-time `SessionSource` — that one is frozen
    // for the process lifetime and goes stale the moment the harness repoints
    // the shared store (`/new`, `/session open`, `/resume`, `/fork`), which
    // would leave the history origin tag pointing at the retired session.
    let primary_session_id = runtime.live_session_id.lock().await.clone();
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
    // A switch also invalidates the navigation state — the cursor and the
    // stashed draft belong to the composer *of* a session, so crossing the
    // boundary must not carry them over.
    if app.current_session_id != viewed_session_id {
        app.current_session_id = viewed_session_id.clone();
        app.on_viewed_session_changed();
    }
    // Derive the viewed conversation's prompt rows from its transcript, so
    // `↑` reflects the conversation on screen — including prompts this
    // client never recorded (a resumed session's earlier turns, typed in
    // another client or before this `history.json` existed). Incremental
    // and cheap: only the transcript tail past the backfill cursor is
    // scanned, so a long streaming session pays for new rows only.
    let backfill_from = app.session_history_backfill_cursor;
    let viewed_len = if app.in_side_view {
        app.side_messages.len()
    } else {
        app.messages.len()
    };
    if backfill_from < viewed_len {
        // `App` is borrowed mutably by the backfill, so the tail is copied
        // out first — only genuine user Chat prompts make the cut, keeping
        // the copy at one small (text, origin, time) triple per prompt.
        let tail: Vec<(String, bool, u64)> = if app.in_side_view {
            user_prompt_tail(&app.side_messages[backfill_from..])
        } else {
            user_prompt_tail(&app.messages[backfill_from..])
        };
        app.session_history_backfill_cursor = viewed_len;
        app.backfill_session_history(&tail, now_epoch_ms());
    }
    // Publish the viewed session to the listener (scopes on-demand query
    // replies; see `UiRuntime::viewed_session_id`). Best-effort lock: a
    // contended listener simply re-reads on its next event.
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
                // The item left the outbox, so a queue pointer at it would
                // dangle. Dissolve *without* restoring the stashed draft —
                // the composer is either empty (the user was elsewhere) or
                // holding an edit of this very item whose commit already
                // raced; either way the draft must not clobber it.
                if app.queue_pointer.is_some() {
                    app.queue_pointer = None;
                    app.queue_pointer_draft.clear();
                    app.queue_pointer_draft_images.clear();
                    app.queue_pointer_draft_text_pastes.clear();
                }
            }
            OutboxSignal::Unavailable {
                session_id,
                input_id,
            } => {
                // The round closed before this content could ship. For an
                // outbox item this is a plain re-queue (paused next-round
                // entry). For a transcript-owned insert (`Ctrl+O`) the held
                // entry stays in the transcript — it never leaves the
                // conversation — and its content is re-queued here under the
                // same id so the next-round lifecycle (auto-drain, pointer
                // recall) takes over. Both transcript buffers are searched:
                // an aside (`/btw`) insert stages into `side_messages`.
                let held = app
                    .messages
                    .iter()
                    .chain(app.side_messages.iter())
                    .rev()
                    .find(|m| {
                        m.insert_id.as_deref() == Some(input_id.as_str())
                            && m.role == muta_contracts::Role::User
                    })
                    .map(|m| (m.raw.clone(), Vec::new(), Vec::new()));
                app.requeue_dispatch(&session_id, &input_id, held);
            }
            OutboxSignal::Inserted {
                session_id,
                input_id,
            } => {
                // The steer crossed a safe turn boundary: the listener
                // already settled the transcript entry (delivery flip). The
                // insert is transcript-owned, so there is no outbox item to
                // drop — the remove is a defensive no-op kept for the legacy
                // shadow-item shape.
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
        // Phase-1 unsend: the interrupted input becomes the new draft — the
        // newest *unsent* slot — but only if the composer is idle. Unlike a
        // queue recall this is not a user gesture: if the user was mid-
        // composition when the interrupt landed, their in-progress draft
        // wins and the unsent prompt stays recoverable via the recorded
        // history (Ctrl+R / ↑).
        let idle = app.input.is_empty()
            && app.pending_images.is_empty()
            && app.pending_text_pastes.is_empty();
        let adopted = idle && {
            app.adopt_as_draft(
                unsent.prompt,
                unsent.images,
                Vec::new(),
                crate::app::DraftAdoption::OnlyIfIdle,
            );
            true
        };
        // The prompt never ships with paste chips — it was flattened by
        // `expand_paste_chips` before send — so the paste slot must not
        // leak chips from an older draft into the restored one. Only clear
        // it when we actually adopted (a busy composer owns its chips).
        if adopted {
            app.pending_text_pastes.clear();
            app.history_draft_text_pastes.clear();
        }
        // Feedback parity with the Web app's "Prompt not sent" toast: the
        // transcript row for this prompt was already popped, so a silent
        // composer refill would look like the send never happened at all.
        let (title, body) = if adopted {
            (
                "Prompt not sent",
                "Interrupted before any output; your prompt is back in the composer.",
            )
        } else {
            (
                "Prompt not sent",
                "Interrupted before any output; the prompt is in history (Ctrl+R) — the composer kept your draft.",
            )
        };
        app.notice_toast_message = title.to_string();
        app.notice_toast_severity = NoticeSeverity::Warning;
        app.notice_toast_until =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(2600));
        let _ = body; // title-only bubble for now, mirroring the copy toast
    }
}

/// Loop stage: auto-run the next-round dispatch for a session that both
/// completed naturally and is idle (and is not user-blocked). Extracted
/// verbatim from `run_app_loop`.
fn auto_dispatch_ready_round(app: &mut App) {
    // A next-round item auto-runs only after both a natural-completion
    // event and the matching session's idle snapshot. Error, interrupt,
    // blocked-hook and vanished-session paths leave it visibly paused.
    // A user block (`Ctrl+P` / queue-modal-open) holds items back even from a
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
                        && item.state == crate::app::QueuedDispatchState::Waiting
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
            input: muta_contracts::QueuedUserInput {
                id: dispatch.id,
                text: expanded_text,
                display_text: Some(dispatch.text),
                images: dispatch.images,
                sent_at_ms: Some(sent_at_ms),
            },
        });
    }
}

/// Loop stage: cursor ownership & IME anchor — consume the cursor sync flag.
/// All physical cursor positioning, shielding during diff execution, and
/// visibility transitions are handled atomically by `Terminal::commit_frame`
/// inside the synchronized frame envelope.
fn sync_caret_and_cursor(
    app: &mut App,
    _terminal: &mut Terminal<std::io::Stdout>,
    _displayed_transcript_changed: bool,
) {
    app.cursor_sync_pending = false;
}

pub(super) async fn run_app_loop(
    terminal: &mut Terminal<std::io::Stdout>,
    app: &mut App,
    runtime: UiRuntime,
    session: crate::SessionSource,
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
            sync_transcripts_and_session(app, &runtime).await;

        // Apply protocol acknowledgements before handling the next key. The
        // transcript listener has already committed admitted/started messages;
        // this side owns only compact outbox and composer state.
        drain_outbox_signals(app, &runtime).await;

        drain_unsent_input(app, &runtime).await;

        auto_dispatch_ready_round(app);

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
            && crate::effort_ignition::ignition_finished(epoch.elapsed().as_millis())
        {
            app.effort_ignition_epoch = None;
        }
        // The empty-state help carousel (ADR-0104) needs a periodic redraw
        // to advance its slides while the hero is on screen. Keyed off the
        // *viewed* transcript so an empty background session doesn't keep
        // the loop hot while the user works elsewhere.
        let empty_state_showing =
            app.focused_messages().is_empty() && app.focus_stack.is_empty() && !app.in_side_view;
        // View-scoped: the redraw animation gate follows the *viewed*
        // session's round (a streaming aside animates its view; the primary
        // idling behind it does not force the aside view to animate).
        let viewed_animating = app.viewed_chrome().responding;
        let animating = viewed_animating
            || app.copy_toast_until.is_some()
            || app.notice_toast_until.is_some()
            || app.ctrl_c_armed()
            || app.esc_armed()
            || !app.pending_images.is_empty()
            || app.effort_ignition_epoch.is_some()
            || empty_state_showing
            || copy_pending.load(Ordering::SeqCst) > 0;
        // While user is actively typing or composing, quiesce background
        // micro-animations (100ms spinner/breathing ticks) to eliminate
        // unnecessary redraw churn and candidate box vibration.
        let is_typing_active = app.last_key_press.elapsed() < std::time::Duration::from_millis(150);
        let animation_draw = animating && !is_typing_active;

        // `swap` consumes the listener's signal exactly once. Folded in: input
        // handled last iteration, background clipboard results this one, and one
        // trailing frame after animation stops (`was_animating`) so the spinner
        // and expiring toasts are actually cleared from the screen.
        let needs_draw = frame_dirty
            || animation_draw
            || was_animating
            || runtime.dirty.swap(false, Ordering::AcqRel);
        was_animating = animation_draw;

        // The breathing indicator's phase is derived from wall-clock time at
        // the draw site (see `spinner_epoch`), not advanced per frame: the loop
        // wakes at irregular intervals (mouse-move/hover floods, streaming,
        // paste), so a per-frame counter would make the breathing speed up and
        // stutter with input activity instead of holding a steady cadence.

        // ── Cursor ownership & IME anchor ───────────────────────────────────
        sync_caret_and_cursor(app, terminal, displayed_transcript_changed);

        // A mutation of the transcript currently on screen (or a transition to
        // a different transcript view) can change the measured bottom after
        // layout. While following that bottom, stage the measurement frame in
        // the retained grid without flushing it; the immediate next pass paints
        // at the final scroll offset and is the only frame the terminal sees.
        let stage_bottom_follow = displayed_transcript_changed && app.follow_bottom;
        // A disclosure toggle (expand/collapse) changes the stream's height, so
        // the toggle's target scroll offset must be validated against the *new*
        // layout. Stage the first frame (it measures `content_lines` and emits
        // no bytes), settle the offset below, and let the next pass paint the
        // final viewport — the terminal never sees the un-clamped intermediate
        // frame that used to flash during expand/collapse.
        let stage_settle = app.scroll_settle_pending && !stage_bottom_follow;

        // Draw frame (skipped when nothing changed — see `needs_draw`).
        // `painted_scroll` is the offset this frame is laid out at, captured
        // before the post-draw clamp runs — the settle check below compares
        // against it to detect a clamp-induced move.
        let painted_scroll = app.scroll;
        // Pre-stage snapshot of the composer-geometry bookkeeping (see the
        // staged draw below): restored when a settle branch discards the
        // staged grid, kept when it is committed.
        let mut staged_rect_snapshot: Option<(mutx_engine::Rect, mutx_engine::Rect, usize)> = None;
        if needs_draw {
            if stage_bottom_follow || stage_settle {
                // A staged pass measures the new layout but commits nothing
                // yet. Its `observe_input_rect` records the *staged* rect —
                // correct only if the grid below is committed as-is. When a
                // settle branch below instead `continue`s (the clamp moved
                // the offset), that rect was never published, so the branch
                // restores the pre-stage snapshot before continuing; the
                // committed case keeps the staged observation. Snapshot here,
                // decide below.
                staged_rect_snapshot = Some((
                    app.last_input_rect,
                    app.last_frame_area,
                    app.last_input_rows,
                ));
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
                if let Some((rect, area, rows)) = staged_rect_snapshot.take() {
                    app.last_input_rect = rect;
                    app.last_frame_area = area;
                    app.last_input_rows = rows;
                }
                continue;
            }
            terminal.commit_staged()?;
            // Committed: the staged rect observation is now the published
            // geometry. Nothing to do — the snapshot is simply dropped.
        }
        // A disclosure toggle's scroll target has now been validated against
        // the layout the staged pass just measured (the clamp above ran on the
        // fresh `content_lines`). If the clamp moved the offset, the staged
        // grid is stale — redraw at the settled position; otherwise commit the
        // staged grid, which is already laid out at the correct offset,
        // without a second layout pass.
        if stage_settle && needs_draw {
            app.scroll_settle_pending = false;
            if app.scroll != painted_scroll {
                input_redraw_pending = true;
                if let Some((rect, area, rows)) = staged_rect_snapshot.take() {
                    app.last_input_rect = rect;
                    app.last_frame_area = area;
                    app.last_input_rows = rows;
                }
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
            input_redraw_pending = true;
            app.last_key_press = std::time::Instant::now();
            // The Ctrl+R history modal's search sub-layer borrows the input line
            // as its fuzzy query, so a literal `/foo` query must NOT trigger the
            // slash completion popup (or `@path` mentions); browse mode keeps the
            // line empty. Either way, suppress completions while the modal is
            // open. The same suppression applies right after an Enter-driven
            // commit: the user just finished a completion, so the popup should
            // stay hidden until the next edit.
            let suppress_completions =
                app.active_modal() == Modal::HistorySearch || app.completion_dismissed;
            // Pre-compute completion data to avoid borrow conflicts with process_event.
            let completions = if suppress_completions {
                Vec::new()
            } else {
                app.completions()
            };
            let suggestion_count = completions.len();
            // Keep the menu's highlight coherent with the live candidates:
            // a freshly opened menu starts with its first row selected (the
            // band + details flyout follow), a stale index clamps into
            // range, and no visible menu clears it.
            app.anchor_completion_selection(&completions);
            let suggestion_index = app.suggestion_index;
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
            // every built-in and trusted project command the daemon published.
            let recognized_command = app
                .input
                .split_whitespace()
                .next()
                .map(|first| app.command_catalog.recognizes(first))
                .unwrap_or(false);
            // The input layer sees the *unsuppressed* classification: the
            // dismissal latch is carried separately (`completion_dismissed`)
            // so Tab's re-open gesture can tell "a slash menu is dismissed"
            // apart from "no menu applies at all". Every other consumer of
            // the kind already consults the latch, so suppressing here would
            // only have hidden the dismissed-but-recoverable state.
            let completion_kind = if app.active_modal() == Modal::HistorySearch {
                crate::CompletionKind::None
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
                .filter(|_| matches!(app.active_modal(), Modal::None));
            // The provider-delete confirm overlay is a sub-layer over the
            // stage-1 Connections list: when it is open it owns every key, so
            // probe the raw event before the general input mapper and skip
            // `process_event` entirely (the latter would otherwise edit the
            // composer or move the list selection behind the panel). The
            // returned action flows through the normal action dispatch below
            // (`DeleteProviderConfirm` / `DeleteProviderCancel` are the
            // overlay-specific arms).
            // Snapshot before the mutable borrow of `app.input` below: Tab's
            // re-open gesture needs to know whether a dismissed menu still
            // has trigger text to come back to, evaluated against the
            // pre-keystroke composer (the keystroke itself decides whether
            // the latch clears via the InsertChar/Backspace passes).
            // `process_event` mutates `cursor_position` in place (it cannot go
            // through `App::set_cursor`), so any keystroke that moved the caret
            // must still mark the terminal cursor for an immediate re-sync.
            // Key events and pastes are the only inputs that move it — a mouse
            // report (hover/scroll/drag) or a resize does not, and arming the
            // flush for those used to emit an out-of-envelope cursor write on
            // essentially every loop iteration (a mode-1002 drag emits one per
            // motion report), which read as per-frame caret jitter. Computing
            // this before the mapper borrows `event` (which it consumes).
            let event_moves_caret = matches!(
                &event,
                crossterm::event::Event::Key(_) | crossterm::event::Event::Paste(_)
            );

            let has_trigger_text = app.completion_trigger_text_present();
            let active_modal = app.active_modal();
            let action = if let Some(overlay_action) = probe_delete_overlay(app, &event) {
                overlay_action
            } else if let Some(relay) = probe_input_selection_relay(app, &event) {
                // A whole-input selection is active and the composer owns the
                // caret. The block cursor is hidden, but its position is
                // implicitly the selection's head — where the mouse button
                // was released. Rather than letting `process_event` move the
                // *stale* `cursor_position` (and, for Backspace/Delete,
                // splice at that stale spot), resolve the relay right here:
                // skip the input mapper entirely, break the selection, and
                // adopt the caret at the proper edge. The returned action
                // keeps the same post-edit passes (focus reclaim, attachment
                // reconcile, completion latch) the ordinary keystroke would
                // have run.
                relay
            } else {
                input::process_event(
                    event,
                    &mut app.input,
                    &mut app.cursor_position,
                    input::InputContext {
                        active_modal,
                        session_info_detail: app.session_info_detail,
                        is_responding: app.running_sessions.contains(&viewed_session_id),
                        completion_kind,
                        suggestion_count,
                        has_exact_suggestion,
                        suggestion_index,
                        completion_dismissed: app.completion_dismissed,
                        has_trigger_text,
                        permission_confirm_always: app.permission_confirm_always,
                        permission_show_details: app.permission_show_details,
                        in_envoy_view,
                        in_side_view: app.in_side_view,
                        has_focused_target: app.focused_target.is_some(),
                        has_queued: app.pending_dispatch.iter().any(|item| {
                            item.session_id == viewed_session_id
                                && item.state == crate::app::QueuedDispatchState::Waiting
                        }),
                        queue_pointer_armed: app.queue_pointer.is_some(),
                        history_searching: app.history_search,
                        model_searching: app.model_search,
                        modal_keymap_open: app.modal_keymap_open,
                        custom_provider_field: (active_modal == Modal::CustomProvider)
                            .then_some(app.custom_field),
                        editor_field: (active_modal == Modal::ModelEditor)
                            .then_some(app.editor_field),
                        question_other_highlighted: app
                            .question
                            .as_ref()
                            .is_some_and(|q| q.is_other_highlighted()),
                        history_clear_confirm: app.history_clear_confirm,
                        host_prompting: app.host_prompting,
                        config_custom_editing: app.config_custom_editing,
                        config_websearch_editing: app.websearch_editing.is_some(),
                    },
                    &mut app.drag,
                )
            };

            if event_moves_caret {
                app.note_cursor_moved();
            }

            // A `/`-leading input whose first token is NOT a recognized command
            // (built-in or discovered project command) is ordinary prose, not
            // a command invocation: the `/` is
            // just a character the user typed. Convert the SendSlash the input
            // layer produced into a SendChat so the message ships normally
            // instead of tripping the backend's "Unknown command" error. The
            // recognition is computed here (not in the input layer) because
            // only the loop has access to the backend-published catalog.
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
            // Ctrl+R history and visible in the scrollback as one command
            // component (`⌘ /models`, ADR-0108). The row is pushed as
            // pending; a modal command never receives a
            // `RoundEvent::CommandResult`, so the next idle reconcile marks
            // it cancelled — the input half stays durable without promising
            // an output. Modal *outcomes* (e.g. a provider switch) are still
            // emitted separately by the harness listener as follow-up
            // notices.
            //
            // The composer text was consumed by `process_event` (the action is
            // data-less), so we replay it from the pre-dispatch snapshot.
            if action.is_text_modal_command()
                && let Some(entry) = modal_cmd_history
            {
                let (name, args) = actions::split_command_word(&entry);
                runtime.messages.write().await.push(
                    TranscriptMessage::pending_command(name, args).with_sent_at_ms(now_epoch_ms()),
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

            // Post-dispatch anchor: an action may have replaced the composer
            // programmatically (a Tab/Esc completion gesture, a queue recall,
            // a paste applied below), so re-derive the candidate list and
            // re-anchor the highlight before the next event shares this
            // redraw. Suppressed exactly when the pre-compute above was.
            let completions = if suppress_completions {
                Vec::new()
            } else {
                app.completions()
            };
            app.anchor_completion_selection(&completions);
        }
    }
}

pub(super) fn tool_activity_status(name: &str) -> &'static str {
    match name {
        "read_text" | "read_image" | "list_dir" | "find" | "glob" | "use_skill" => "exploring",
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

/// Snapshot the active model's effective reasoning effort at the moment a
/// message is created, so the turn header shows the depth each turn actually
/// ran with rather than today's live setting. Resolves from the provider
/// picker mirror (pushed unconditionally at session start and on every
/// provider/model mutation) and applies the same per-protocol gating as the
/// hint bar ([`effective_reasoning_effort`], ADR-0046): Anthropic effort
/// counts only while thinking is opted in, OpenAI effort whenever the channel
/// reports one, Google never. `None` keeps non-reasoning channels quiet.
pub(super) async fn picker_effort(
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

/// Resolve a mutable reference to the message at index `mi` within the
/// currently focused view: the root conversation when the focus stack is empty,
/// or the focused envoy task's child stream otherwise. Selection and layout
/// indices are recorded against whichever slice was rendered, so mutations must
/// resolve through the same context.
pub(super) fn resolve_focused_mut<'a>(
    messages: &'a mut [TranscriptMessage],
    focus_stack: &[crate::app::ZoomFrame],
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
    focus_stack: &[crate::app::ZoomFrame],
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
    messages: &[crate::model::document::TranscriptMessage],
    input: &str,
    layout_map: &crate::model::layout::LayoutMap,
    cell_info: Option<&CellDragInfo>,
) -> Option<String> {
    let on_modal = match sel {
        SelectionState::None => false,
        SelectionState::Block { message_idx, .. } => {
            *message_idx == crate::model::layout::MODAL_DOC_MSG_IDX
        }
        SelectionState::TableCell { message_idx, .. } => {
            *message_idx == crate::model::layout::MODAL_DOC_MSG_IDX
        }
        SelectionState::Range { anchor, head } => {
            anchor.message_idx == crate::model::layout::MODAL_DOC_MSG_IDX
                || head.message_idx == crate::model::layout::MODAL_DOC_MSG_IDX
        }
    };
    if on_modal {
        return layout_map.extract_text_for_range(sel);
    }

    let on_input = match sel {
        SelectionState::None => false,
        SelectionState::Block { message_idx, .. } => *message_idx == crate::view::INPUT_MSG_IDX,
        SelectionState::TableCell { message_idx, .. } => *message_idx == crate::view::INPUT_MSG_IDX,
        SelectionState::Range { anchor, head } => {
            anchor.message_idx == crate::view::INPUT_MSG_IDX
                && head.message_idx == crate::view::INPUT_MSG_IDX
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
mod transcript_patch_tests {
    //! Behavior locks for the targeted streaming patches (ADR-0114 replay
    //! path): a streaming boundary must mutate only its own message —
    //! replacing it in place or appending at the tail — and a stale local
    //! copy must refuse the patch so the caller falls back to the snapshot.

    use super::*;
    use crate::model::document::{MessageKind, TranscriptMessage};
    use crate::versioned::{TranscriptPatch, TranscriptUpdate};
    use muta_contracts::Role;

    fn text(content: &str) -> TranscriptMessage {
        TranscriptMessage::new(Role::Assistant, content)
    }

    #[test]
    fn replace_message_swaps_in_place_without_touching_neighbors() {
        let mut messages = vec![text("before"), text("target"), text("after")];
        let target_id = messages[1].id;
        let mut finalized = messages[1].clone();
        finalized.raw = "finalized".to_string();
        finalized.reparse();

        let patch = TranscriptPatch::Updates(vec![TranscriptUpdate::ReplaceMessage {
            message_id: target_id,
            message: finalized,
        }]);
        assert!(apply_transcript_patch(&mut messages, patch));
        assert_eq!(messages.len(), 3, "replace never changes length");
        assert_eq!(messages[0].raw, "before");
        assert_eq!(messages[1].raw, "finalized");
        assert_eq!(messages[2].raw, "after");
    }

    #[test]
    fn replace_missing_message_falls_back_to_snapshot() {
        let mut messages = vec![text("only")];
        let ghost = text("ghost");
        let patch = TranscriptPatch::Updates(vec![TranscriptUpdate::ReplaceMessage {
            message_id: ghost.id,
            message: ghost,
        }]);
        assert!(!apply_transcript_patch(&mut messages, patch));
    }

    #[test]
    fn append_message_pushes_when_tail_matches() {
        let mut messages = vec![text("tail")];
        let tail_id = messages[0].id;
        let fresh = text("new");
        let patch = TranscriptPatch::Updates(vec![TranscriptUpdate::AppendMessage {
            pre_append_tail: Some(tail_id),
            message: fresh,
        }]);
        assert!(apply_transcript_patch(&mut messages, patch));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].raw, "new");
    }

    #[test]
    fn append_with_diverged_tail_refuses_instead_of_forking() {
        // The listener appended against tail X, but the local copy's tail is
        // someone else (a missed update). Accepting the append would fork the
        // transcript; the patch must refuse so the snapshot path reconciles.
        let stale_tail = text("stale");
        let mut messages = vec![text("different")];
        let fresh = text("new");
        let patch = TranscriptPatch::Updates(vec![TranscriptUpdate::AppendMessage {
            pre_append_tail: Some(stale_tail.id),
            message: fresh,
        }]);
        assert!(!apply_transcript_patch(&mut messages, patch));
        assert_eq!(messages.len(), 1, "nothing may be pushed on refusal");
    }

    #[test]
    fn append_into_empty_transcript_requires_none_tail() {
        let mut messages: Vec<TranscriptMessage> = Vec::new();
        let fresh = text("first");
        assert!(apply_transcript_patch(
            &mut messages,
            TranscriptPatch::Updates(vec![TranscriptUpdate::AppendMessage {
                pre_append_tail: None,
                message: fresh,
            }])
        ));
        assert_eq!(messages.len(), 1);

        // A non-None expected tail against an empty local copy must refuse.
        let mut messages: Vec<TranscriptMessage> = Vec::new();
        let other = text("x");
        assert!(!apply_transcript_patch(
            &mut messages,
            TranscriptPatch::Updates(vec![TranscriptUpdate::AppendMessage {
                pre_append_tail: Some(other.id),
                message: text("y"),
            }])
        ));
    }

    #[test]
    fn reasoning_finalize_is_one_replace_not_a_snapshot() {
        // The streaming `StreamReasoningEnd` path records a ReplaceMessage;
        // the replay must apply it without disturbing the settled history.
        let mut messages = vec![text("history")];
        let mut trace = TranscriptMessage::thinking("partial");
        let trace_id = trace.id;
        messages.push(trace.clone());

        trace.raw = "full trace".to_string();
        trace.reparse();
        if let MessageKind::Thinking {
            content,
            duration_ms,
            ..
        } = &mut trace.kind
        {
            *content = "full trace".to_string();
            *duration_ms = Some(1200);
        }

        assert!(apply_transcript_patch(
            &mut messages,
            TranscriptPatch::Updates(vec![TranscriptUpdate::ReplaceMessage {
                message_id: trace_id,
                message: trace,
            }])
        ));
        assert_eq!(messages[0].raw, "history");
        assert!(!messages[1].is_thinking_streaming());
    }
}

#[cfg(test)]
mod selection_text_tests {
    use super::*;
    use crate::model::layout::{LayoutMap, SemanticCursor};
    use crate::model::selection::SelectionState;
    use crate::view::INPUT_MSG_IDX;

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
    use super::{AgentRequest, App, UiRuntime};

    pub(super) async fn apply(
        effects: &[crate::question_model::QuestionEffect],
        app: &mut App,
        runtime: &UiRuntime,
    ) {
        for effect in effects {
            match effect {
                crate::question_model::QuestionEffect::Reply {
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
                crate::question_model::QuestionEffect::Cancelled { request_id } => {
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
                crate::question_model::QuestionEffect::Closed { request_id } => {
                    let mut queue = runtime.pending_question.lock().await;
                    queue.retain(|r| r.id != *request_id);
                    // If the queue is now empty the modal closes; the sync block
                    // will also clear `app.question`, but clearing it here keeps
                    // the very next render (same frame) consistent.
                    if queue.is_empty() {
                        app.question = None;
                        app.pop_transient_surface();
                        app.modal_index = 0;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod caret_flush_gate_tests {
    //! Behavior locks for the caret anti-drift gate in `sync_caret_and_cursor`
    //! and `App::input_geometry_is_clean`. The immediate cursor flush and the
    //! frame's `set_cursor_position` are the two writers of the caret
    //! coordinate; when the flush fires against geometry the next frame
    //! re-measures differently, the terminal shows a two-step caret jump —
    //! the "反复漂移/闪烁" symptom. These lock the gate's contract: the flush
    //! is permitted exactly when the cached rect provably still matches.

    use super::*;

    fn sized_app(input: &str, rect_w: u16, rect_h: u16, frame_w: u16, frame_h: u16) -> App {
        let mut app = crate::tests::new_app_for_relay_tests();
        app.input = input.to_string();
        app.set_cursor_end();
        let rect = mutx_engine::Rect::new(0, 10, rect_w, rect_h);
        let area = mutx_engine::Rect::new(0, 0, frame_w, frame_h);
        // Mirror the renderer: rows measured through the same width formula
        // the transcript layout uses for the frame, not the placed rect.
        let rows = crate::composer::input_row_count(
            input,
            crate::view::composer_layout_text_width(frame_w as usize),
            app.byte_cursor(),
        );
        app.observe_input_rect(rect, area, rows);
        app
    }

    /// Geometry unchanged → the cached rect is clean and the flush may fire.
    #[test]
    fn geometry_clean_when_rows_and_size_match() {
        // Frame width 80 → text width 72; "hello world" (11 chars) is one row
        // and the probe measures through the same frame-derived width.
        let app = sized_app("hello world", 76, 3, 80, 24);
        assert!(
            app.input_geometry_is_clean((80, 24)),
            "same size, same row count → clean"
        );
    }

    /// A keystroke that crosses a wrap boundary (row count changes) must
    /// invalidate the cached rect so the flush defers to the frame. The
    /// probe measures through `composer_layout_text_width(frame_width)` —
    /// the same formula the renderer used — so the boundary case is
    /// constructed against the frame width, not the placed rect.
    #[test]
    fn geometry_dirty_when_wrap_boundary_crossed() {
        // Frame width 80 → text width 80 - 2*2 - 2 - 2 = 72 columns.
        let text_width = crate::view::composer_layout_text_width(80);
        assert_eq!(text_width, 72);
        // Exactly one full row: clean. One more char wraps → 2 rows → dirty.
        let full_row = "a".repeat(text_width);
        let mut app = sized_app(&full_row, 76, 3, 80, 24);
        assert!(
            app.input_geometry_is_clean((80, 24)),
            "exactly one full row → still one wrapped row → clean"
        );
        app.input.push('e');
        assert!(
            !app.input_geometry_is_clean((80, 24)),
            "wrap boundary crossed → the box height moves → flush must defer to the frame"
        );
    }

    /// A resize between the observed rect and the current terminal size must
    /// invalidate the cached rect (every wrap reflows).
    #[test]
    fn geometry_dirty_on_resize() {
        let app = sized_app("hello", 20, 3, 80, 24);
        assert!(!app.input_geometry_is_clean((100, 30)));
    }

    /// A never-observed rect (width 0, e.g. before the first frame) must be
    /// treated as dirty — there is nothing valid to flush against.
    #[test]
    fn geometry_dirty_before_first_observation() {
        let app = crate::tests::new_app_for_relay_tests();
        assert!(!app.input_geometry_is_clean((80, 24)));
    }

    /// Only key events and pastes arm the immediate flush; mouse reports and
    /// resizes must not (they do not move the caret, and the armed flush used
    /// to emit an out-of-envelope cursor write per motion report).
    #[test]
    fn non_caret_events_do_not_arm_flush() {
        let mut app = crate::tests::new_app_for_relay_tests();
        app.cursor_sync_pending = false;

        // Simulate the loop's gate for a mouse-move event.
        let event = crossterm::event::Event::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: 1,
            row: 1,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        let moves_caret = matches!(
            &event,
            crossterm::event::Event::Key(_) | crossterm::event::Event::Paste(_)
        );
        assert!(!moves_caret);
        if moves_caret {
            app.note_cursor_moved();
        }
        assert!(
            !app.cursor_sync_pending,
            "a mouse report must not arm the immediate cursor flush"
        );
    }
}
