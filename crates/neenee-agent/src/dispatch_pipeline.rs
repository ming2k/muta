//! Four-stage tool-call dispatch pipeline (stage 3 machinery).
//!
//! Ports kimi-code's `runToolCallBatch` (preflight / prepare / schedule /
//! finalize) onto neenee's dispatch path. Each stage owns a precise slice of
//! the historical `dispatch_tool_calls` / `execute_tools_concurrent` /
//! `execute_tool` / `record_tool_result` behavior, so the whole can be
//! reasoned about stage-by-stage instead of as one 250-line function.
//!
//! ### Stage ownership (mapped from the old code)
//!
//! | stage | owns (from the old dispatch path) |
//! |---|---|
//! | **preflight** | turn classification (`consecutive_readonly_turns`), checkpoint-replay scan + `ProviderRetry` notice, doom-guard `check_doom_ahead` (signature masking + `NudgeInjected` notice + nudge capture), text-fallback decision (`parse_text_tool_call` + `AssistantDiscard` + `attach_fallback_tool_call`). |
//! | **prepare** (per call) | generate `call_<uuid>`, emit `AgentEvent::ToolCall` up front, decide short-circuit (checkpoint-replay / doom-blocked) vs execute, and for short-circuits emit the terminal `ToolResult(duration_ms=0)` + fill the result slot. For executable calls: resolve the tool (resolved → dynamic fallback), run the [`PermissionChain`](crate::permission_policy::PermissionChain) (which folds in hook/disabled/schema/scope/bash/ask-user/broker), resolve stdin policy. |
//! | **schedule** | the concurrent fan-out via [`ToolScheduler`]: a shared `mpsc` channel, biased `select!` with `cancel.cancelled()` first (drain + `ToolCancelled` per dispatched id + `Err(Interrupted)`), interleaved forwarding of `Envoy`/`ToolStream`/`PermissionRequest`, per-task terminal `ToolResult` the instant it finishes, results in input order. |
//! | **finalize** (per call, input order) | [`record_tool_result`](crate::Agent) (token accounting w/ envoy special-case, optional `TodosUpdated`, `Message::tool_result` with `.with_children().with_envoy_meta()` or plain, image peel-out), `run_post_tool_hooks` unless replay, turn-level doom nudge injection, `Ok(!denied)`. |
//!
//! ### Why land the machinery first
//!
//! Like stages 1-2, this lands the *types and the stage boundaries* without
//! rewiring `dispatch_tool_calls` onto them. The actual switchover is a
//! follow-up so the behavior diff against the old path can be reviewed
//! stage-by-stage. Every type here is `#[allow(dead_code)]` until then.

use std::sync::Arc;

use neenee_core::{Message, ScopeTarget, ToolCall, ToolOutput};

use crate::permission_policy::{PermissionChain, PolicyContext, PolicyDecision};
use crate::tool_scheduler::{RunClosure, ToolScheduler};

/// The kind of outcome `prepare` produces for one call. Mirrors kimi-code's
/// `runnable | rejected`, plus neenee's short-circuit kinds. Not `Debug`
/// because it embeds an `Arc<dyn Tool>`; use [`PreparedCall::debug_summary`].
pub enum PreparedCall {
    /// The call will execute: tool resolved, permission admitted (or `Ask`
    /// pending), accesses + run-closure ready for the scheduler.
    Runnable {
        call_id: String,
        call: ToolCall,
        #[allow(unused)]
        tool: Arc<dyn neenee_core::Tool>,
        accesses: neenee_core::ToolAccesses,
        /// A pending permission ask, if the policy chain returned `Ask`. The
        /// scheduler task parks on it before executing; `None` means admitted.
        pending_ask: Option<PendingAsk>,
    },
    /// Short-circuited at prepare: result already known, no execution. The
    /// `ToolResult` event has already been emitted; finalize just records.
    ShortCircuited {
        call_id: String,
        call: ToolCall,
        output: ToolOutput,
        /// Whether this short-circuit is a checkpoint replay (affects
        /// finalize: skips token re-accounting + post-tool hooks).
        is_replay: bool,
    },
    /// Rejected at prepare (tool not found, schema invalid, permission denied).
    /// Distinct from `ShortCircuited` so finalize can route `PermissionDenied`
    /// to "stop the round" semantics.
    Rejected {
        call_id: String,
        call: ToolCall,
        output: ToolOutput,
    },
}

