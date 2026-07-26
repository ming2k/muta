//! On-demand dynamic-tool disclosure ledger (stage 4 machinery).
//!
//! Ports kimi-code's progressive-disclosure model: when the tool universe is
//! large (many MCP servers, many tools), declaring every tool's schema up
//! front bloats context and distracts the model. Instead, dynamic tools
//! (MCP + future user tools) are *loadable* — their names are advertised, but
//! their full schema is injected only after the model calls the built-in
//! `select_tools` tool.
//!
//! ### The ledger's single job
//!
//! Track, per agent, which dynamic tools are **loadable** (known to exist)
//! and which are **loaded** (schema already disclosed). It is the source of
//! truth that two consumers read:
//!
//! - **schema assembly** (`ModelRequest.tool_specs`): loaded dynamic tools are
//!   added to the spec list alongside builtins; loadable-but-not-loaded ones
//!   are *not*, so their schema never reaches the model until selected.
//! - **the `select_tools` tool**: consults `loadable` to validate a request,
//!   moves names into `loaded`, and returns the schemas to inject.
//!
//! ### Adaptation note (neenee vs kimi-code)
//!
//! kimi-code injects a loaded tool's schema by appending a `system` message
//! carrying a `tools` field (message-level schema). neenee has **no
//! message-level `tools` field** — schema reaches the model only via the
//! top-level `ModelRequest.tool_specs`. So the neenee adaptation is
//! "double-booked": selecting a tool both (a) records it in this ledger so
//! the next `tool_specs` assembly includes it, and (b) returns the schema
//! text so the dispatcher can push a hidden `DynamicToolSchema` injection
//! message (provenance for resume/undo). The ledger owns (a); the dispatcher
//! owns (b).
//!
//! ### History as truth (kimi-code invariant preserved)
//!
//! `loaded` is **rebuilt from conversation history** on resume/compaction,
//! not persisted separately — the in-memory set is only a "lead" over the
//! history scan. This means undo/compaction need no rollback logic: clearing
//! the ledger and re-scanning history reconstructs the correct loaded set.
//! (The scan helper lands with the model-request switchover; the ledger type
//! is the foundation.)

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// A loadable dynamic tool's schema payload — what `select_tools` injects.
#[derive(Debug, Clone)]
pub struct DynamicToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl DynamicToolSchema {
    /// Render as the OpenAI function-spec shape used in `tool_specs`.
    pub fn to_openai_function(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

/// The disclosure ledger: loadable schemas + the loaded-name set.
///
/// Thread-safe (`Arc<Mutex<...>>`) so the `select_tools` tool and the
/// model-request assembly can both read/write it from different async tasks.
/// Cheap to clone (one `Arc`).
#[derive(Clone, Default)]
pub struct DisclosureLedger {
    inner: Arc<Mutex<LedgerInner>>,
}

#[derive(Default)]
struct LedgerInner {
    /// Every dynamic tool known to be *loadable* (MCP `tools/list` results,
    /// future user-tool registrations). The model sees these *names*
    /// advertised (via a system reminder) but not their schema until loaded.
    loadable: Vec<DynamicToolSchema>,
    /// The subset whose schema has been disclosed this session. Rebuilt from
    /// history on resume (see module docs).
    loaded: HashSet<String>,
}

/// Outcome of a [`DisclosureLedger::select`] call — what the dispatcher turns
/// into an injected schema message.
#[derive(Debug, Clone)]
pub struct SelectionOutcome {
    /// Schemas newly loaded by this call (already loaded names are excluded).
    pub newly_loaded: Vec<DynamicToolSchema>,
    /// Names the model asked for but were already loaded (no-op).
    pub already_loaded: Vec<String>,
    /// Names the model asked for that don't exist (typo / stale).
    pub unknown: Vec<String>,
}

impl DisclosureLedger {
    /// Replace the loadable set (called when MCP servers connect/disconnect).
    /// Loaded names that are no longer loadable are dropped (their source is
    /// gone). Matches kimi-code's `loadableDynamicToolNames` recompute.
    pub fn set_loadable(&self, schemas: Vec<DynamicToolSchema>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let still_valid: HashSet<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
        inner.loaded.retain(|name| still_valid.contains(name.as_str()));
        inner.loadable = schemas;
    }

    /// Names of all loadable dynamic tools (the advertised catalogue).
    pub fn loadable_names(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .loadable
            .iter()
            .map(|s| s.name.clone())
            .collect()
    }

    /// Names whose schema is currently disclosed (drives `tool_specs`).
    pub fn loaded_names(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .loaded
            .iter()
            .cloned()
            .collect()
    }

    /// The schemas of all currently-loaded tools, for `tool_specs` assembly.
    pub fn loaded_schemas(&self) -> Vec<DynamicToolSchema> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .loadable
            .iter()
            .filter(|s| inner.loaded.contains(&s.name))
            .cloned()
            .collect()
    }

    /// Get one tool's schema by name (for injection). Returns `None` if the
    /// name isn't loadable or was already loaded.
    pub fn schema_for(&self, name: &str) -> Option<DynamicToolSchema> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.loaded.contains(name) {
            return None;
        }
        inner.loadable.iter().find(|s| s.name == name).cloned()
    }

