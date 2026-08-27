//! Typed projection of the agent's activity vocabulary onto the TUI bar.
//!
//! The wire protocol deliberately delivers free-form [`RoundEvent::Activity`]
//! labels; historically every consumer re-parsed those strings, which let
//! transport states masquerade as workflow states ("waiting to retry"
//! overwriting the live label) and made the bar's grammar implicit. This
//! module is the **single fold point**: wire label → [`Phase`] at listener
//! level, and everything downstream (bar, modal, saved chrome) reads the
//! enum — never the string.
//!
//! Design rules pinned here (see `vocabulary_closure` test):
//!
//! 1. Every label the backend emits today folds into a named variant.
//! 2. Transport setbacks are *clauses*, never phases: `ProviderRetryState`
//!    rides beside the phase as a dim annotation and owns no label slot.
//! 3. Unknown future labels degrade to [`Phase::Other`] instead of being
//!    dropped — the bar never goes blank just because a new label shipped.

use std::borrow::Cow;

/// The closed set of live-round phases the activity bar can be in.
///
/// `None` (absence) means idle — the bar is hidden entirely. There is no
/// `Idle` variant by construction: absence of a phase *is* idle, which makes
/// "bar visible" ⇔ "phase is Some" a type-level invariant instead of a
/// string comparison against `"idle"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Admission acknowledged; the driver hasn't started building the
    /// request yet (`queued`, armed by the frontend itself).
    Queued,
    /// Local request assembly: `"starting request"` → `"saving request"` →
    /// `"preparing context"`.
    Preparing,
    /// The model request is in flight, waiting for the first byte.
    AwaitingModel,
    /// A reasoning (`thinking`) stream is actively producing deltas.
    Thinking,
    /// An answer (visible text) stream is actively producing deltas.
    Answering,
    /// Stream finished; the harness is persisting and settling the turn
    /// (`finalizing response`).
    Finalizing,
    /// An agent tool is executing locally. The verb is the stable subset the
    /// bar knows how to phrase.
    Tool(ToolVerb),
    /// The round is paused for a human decision (permission sheet or
    /// `ask_user`). Rendered in the warning hue.
    AwaitingUser,
    /// An unmapped wire label. Carries the raw text so a new backend label
    /// still surfaces verbatim until this fold learns it.
    Other(String),
}

/// Stable phrasings for local tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolVerb {
    Exploring,
    Searching,
    Editing,
    Running,
    UpdatingTasks,
    Delegating,
    Mcp,
    Generic,
}

impl ToolVerb {
    pub(crate) fn iter() -> impl Iterator<Item = Self> {
        [
            Self::Exploring,
            Self::Searching,
            Self::Editing,
            Self::Running,
            Self::UpdatingTasks,
            Self::Delegating,
            Self::Mcp,
            Self::Generic,
        ]
        .into_iter()
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Exploring => "exploring",
            Self::Searching => "searching codebase",
            Self::Editing => "making edits",
            Self::Running => "running command",
            Self::UpdatingTasks => "updating tasks",
            Self::Delegating => "running runner",
            Self::Mcp => "using MCP",
            Self::Generic => "using tool",
        }
    }
}

impl Phase {
    /// Fold a wire `Activity` label into the typed phase. Pure function of
    /// the string — no ambient state, safe to call anywhere.
    ///
    /// `document_tests`-style callers keep passing historical strings into
    /// transcript storage; this fold is only ever applied at the *live bar*
    /// sinks.
    pub fn classify(label: &str) -> Self {
        match label {
            "queued" => Self::Queued,
            "starting request" | "saving request" | "preparing context" => Self::Preparing,
            "waiting for model" => Self::AwaitingModel,
            // Historical primary-stream labels; today superseded by direct
            // delta stamping but kept folding for resilience.
            "responding" => Self::Answering,
            "finalizing response" => Self::Finalizing,
            // Permission / ask_user gate. Displayed with attention styling.
            "awaiting permission" | "awaiting user" => Self::AwaitingUser,
            // The bar's own canonical verb phrases (historically produced by
            // the TUI's `tool_verb_for` on `ToolCall`) fold back first —
            // before the `running <tool>` prefix form, because e.g.
            // "running command" is itself a canonical phrase.
            _ => match tool_verb_by_label(label) {
                Some(verb) => Self::Tool(verb),
                None if label.starts_with("running ") => {
                    Self::Tool(tool_verb(&label["running ".len()..]))
                }
                None => Self::Other(label.to_string()),
            },
        }
    }

