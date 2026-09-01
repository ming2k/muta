//! Request-scoped model-request assembly (ADR-0056 / ADR-0160).
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

    /// Project a conversation window without mutating historical nodes.
    pub(crate) fn assemble(
        &self,
        window: &[Message],
        context: &SystemPromptContext,
        tools: &[Arc<dyn Tool>],
    ) -> muta_contracts::ModelRequest {
        let mut messages = window.to_vec();
        crate::agent::remove_empty_assistant_messages(&mut messages);
        messages.retain(|message| message.role != Role::System && !message.is_command_echo());
        let instructions = self.system_prompt_registry.build_bundle(context);
        muta_contracts::ModelRequest::with_instructions_and_tools(instructions, messages, tools)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Transcript turns are pure conversation messages.
        assert_eq!(request.messages[0].role, Role::User);
        assert_eq!(request.messages[0].content, "hello");
        assert_eq!(request.messages[1].role, Role::Assistant);
        assert_eq!(request.messages[1].content, "hi");
        // Tool spec travels with the request.
        assert_eq!(request.tool_specs.len(), 1);
        assert_eq!(request.tool_specs[0].name, "inspect");
        // Repeat assembly of the same window yields identical bytes.
        let again = assembler.assemble(&window, &context, &[tool]);
        assert_eq!(
            request.instructions, again.instructions,
            "system prompt instructions must be byte-stable across assemblies"
        );
    }
}
