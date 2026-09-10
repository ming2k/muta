//! Harness Facets implementation (ADR-0211).
//!
//! Provides concrete ambient environmental capabilities bound to Agent instances.

use std::path::Path;

use async_trait::async_trait;
use muta_contracts::HarnessFacet;

/// Code Intelligence Facet powered by Tree-sitter (ADR-0211).
///
/// Binds AST-level ambient capabilities to an Agent instance:
/// - Projects true-ephemeral 1024-token Repo Map into Zone 3 (request tail)
/// - Performs 1ms pre-mutation syntax verification
#[derive(Debug, Clone)]
pub struct CodeIntelligenceFacet {
    token_budget: usize,
    read_only: bool,
}

impl Default for CodeIntelligenceFacet {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl CodeIntelligenceFacet {
    /// Create a new code intelligence facet with the specified token budget.
    pub fn new(token_budget: usize) -> Self {
        Self {
            token_budget,
            read_only: false,
        }
    }

    /// Create a read-only variant (for Explore roles) that only projects outlines
    /// but skips mutation gating.
    pub fn read_only(token_budget: usize) -> Self {
        Self {
            token_budget,
            read_only: true,
        }
    }
}

#[async_trait]
impl HarnessFacet for CodeIntelligenceFacet {
    fn name(&self) -> &'static str {
        "code_intelligence"
    }

    fn project_ephemeral_context(&self, workspace_root: Option<&Path>) -> Option<String> {
        let root = workspace_root?;
        let outline = crate::syntax::generate_repo_map(root, self.token_budget)?;
        Some(format!(
            "<codebase-structure-outline>\n{outline}</codebase-structure-outline>"
        ))
    }

    fn intercept_file_mutation(&self, path: &Path, content: &str) -> Result<(), String> {
        if self.read_only {
            return Ok(());
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        crate::syntax::verify_ast_syntax(ext, content)
    }

    fn accompanying_tools(&self) -> Vec<std::sync::Arc<dyn muta_contracts::Tool>> {
        vec![std::sync::Arc::new(crate::tools::GetOutlineTool::new(None))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn facet_intercepts_broken_syntax() {
        let facet = CodeIntelligenceFacet::new(1024);
        let p = Path::new("main.rs");
        assert!(facet.intercept_file_mutation(p, "fn broken(").is_err());
        assert!(facet.intercept_file_mutation(p, "fn ok() {}").is_ok());
    }

    #[test]
    fn read_only_facet_skips_interception() {
        let facet = CodeIntelligenceFacet::read_only(1024);
        let p = Path::new("main.rs");
        assert!(facet.intercept_file_mutation(p, "fn broken(").is_ok());
    }

    #[test]
    fn facet_projects_outline_from_workspace() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("lib.rs");
        std::fs::write(&file_path, "pub struct Engine;\npub fn start() {}\n").unwrap();

        let facet = CodeIntelligenceFacet::new(1024);
        let ctx = facet.project_ephemeral_context(Some(dir.path()));
        assert!(ctx.is_some());
        let text = ctx.unwrap();
        assert!(text.contains("codebase-structure-outline"));
        assert!(text.contains("pub struct Engine"));
    }

    #[test]
    fn facet_provides_accompanying_tools() {
        let facet = CodeIntelligenceFacet::new(1024);
        let tools = facet.accompanying_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "get_outline");
    }

    #[test]
    fn model_request_injects_ephemeral_facet_context_without_mutating_source_messages() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("main.rs");
        std::fs::write(&file_path, "pub struct AppState { pub id: u64 }\n").unwrap();

        let provider = std::sync::Arc::new(crate::NoProvider);
        let agent = crate::Agent::new(
            provider,
            Vec::new(),
            crate::AgentIdentity::new("dev", "developer"),
        );
        agent.set_project_root(Some(dir.path().to_path_buf()));

        let source_messages = vec![
            muta_contracts::Message::new(muta_contracts::Role::User, "Hello"),
        ];

        let req = agent.model_request(&source_messages);

        // Source slice was completely unmutated
        assert_eq!(source_messages.len(), 1);

        // Assembled wire request received Zone 3 ephemeral outline at tail
        let wire_messages = &req.messages;
        assert!(wire_messages.len() > source_messages.len());
        let tail_msg = wire_messages.last().unwrap();
        assert!(tail_msg.content.contains("codebase-structure-outline"));
        assert!(tail_msg.content.contains("pub struct AppState"));
    }
}
