//! Harness Facets implementation (ADR-0211, revised by ADR-0214).
//!
//! Provides concrete ambient environmental capabilities bound to Agent instances.

use std::path::Path;

use async_trait::async_trait;
use muta_contracts::HarnessFacet;

/// Code Intelligence Facet powered by Tree-sitter (ADR-0211; ADR-0214).
///
/// Binds AST-level ambient capabilities to an Agent instance:
/// - Registers the on-demand `get_outline` inspection tool
/// - Exposes an optional pre-mutation gate that delegates to the single
///   `syntax_guard` production mutation lifecycle
///
/// Per ADR-0214 this facet no longer projects an automatic per-request Repo
/// Map: code structure enters the model context through scoped, authorized
/// tool retrieval (`get_outline`), not through an ambient request tail. It also
/// does not implement an independent validation policy: the write paths own
/// validation through `syntax_guard`, and this hook only routes to it so a
/// future integration cannot silently diverge.
#[derive(Debug, Clone)]
pub struct CodeIntelligenceFacet {
    read_only: bool,
}

impl Default for CodeIntelligenceFacet {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeIntelligenceFacet {
    /// Create a new code intelligence facet with mutation gating enabled.
    pub fn new() -> Self {
        Self { read_only: false }
    }

    /// Create a read-only variant (for Explore roles) that skips mutation
    /// gating but still equips the on-demand inspection tool.
    pub fn read_only() -> Self {
        Self { read_only: true }
    }
}

#[async_trait]
impl HarnessFacet for CodeIntelligenceFacet {
    fn name(&self) -> &'static str {
        "code_intelligence"
    }

    // ADR-0214: no automatic ambient projection. `project_temporary_context`
    // inherits the `HarnessFacet` default (None); code structure is provided
    // on demand via the registered `get_outline` tool and read-time analysis.

    fn intercept_file_mutation(&self, path: &Path, content: &str) -> Result<(), String> {
        if self.read_only {
            return Ok(());
        }
        // ADR-0214: the production mutation lifecycle (`syntax_guard`, invoked
        // by `edit_text` / `write_file`) owns validation. This hook must not
        // maintain a second, contradictory parser policy, so it routes to the
        // same one.
        match crate::tools::syntax_guard::verify_syntax(path, content) {
            crate::tools::syntax_guard::SyntaxCheckResult::Invalid(err) => Err(err),
            crate::tools::syntax_guard::SyntaxCheckResult::Valid => Ok(()),
        }
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
        let facet = CodeIntelligenceFacet::new();
        let p = Path::new("main.rs");
        assert!(facet.intercept_file_mutation(p, "fn broken(").is_err());
        assert!(facet.intercept_file_mutation(p, "fn ok() {}").is_ok());
    }

    #[test]
    fn read_only_facet_skips_interception() {
        let facet = CodeIntelligenceFacet::read_only();
        let p = Path::new("main.rs");
        assert!(facet.intercept_file_mutation(p, "fn broken(").is_ok());
    }

    /// ADR-0214: the facet gate must route to the same production policy the
    /// write tools use (`syntax_guard`), covering data formats too, rather than
    /// maintaining a second parser policy.
    #[test]
    fn facet_mutation_gate_uses_the_production_syntax_policy() {
        let facet = CodeIntelligenceFacet::new();
        assert!(facet
            .intercept_file_mutation(Path::new("config.json"), r#"{"a":}"#)
            .is_err());
        assert!(facet
            .intercept_file_mutation(Path::new("config.json"), r#"{"a":1}"#)
            .is_ok());
    }

    /// ADR-0214: the facet must not project any automatic per-request context
    /// (no ambient Repo Map), regardless of workspace contents.
    #[test]
    fn facet_projects_no_temporary_context() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("lib.rs");
        std::fs::write(&file_path, "pub struct Engine;\npub fn start() {}\n").unwrap();

        let facet = CodeIntelligenceFacet::new();
        assert!(facet.project_temporary_context(Some(dir.path())).is_none());
    }

    #[test]
    fn facet_provides_accompanying_tools() {
        let facet = CodeIntelligenceFacet::new();
        let tools = facet.accompanying_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "get_outline");
    }

    /// ADR-0214: `model_request` must not append an ambient code-structure
    /// outline; the assembled window equals the filtered source window plus
    /// only skill injections, and source messages stay unmutated.
    #[tokio::test]
    async fn model_request_injects_no_temporary_facet_context_and_does_not_mutate_source_messages()
    {
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

        let source_messages = vec![muta_contracts::Message::new(
            muta_contracts::Role::User,
            "Hello",
        )];

        let req = agent.model_request(&source_messages);

        // Source slice was completely unmutated
        assert_eq!(source_messages.len(), 1);

        // ADR-0214: no ambient repo-map tail is appended to the wire request.
        let wire_messages = &req.messages;
        assert_eq!(wire_messages.len(), source_messages.len());
        for (wire, source) in wire_messages.iter().zip(source_messages.iter()) {
            assert_eq!(wire.content, source.content);
        }
        assert!(wire_messages
            .iter()
            .all(|message| !message.content.contains("codebase-structure-outline")));
        assert!(req.temporary_context().is_empty());
    }

    /// A facet that contributes a known request-local payload, used to prove
    /// `E_n` lands in `temporary_context` and never in durable history
    /// (ADR-0213/0217).
    #[derive(Debug)]
    struct TestTemporaryContextFacet;

    #[async_trait]
    impl HarnessFacet for TestTemporaryContextFacet {
        fn name(&self) -> &'static str {
            "test_temporary_context"
        }

        fn project_temporary_context(&self, _root: Option<&Path>) -> Option<String> {
            Some("<temporary-context>request-local</temporary-context>".to_string())
        }
    }

    #[tokio::test]
    async fn temporary_facet_output_lands_in_temporary_context_not_history() {
        let provider = std::sync::Arc::new(crate::NoProvider);
        let agent = crate::Agent::new(
            provider,
            Vec::new(),
            crate::AgentIdentity::new("dev", "developer"),
        );
        agent.add_facet(std::sync::Arc::new(TestTemporaryContextFacet));

        let source = vec![muta_contracts::Message::new(
            muta_contracts::Role::User,
            "Hello",
        )];
        let req = agent.model_request(&source);

        // History is untouched; E is request-local.
        assert_eq!(req.messages.len(), source.len());
        assert!(req
            .messages
            .iter()
            .all(|message| !message.content.contains("<temporary-context>")));
        assert_eq!(req.temporary_context().len(), 1);
        assert!(req.temporary_context()[0]
            .content
            .contains("<temporary-context>"));

        // The cacheable prefix identity deliberately ignores E.
        let plan = req.cache_plan();
        let mut cleared = req.clone();
        cleared.temporary_context.clear();
        assert_eq!(plan.prefix_fingerprint, cleared.cache_plan().prefix_fingerprint);
        assert_eq!(plan.temporary_context_messages, 1);
    }
}
