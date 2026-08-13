//! Per-session monitor state machines (ADR-0093 §3).
//!
//! A [`MonitorTracker`] taps one hosted session's [`AgentResponse`] broadcast
//! and maintains the [`SessionStatus`] + accounting row a control panel
//! renders. The tap lives in the session's broadcast-tap task
//! ([`crate::registry`]), which calls [`MonitorTracker::observe`] for every
//! response; the registry owns publishing the resulting
//! [`neenee_core::MonitorEvent`] diffs to the host-level topic.
//!
//! The tracker is deliberately *derived state, not protocol state*: the round
//! lifecycle itself stays binary (ADR-0078) — `Idle`/`Running` here are a
//! display badge, and the `Needs*` states are overlays on a still-running
//! round, exactly like the single-session `ParentStatus` (ADR-0017).

use neenee_core::{AgentResponse, MonitoredSession, RoundEvent, SessionStatus, WipStatus};

/// Tracks one hosted session. `base` is the cheap header row (from the same
/// deferred parse that feeds the sessions picker); every other field is
/// folded from the live event stream, starting at [`Self::bootstrap`].
pub struct MonitorTracker {
    base: MonitoredSession,
    status: SessionStatus,
    round: u64,
    turn: Option<usize>,
    output_tokens: u64,
    round_started: Option<std::time::Instant>,
    elapsed_ms: u64,
    current_tool: Option<String>,
    activity: Option<String>,
    context_tokens: Option<usize>,
    note: Option<String>,
    /// The session's declared WIP (ADR-0097 §5). Set by the registry's WIP
    /// coordination registry (not folded from the event stream), so it lives
    /// beside the tracked fields and is projected onto the row by [`row`].
    wip: Option<WipStatus>,
}

impl MonitorTracker {
    /// Seed the tracker for a session that has just been (re-)hosted.
    ///
    /// `seed_status` is the registry's bootstrap verdict: [`SessionStatus::Running`]
    /// when the resumed transcript's tail shows an unfinished round (the
    /// driver will re-drive it), otherwise [`SessionStatus::Idle`].
    pub fn bootstrap(mut base: MonitoredSession, seed_status: SessionStatus) -> Self {
        base.status = seed_status;
        Self {
            base,
            status: seed_status,
            round: 0,
            turn: None,
            output_tokens: 0,
            round_started: if seed_status == SessionStatus::Running {
                Some(std::time::Instant::now())
            } else {
                None
            },
            elapsed_ms: 0,
            current_tool: None,
            activity: None,
            context_tokens: None,
            note: None,
            wip: None,
        }
    }

    /// Set or clear the session's declared WIP (ADR-0097 §5). Called by the
    /// registry's WIP coordination registry; the next [`row`] projection
    /// carries it to the dashboard.
    pub fn set_wip(&mut self, wip: Option<WipStatus>) {
        self.wip = wip;
    }

    /// The current panel row. `updated_at` advances with every observed event
    /// so a consumer can sort by liveness without the session store.
    pub fn row(&self) -> MonitoredSession {
        let mut row = self.base.clone();
        row.updated_at = now_secs();
        row.status = self.status;
        row.round = self.round;
        row.turn = self.turn;
        row.output_tokens = self.output_tokens;
        row.elapsed_ms = self.elapsed_ms();
        row.current_tool = self.current_tool.clone();
        row.activity = self.activity.clone();
        row.context_tokens = self.context_tokens;
        row.note = self.note.clone();
        row.wip = self.wip.clone();
        row
    }

