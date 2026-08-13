//! Typed slash-command records and results (ADR-0091).
//!
//! A `/` command is an *operation on the session*, not a conversation turn.
//! ADR-0091 therefore records each invocation in a durable **command ledger**
//! ([`CommandRecord`]) on the session instead of in the message stream, and
//! gives every command a typed [`CommandResult`] — the schema for its reply,
//! its text rendering ([`CommandResult::to_text`]), and its persisted form —
//! following the ADR-0001 `ToolOutput` precedent (Strangler migration from the
//! `Text` / `Error` bridge variants).
//!
//! Legacy sessions that still carry `Message::command_echo` rows fold into
//! this ledger at schema migration time with `result: None` (the invocation is
//! recorded, the reply was never persisted).

use serde::{Deserialize, Serialize};

use crate::session_review::ReviewVerdict;

/// Terminal status of a slash-command invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// The command completed and produced a result.
    Success,
    /// The command failed (an `AgentResponse::Error` was surfaced).
    Error,
    /// The command was aborted by the user before completing.
    UserCancelled,
}

/// One hit from a `/search` over the session-history embedding store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    /// The matched transcript excerpt.
    pub text: String,
    /// Embedding similarity in `[0, 1]`.
    pub score: f32,
}

/// Typed result of a slash command.
///
/// Each variant is the *schema* for one command family's reply. The closed
/// enum is the "return-result constraint": a new command must choose a
/// variant, and a new variant must implement [`CommandResult::to_text`] and
/// serde — enforced by exhaustiveness, so command replies can never drift into
/// unstructured strings that a consumer would have to string-sniff.
///
/// Serde uses the default externally-tagged representation, matching
/// [`ToolOutput`](crate::ToolOutput) (ADR-0001) — the precedessor this type
/// mirrors. The ledger (`session.commands`) persists these directly, so resume
/// and `/export` reconstruct the full result without re-running the command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CommandResult {
    /// Plain text / markdown. The back-compat bridge variant produced by any
    /// handler that has not yet migrated to a rich variant (the ADR-0001
    /// Strangler pattern): the reply is still recorded and rendered, just not
    /// schema-shaped yet.
    Text(String),
    /// A structured error, distinct from a successful textual reply that
    /// merely starts with "Error".
    Error {
        message: String,
        detail: Option<String>,
    },
    /// A one-line confirmation of a state change (the durable twin of the
    /// ADR-0088 `CommandAck` toast). The live surface stays a transient toast;
    /// the ledger keeps the confirmation for audit.
    Ack { title: String },
    /// `/permissions` — the current always-allowed tool rules.
    PermissionList { allowed: Vec<String> },
    /// `/search <query>` — semantic hits over the session-history store.
    Search { query: String, hits: Vec<SearchHit> },
    /// `/session status` — the live session's identity and window shape.
    SessionStatus {
        id: String,
        parent_id: Option<String>,
        message_count: usize,
        archived_count: usize,
        last_projection: Option<String>,
    },
    /// `/review` — the on-demand diagnostic verdicts (ADR-0018).
    Review {
        verdicts: Vec<ReviewVerdict>,
        turns: u64,
    },
    /// `/schedule` / `/repeat` — a registered scheduled prompt.
    Scheduled {
        /// `"cron"` / `"countdown"` / `"absolute"` (from `Schedule::kind_label`).
        kind: String,
        /// The short job id shown to the user.
        id: String,
        /// Human-readable trigger, e.g. `"every 5 minutes"` / `"in 10 minutes"`.
        trigger: String,
        /// Next fire time as `YYYY-MM-DD HH:MM`, plus `" Running now."` for
        /// recurring cron jobs.
        next: String,
    },
}

