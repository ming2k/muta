//! Request-scoped model-request assembly.
//!
//! The assembler is intentionally independent of [`crate::Agent`]. The agent
//! owns when assembly occurs and supplies a plain state snapshot; this module
//! owns the pure projection from a live conversation window to one immutable
//! [`muta_contracts::ModelRequest`].

pub mod policies;
pub(crate) mod system_prompt;

pub(crate) use policies::default_system_prompt_registry;

use std::sync::Arc;

use crate::{Message, Role, SystemPromptContext, SystemPromptRegistry, Tool};

/// Pure request projector configured with the system-prompt policy for one
/// agent. It owns no live agent state and performs no persistence.
pub(crate) struct ModelRequestAssembler {
    system_prompt_registry: SystemPromptRegistry,
}

impl ModelRequestAssembler {
    pub(crate) fn new(system_prompt_registry: SystemPromptRegistry) -> Self {
        Self {
            system_prompt_registry,
        }
    }

    pub(crate) fn registry_mut(&mut self) -> &mut SystemPromptRegistry {
        &mut self.system_prompt_registry
    }

    pub(crate) fn replace_registry(&mut self, registry: SystemPromptRegistry) {
        self.system_prompt_registry = registry;
    }

    /// Project historical tool outputs into their provider shape **without
    /// mutation**. Live windows were already frozen in place by the turn loop
    /// (`freeze_for_cache_stability`), so the scan below is a flag-checked
    /// no-op for them. For windows persisted by older builds (no
    /// `cache_frozen` flags), it assigns each historical tool output its
    /// final shape deterministically — the same input always yields the same
    /// bytes — keeping the resumed session's projection stable for its whole
    /// lifetime (KV-cache alignment, ADR-0137).
    pub(crate) fn assemble(
        &self,
        window: &[Message],
        context: &SystemPromptContext,
        tools: &[Arc<dyn Tool>],
    ) -> muta_contracts::ModelRequest {
        let mut messages = window.to_vec();
        crate::agent::remove_empty_assistant_messages(&mut messages);
        messages.retain(|message| message.role != Role::System && !message.is_command_echo());
        freeze_for_cache_stability(&mut messages, 6);
        messages.insert(0, self.system_prompt_registry.build_message(context));
        muta_contracts::ModelRequest::with_tools(messages, tools)
    }
}

/// Freeze historical tool outputs **in the live window**, in place, once
/// each. This is the canonical freeze point: called by the turn loop right
/// before request assembly, it gives every tool result exactly one shape
/// transition in its lifetime — full fidelity while it sits inside the
/// `recent`-message protection window, then one deterministic freeze when it
/// slides out, then byte-stability forever (the `cache_frozen` flag makes
/// later passes no-ops). Server-side KV-cache prefixes stay aligned from the
/// round after the freeze onward (ADR-0137). Also invoked by the assembler so
/// windows persisted by pre-freeze builds settle into the same frozen shapes.
pub(crate) fn freeze_for_cache_stability(messages: &mut [Message], recent: usize) {
    let total = messages.len();
    if total <= recent {
        return;
    }
    for msg in &mut messages[..total - recent] {
        if msg.role == Role::Tool && !msg.cache_frozen && msg.content.len() > 1200 {
            msg.content = muta_contracts::pressure::freeze_tool_output(&msg.content);
            msg.cache_frozen = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::Role;

    struct TestTool;

    #[async_trait::async_trait]
    impl Tool for TestTool {
        fn name(&self) -> &str {
            "inspect"
        }

        fn description(&self) -> &str {
            "Inspect the request boundary"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("ok".to_string())
        }
    }

    fn big_tool_content() -> String {
        (0..50)
            .map(|i| format!("line {i}: some large output data"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn freeze_is_single_pass_and_idempotent() {
        let content = big_tool_content();
        let mut messages = vec![
            Message::new(Role::User, "q1"),
            Message::new(Role::Assistant, "calling tool"),
            Message::new(Role::Tool, content.clone()),
            Message::new(Role::Assistant, "answered q1"),
            Message::new(Role::User, "q2"),
            Message::new(Role::Assistant, "calling tool 2"),
            Message::new(Role::Tool, content.clone()),
            Message::new(Role::Assistant, "answered q2"),
        ];

        // Preserve last 4 messages, freeze older ones.
        freeze_for_cache_stability(&mut messages, 4);

        // The first tool message is frozen once, with a representative prefix.
        assert!(messages[2].cache_frozen);
        assert!(
            messages[2]
                .content
                .contains("[... Previous turn output compacted")
        );
        assert!(messages[2].content.starts_with("line 0:"));
        assert!(messages[2].content.len() < content.len());
        // Inside the recency window: untouched.
        assert!(!messages[6].cache_frozen);
        assert_eq!(messages[6].content, content);

        // Sliding the window must NOT re-freeze: byte-stability is the whole
        // point (KV-cache prefix alignment, ADR-0137).
        let frozen_before = messages[2].content.clone();
        freeze_for_cache_stability(&mut messages, 0);
        assert_eq!(messages[2].content, frozen_before);
        assert!(messages[6].cache_frozen);
        let frozen_recent = messages[6].content.clone();
        freeze_for_cache_stability(&mut messages, 0);
        assert_eq!(messages[6].content, frozen_recent);
    }

    #[test]
    fn assemble_projects_window_deterministically() {
        let tool: Arc<dyn Tool> = Arc::new(TestTool);
        let assembler = ModelRequestAssembler::new(SystemPromptRegistry::new());
        let window = vec![
            Message::new(Role::User, "hello"),
            Message::new(Role::Assistant, "hi"),
        ];
        let context = SystemPromptContext::empty();
        let request = assembler.assemble(&window, &context, &[Arc::clone(&tool)]);
        // Head system message stamped at position 0, transcript follows in order.
        assert_eq!(request.messages[0].role, Role::System);
        assert_eq!(request.messages[1].content, "hello");
        assert_eq!(request.messages[2].content, "hi");
        // Tool spec travels with the request.
        assert_eq!(request.tool_specs.len(), 1);
        assert_eq!(request.tool_specs[0].name, "inspect");
        // Repeat assembly of the same window yields identical bytes (the
        // memoized prompt + projection are pure functions of the inputs).
        let again = assembler.assemble(&window, &context, &[tool]);
        assert_eq!(
            request.messages[0].content, again.messages[0].content,
            "system prompt must be byte-stable across assemblies"
        );
    }

    #[test]
    fn freeze_tool_output_is_idempotent() {
        let content = big_tool_content();
        let once = muta_contracts::pressure::freeze_tool_output(&content);
        let twice = muta_contracts::pressure::freeze_tool_output(&once);
        assert_eq!(once, twice);
    }
}
