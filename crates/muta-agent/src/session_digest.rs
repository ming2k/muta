//! Session-level AI digest runner (ADR-0022 evolution): the LLM-backed side
//! of the Chronicler's [`SessionDigestTask`].
//!
//! The digest is the session's *working-memory projection* — title, user
//! intent, and a running history checklist — structured so a resumed (or
//! merely revisited) session can be understood at a glance; the session
//! picker's detail view renders it. Like the retired title-only runner, the
//! domain vocabulary lives in `muta-contracts` ([`SessionDigest`],
//! [`clean_title`]); the provider call lives here next to the `Agent`. It is
//! a bounded, zero-tool, single-shot Steward consult — never a full ReAct
//! turn.
//!
//! ## Lifecycle
//!
//! [`Agent::generate_digest`] returns a cleaned digest or `None` (on
//! provider error, timeout, or an unparseable answer). Orchestration decides
//! *when*: the first admitted user round generates immediately — the opening
//! request alone names the session's title and intent — and later rounds
//! refresh the digest once the transcript has grown past its stored char
//! anchor, so a resumed session's digest stays representative without
//! consulting the Chronicler on every message.
//!
//! [`SessionDigest`]: muta_contracts::SessionDigest
//! [`SessionDigestTask`]: muta_contracts::SessionDigestTask
//! [`clean_title`]: muta_contracts::clean_title

#[cfg(test)]
use muta_contracts::Provider;
use muta_contracts::{Message, Role, SessionDigestInput};

use crate::agent::Agent;

/// Character budget for the transcript excerpt handed to the digest runner.
/// Generous enough to show the opening request and the recent arc (so a
/// refresh after a topic shift sees the new direction), bounded enough that
/// the call stays cheap. The opening user message is always included in full
/// because the digest's title/intent anchor on what the session is *about*.
const TRANSCRIPT_BUDGET_CHARS: usize = 3_000;

/// Post-processing caps: keep the digest terse and render-safe regardless of
/// what the model returns.
const INTENT_MAX_CHARS: usize = 320;
const HISTORY_ENTRY_MAX_CHARS: usize = 160;
/// Hard cap beyond the prompt's 12-entry guidance — a miscounting model
/// cannot flood the picker's detail view.
const HISTORY_MAX_ENTRIES: usize = 16;

impl Agent {
    /// Generate a session digest from `transcript`, or `None` on failure.
    ///
    /// `previous` is the stored digest (if any): the Chronicler revises it —
    /// title/intent shift only when the session's focus did, and the history
    /// checklist grows by merging — instead of starting from scratch. The
    /// model's answer is normalized by `clean_digest`.
    pub async fn generate_digest(
        &self,
        transcript: &[Message],
        previous: Option<&muta_contracts::SessionDigest>,
    ) -> Option<muta_contracts::SessionDigest> {
        let excerpt = serialize_for_digest(transcript, TRANSCRIPT_BUDGET_CHARS);
        if excerpt.trim().is_empty() {
            return None;
        }
        let previous_json = match previous {
            Some(digest) => serde_json::to_string(digest).ok(),
            None => None,
        };
        let digest = self
            .steward()
            .generate_digest(SessionDigestInput {
                excerpt,
                previous: previous_json,
            })
            .await?;
        clean_digest(digest)
    }
}

/// Normalize the model's digest: a cleaned title, a bounded one-line intent,
/// and flattened, capped history entries. Returns `None` when the title
/// cleans to nothing — a digest without a headline is not worth storing, and
/// the first-user-message fallback keeps rendering.
fn clean_digest(raw: muta_contracts::SessionDigest) -> Option<muta_contracts::SessionDigest> {
    let title = muta_contracts::clean_title(&raw.title)?;
    let intent = one_line_capped(&raw.intent, INTENT_MAX_CHARS)?;
    let history = raw
        .history
        .iter()
        .filter_map(|entry| one_line_capped(entry, HISTORY_ENTRY_MAX_CHARS))
        .take(HISTORY_MAX_ENTRIES)
        .collect();
    Some(muta_contracts::SessionDigest {
        title,
        intent,
        history,
    })
}

/// Flatten to one render-safe line (control chars would spill the picker
/// row), trim, and cap with an ellipsis. `None` for effectively-empty text.
fn one_line_capped(text: &str, max: usize) -> Option<String> {
    let flat: String = text.chars().filter(|c| !c.is_control()).collect();
    let trimmed = flat.trim();
    if trimmed.is_empty() {
        return None;
    }
    let count = trimmed.chars().count();
    let mut out: String = trimmed.chars().take(max).collect();
    if count > max {
        out.push('…');
    }
    Some(out)
}

