//! The `AgentRequest::SlashCommand` dispatcher and domain sub-handlers.

pub mod dispatch;
pub mod record;
pub mod schedule_ops;
pub mod security_ops;
pub mod session_ops;

#[cfg(test)]
mod tests;

use std::sync::{Arc, RwLock};
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::side::SideRegistry;
use crate::slash_handler::SlashCommandRegistry;
use crate::startup::SessionStart;
use muta_agent::{Agent, RoundLifecycle};
use muta_contracts::{AgentRequest, AgentResponse, Provider, Tool};
use muta_mcp::McpRuntime;
use muta_persistence::{
    config::Config, connection_usage::ConnectionUsage, session::SessionStore,
    workspace_security::WorkspaceSecurityStore,
};
use muta_skills::SkillRegistry;

pub use dispatch::dispatch;
pub use session_ops::teardown_sides_for_session_switch;

/// Bundled slash-dispatch environment: the daemon plumbing a slash command
/// needs beyond the command text itself.
pub struct SlashEnv<'a> {
    pub config: &'a Config,
    pub agent: &'a Arc<Agent>,
    pub mcp_runtime: &'a Arc<McpRuntime>,
    pub workspace_security: &'a Arc<WorkspaceSecurityStore>,
    pub shared_additional_roots: &'a muta_contracts::SharedAdditionalRoots,
    pub shared_unconfined: &'a muta_contracts::SharedUnconfined,
    pub resp_tx: &'a mpsc::UnboundedSender<AgentResponse>,
    pub session: &'a Arc<SessionStore>,
    pub lifecycle: &'a Arc<RoundLifecycle>,
    pub side: &'a Arc<AsyncRwLock<SideRegistry>>,
    pub base_tools_for_side: &'a Arc<Vec<Arc<dyn Tool>>>,
    pub provider_for_task: &'a Arc<RwLock<Arc<dyn Provider>>>,
    pub provider_usage: &'a mut ConnectionUsage,
    pub skills_registry: &'a Arc<SkillRegistry>,
    pub req_tx_for_commands: &'a mpsc::UnboundedSender<AgentRequest>,
    pub project_root_for_side: &'a std::path::Path,
    pub startup: &'a SessionStart,
    pub ui: &'a dyn crate::UiBridge,
    pub extra_commands: &'a SlashCommandRegistry,
    pub websearch_shared: &'a Arc<muta_contracts::SharedWebSearchConfig>,
    pub background_jobs: &'a crate::background_jobs::BackgroundJobManager,
}
