//! Bridge between the dynamic-tool registry and the disclosure ledger
//! (stage 5 — MCP tools as loadable dynamic tools).
//!
//! When progressive disclosure is **off** (the default, and the historical
//! behavior), MCP tools flow straight into the toolset like any builtin: the
//! model sees every MCP tool's schema up front. When disclosure is **on**, MCP
//! tools become *loadable*: their names are advertised but their schemas are
//! injected only after the model calls `select_tools`. This keeps context
//! lean when many MCP servers expose many tools.
//!
//! This module is the single sync point that turns a dynamic-tool snapshot
//! into a [`DisclosureLedger`] loadable set, and the helper the
//! model-request assembly consults to decide which MCP tools' schemas reach
//! the model this turn.
//!
//! ### Why a separate module (not folded into ToolManager)
//!
//! ToolManager owns *classification* (builtin/user/mcp buckets); the ledger
//! owns *disclosure state* (loadable/loaded). The two concerns are orthogonal
//! — disclosure applies only to the `mcp` (and future `user`) buckets, and
//! only when enabled. Keeping the bridge separate lets ToolManager stay
//! disclosure-agnostic: it produces the raw mcp snapshot, and this module
//! applies the disclosure filter on top.

use std::sync::Arc;

use neenee_core::Tool;

use crate::disclosure_ledger::{DisclosureLedger, DynamicToolSchema};
use crate::dynamic_tools::{DynamicToolEntry, DynamicToolRegistry};

/// Whether progressive disclosure is active. Off by default; turned on by the
/// agent when the MCP tool count crosses a threshold (or via config). This is
/// a pure toggle read at model-request assembly time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisclosureMode(pub bool);

impl DisclosureMode {
    pub fn enabled(self) -> bool {
        self.0
    }
}

/// Snapshot the registry's dynamic (MCP) tools and refresh the ledger's
/// loadable set from them. Called whenever MCP servers connect/disconnect
/// (the registry's `replace`/`remove` already fire on those events; this
/// mirrors the change into the ledger).
///
/// Idempotent: re-syncing the same registry produces the same loadable set.
/// Tools whose source disappeared are dropped from `loaded` by the ledger's
/// own `set_loadable`.
pub fn sync_dynamic_to_ledger(registry: &DynamicToolRegistry, ledger: &DisclosureLedger) {
    let snapshot: Vec<DynamicToolEntry> = registry.snapshot();
    let schemas: Vec<DynamicToolSchema> = snapshot
        .into_iter()
        .map(|entry| DynamicToolSchema {
            name: entry.tool.name().to_string(),
            description: entry.tool.description().to_string(),
            parameters: entry.tool.parameters(),
        })
        .collect();
    ledger.set_loadable(schemas);
}

/// Decide which dynamic (MCP) tool schemas reach the model this turn.
///
/// - Disclosure **off**: every dynamic tool is schema-eligible (historical
///   behavior — full schemas up front).
/// - Disclosure **on**: only the ledger's *loaded* tools are schema-eligible;
///   the rest wait for `select_tools`.
///
/// Returns the tools (not just names) so the caller can pass them straight to
/// `ModelRequest::with_tools`.
pub fn schema_eligible_dynamic_tools(
    registry: &DynamicToolRegistry,
    ledger: &DisclosureLedger,
    mode: DisclosureMode,
) -> Vec<Arc<dyn Tool>> {
    let snapshot: Vec<DynamicToolEntry> = registry.snapshot();
    if !mode.enabled() {
        return snapshot.into_iter().map(|e| e.tool).collect();
    }
    let loaded: std::collections::HashSet<String> =
        ledger.loaded_names().into_iter().collect();
    snapshot
        .into_iter()
        .filter(|e| loaded.contains(&e.tool.name().to_string()))
        .map(|e| e.tool)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use async_trait::async_trait;
    use neenee_core::DynamicToolSink;
    use neenee_core::ToolAccesses;
    use std::sync::{Arc, RwLock};

    /// Minimal dynamic tool stub.
    struct DynTool {
        name: String,
    }
    #[async_trait]
    impl Tool for DynTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "a dynamic tool"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _a: &str) -> Result<String, String> {
            Ok("ok".into())
        }
        fn accesses(&self, _a: &str) -> ToolAccesses {
            ToolAccesses::none()
        }
    }

    fn registry_with(tools: Vec<Arc<dyn Tool>>) -> DynamicToolRegistry {
        let reg = DynamicToolRegistry::default();
        reg.replace("mcp:test", tools);
        reg
    }

    #[test]
    fn sync_populates_loadable_from_registry() {
        let reg = registry_with(vec![
            Arc::new(DynTool { name: "mcp__a__x".into() }),
            Arc::new(DynTool { name: "mcp__a__y".into() }),
        ]);
        let ledger = DisclosureLedger::default();
        sync_dynamic_to_ledger(&reg, &ledger);
        let mut names = ledger.loadable_names();
        names.sort();
        assert_eq!(names, vec!["mcp__a__x", "mcp__a__y"]);
    }

    #[test]
    fn disclosure_off_eligible_returns_all() {
        let reg = registry_with(vec![
            Arc::new(DynTool { name: "mcp__a__x".into() }),
            Arc::new(DynTool { name: "mcp__a__y".into() }),
        ]);
        let ledger = DisclosureLedger::default();
        sync_dynamic_to_ledger(&reg, &ledger);
        let eligible = schema_eligible_dynamic_tools(&reg, &ledger, DisclosureMode(false));
        assert_eq!(eligible.len(), 2, "disclosure off → all schemas");
    }

    #[test]
    fn disclosure_on_eligible_returns_only_loaded() {
        let reg = registry_with(vec![
            Arc::new(DynTool { name: "mcp__a__x".into() }),
            Arc::new(DynTool { name: "mcp__a__y".into() }),
        ]);
        let ledger = DisclosureLedger::default();
        sync_dynamic_to_ledger(&reg, &ledger);
        // Load only x.
        ledger.select(&["mcp__a__x".into()]);
        let eligible = schema_eligible_dynamic_tools(&reg, &ledger, DisclosureMode(true));
        assert_eq!(eligible.len(), 1, "disclosure on → only loaded");
        assert_eq!(eligible[0].name(), "mcp__a__x");
    }

    #[test]
    fn resync_after_disconnect_drops_loaded() {
        let reg = registry_with(vec![
            Arc::new(DynTool { name: "mcp__a__x".into() }),
            Arc::new(DynTool { name: "mcp__a__y".into() }),
        ]);
        let ledger = DisclosureLedger::default();
        sync_dynamic_to_ledger(&reg, &ledger);
        ledger.select(&["mcp__a__x".into(), "mcp__a__y".into()]);
        assert_eq!(ledger.loaded_names().len(), 2);
        // Server disconnects: only x remains.
        let reg2 = registry_with(vec![Arc::new(DynTool { name: "mcp__a__x".into() })]);
        sync_dynamic_to_ledger(&reg2, &ledger);
        assert_eq!(ledger.loaded_names(), vec!["mcp__a__x"]);
    }
}