impl PreparedCall {
    /// The dispatch id of this prepared call, regardless of kind.
    pub fn call_id(&self) -> &str {
        match self {
            PreparedCall::Runnable { call_id, .. }
            | PreparedCall::ShortCircuited { call_id, .. }
            | PreparedCall::Rejected { call_id, .. } => call_id,
        }
    }
    /// A non-owning Debug representation (the embedded tool isn't Debug).
    pub fn debug_summary(&self) -> String {
        match self {
            PreparedCall::Runnable {
                call_id,
                call,
                pending_ask,
                ..
            } => format!(
                "Runnable({}, {}, ask={})",
                call_id,
                call.name,
                pending_ask.is_some()
            ),
            PreparedCall::ShortCircuited {
                call_id,
                call,
                is_replay,
                ..
            } => {
                format!(
                    "ShortCircuited({}, {}, replay={})",
                    call_id, call.name, is_replay
                )
            }
            PreparedCall::Rejected { call_id, call, .. } => {
                format!("Rejected({}, {})", call_id, call.name)
            }
        }
    }
}

/// A permission ask deferred by the policy chain, parked until the user
/// replies. The scheduler task awaits the receiver; `Always` seeds the store.
#[derive(Debug)]
pub struct PendingAsk {
    pub request: neenee_core::PermissionRequest,
    pub rule: crate::permission_store::PermissionRule,
}

/// The product of one scheduler task: the call's result, its wall-clock
/// duration, and whether the permission broker rejected it (round-stop).
#[derive(Debug, Clone)]
pub struct ExecutedCall {
    pub call_id: String,
    pub call: ToolCall,
    pub output: ToolOutput,
    pub duration_ms: u64,
}

/// Per-turn signals computed by preflight and consumed by finalize.
#[derive(Debug, Default)]
pub struct TurnSignals {
    /// The hidden `LoopReviewNudge` to inject after the batch, if the doom
    /// guard blocked anything this turn.
    pub doom_nudge: Option<String>,
    /// Whether *any* call this turn is a checkpoint replay (excludes it from
    /// `remember_completed_tool`, post-tool hooks, and doom-guard input).
    pub has_replay: bool,
}

// ---------------------------------------------------------------------------
// The pipeline. Constructed per dispatch (one batch of tool calls); the four
// stages are methods so each is unit-testable in isolation once wired.
// ---------------------------------------------------------------------------

/// A four-stage dispatch pipeline over one batch of tool calls.
///
/// Holds the cross-cutting dependencies (permission chain, scheduler) so the
/// agent constructs it once and drives batches through `run`. The actual
/// per-call execution closure (`run_tool`) is injected by the agent, keeping
/// this struct free of `&Agent` coupling — it only knows the types it needs.
pub struct DispatchPipeline<'a> {
    pub permission: &'a PermissionChain,
    pub scheduler: ToolScheduler<ExecutedCall>,
}

impl<'a> DispatchPipeline<'a> {
    pub fn new(permission: &'a PermissionChain, scheduler: ToolScheduler<ExecutedCall>) -> Self {
        Self {
            permission,
            scheduler,
        }
    }

    // Stages are documented here as the contract the switchover must honor.
    // Full bodies arrive with the execute_tool rewrite; these signatures pin
    // the boundaries now so the rewrite is mechanical.

    /// **Stage 1 — preflight** (per turn). Computes turn signals (doom nudge,
    /// replay flags) from the batch before any per-call work.
    ///
    /// Maps the old `dispatch_tool_calls` steps B+C+D.
    pub async fn preflight(
        &self,
        _response: &Message,
        _state: &mut crate::agent::RoundState,
    ) -> Result<(Vec<ToolCall>, TurnSignals), crate::HarnessError> {
        // TODO(stage-3-switch): turn classification, checkpoint-replay scan,
        // doom-guard check_doom_ahead, text-fallback parse.
        Ok((Vec::new(), TurnSignals::default()))
    }

    /// **Stage 2 — prepare** (per call). Resolves the tool, runs the permission
    /// chain, decides runnable vs short-circuit vs reject.
    ///
    /// Maps the old `execute_tool` gate sequence (now folded into
    /// [`PermissionChain`]) plus the doom-guard `is_blocked` short-circuit.
    pub async fn prepare(&self, _call: ToolCall, _ctx: &PolicyContext<'_>) -> PreparedCall {
        // TODO(stage-3-switch): tool lookup, permission.evaluate(ctx),
        // short-circuit on Ask/Deny, build Runnable otherwise.
        unimplemented!("stage 3 switchover")
    }

