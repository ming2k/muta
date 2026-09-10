//! Harness Facets: Ambient perception and invariant gating (ADR-0211).
//!
//! A [`HarnessFacet`] represents an ambient environmental capability equipped
//! by an [`crate::AgentRole`]. Unlike active [`crate::Tool`]s which require explicit
//! model-driven invocations, facets hook directly into the harness lifecycle:
//! - Projecting true-ephemeral context into Zone 3 (request tail)
//! - Intercepting mutations before disk writes (Phase 4 Tool Gating)
//! - Registering companion read-only inspection tools

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

/// Contract for ambient harness facets bound to an AgentRole (ADR-0211).
#[async_trait]
pub trait HarnessFacet: Send + Sync + std::fmt::Debug {
    /// Unique facet identifier, e.g. "code_intelligence".
    fn name(&self) -> &'static str;

    /// Zone 3 Ephemeral Context projection hook (ADR-0137 / ADR-0211).
    ///
    /// Contributes true-ephemeral context (such as 1024-token Repo Map or AST delta)
    /// to the request tail without committing into persistent conversation transcripts.
    fn project_ephemeral_context(&self, _workspace_root: Option<&Path>) -> Option<String> {
        None
    }

    /// Pre/post-mutation safety guard hook (Phase 4).
    ///
    /// Intercepts and validates file mutations before disk commit (e.g. 1ms Tree-sitter
    /// syntax validation to block broken brackets/delimiters).
    fn intercept_file_mutation(&self, _path: &Path, _content: &str) -> Result<(), String> {
        Ok(())
    }

    /// Optional companion read-only tools registered to the agent's tool catalog.
    fn accompanying_tools(&self) -> Vec<Arc<dyn crate::Tool>> {
        Vec::new()
    }
}
