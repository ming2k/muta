//! Harness Facets: Ambient perception and invariant gating (ADR-0211).
//!
//! A [`HarnessFacet`] represents an ambient environmental capability equipped
//! by an [`crate::AgentRole`]. Unlike active [`crate::Tool`]s which require explicit
//! model-driven invocations, facets hook directly into the harness lifecycle:
//! - Contributing optional, bounded request-local temporary context (`E_n`)
//! - Routing pre-mutation validation through the production mutation lifecycle
//! - Registering companion read-only inspection tools
//!
//! Per ADR-0214 no facet projects an automatic repository-wide map; code
//! structure is retrieved on demand, and any temporary-context contribution is
//! finite and request-local.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

/// Contract for ambient harness facets bound to an AgentRole (ADR-0211).
#[async_trait]
pub trait HarnessFacet: Send + Sync + std::fmt::Debug {
    /// Unique facet identifier, e.g. "code_intelligence".
    fn name(&self) -> &'static str;

    /// Optional request-local temporary-context hook (ADR-0213 / ADR-0214).
    ///
    /// Contributes a bounded, relevance-gated payload to the request tail
    /// without committing it into the persistent conversation transcript. The
    /// default is empty; a producer must define a finite budget. Ambient
    /// repository-wide structures are not delivered here.
    fn project_temporary_context(&self, _workspace_root: Option<&Path>) -> Option<String> {
        None
    }

    /// Optional pre-mutation validation hook.
    ///
    /// The production mutation lifecycle (the write tools' syntax guard) owns
    /// validation. A facet that participates must route to that same policy
    /// rather than maintain an independent, contradictory implementation.
    fn intercept_file_mutation(&self, _path: &Path, _content: &str) -> Result<(), String> {
        Ok(())
    }

    /// Optional companion read-only tools registered to the agent's tool catalog.
    fn accompanying_tools(&self) -> Vec<Arc<dyn crate::Tool>> {
        Vec::new()
    }
}