    /// Fold one broadcast response into the tracked state.
    pub fn observe(&mut self, response: &AgentResponse) {
        let AgentResponse::Round { event, .. } = response else {
            return;
        };
        match event {
            RoundEvent::TurnStarted { round, turn } => {
                if self.round_started.is_none() {
                    // A fresh round begins with its first turn; subsequent
                    // turns of the same round arrive while a start time is
                    // already recorded and only update the position.
                    self.round_started = Some(std::time::Instant::now());
                    self.output_tokens = 0;
                    self.current_tool = None;
                }
                self.round = *round;
                self.turn = Some(*turn);
                self.note = None;
                self.status = SessionStatus::Running;
            }
            RoundEvent::ToolCall { name, .. } => {
                self.current_tool = Some(name.clone());
                self.status = SessionStatus::Running;
                self.note = None;
            }
            RoundEvent::ToolResult { .. } | RoundEvent::ToolCancelled { .. } => {
                self.current_tool = None;
            }
            RoundEvent::PermissionRequest(request) => {
                self.status = SessionStatus::NeedsApproval;
                self.note = Some(format!("permission: {}", request.tool));
            }
            RoundEvent::UserQuestionRequest(request) => {
                self.status = SessionStatus::NeedsInput;
                self.note = Some(
                    request
                        .questions
                        .first()
                        .map(|q| truncate(&q.question, 80))
                        .unwrap_or_else(|| "question pending".to_string()),
                );
            }
            RoundEvent::InputRequest(request) => {
                self.status = SessionStatus::NeedsInput;
                self.note = Some(format!("input: {}", truncate(&request.prompt, 80)));
            }
            RoundEvent::StreamStart | RoundEvent::StreamDelta(_) | RoundEvent::StreamEnd(_) => {
                // Model output is flowing again: any human-decision overlay
                // has been resolved.
                if matches!(
                    self.status,
                    SessionStatus::NeedsApproval | SessionStatus::NeedsInput
                ) {
                    self.status = SessionStatus::Running;
                    self.note = None;
                }
            }
            RoundEvent::RoundCompleted(summary) => {
                self.finish_round(SessionStatus::Idle);
                self.round = summary.round;
                self.output_tokens = summary.output_tokens;
                self.elapsed_ms = summary.duration_ms;
            }
            RoundEvent::Error(message) => {
                self.finish_round(SessionStatus::Failed);
                self.note = Some(truncate(message, 120));
            }
            RoundEvent::UnsentInput { .. } => {
                self.finish_round(SessionStatus::Interrupted);
            }
            RoundEvent::Activity(text) => {
                self.activity = Some(truncate(text, 120));
            }
            RoundEvent::ContextTokens(snapshot) => {
                self.context_tokens = Some(snapshot.tokens);
            }
            _ => {}
        }
    }

    /// A `Chat`/`ChatToSession` was admitted for a fresh round: pre-mark the
    /// session running so the panel reflects it before the first `TurnStarted`
    /// arrives. Idempotent within a round. (Currently the round boundary is
    /// learned from the event stream itself — see `observe`'s `TurnStarted`
    /// arm — so this is a public seam for the control plane, not yet called.)
    #[allow(dead_code)]
    pub fn note_new_round(&mut self) {
        if self.round_started.is_none() {
            self.round += 1;
            self.round_started = Some(std::time::Instant::now());
            self.turn = None;
            self.output_tokens = 0;
            self.current_tool = None;
            self.note = None;
            self.status = SessionStatus::Running;
        }
    }

    fn elapsed_ms(&self) -> u64 {
        match self.round_started {
            Some(start) => start.elapsed().as_millis() as u64,
            None => self.elapsed_ms,
        }
    }

