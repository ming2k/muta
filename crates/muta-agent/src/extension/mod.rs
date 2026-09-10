//! Atomic harness extensions implementation (ADR-0224).
//!
//! `CodeIntelligenceExtension` is a hook-only extension that routes pending
//! file mutations through the single production syntax policy.

use muta_contracts::extension::{HookContext, HookOutcome};
use muta_contracts::{Extension, HookPhase};

/// Code-intelligence extension (ADR-0211, revised by ADR-0214 / ADR-0224).
///
/// Ambient behavior only: it declares the [`HookPhase::InterceptFileMutation`]
/// phase and routes that gate to the production `syntax_guard` policy the write
/// tools already use, so it cannot diverge. Per ADR-0214 it projects no ambient
/// repository map — code structure is retrieved on demand via the separately
/// registered `get_outline` tool.
#[derive(Debug, Clone)]
pub struct CodeIntelligenceExtension {
    read_only: bool,
}

impl Default for CodeIntelligenceExtension {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeIntelligenceExtension {
    /// Mutation gating enabled.
    pub fn new() -> Self {
        Self { read_only: false }
    }

    /// Read-only variant (for Explore roles): no mutation gating.
    pub fn read_only() -> Self {
        Self { read_only: true }
    }

    /// Validate a pending mutation, or `None` when allowed.
    pub fn check_mutation(&self, path: &std::path::Path, content: &str) -> Option<String> {
        if self.read_only {
            return None;
        }
        match crate::tools::syntax_guard::verify_syntax(path, content) {
            crate::tools::syntax_guard::SyntaxCheckResult::Invalid(err) => Some(err),
            crate::tools::syntax_guard::SyntaxCheckResult::Valid => None,
        }
    }
}

impl Extension for CodeIntelligenceExtension {
    fn id(&self) -> &str {
        "code_intelligence"
    }

    fn hooks(&self) -> &'static [HookPhase] {
        &[HookPhase::InterceptFileMutation]
    }

    fn run(&self, phase: HookPhase, ctx: &HookContext<'_>) -> HookOutcome {
        match phase {
            HookPhase::InterceptFileMutation => {
                let Some((path, content)) = ctx.mutation else {
                    return HookOutcome::None;
                };
                match self.check_mutation(path, content) {
                    Some(error) => HookOutcome::Block(error),
                    None => HookOutcome::None,
                }
            }
            HookPhase::ProjectTemporaryContext => HookOutcome::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn extension_blocks_broken_syntax() {
        let ext = CodeIntelligenceExtension::new();
        assert!(
            ext.check_mutation(Path::new("main.rs"), "fn broken(")
                .is_some()
        );
        assert!(
            ext.check_mutation(Path::new("main.rs"), "fn ok() {}")
                .is_none()
        );
    }

    #[test]
    fn read_only_extension_skips_interception() {
        let ext = CodeIntelligenceExtension::read_only();
        assert!(
            ext.check_mutation(Path::new("main.rs"), "fn broken(")
                .is_none()
        );
    }

    #[test]
    fn mutation_gate_uses_the_production_syntax_policy() {
        let ext = CodeIntelligenceExtension::new();
        assert!(
            ext.check_mutation(Path::new("config.json"), r#"{"a":}"#)
                .is_some()
        );
        assert!(
            ext.check_mutation(Path::new("config.json"), r#"{"a":1}"#)
                .is_none()
        );
    }

    #[test]
    fn hook_dispatch_blocks_and_allows() {
        let ext = CodeIntelligenceExtension::new();
        assert!(matches!(
            ext.run(
                HookPhase::InterceptFileMutation,
                &HookContext::mutation(Path::new("main.rs"), "fn broken(")
            ),
            HookOutcome::Block(_)
        ));
        assert_eq!(
            ext.run(
                HookPhase::InterceptFileMutation,
                &HookContext::mutation(Path::new("main.rs"), "fn ok() {}")
            ),
            HookOutcome::None
        );
        assert_eq!(
            ext.run(
                HookPhase::ProjectTemporaryContext,
                &HookContext::temporary_context(Some(Path::new("/tmp")))
            ),
            HookOutcome::None
        );
    }

    #[test]
    fn extension_declares_only_the_mutation_phase() {
        let ext = CodeIntelligenceExtension::new();
        assert_eq!(ext.id(), "code_intelligence");
        assert_eq!(ext.hooks(), &[HookPhase::InterceptFileMutation]);
    }

    /// ADR-0214: `model_request` must not append an ambient code-structure
    /// outline; the assembled window equals the filtered source window plus
    /// only skill injections, and source messages stay unmutated.
    #[tokio::test]
    async fn model_request_injects_no_temporary_context_and_does_not_mutate_source_messages() {
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

        assert_eq!(source_messages.len(), 1);
        let wire_messages = &req.messages;
        assert_eq!(wire_messages.len(), source_messages.len());
        for (wire, source) in wire_messages.iter().zip(source_messages.iter()) {
            assert_eq!(wire.content, source.content);
        }
        assert!(
            wire_messages
                .iter()
                .all(|message| !message.content.contains("codebase-structure-outline"))
        );
        assert!(req.temporary_context().is_empty());
    }
}