/// Render `transcript` as a compact excerpt for the digest prompt.
///
/// The opening user message is always included in full (the digest's
/// title/intent anchor on it). Subsequent user/assistant turns are then
/// appended oldest-to-newest, each capped, until the budget is exhausted —
/// so a first-turn session shows its single exchange, and a refresh shows
/// the opening plus the recent arc. System and tool-role messages are
/// dropped: the digest narrates the dialogue's arc, not harness plumbing.
fn serialize_for_digest(transcript: &[Message], budget: usize) -> String {
    // Opening user message, in full, as the anchor.
    let mut opening: Option<&str> = None;
    for message in transcript {
        if message.role == Role::User && !message.hidden {
            opening = Some(message.content.as_str());
            break;
        }
    }

    // A digest captures what the session is *about*; a transcript with no
    // user traffic (e.g. only system/assistant turns) carries no digestible
    // intent, so it serializes to empty — `generate_digest` then returns
    // `None` and the first-user-message fallback keeps rendering.
    if opening.is_none() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    let mut total = 0usize;
    for message in transcript {
        if message.hidden || matches!(message.role, Role::System | Role::Tool) {
            continue;
        }
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            _ => continue,
        };
        let content = message.content.trim();
        if content.is_empty() {
            continue;
        }
        // Keep the opening user line unshortened (it is the primary signal);
        // cap later turns so the recent arc fits alongside it.
        let is_opening = opening.is_some_and(|o| o == content);
        let cap = if is_opening { usize::MAX } else { 200 };
        let body = truncate(content, cap);
        let line = format!("{role}: {body}");
        total = total.saturating_add(line.len() + 1);
        if total > budget && !lines.is_empty() {
            break;
        }
        lines.push(line);
    }
    lines.join("\n")
}

