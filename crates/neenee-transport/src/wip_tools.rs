//! The WIP-coordination tools (ADR-0097 §5): `declare_wip`, `wip_done`, and
//! `check_wip`. They let a session agent register its own work-in-progress
//! and consult the orchestrator's coordination registry before doing
//! whole-workspace verification (full test suite, direct run) that a peer's
//! conflicting WIP would make meaningless.
//!
//! These live in `neenee-transport` (not `neenee-agent`) because they drive
//! the daemon's coordination state — only the transport layer holds it.
//! They are advisory by design: `check_wip` never blocks, and an absent
//! coordinator yields a clean "proceed" verdict so a session is never broken
//! by the coordination layer being down.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use neenee_core::Tool;

use crate::registry::{WipRegistry, clear_wip_on, declare_wip_on};

/// The boxed future a [`CheckWipQuery`] returns.
pub type CheckWipFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = (Vec<neenee_core::WipConflict>, neenee_core::WipAdvice)>
            + Send,
    >,
>;

/// The `check_wip` query a session's tool drives: given the query's paths and
/// concern, answer with the conflicting WIPs and the advice. Bound to one
/// session by the registry (which owns the sessions index the answer needs).
pub type CheckWipQuery = Arc<dyn Fn(Vec<String>, Option<String>) -> CheckWipFuture + Send + Sync>;

/// Shared handle injected into `declare_wip`/`wip_done`: the session they
/// belong to plus the daemon's WIP-coordination registry.
#[derive(Clone)]
pub struct WipToolContext {
    registry: WipRegistry,
    session_id: String,
}

impl WipToolContext {
    pub fn new(registry: WipRegistry, session_id: String) -> Self {
        Self {
            registry,
            session_id,
        }
    }
}

const DECLARE_WIP_DESCRIPTION: &str = "Declare your current work-in-progress so peer sessions in the same \
     workspace can avoid colliding with you. Call this when you begin a multi-step edit that leaves the \
     tree in a non-building or partially-applied state: it tells the orchestrator which paths you own and \
     what you're doing, so another session's `check_wip` can see the conflict and narrow or defer its own \
     whole-workspace verification. Re-declare as the scope shifts; call `wip_done` when the tree is whole again.";

const WIP_DONE_DESCRIPTION: &str = "Clear your declared work-in-progress. Call this when your edits reach a \
     consistent state (the tree builds again, your change is committed or abandoned), so peer sessions stop \
     seeing a conflict where none remains.";

const CHECK_WIP_DESCRIPTION: &str = "Check whether a peer session's declared work-in-progress conflicts with \
     what you are about to do. Call this before whole-workspace verification (running the full test suite, \
     building, or launching the app) or before touching paths another session might be editing: the verdict \
     tells you to `proceed` (no conflict — global verification is fine), `proceed_scoped` (a WIP exists — \
     narrow to your own paths and skip the global run), or `defer` (a WIP directly overlaps — wait or ask the \
     human). Advisory, not a lock: you may proceed regardless, but the default is to focus on your own scope \
     while a conflicting WIP exists.";

/// `declare_wip`: register the calling session's WIP paths + summary.
pub struct DeclareWipTool {
    context: WipToolContext,
}

impl DeclareWipTool {
    pub fn new(context: WipToolContext) -> Self {
        Self { context }
    }
}

#[async_trait]
impl Tool for DeclareWipTool {
    fn name(&self) -> &str {
        "declare_wip"
    }

    fn description(&self) -> &str {
        DECLARE_WIP_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Paths you are actively editing (files or directories, workspace-relative or absolute)."
                },
                "summary": {
                    "type": "string",
                    "description": "One-line description of the in-flight work (e.g. 'refactoring the retry loop — tree doesn't build')."
                }
            },
            "required": ["paths", "summary"]
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        #[derive(serde::Deserialize)]
        struct Arguments {
            paths: Vec<String>,
            summary: String,
        }
        let parsed: Arguments =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;
        let paths: Vec<String> = parsed
            .paths
            .into_iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        if paths.is_empty() {
            return Err("declare_wip needs at least one non-empty path.".to_string());
        }
        let summary = parsed.summary.trim().to_string();
        if summary.is_empty() {
            return Err("declare_wip needs a non-empty summary.".to_string());
        }
        declare_wip_on(
            &self.context.registry,
            &self.context.session_id,
            neenee_core::WipStatus {
                paths: paths.clone(),
                summary: summary.clone(),
            },
        )
        .await;
        Ok(format!(
            "WIP declared on {} path(s): {}",
            paths.len(),
            summary
        ))
    }
}

/// `wip_done`: clear the calling session's declared WIP.
pub struct WipDoneTool {
    context: WipToolContext,
}

impl WipDoneTool {
    pub fn new(context: WipToolContext) -> Self {
        Self { context }
    }
}

#[async_trait]
impl Tool for WipDoneTool {
    fn name(&self) -> &str {
        "wip_done"
    }

    fn description(&self) -> &str {
        WIP_DONE_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn call(&self, _arguments: &str) -> Result<String, String> {
        clear_wip_on(&self.context.registry, &self.context.session_id).await;
        Ok("WIP cleared.".to_string())
    }
}

/// `check_wip`: consult the coordination registry for conflicting peer WIP.
/// Unlike the two mutations, the answer needs the registry's *sessions index*
/// (which peers share the workspace), so it goes through an injected query
/// closure rather than the shared handle.
pub struct CheckWipTool {
    query: CheckWipQuery,
}

impl CheckWipTool {
    pub fn new(query: CheckWipQuery) -> Self {
        Self { query }
    }
}

#[async_trait]
impl Tool for CheckWipTool {
    fn name(&self) -> &str {
        "check_wip"
    }