    fn finish_round(&mut self, status: SessionStatus) {
        self.elapsed_ms = self.elapsed_ms();
        self.round_started = None;
        self.current_tool = None;
        self.status = status;
        if status != SessionStatus::Failed {
            self.note = None;
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use neenee_core::{PermissionRequest, RoundSummary};

    fn round_event(event: RoundEvent) -> AgentResponse {
        AgentResponse::Round {
            session_id: "s".into(),
            event,
        }
    }

    fn tracker() -> MonitorTracker {
        MonitorTracker::bootstrap(
            MonitoredSession {
                id: "s".into(),
                overview: "task".into(),
                created_at: 1,
                updated_at: 1,
                message_count: 2,
                status: SessionStatus::Idle,
                hosting: neenee_core::SessionHosting::Hosted,
                round: 0,
                turn: None,
                output_tokens: 0,
                elapsed_ms: 0,
                current_tool: None,
                activity: None,
                context_tokens: None,
                note: None,
                project_root: "/tmp/proj".into(),
                wip: None,
            },
            SessionStatus::Idle,
        )
    }

    #[test]
    fn set_wip_projects_onto_the_row() {
        let mut t = tracker();
        assert!(t.row().wip.is_none());
        t.set_wip(Some(neenee_core::WipStatus {
            paths: vec!["src".into()],
            summary: "mid-refactor".into(),
        }));
        let wip = t.row().wip.expect("wip projected");
        assert_eq!(wip.paths, vec!["src".to_string()]);
        assert_eq!(wip.summary, "mid-refactor");
        t.set_wip(None);
        assert!(t.row().wip.is_none());
    }

    #[test]
    fn fresh_round_flows_running_to_idle_with_summary() {
        let mut t = tracker();
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 0 }));
        let row = t.row();
        assert_eq!(row.status, SessionStatus::Running);
        assert_eq!(row.round, 1);
        assert_eq!(row.turn, Some(0));
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 1 }));
        assert_eq!(t.row().turn, Some(1), "a second turn keeps the round open");
        t.observe(&round_event(RoundEvent::ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            arguments: "{}".into(),
        }));
        assert_eq!(t.row().current_tool.as_deref(), Some("bash"));
        t.observe(&round_event(RoundEvent::RoundCompleted(RoundSummary {
            round: 1,
            output_tokens: 300,
            duration_ms: 5_000,
            paused_ms: 0,
            generation_ms: 2_000,
        })));
        let row = t.row();
        assert_eq!(row.status, SessionStatus::Idle);
        assert_eq!(row.round, 1);
        assert_eq!(row.output_tokens, 300);
        assert_eq!(row.elapsed_ms, 5_000);
        assert!(row.current_tool.is_none());
    }

    #[test]
    fn permission_overlay_clears_on_stream_resume() {
        let mut t = tracker();
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 0 }));
        t.observe(&round_event(RoundEvent::PermissionRequest(
            PermissionRequest {
                id: "r".into(),
                tool: "write_file".into(),
                label: "Write file".into(),
                description: String::new(),
                arguments: "{}".into(),
                scope: String::new(),
                elevation: false,
                one_off: false,
            },
        )));
        assert_eq!(t.row().status, SessionStatus::NeedsApproval);
        assert!(t.row().note.unwrap().contains("write_file"));
        t.observe(&round_event(RoundEvent::StreamDelta("…".into())));
        assert_eq!(t.row().status, SessionStatus::Running);
        assert!(t.row().note.is_none());
    }

    #[test]
    fn error_round_reports_failed_and_keeps_note() {
        let mut t = tracker();
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 0 }));
        t.observe(&round_event(RoundEvent::Error("provider blew up".into())));
        let row = t.row();
        assert_eq!(row.status, SessionStatus::Failed);
        assert_eq!(row.note.as_deref(), Some("provider blew up"));
    }

    #[test]
    fn interrupted_round_reports_interrupted() {
        let mut t = tracker();
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 0 }));
        t.observe(&round_event(RoundEvent::UnsentInput {
            prompt: "redo".into(),
            images: Vec::new(),
        }));
        assert_eq!(t.row().status, SessionStatus::Interrupted);
    }

    #[test]
    fn repeated_turn_starts_keep_one_open_round() {
        let mut t = tracker();
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 0 }));
        t.observe(&round_event(RoundEvent::TurnStarted { round: 1, turn: 1 }));
        let row = t.row();
        assert_eq!(row.round, 1, "turns of one round must not double-count");
        assert_eq!(row.turn, Some(1));
        assert_eq!(row.status, SessionStatus::Running);
    }
}
