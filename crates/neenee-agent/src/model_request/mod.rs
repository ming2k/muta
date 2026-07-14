//! Request-scoped model-request assembly.
//!
//! The assembler is intentionally independent of [`crate::Agent`]. The agent
//! owns when assembly occurs and supplies a plain state snapshot; this module
//! owns the pure projection from a live conversation window to one immutable
//! [`neenee_core::ModelRequest`].

mod policies;
pub(crate) mod system_prompt;

pub(crate) use policies::{default_system_prompt_registry, reviewer_system_prompt_registry};

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

    /// Materialize the exact request sent to a provider without changing the
    /// source window. Durable command echoes and legacy system messages stay in
    /// storage but are projected out; one freshly composed system message is
    /// inserted at the request head.
    pub(crate) fn assemble(
        &self,
        window: &[Message],
        context: &SystemPromptContext,
        tools: &[Arc<dyn Tool>],
    ) -> neenee_core::ModelRequest {
        let mut messages = window.to_vec();
        crate::agent::remove_empty_assistant_messages(&mut messages);
        messages.retain(|message| message.role != Role::System && !message.is_command_echo());
        messages.insert(0, self.system_prompt_registry.build_message(context));
        neenee_core::ModelRequest::with_tools(messages, tools)
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
            Ok(String::new())
        }
    }

    #[test]
    fn assembly_is_a_pure_atomic_projection() {
        let assembler = ModelRequestAssembler::new(SystemPromptRegistry::new());
        let source = vec![
            Message::new(Role::System, "legacy prompt"),
            Message::new(Role::User, "real prompt"),
            Message::command_echo("/title"),
            Message::new(Role::Assistant, ""),
        ];
        let tool: Arc<dyn Tool> = Arc::new(TestTool);

        let request = assembler.assemble(
            &source,
            &SystemPromptContext::empty(),
            std::slice::from_ref(&tool),
        );

        assert_eq!(
            source.len(),
            4,
            "assembly must not mutate its source window"
        );
        assert_eq!(source[0].role, Role::System);
        assert!(source[2].is_command_echo());

        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, Role::System);
        assert_eq!(request.messages[1].content, "real prompt");
        assert!(
            request
                .messages
                .iter()
                .all(|message| !message.is_command_echo())
        );
        assert_eq!(request.tool_specs.len(), 1);
        assert_eq!(request.tool_specs[0]["function"]["name"], "inspect");
    }
}