    /// **Stage 3 — schedule** (the batch). Fans executable calls out through
    /// the [`ToolScheduler`], interleaving events and preserving input order.
    ///
    /// Maps the old `execute_tools_concurrent` (join_all + mpsc + biased
    /// cancel + per-id ToolCancelled), but with conflict arbitration from
    /// `accesses` replacing blanket join_all.
    pub async fn schedule(
        &self,
        _runnables: Vec<PreparedCall>,
        _run_tool: RunClosure<ExecutedCall>,
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Vec<ExecutedCall>, crate::HarnessError> {
        // TODO(stage-3-switch): for each Runnable, scheduler.add(ToolCallTask),
        // then await all receivers in input order, forwarding events.
        unimplemented!("stage 3 switchover")
    }

    /// **Stage 4 — finalize** (per call, input order). Records the result,
    /// runs post-tool hooks, injects the doom nudge.
    ///
    /// Maps the old `record_tool_result` + `run_post_tool_hooks` + nudge push.
    pub async fn finalize(
        &self,
        _executed: &[ExecutedCall],
        _signals: &TurnSignals,
        _messages: &mut Vec<Message>,
    ) -> Result<bool, crate::HarnessError> {
        // TODO(stage-3-switch): record_tool_result per call, post-tool hooks
        // unless replay, doom-nudge hidden-user push, return !denied.
        unimplemented!("stage 3 switchover")
    }
}

/// Helper: classify a policy-chain decision into a prepare outcome.
///
/// `Approve`/`Pass` (chain fallback) → the call may run (caller builds the
/// Runnable). `Deny` → Rejected (a `ToolOutput::PermissionDenied` output marks
/// a user-style abort, which the store resolves collectively across the batch).
/// `Ask` → Runnable with a pending ask.
#[allow(dead_code)]
pub fn decision_to_prepare(
    decision: PolicyDecision,
    call_id: String,
    call: ToolCall,
    tool: Arc<dyn neenee_core::Tool>,
    _scope_target: ScopeTarget,
    arguments: &str,
) -> PreparedCall {
    match decision {
        PolicyDecision::Pass | PolicyDecision::Approve => {
            let accesses = tool.accesses(arguments);
            PreparedCall::Runnable {
                call_id,
                call,
                tool,
                accesses,
                pending_ask: None,
            }
        }
        PolicyDecision::Deny { output, .. } => PreparedCall::Rejected {
            call_id,
            call,
            output,
        },
        PolicyDecision::Ask { request, rule } => {
            let accesses = tool.accesses(arguments);
            PreparedCall::Runnable {
                call_id,
                call,
                tool,
                accesses,
                pending_ask: Some(PendingAsk { request, rule }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use async_trait::async_trait;
    use neenee_core::ToolAccesses;

    struct StubTool {
        name: String,
        target: ScopeTarget,
    }
    #[async_trait]
    impl neenee_core::Tool for StubTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _a: &str) -> Result<String, String> {
            Ok("ok".into())
        }
        fn scope_target(&self, _a: &str) -> ScopeTarget {
            self.target.clone()
        }
        fn accesses(&self, _a: &str) -> ToolAccesses {
            ToolAccesses::none()
        }
    }

    fn mkcall(name: &str) -> ToolCall {
        ToolCall {
            id: "orig".into(),
            name: name.into(),
            arguments: "{}".into(),
        }
    }

    #[test]
    fn decision_approve_yields_runnable_no_ask() {
        let tool: Arc<dyn neenee_core::Tool> = Arc::new(StubTool {
            name: "read_text".into(),
            target: ScopeTarget::Unspecified,
        });
        let prepared = decision_to_prepare(
            PolicyDecision::Approve,
            "call_1".into(),
            mkcall("read_text"),
            tool,
            ScopeTarget::Unspecified,
            "{}",
        );
        match prepared {
            PreparedCall::Runnable {
                pending_ask: None, ..
            } => {}
            other => panic!(
                "expected Runnable without ask, got {}",
                other.debug_summary()
            ),
        }
    }

    #[test]
    fn decision_deny_yields_rejected() {
        let tool: Arc<dyn neenee_core::Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path("/x".into()),
        });
        let prepared = decision_to_prepare(
            PolicyDecision::Deny {
                output: ToolOutput::Text("no".into()),
            },
            "call_2".into(),
            mkcall("write_file"),
            tool,
            ScopeTarget::Path("/x".into()),
            "{}",
        );
        assert!(matches!(prepared, PreparedCall::Rejected { .. }));
    }

    #[test]
    fn decision_ask_yields_runnable_with_pending_ask() {
        let tool: Arc<dyn neenee_core::Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path("/y".into()),
        });
        let prepared = decision_to_prepare(
            PolicyDecision::Ask {
                request: neenee_core::PermissionRequest {
                    id: String::new(),
                    tool: "write_file".into(),
                    label: "Write".into(),
                    description: "".into(),
                    arguments: "{}".into(),
                    scope: "/y".into(),
                    elevation: false,
                    one_off: false,
                },
                rule: crate::permission_store::PermissionRule {
                    tool: "write_file".into(),
                    scope: "/y".into(),
                },
            },
            "call_3".into(),
            mkcall("write_file"),
            tool,
            ScopeTarget::Path("/y".into()),
            "{}",
        );
        match prepared {
            PreparedCall::Runnable {
                pending_ask: Some(_),
                ..
            } => {}
            other => panic!("expected Runnable with ask, got {}", other.debug_summary()),
        }
    }
}
