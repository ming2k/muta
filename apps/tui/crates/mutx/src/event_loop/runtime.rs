//! Shared runtime state crossing the response-listener / event-loop boundary.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;

#[cfg(test)]
use muta_contracts::LoopStatus;
use muta_contracts::{
    HarnessSnapshot, ParentStatus, PermissionRequest, ProviderPickerSnapshot, SessionOverview,
    TodoList, UserQuestionRequest,
};

use crate::model::document::{NoticeSeverity, TranscriptMessage};
use crate::versioned::Versioned;

pub(crate) fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

#[derive(Debug)]
pub(crate) struct CompletionSignal {
    pub request_id: u64,
    pub input: String,
    pub cursor: usize,
    pub items: Vec<muta_contracts::InputCompletion>,
}

/// A toast-surfaced notice forwarded across the listener → loop boundary.
pub(crate) struct NoticeToastSignal {
    pub severity: NoticeSeverity,
    pub text: String,
}

pub(crate) enum OutboxSignal {
    Unavailable {
        session_id: String,
        input_id: String,
    },
    FollowUpStarted {
        session_id: String,
        input_id: String,
    },
    SteerAdmitted {
        session_id: String,
        input_id: String,
    },
    RoundCompleted {
        session_id: String,
    },
    RoundInterrupted {
        session_id: String,
    },
    HarnessState {
        session_id: String,
        idle: bool,
    },
}

pub(crate) enum SideViewSignal {
    Opened { side_id: String },
    Closed,
}

