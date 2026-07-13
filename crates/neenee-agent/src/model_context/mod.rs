//! Model-context assembly for provider requests.
//!
//! This is the single agent-layer home for harness-authored messages that the
//! model can see. The system message is composed from ranked policy sections;
//! event-driven user context is built through the typed helpers in
//! [`messages`]; request preparation applies the final projection immediately
//! before a provider call.

mod messages;
mod skills;
mod system;

pub(crate) use messages::{hidden_user, hidden_user_with_reason, tool_image, visible_user};
pub(crate) use system::{default_system_prompt_registry, reviewer_system_prompt_registry};

use crate::{Agent, Message, Role, SystemPromptContext};

impl Agent {
    /// Snapshot the live state used by system-prompt sections.
    ///
    /// The value is rebuilt before every provider request and owns its data, so
    /// policy sections never borrow the mutable agent or transcript.
    pub(crate) fn build_system_prompt_context(&self) -> SystemPromptContext {
        let tool_names = self
            .visible_tools()
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        let model_guidance = neenee_core::resolve_model(&self.provider.model()).model_guidance;
        let provider_guidance = self.provider.prompt_hints().system_guidance;

        SystemPromptContext {
            identity_preamble: self.identity.preamble(),
            pursuit: self.get_pursuit(),
            tool_names,
            model_guidance,
            provider_guidance,
            unattended: self.get_unattended(),
        }
    }

    /// Rebuild the singleton system message and place it at the transcript head.
    pub(crate) fn ensure_system_message(&self, messages: &mut Vec<Message>) {
        let context = self.build_system_prompt_context();
        let system = self.system_prompt_registry.build_message(&context);
        match messages.first_mut() {
            Some(first) if first.role == Role::System => *first = system,
            _ => messages.insert(0, system),
        }
    }

    /// Prepare the exact model-visible message list for the next provider request.
    ///
    /// Both streaming and non-streaming loops pass through this chokepoint.
    pub(crate) fn prepare_request_messages(&self, messages: &mut Vec<Message>) {
        crate::agent::remove_empty_assistant_messages(messages);
        // Command echoes remain durable and visible in the session transcript,
        // but are non-driving and must never reach a provider (ADR-0050).
        messages.retain(|message| !message.is_command_echo());
        self.ensure_system_message(messages);
        skills::inject_mentioned(self, messages);
    }
}