impl CommandResult {
    /// The text scheme: how this result renders for live display, resume, and
    /// `/export`. This is the single renderer for every consumer — the TUI
    /// calls it to display the command block, export calls it for the markdown
    /// block, resume reads it straight off the persisted variant.
    pub fn to_text(&self) -> String {
        match self {
            CommandResult::Text(text) => text.clone(),
            CommandResult::Error { message, detail } => {
                let mut out = format!("Error: {message}");
                if let Some(detail) = detail
                    && !detail.trim().is_empty()
                {
                    out.push('\n');
                    out.push_str(detail);
                }
                out
            }
            CommandResult::Ack { title } => title.clone(),
            CommandResult::PermissionList { allowed } => {
                if allowed.is_empty() {
                    "No tools are always allowed for this process.".to_string()
                } else {
                    format!("Always-allowed tools:\n- {}", allowed.join("\n- "))
                }
            }
            CommandResult::Search { hits, .. } => {
                if hits.is_empty() {
                    "No relevant history found.".to_string()
                } else {
                    let mut lines = vec!["Relevant history (most similar first):".to_string()];
                    for (i, hit) in hits.iter().enumerate() {
                        lines.push(format!("{}. [score={:.3}]\n{}", i + 1, hit.score, hit.text));
                    }
                    lines.join("\n\n")
                }
            }
            CommandResult::SessionStatus {
                id,
                parent_id,
                message_count,
                archived_count,
                last_projection,
            } => {
                format!(
                    "Session: {id}\nForked from: {}\nModel-window messages: {message_count}\n\
                     Archived transcript messages: {archived_count}\nLast context projection: {}",
                    parent_id.as_deref().unwrap_or("none"),
                    last_projection.as_deref().unwrap_or("none"),
                )
            }
            CommandResult::Review { verdicts, turns } => review_to_text(verdicts, *turns),
            CommandResult::Scheduled {
                kind,
                id,
                trigger,
                next,
            } => format!("Scheduled {kind} job {id} ({trigger}), next {next}."),
        }
    }

    /// Whether this result is a pure acknowledgment (the durable twin of a
    /// toast). Frontends may render it more tersely than a content result.
    pub fn is_ack(&self) -> bool {
        matches!(self, CommandResult::Ack { .. })
    }
}

/// One durable slash-command invocation with its structured result.
///
/// Persisted on the session as the `commands` ledger (ADR-0091). The message
/// stream is pure dialogue; these records are the operations that happened.
/// `result: None` means the invocation is recorded but the reply was never
/// persisted (the legacy-echo fold and the shell-passthrough case).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRecord {
    /// Command word without the leading slash (e.g. `"search"`), or `"shell"`
    /// for a `!command` passthrough.
    pub name: String,
    /// Raw argument remainder after the command word (empty when none).
    pub args: String,
    pub status: CommandStatus,
    pub result: Option<CommandResult>,
    /// Unix-epoch milliseconds at invocation.
    pub timestamp: u64,
    /// Wall-clock duration of the command run, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl CommandRecord {
    pub fn new(name: impl Into<String>, args: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            args: args.into(),
            status: CommandStatus::Success,
            result: None,
            timestamp: crate::todos::unix_now().saturating_mul(1000),
            duration_ms: None,
        }
    }

    pub fn with_result(mut self, result: CommandResult) -> Self {
        self.status = match result {
            CommandResult::Error { .. } => CommandStatus::Error,
            _ => CommandStatus::Success,
        };
        self.result = Some(result);
        self
    }

    pub fn with_error(mut self, message: impl Into<String>, detail: Option<String>) -> Self {
        self.status = CommandStatus::Error;
        self.result = Some(CommandResult::Error {
            message: message.into(),
            detail,
        });
        self
    }

    pub fn with_status(mut self, status: CommandStatus) -> Self {
        self.status = status;
        self
    }

    pub fn with_duration_ms(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }
}

