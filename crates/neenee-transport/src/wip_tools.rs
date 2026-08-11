//! The WIP-coordination tools (ADR-0097 §5): `declare_wip`, `wip_done`, and
//! `check_wip`. They let a session agent register its own work-in-progress
//! and consult the orchestrator's coordination registry before doing
//! whole-workspace verification (full test suite, direct run) that a peer's
//! conflicting WIP would make meaningless.
//!
//! These live in `neenee-transport` (not `neenee-agent`) because they drive
//! the daemon's [`SessionRegistry`] — the coordination registry is
//! daemon-level state, and only the transport layer holds it. They are
//! advisory by design: `check_wip` never blocks, and an absent coordinator
//! yields a clean "proceed" verdict so a session is never broken by the
//! coordination layer being down.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use neenee_core::{Tool, ToolOutput};

use crate::registry::SessionRegistry;

/// Shared handle injected into the three WIP tools: the session they belong
/// to plus the daemon's registry (which owns the coordination state).
#[derive(Clone)]
pub struct WipToolContext {
    registry: Arc<SessionRegistry>,
    session_id: String,
}

impl WipToolContext {
    pub fn new(registry: Arc<SessionRegistry>, session_id: String) -> Self {
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

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
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
        self.context
            .registry
            .declare_wip(&self.context.session_id, paths.clone(), summary.clone())
            .await;
        Ok(ToolOutput::text(format!(
            "WIP declared on {} path(s): {}",
            paths.len(),
            summary
        )))
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

    async fn call_structured(&self, _arguments: &str) -> Result<ToolOutput, String> {
        self.context
            .registry
            .clear_wip(&self.context.session_id)
            .await;
        Ok(ToolOutput::text("WIP cleared.".to_string()))
    }
}

/// `check_wip`: consult the coordination registry for conflicting peer WIP.
pub struct CheckWipTool {
    context: WipToolContext,
}

impl CheckWipTool {
    pub fn new(context: WipToolContext) -> Self {
        Self { context }
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

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        #[derive(serde::Deserialize)]
        struct Arguments {
            #[serde(default)]
            paths: Vec<String>,
            #[serde(default)]
            concern: Option<String>,
        }
        let parsed: Arguments =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;
        let (conflicts, advice) = self
            .context
            .registry
            .check_wip(
                &self.context.session_id,
                &parsed.paths,
                parsed.concern.as_deref(),
            )
            .await;

        if conflicts.is_empty() {
            return Ok(ToolOutput::text(
                "No conflicting WIP. advice=proceed — global verification is fine.".to_string(),
            ));
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
                out.push_str(&format!("  overlaps your paths: {}\n", c.overlap.join(", ")));
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
        Ok(ToolOutput::text(out))
    }
}

/// Install the three WIP-coordination tools onto a session's toolset. Called
/// once per hosted session, after the daemon's registry and the session id
/// are both known (the tools need both to address the coordination registry).
pub fn install_wip_tools(
    toolset: &mut neenee_core::ToolSet,
    registry: Arc<SessionRegistry>,
    session_id: String,
) {
    let context = WipToolContext::new(registry, session_id);
    toolset.upsert(Arc::new(DeclareWipTool::new(context.clone())));
    toolset.upsert(Arc::new(WipDoneTool::new(context.clone())));
    toolset.upsert(Arc::new(CheckWipTool::new(context)));
}