    /// Mark `names` as loaded (advance the in-memory lead over history).
    /// Returns the schemas that were actually newly loaded.
    pub fn mark_loaded(&self, names: &[String]) -> Vec<DynamicToolSchema> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut newly = Vec::new();
        for name in names {
            if inner.loaded.contains(name) {
                continue;
            }
            // Clone out of the lock's immutable borrow before mutating.
            if let Some(schema) = inner.loadable.iter().find(|s| &s.name == name).cloned() {
                inner.loaded.insert(name.clone());
                newly.push(schema);
            }
        }
        newly
    }

    /// The full select operation: classify requested names and load the new
    /// ones. This is what the `select_tools` tool calls.
    pub fn select(&self, requested: &[String]) -> SelectionOutcome {
        let mut newly_loaded = Vec::new();
        let mut already_loaded = Vec::new();
        let mut unknown = Vec::new();
        // Dedup requested names while preserving deterministic order.
        let mut seen_req = HashSet::new();
        let mut ordered: Vec<String> = Vec::new();
        for name in requested {
            if seen_req.insert(name.as_str()) {
                ordered.push(name.clone());
            }
        }
        // One lock acquisition for the whole classify+load pass.
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for name in ordered {
            let found = inner.loadable.iter().find(|s| s.name == name).cloned();
            match found {
                None => unknown.push(name),
                Some(schema) => {
                    if inner.loaded.contains(&name) {
                        already_loaded.push(name);
                    } else {
                        inner.loaded.insert(name);
                        newly_loaded.push(schema);
                    }
                }
            }
        }
        // Stable, deterministic ordering for reproducible injection.
        newly_loaded.sort_by(|a, b| a.name.cmp(&b.name));
        already_loaded.sort();
        unknown.sort();
        SelectionOutcome {
            newly_loaded,
            already_loaded,
            unknown,
        }
    }

    /// Clear the loaded set (history-was-compacted / session-cleared). The
    /// loadable set stays; re-selection re-loads. Mirrors kimi-code's
    /// `onContextCleared` / `onContextCompacted`.
    pub fn clear_loaded(&self) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .loaded
            .clear();
    }

    /// Whether disclosure is active at all — i.e., whether there is anything
    /// loadable. When empty, `select_tools` is a no-op and need not be
    /// advertised.
    pub fn has_loadable(&self) -> bool {
        !self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .loadable
            .is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn schema(name: &str) -> DynamicToolSchema {
        DynamicToolSchema {
            name: name.to_string(),
            description: format!("tool {}", name),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn select_loads_new_and_skips_already_loaded() {
        let ledger = DisclosureLedger::default();
        ledger.set_loadable(vec![schema("mcp__a__x"), schema("mcp__a__y")]);
        let out = ledger.select(&["mcp__a__x".into()]);
        assert_eq!(out.newly_loaded.len(), 1);
        assert_eq!(out.newly_loaded[0].name, "mcp__a__x");
        assert!(out.already_loaded.is_empty());
        assert!(out.unknown.is_empty());

        // Second select of the same name → already_loaded, not re-loaded.
        let out2 = ledger.select(&["mcp__a__x".into()]);
        assert!(out2.newly_loaded.is_empty());
        assert_eq!(out2.already_loaded, vec!["mcp__a__x"]);
    }

    #[test]
    fn select_reports_unknown_names_without_failing_batch() {
        let ledger = DisclosureLedger::default();
        ledger.set_loadable(vec![schema("mcp__a__x")]);
        let out = ledger.select(&["mcp__a__x".into(), "typo".into(), "mcp__a__x".into()]);
        assert_eq!(out.newly_loaded.len(), 1);
        assert_eq!(out.unknown, vec!["typo"]);
        // Dedup: mcp__a__x requested twice but loaded once.
        assert_eq!(out.newly_loaded[0].name, "mcp__a__x");
    }

    #[test]
    fn loaded_schemas_drives_tool_specs() {
        let ledger = DisclosureLedger::default();
        ledger.set_loadable(vec![schema("a"), schema("b"), schema("c")]);
        assert!(ledger.loaded_schemas().is_empty());
        ledger.select(&["b".into()]);
        let loaded = ledger.loaded_schemas();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "b");
    }

    #[test]
    fn set_loadable_drops_loaded_whose_source_disappeared() {
        let ledger = DisclosureLedger::default();
        ledger.set_loadable(vec![schema("a"), schema("b")]);
        ledger.select(&["a".into(), "b".into()]);
        assert_eq!(ledger.loaded_names().len(), 2);
        // MCP server disconnects: only "a" remains loadable.
        ledger.set_loadable(vec![schema("a")]);
        assert_eq!(ledger.loaded_names(), vec!["a"], "b dropped (source gone)");
    }

    #[test]
    fn clear_loaded_allows_reselection() {
        let ledger = DisclosureLedger::default();
        ledger.set_loadable(vec![schema("a")]);
        ledger.select(&["a".into()]);
        assert_eq!(ledger.loaded_names(), vec!["a"]);
        // Compaction clears the lead; history rescan will rebuild.
        ledger.clear_loaded();
        assert!(ledger.loaded_names().is_empty());
        let out = ledger.select(&["a".into()]);
        assert_eq!(out.newly_loaded.len(), 1, "re-loadable after clear");
    }

    #[test]
    fn to_openai_function_shape() {
        let s = schema("mcp__srv__t");
        let f = s.to_openai_function();
        assert_eq!(f["type"], "function");
        assert_eq!(f["function"]["name"], "mcp__srv__t");
        assert_eq!(f["function"]["parameters"]["type"], "object");
    }
}