/// Mirror of `neenee-transport::review::format_review_report`, kept here so
/// `CommandResult::to_text` owns the rendering and transport stays a thin
/// caller. Kept in sync with the transport report by construction (the TUI
/// renders the persisted variant through this function on resume).
fn review_to_text(verdicts: &[ReviewVerdict], turns: u64) -> String {
    let turn_unit = if turns == 1 { "turn" } else { "turns" };
    let worst = verdicts.iter().map(|v| v.status).max();
    let headline = match worst {
        None => {
            return format!(
                "Session review (~{turns} {turn_unit}): no review dimensions registered."
            );
        }
        Some(crate::session_review::ReviewStatus::Healthy) => {
            format!("Session review (~{turns} {turn_unit}): no concerns found.")
        }
        Some(status) => {
            format!(
                "Session review (~{turns} {turn_unit}) — verdict: {}.",
                status.label()
            )
        }
    };
    let mut lines = vec![headline];
    for verdict in verdicts {
        let detail = verdict.detail.trim();
        if detail.is_empty() {
            lines.push(format!(
                "  • {} — {}",
                verdict.dimension,
                verdict.status.label()
            ));
        } else {
            lines.push(format!(
                "  • {} — {}: {}",
                verdict.dimension,
                verdict.status.label(),
                detail
            ));
        }
    }
    lines.push("Interrupt the turn with Esc if it looks stuck.".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_result_round_trips_through_json() {
        // The ledger persists CommandResult directly; every variant must
        // survive a serde round trip so resume reconstructs the exact reply.
        let cases = vec![
            CommandResult::Text("hello".to_string()),
            CommandResult::Error {
                message: "boom".to_string(),
                detail: Some("detail".to_string()),
            },
            CommandResult::Ack {
                title: "Autopilot ON".to_string(),
            },
            CommandResult::PermissionList {
                allowed: vec!["bash".to_string(), "edit".to_string()],
            },
            CommandResult::Search {
                query: "foo".to_string(),
                hits: vec![SearchHit {
                    text: "match".to_string(),
                    score: 0.42,
                }],
            },
            CommandResult::SessionStatus {
                id: "abc".to_string(),
                parent_id: Some("def".to_string()),
                message_count: 3,
                archived_count: 7,
                last_projection: Some("compact: 100 -> 50 chars".to_string()),
            },
            CommandResult::Review {
                verdicts: vec![ReviewVerdict::healthy("progress")],
                turns: 2,
            },
            CommandResult::Scheduled {
                kind: "cron".to_string(),
                id: "abcd1234".to_string(),
                trigger: "every 5 minutes".to_string(),
                next: "2026-02-18 10:00 Running now.".to_string(),
            },
        ];
        for case in cases {
            let json = serde_json::to_string(&case).unwrap();
            let restored: CommandResult = serde_json::from_str(&json).unwrap();
            // Compare via to_text — CommandResult has no PartialEq.
            assert_eq!(restored.to_text(), case.to_text());
        }
    }

    #[test]
    fn to_text_renders_each_variant_distinctly() {
        assert_eq!(
            CommandResult::PermissionList { allowed: vec![] }.to_text(),
            "No tools are always allowed for this process."
        );
        assert_eq!(
            CommandResult::PermissionList {
                allowed: vec!["bash".to_string()],
            }
            .to_text(),
            "Always-allowed tools:\n- bash"
        );
        assert_eq!(
            CommandResult::Search {
                query: "q".to_string(),
                hits: vec![],
            }
            .to_text(),
            "No relevant history found."
        );
        assert_eq!(
            CommandResult::Error {
                message: "bad".to_string(),
                detail: None,
            }
            .to_text(),
            "Error: bad"
        );
        assert!(
            CommandResult::Ack {
                title: "x".to_string(),
            }
            .is_ack()
        );
        assert!(!CommandResult::Text("x".to_string()).is_ack());
    }

    #[test]
    fn record_with_result_sets_status() {
        let ok = CommandRecord::new("search", "foo").with_result(CommandResult::Text("ok".into()));
        assert_eq!(ok.status, CommandStatus::Success);
        assert_eq!(ok.result.as_ref().unwrap().to_text(), "ok");

        let err = CommandRecord::new("session", "open x")
            .with_error("no such session", Some("detail".into()));
        assert_eq!(err.status, CommandStatus::Error);
        assert!(matches!(err.result, Some(CommandResult::Error { .. })));
    }

    #[test]
    fn record_round_trips_through_json() {
        let record = CommandRecord::new("session", "status")
            .with_result(CommandResult::SessionStatus {
                id: "abc".to_string(),
                parent_id: None,
                message_count: 1,
                archived_count: 0,
                last_projection: None,
            })
            .with_duration_ms(12);
        let json = serde_json::to_string(&record).unwrap();
        let restored: CommandRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.name, "session");
        assert_eq!(restored.args, "status");
        assert_eq!(restored.status, CommandStatus::Success);
        assert_eq!(restored.duration_ms, Some(12));
        assert!(restored.result.is_some());
    }
}