    /// Bar text for this phase. Single source of phrasing for the bar and
    /// the modal's status section.
    pub fn label(&self) -> Cow<'_, str> {
        match self {
            Self::Queued => Cow::Borrowed("queued"),
            Self::Preparing => Cow::Borrowed("preparing context"),
            Self::AwaitingModel => Cow::Borrowed("waiting for model"),
            Self::Thinking => Cow::Borrowed("thinking"),
            Self::Answering => Cow::Borrowed("answering"),
            Self::Finalizing => Cow::Borrowed("finalizing response"),
            Self::Tool(verb) => Cow::Borrowed(verb.label()),
            Self::AwaitingUser => Cow::Borrowed("awaiting permission"),
            Self::Other(raw) => Cow::Owned(raw.clone()),
        }
    }

    /// True while the phase proves the round is paused on a human decision —
    /// rendered in the warning hue rather than ordinary activity.
    pub fn is_gate(self) -> bool {
        matches!(self, Self::AwaitingUser)
    }
}

fn tool_verb(name: &str) -> ToolVerb {
    match name {
        "find_files" | "list_dir" | "read_image" | "read_text" | "use_skill" | "fetch_url"
        | "webfetch" => ToolVerb::Exploring,
        "search_text" | "search_web" | "websearch" => ToolVerb::Searching,
        "write_file" | "edit_file" => ToolVerb::Editing,
        "run_command" | "execute_command" | "bash" => ToolVerb::Running,
        "write_todos" | "update_todo" | "todo" | "todo_update" => ToolVerb::UpdatingTasks,
        "spawn_runner" | "runner" | "runner_code" | "runner_mcp" => ToolVerb::Delegating,
        n if n.starts_with("mcp__") => ToolVerb::Mcp,
        _ => ToolVerb::Generic,
    }
}

fn tool_verb_by_label(label: &str) -> Option<ToolVerb> {
    ToolVerb::iter().find(|verb| verb.label() == label)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closure guarantee: every wire label the backend emits today folds into
    /// a *named* variant (never `Other`). Emitting a new label requires
    /// extending this table — the test fails first, which is the point.
    #[test]
    fn vocabulary_closure() {
        let known = [
            ("queued", Phase::Queued),
            ("starting request", Phase::Preparing),
            ("saving request", Phase::Preparing),
            ("preparing context", Phase::Preparing),
            ("waiting for model", Phase::AwaitingModel),
            ("responding", Phase::Answering),
            ("finalizing response", Phase::Finalizing),
            ("awaiting permission", Phase::AwaitingUser),
            ("exploring", Phase::Tool(ToolVerb::Exploring)),
            ("searching codebase", Phase::Tool(ToolVerb::Searching)),
            ("making edits", Phase::Tool(ToolVerb::Editing)),
            ("running command", Phase::Tool(ToolVerb::Running)),
            ("updating tasks", Phase::Tool(ToolVerb::UpdatingTasks)),
            ("running runner", Phase::Tool(ToolVerb::Delegating)),
        ];
        for (label, expected) in known {
            assert_eq!(Phase::classify(label), expected, "label {label:?}");
        }
    }

    #[test]
    fn unknown_labels_survive_as_other() {
        let folded = Phase::classify("brand new backend label");
        assert_eq!(folded, Phase::Other("brand new backend label".into()));
        assert_eq!(folded.label(), "brand new backend label");
    }

    /// Regression for the original sin: the transport setback must NOT own a
    /// master phase. No emitted wire label folds to a "retrying" phase; the
    /// backoff story is told exclusively by the clause channel.
    #[test]
    fn transport_setbacks_are_never_master_phases() {
        assert_ne!(
            Phase::classify("waiting for model").label(),
            "waiting to retry"
        );
        assert!(!matches!(
            Phase::classify("Retrying after backoff (2/8)"),
            Phase::AwaitingModel | Phase::Preparing | Phase::Queued
        ));
    }

    #[test]
    fn gate_phases_report_attention() {
        assert!(Phase::AwaitingUser.is_gate());
        assert!(!Phase::AwaitingModel.is_gate());
    }
}