pub(crate) enum OauthAddSignal {
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

pub struct UiRuntime {
    pub current_provider: Arc<Mutex<String>>,
    pub current_model: Arc<Mutex<String>>,
    pub context_tokens: Arc<Mutex<HashMap<String, muta_contracts::ContextTokenSnapshot>>>,
    pub harness: Arc<Mutex<HarnessSnapshot>>,
    pub phase: Arc<Mutex<Option<crate::phase::Phase>>>,
    pub provider_retry: Arc<Mutex<Option<crate::app::ProviderRetryState>>>,
    pub pending_permission: Arc<Mutex<VecDeque<PermissionRequest>>>,
    pub pending_question: Arc<Mutex<VecDeque<UserQuestionRequest>>>,
    pub pending_input: Arc<Mutex<VecDeque<muta_contracts::InputRequest>>>,
    /// ADR-0175: the listener task publishes a freshly-arrived
    /// `WorkspaceSecuritySnapshot` to this cell when its
    /// `aggregate() == Quarantined`, so the per-frame sync can mount
    /// the PreAttach interstitial. Drained back to `None` once the
    /// loop has consumed the signal.
    pub pre_attach_signal: Arc<Mutex<Option<crate::PreAttachSignal>>>,
    pub is_responding: Arc<AtomicBool>,
    pub trust_gate_dismissed: Arc<AtomicBool>,
    pub dirty: Arc<AtomicBool>,
    pub dirty_notify: Arc<tokio::sync::Notify>,
    pub completion_signal: Arc<Mutex<Option<CompletionSignal>>>,
    pub runner_permission_parent: Arc<Mutex<HashMap<String, String>>>,
    pub runner_question_parent: Arc<Mutex<HashMap<String, String>>>,
    pub messages: Arc<Versioned<Vec<TranscriptMessage>>>,
    pub side_messages: Arc<Versioned<Vec<TranscriptMessage>>>,
    pub parent_status: Arc<Mutex<ParentStatus>>,
    pub side_view_signal: Arc<Mutex<Option<SideViewSignal>>>,
    pub btw_list: Arc<Mutex<Vec<muta_contracts::BtwAsideSummary>>>,
    pub session_chrome:
        Arc<std::sync::Mutex<std::collections::HashMap<String, crate::app::SessionChrome>>>,
    pub host_console_signal: Arc<Mutex<VecDeque<crate::overlays::ConsoleLine>>>,
    pub viewed_session_id: Arc<Mutex<Option<String>>>,
    pub live_session_id: Arc<Mutex<String>>,
    pub key_status: Arc<Mutex<HashMap<String, bool>>>,
    pub websearch_config: Arc<Mutex<Option<muta_contracts::WebSearchConfigView>>>,
    pub provider_picker: Arc<Mutex<ProviderPickerSnapshot>>,
    pub sessions_overview: Arc<Mutex<Vec<SessionOverview>>>,
    pub sessions_overview_rev: Arc<std::sync::atomic::AtomicU64>,
    pub session_detail: Arc<Mutex<Option<muta_contracts::SessionDetail>>>,
    pub connection_detail: Arc<Mutex<Option<muta_contracts::ConnectionDetail>>>,
    #[allow(dead_code)]
    pub session_tree: Arc<Mutex<Option<muta_contracts::SessionTree>>>,
    pub token_report: Arc<Mutex<Option<muta_contracts::TokenSourceReport>>>,
    pub usage_stats: Arc<Mutex<Option<muta_contracts::usage_stats::UsageStatsReport>>>,
    pub open_sessions: Arc<AtomicBool>,
    pub open_tree: Arc<AtomicBool>,
    pub host_sessions: Arc<Mutex<Vec<muta_contracts::MonitoredSession>>>,
    pub host_sessions_rev: Arc<std::sync::atomic::AtomicU64>,
    pub open_host: Arc<AtomicBool>,
    pub oauth_add_signal: Arc<Mutex<Option<OauthAddSignal>>>,
    pub awaiting_oauth_add: Arc<AtomicBool>,
    #[allow(dead_code)]
    pub session_context: Arc<Mutex<Option<muta_contracts::SessionContextSnapshot>>>,
    pub todos: Arc<Mutex<Option<TodoList>>>,
    pub round_count: Arc<Mutex<u64>>,
    pub current_turn: Arc<Mutex<u64>>,
    pub round_started_at: Arc<Mutex<Option<std::time::Instant>>>,
    pub notice_toast_signal: Arc<Mutex<Option<NoticeToastSignal>>>,
    pub outbox_signals: Arc<Mutex<VecDeque<OutboxSignal>>>,
}

impl UiRuntime {
    #[cfg(test)]
    pub fn minimal_for_test() -> Self {
        Self {
            current_provider: Arc::new(Mutex::new(String::new())),
            current_model: Arc::new(Mutex::new(String::new())),
            context_tokens: Arc::new(Mutex::new(HashMap::new())),
            harness: Arc::new(Mutex::new(HarnessSnapshot {
                loop_status: LoopStatus::Idle,
                round_counter: 0,
                delegated: false,
                unconfined: false,
                workspace_security: muta_contracts::WorkspaceSecuritySnapshot::default(),
                retry_pending: false,
            })),
            phase: Arc::new(Mutex::new(None)),
            provider_retry: Arc::new(Mutex::new(None)),
            pending_permission: Arc::new(Mutex::new(VecDeque::new())),
            pending_question: Arc::new(Mutex::new(VecDeque::new())),
            pending_input: Arc::new(Mutex::new(VecDeque::new())),
            pre_attach_signal: Arc::new(Mutex::new(None)),
            is_responding: Arc::new(AtomicBool::new(false)),
            trust_gate_dismissed: Arc::new(AtomicBool::new(false)),
            dirty: Arc::new(AtomicBool::new(false)),
            dirty_notify: Arc::new(tokio::sync::Notify::new()),
            completion_signal: Arc::new(Mutex::new(None)),
            runner_permission_parent: Arc::new(Mutex::new(HashMap::new())),
            runner_question_parent: Arc::new(Mutex::new(HashMap::new())),
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
            connection_detail: Arc::new(Mutex::new(None)),
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
            notice_toast_signal: Arc::new(Mutex::new(None)),
            outbox_signals: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}