    fn description(&self) -> &str {
        CHECK_WIP_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Paths you are about to touch. Omit for a whole-workspace concern (full test suite, build, launch)."
                },
                "concern": {
                    "type": "string",
                    "description": "What you are about to do (e.g. 'run the full test suite', 'launch the app')."
                }
            }
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        #[derive(serde::Deserialize)]
        struct Arguments {
            #[serde(default)]
            paths: Vec<String>,
            #[serde(default)]
            concern: Option<String>,
        }
        let parsed: Arguments =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;
        let (conflicts, advice) = (self.query)(parsed.paths, parsed.concern).await;

        if conflicts.is_empty() {
            return Ok(
                "No conflicting WIP. advice=proceed — global verification is fine.".to_string(),
            );
        }
        let mut out = String::new();
        for c in &conflicts {
            let short: String = c.session.chars().take(8).collect();
            out.push_str(&format!(
                "WIP conflict: session {short} — {} (paths: {})\n",
                c.summary,
                c.paths.join(", ")
            ));
            if !c.overlap.is_empty() {
                out.push_str(&format!(
                    "  overlaps your paths: {}\n",
                    c.overlap.join(", ")
                ));
            }
        }
        out.push_str(&format!("advice={advice}"));
        match advice {
            neenee_core::WipAdvice::ProceedScoped => out.push_str(
                " — narrow to your own non-overlapping paths and skip whole-workspace verification (no full test suite / no direct run) while this WIP exists.",
            ),
            neenee_core::WipAdvice::Defer => out.push_str(
                " — a peer WIP directly overlaps what you're about to do; wait for it or ask the human rather than ploughing ahead.",
            ),
            neenee_core::WipAdvice::Proceed => {}
        }
        Ok(out)
    }
}

/// Build the three WIP-coordination tools for one session. Called once per
/// hosted session by the registry, with the shared coordination handle, the
/// session id, and the session-bound `check_wip` query.
pub fn build_wip_tools(
    registry: WipRegistry,
    session_id: String,
    check_query: CheckWipQuery,
) -> Vec<Arc<dyn Tool>> {
    let context = WipToolContext::new(registry, session_id);
    vec![
        Arc::new(DeclareWipTool::new(context.clone())),
        Arc::new(WipDoneTool::new(context)),
        Arc::new(CheckWipTool::new(check_query)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> WipRegistry {
        std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()))
    }

    #[tokio::test]
    async fn declare_then_clear_updates_the_shared_registry() {
        let reg = registry();
        let ctx = WipToolContext::new(reg.clone(), "sess-1".to_string());
        let declare = DeclareWipTool::new(ctx.clone());
        let out = declare
            .call(r#"{"paths":["src/a.rs"],"summary":"refactoring retry"}"#)
            .await
            .unwrap();
        assert!(out.contains("WIP declared"), "{out}");
        {
            let guard = reg.lock().await;
            let wip = guard.get("sess-1").expect("wip registered");
            assert_eq!(wip.paths, vec!["src/a.rs".to_string()]);
            assert_eq!(wip.summary, "refactoring retry");
        }
        let done = WipDoneTool::new(ctx);
        let out = done.call("{}").await.unwrap();
        assert!(out.contains("cleared"), "{out}");
        assert!(reg.lock().await.get("sess-1").is_none());
    }

    #[tokio::test]
    async fn declare_rejects_empty_paths_and_summary() {
        let ctx = WipToolContext::new(registry(), "s".to_string());
        let declare = DeclareWipTool::new(ctx);
        assert!(declare.call(r#"{"paths":[],"summary":"x"}"#).await.is_err());
        assert!(
            declare
                .call(r#"{"paths":["a"],"summary":"  "}"#)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn check_wip_surfaces_conflicts_and_advice() {
        // A query closure that reports one overlapping conflict.
        let query: CheckWipQuery = Arc::new(|_paths, _concern| {
            Box::pin(async {
                (
                    vec![neenee_core::WipConflict {
                        session: "peer-session-xyz".to_string(),
                        paths: vec!["src".to_string()],
                        summary: "mid-refactor".to_string(),
                        overlap: vec!["src/a.rs".to_string()],
                    }],
                    neenee_core::WipAdvice::Defer,
                )
            })
        });
        let tool = CheckWipTool::new(query);
        let out = tool
            .call(r#"{"paths":["src/a.rs"],"concern":"run tests"}"#)
            .await
            .unwrap();
        assert!(out.contains("WIP conflict"), "{out}");
        assert!(out.contains("mid-refactor"), "{out}");
        assert!(out.contains("src/a.rs"), "{out}");
        assert!(out.contains("advice=defer"), "{out}");
    }

    #[tokio::test]
    async fn check_wip_clean_verdict_reads_as_proceed() {
        let query: CheckWipQuery =
            Arc::new(|_p, _c| Box::pin(async { (Vec::new(), neenee_core::WipAdvice::Proceed) }));
        let tool = CheckWipTool::new(query);
        let out = tool.call(r#"{}"#).await.unwrap();
        assert!(out.contains("No conflicting WIP"), "{out}");
        assert!(out.contains("advice=proceed"), "{out}");
    }

    #[tokio::test]
    async fn build_wip_tools_installs_three_named_tools() {
        let query: CheckWipQuery =
            Arc::new(|_p, _c| Box::pin(async { (Vec::new(), neenee_core::WipAdvice::Proceed) }));
        let tools = build_wip_tools(registry(), "sess".to_string(), query);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["declare_wip", "wip_done", "check_wip"]);
    }
}