fn truncate(s: &str, max: usize) -> &str {
    let s = s.trim();
    if s.chars().count() <= max {
        return s;
    }
    let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use muta_contracts::{ModelRequest, SessionDigestTask, StewardTask};
    use std::sync::{Arc, Mutex};

    /// A provider double that returns a canned assistant message. Captures the
    /// last request so tests can assert the digest prompt shape and tool scope.
    struct CannedProvider {
        reply: String,
        last_messages: Mutex<Vec<Message>>,
        last_tool_specs: Mutex<Vec<muta_contracts::ToolSpec>>,
    }

    #[async_trait]
    impl Provider for CannedProvider {
        async fn chat(&self, request: ModelRequest) -> Result<Message, String> {
            *self.last_messages.lock().unwrap() = request.messages;
            *self.last_tool_specs.lock().unwrap() = request.tool_specs;
            Ok(Message::new(Role::Assistant, self.reply.clone()))
        }

        async fn stream_chat(
            &self,
            _request: ModelRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    async fn agent_with_reply(reply: &str) -> (Agent, Arc<CannedProvider>) {
        let provider = Arc::new(CannedProvider {
            reply: reply.to_string(),
            last_messages: Mutex::new(Vec::new()),
            last_tool_specs: Mutex::new(Vec::new()),
        });
        let agent = Agent::new(
            provider.clone(),
            Vec::new(),
            crate::AgentIdentity::default(),
        );
        (agent, provider)
    }

    fn transcript_of_opening(opening: &str) -> Vec<Message> {
        vec![Message::new(Role::User, opening)]
    }

    fn digest_json(title: &str, intent: &str, history: &[&str]) -> String {
        let history = history
            .iter()
            .map(|entry| format!("\"{entry}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{{\"title\":\"{title}\",\"intent\":\"{intent}\",\"history\":[{history}]}}")
    }

    #[tokio::test]
    async fn generate_digest_returns_cleaned_model_output() {
        let (agent, provider) = agent_with_reply(&digest_json(
            "Fix login button on mobile",
            "User wants the mobile login button working.",
            &["Reproduced the bug on iOS", "Patched button handler"],
        ))
        .await;

        let digest = agent
            .generate_digest(&transcript_of_opening("fix the login button"), None)
            .await
            .expect("digest parses");
        assert_eq!(digest.title, "Fix login button on mobile");
        assert_eq!(digest.intent, "User wants the mobile login button working.");
        assert_eq!(digest.history.len(), 2);

        // Zero tools — the Chronicler is cognitive infrastructure, not an agent.
        assert!(provider.last_tool_specs.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn generate_digest_normalizes_wrapped_output() {
        let inner = digest_json("Refactor auth", "Clean up auth.", &["Read auth.rs"]);
        let (agent, _) = agent_with_reply(&format!("```json\n{inner}\n```")).await;

        let digest = agent
            .generate_digest(&transcript_of_opening("refactor the auth"), None)
            .await
            .expect("fenced JSON parses");
        assert_eq!(digest.title, "Refactor auth");
    }

    #[tokio::test]
    async fn generate_digest_drops_blank_and_caps_overlong_history_entries() {
        let long_entry = "E ".repeat(120);
        let (agent, _) =
            agent_with_reply(&digest_json("T", "I", &[&long_entry, "  ", "kept"])).await;

        let digest = agent
            .generate_digest(&transcript_of_opening("work"), None)
            .await
            .expect("digest parses");
        assert_eq!(digest.history.len(), 2, "blank entries are dropped");
        let first = &digest.history[0];
        assert!(first.chars().count() <= HISTORY_ENTRY_MAX_CHARS + 1);
        assert!(first.ends_with('…'), "overlong entries are ellipsized");
        assert_eq!(digest.history[1], "kept");
    }

    #[test]
    fn one_line_capped_flattens_and_trims() {
        assert_eq!(
            one_line_capped("line one\nline two\ttab", 40).as_deref(),
            Some("line oneline twotab"),
            "control chars are stripped, not escaped"
        );
        assert_eq!(one_line_capped("  \n ", 40), None);
        let capped = one_line_capped("x".repeat(50).as_str(), 10).unwrap();
        assert_eq!(capped.chars().count(), 11);
        assert!(capped.ends_with('…'));
    }

    #[tokio::test]
    async fn generate_digest_none_on_empty_model_output() {
        let (agent, _) = agent_with_reply("").await;
        assert!(
            agent
                .generate_digest(&transcript_of_opening("hi"), None)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn generate_digest_none_on_empty_transcript() {
        let (agent, _) = agent_with_reply(&digest_json("T", "I", &[])).await;
        assert!(agent.generate_digest(&[], None).await.is_none());
    }

    #[tokio::test]
    async fn generate_digest_uses_chronicler_office_and_digest_task_prompt() {
        let (agent, provider) = agent_with_reply(&digest_json("T", "I", &[])).await;
        agent
            .generate_digest(&transcript_of_opening("a session about rust"), None)
            .await
            .expect("digest parses");
        let messages = provider.last_messages.lock().unwrap().clone();
        let system = messages
            .iter()
            .find(|message| message.role == Role::System)
            .expect("a system frame");
        assert!(
            system
                .content
                .ends_with(muta_contracts::SessionDigestTask.system_prompt()),
            "the task's system prompt frames the consult"
        );
        assert!(
            system.content.contains("Chronicler"),
            "the Chronicler office charter is embedded"
        );
        let _ = SessionDigestTask; // referenced for the import assertion
    }

    #[tokio::test]
    async fn generate_digest_sends_previous_digest_for_revision() {
        let (agent, provider) = agent_with_reply(&digest_json("T2", "I2", &[])).await;
        let previous = muta_contracts::SessionDigest {
            title: "Old title".to_string(),
            intent: "Old intent".to_string(),
            history: vec!["Did a thing".to_string()],
        };
        agent
            .generate_digest(&transcript_of_opening("continue"), Some(&previous))
            .await
            .expect("digest parses");
        let messages = provider.last_messages.lock().unwrap().clone();
        let user = messages
            .iter()
            .rev()
            .find(|message| message.role == Role::User)
            .expect("a user turn");
        assert!(user.content.contains("Previous digest (revise it)"));
        assert!(user.content.contains("Old title"));
    }

    #[test]
    fn serialize_for_digest_keeps_opening_in_full_and_caps_the_arc() {
        let opening = "Please fix the login flow end to end";
        let mut transcript = transcript_of_opening(opening);
        for turn in ["first reply", "second reply"] {
            transcript.push(Message::new(Role::Assistant, turn));
        }
        let serialized = serialize_for_digest(&transcript, TRANSCRIPT_BUDGET_CHARS);
        assert!(serialized.contains(opening));
        assert!(serialized.contains("assistant: first reply"));
        // Budget-enforcing path: a tight budget keeps the opening exchange
        // and stops before overflowing.
        let tight = serialize_for_digest(&transcript, 45);
        assert!(tight.contains(opening));
        assert!(tight.chars().count() <= 45 + 200); // opening + at most one capped line
    }
}
