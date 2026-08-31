//! Public-API tests for embedding-owned prompt composition.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use muta_agent::{
    Agent, AgentIdentity, Message, Provider, Role, SystemPromptContext, SystemPromptRegistryError,
    SystemPromptSection,
};

struct IdleProvider;

#[async_trait]
impl Provider for IdleProvider {
    async fn chat(
        &self,
        _request: muta_contracts::ModelRequest,
    ) -> Result<muta_contracts::ProviderCompletion, muta_contracts::ProviderError> {
        Ok(muta_contracts::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "done",
        )))
    }

    async fn stream_chat(
        &self,
        _request: muta_contracts::ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, muta_contracts::ProviderError>>,
        muta_contracts::ProviderError,
    > {
        Ok(Box::pin(stream::once(async { Ok("done".to_owned()) })))
    }
}

struct ProductPolicy {
    id: &'static str,
    rank: u32,
    text: &'static str,
}

impl SystemPromptSection for ProductPolicy {
    fn id(&self) -> &'static str {
        self.id
    }

    fn rank(&self) -> u32 {
        self.rank
    }

    fn render(&self, _ctx: &SystemPromptContext) -> Option<String> {
        Some(self.text.to_owned())
    }
}

fn builder() -> muta_agent::AgentBuilder {
    Agent::builder(Arc::new(IdleProvider), Vec::new(), AgentIdentity::default())
}

#[test]
fn embedding_can_extend_and_disable_prompt_policy_before_build() {
    let agent = builder()
        .register_system_prompt_section(ProductPolicy {
            id: "system.embedding.test.policy",
            rank: 15,
            text: "PRODUCT-POLICY",
        })
        .unwrap()
        .disable_system_prompt_section("system.persistence")
        .unwrap()
        .build();

    let mut messages = vec![Message::new(Role::User, "hello")];
    agent.prepare_request_messages_debug(&mut messages);

    assert_eq!(messages[0].role, Role::System);
    assert!(messages[0].content.contains("PRODUCT-POLICY"));
    assert!(
        !messages[0]
            .content
            .contains("See the task through to a real result")
    );
}

#[test]
fn embedding_configuration_errors_are_structured() {
    let result = builder().register_system_prompt_section(ProductPolicy {
        id: "system.persistence",
        rank: 1,
        text: "collision",
    });
    assert!(matches!(
        result,
        Err(SystemPromptRegistryError::DuplicateId("system.persistence"))
    ));

    let result = builder().disable_system_prompt_section("system.missing");
    assert!(matches!(
        result,
        Err(SystemPromptRegistryError::UnknownId(id)) if id == "system.missing"
    ));
}

#[test]
fn host_environment_guidance_renders_in_default_prompt() {
    let agent = builder().build();
    let mut messages = vec![Message::new(Role::User, "test prompt")];
    agent.prepare_request_messages_debug(&mut messages);

    assert_eq!(messages[0].role, Role::System);
    assert!(
        messages[0]
            .content
            .contains("## Host Execution Environment"),
        "system message must include host environment section: {}",
        messages[0].content
    );
    assert!(
        messages[0]
            .content
            .contains("ALWAYS prefer built-in tools (`read_text`, `write_file`, `edit_file`"),
        "system message must emphasize built-in tools"
    );
}
