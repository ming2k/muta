//! Policy-chain permission model (full async chain).
//!
//! Replaces the harness's hand-coded sequence of "gates" inside
//! [`execute_tool`](crate::Agent) with a single ordered chain of
//! [`PermissionPolicy`] implementations — every gate, sync and async alike,
//! becomes a policy. The chain evaluates first-non-`Pass`-wins, exactly
//! mirroring kimi-code's `PermissionManager` + `policies/` design.
//!
//! ### Why a chain
//!
//! The previous model hard-coded `if`/`match` arms in sequence inside a
//! 250-line function. Adding a rule meant editing that function; the order was
//! implicit. A policy chain makes each rule an isolated, unit-testable type
//! whose position in the chain is explicit, and lets new rules slot in
//! without touching the others.
//!
//! ### Async policies
//!
//! Several gates are inherently async — PreToolUse hooks (they run an external
//! process), the bash command policy (may confirm), the permission broker
//! (parks for a live user decision). So [`PermissionPolicy::evaluate`] is
//! `async`. Policies that need agent machinery (hooks, bash policy, the
//! permission store) reach it through a [`PermissionContext`] trait that
//! `Agent` implements — this keeps the module free of a direct `&Agent`
//! dependency (and its cycle), while letting policies call back into the
//! agent's async methods.
//!
//! ### Behavior invariants (preserved verbatim from the old path)
//!
//! 1. **Order is load-bearing** (see [`default_chain`]): schema validation
//!    comes *after* the hook (so hooks observe every call, including malformed
//!    ones); the scope gate precedes the broker.
//! 2. **`ScopeTarget` is the shared switch** for scope-gate / bash-policy /
//!    broker: `Unspecified` skips all three.
//! 3. **`Reject` is collective** — one reject rejects the whole pending batch.
//! 4. **`unattended` bypasses interactive policies only** (broker, bash-confirm,
//!    ask-user), never the hook or scope gate.
//! 5. **`PermissionDenied` vs `Error`** distinguish user-aborts from hard
//!    failures.

use std::sync::Arc;

use async_trait::async_trait;
use neenee_core::{RestorePoint, ScopeTarget, Tool, ToolOutput};

use crate::agent::ScopedToolDisable;
use crate::hooks::PreToolUseVerdict;
use crate::permission_store::{PermissionRule, PermissionStore};

/// The outcome a policy returns for one tool call. `Pass` is the chain's
/// continuation signal; the first non-`Pass` wins.
#[derive(Debug)]
pub enum PolicyDecision {
    /// No opinion — evaluate the next policy.
    Pass,
    /// Admit the call; no further policies consulted.
    Approve,
    /// Reject the call with a typed output. `collective` flags whether this
    /// deny should also reject sibling pending calls (true for broker rejects).
    Deny {
        output: ToolOutput,
        collective: bool,
    },
    /// Defer to the user: park and await a [`neenee_core::PermissionDecision`].
    /// The chain caller parks, emits the request, awaits; the policy only
    /// contributes the request payload + the rule to remember on `Always`.
    Ask {
        request: neenee_core::PermissionRequest,
        rule: PermissionRule,
    },
}

/// Agent capabilities a policy may need to invoke. Implemented by `Agent`; the
/// trait keeps this module decoupled from the concrete agent type (no cycle,
/// and policies are testable with a stub context).
#[async_trait]
pub trait PermissionContext: Send + Sync {
    /// Run PreToolUse hooks for this call, returning the verdict (deny +
    /// scoped-disable side effects).
    async fn check_pre_tool_use(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> PreToolUseVerdict;

    /// Apply scoped-disable side effects from a hook verdict.
    fn apply_scoped_disables(&self, disables: &[(String, RestorePoint)]);

    /// The bash command policy for `command`. Returns `Some(output)` to
    /// short-circuit (Deny/Confirm→Reject under unattended), `None` to allow.
    async fn check_bash_policy(
        &self,
        command: &str,
        arguments: &str,
    ) -> Option<ToolOutput>;

    /// The permission store, for synchronous `is_always_allowed` checks.
    fn permissions(&self) -> &PermissionStore;

    /// Whether the session is running unattended.
    fn unattended(&self) -> bool;
}

/// Everything a policy needs to decide one call.
///
/// Owns its disable-mask snapshots (cloned before the chain runs) so no
/// `MutexGuard` is held across the chain's `.await` points — the chain is
/// async and guards are not `Send`.
pub struct PolicyContext<'a> {
    pub tool: &'a Arc<dyn Tool>,
    pub call_name: &'a str,
    pub arguments: &'a str,
    pub scope_target: ScopeTarget,
    pub unattended: bool,
    pub operation_scope: neenee_core::OperationScope,
    pub disabled: std::collections::HashSet<String>,
    pub scoped_disabled: ScopedToolDisable,
    /// Agent capabilities (hooks, bash policy, permission parking). Sync
    /// policies ignore this; async policies call through it.
    pub ctx: &'a dyn PermissionContext,
}

impl<'a> PolicyContext<'a> {
    pub fn is_name_disabled(&self) -> bool {
        self.disabled.contains(self.call_name) || self.scoped_disabled.contains(self.call_name)
    }
    pub fn is_user_disabled(&self) -> bool {
        self.disabled.contains(self.call_name)
    }
}

/// One rule in the permission chain. Async because some gates await.
#[async_trait]
pub trait PermissionPolicy: Send + Sync {
    fn name(&self) -> &'static str;
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision;
}

/// The ordered chain. Evaluate by walking until the first non-`Pass`.
pub struct PermissionChain {
    policies: Vec<Box<dyn PermissionPolicy>>,
}

impl PermissionChain {
    pub fn new(policies: Vec<Box<dyn PermissionPolicy>>) -> Self {
        Self { policies }
    }
    /// Evaluate the chain. First non-`Pass` wins; if all pass, `Approve`.
    pub async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        for policy in &self.policies {
            let decision = policy.evaluate(ctx).await;
            match decision {
                PolicyDecision::Pass => continue,
                other => return other,
            }
        }
        PolicyDecision::Approve
    }
    pub fn policy_names(&self) -> Vec<&'static str> {
        self.policies.iter().map(|p| p.name()).collect()
    }
}

/// The canonical chain order. Each entry is one historical gate; reordering
/// here changes load-bearing behavior.
pub fn default_chain() -> Vec<Box<dyn PermissionPolicy>> {
    vec![
        Box::new(HookPolicy),
        Box::new(DisabledPolicy),
        Box::new(SchemaPolicy),
        Box::new(ScopeGatePolicy),
        Box::new(BashPolicy),
        Box::new(AskUserPolicy),
        Box::new(BrokerPolicy),
    ]
}

// ---------------------------------------------------------------------------
// Concrete policies. Hook/Bash/Broker are async; the rest decide from the
// context alone.
// ---------------------------------------------------------------------------

/// Gate 1: PreToolUse hook. Runs the (async) hook verdict and honours a deny.
pub struct HookPolicy;
#[async_trait]
impl PermissionPolicy for HookPolicy {
    fn name(&self) -> &'static str {
        "hook"
    }
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        let tool_input = serde_json::from_str::<serde_json::Value>(ctx.arguments)
            .unwrap_or(serde_json::Value::Null);
        let verdict = ctx.ctx.check_pre_tool_use(ctx.call_name, &tool_input).await;
        // Apply scoped disables from hooks first (narrows the toolset for
        // subsequent calls this round), then honour the deny.
        ctx.ctx.apply_scoped_disables(&verdict.side.scoped_disables);
        if let Some(reason) = verdict.deny {
            return PolicyDecision::Deny {
                output: ToolOutput::Error {
                    message: format!("Blocked by hook: {}", reason),
                    detail: None,
                },
                collective: false,
            };
        }
        PolicyDecision::Pass
    }
}

/// Gate 2: user + scoped disable masks.
pub struct DisabledPolicy;
#[async_trait]
impl PermissionPolicy for DisabledPolicy {
    fn name(&self) -> &'static str {
        "disabled"
    }
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        if !ctx.is_name_disabled() {
            return PolicyDecision::Pass;
        }
        let message = if ctx.is_user_disabled() {
            format!(
                "Tool '{}' is disabled for this session. Re-enable it in the Tools modal (/tools).",
                ctx.call_name
            )
        } else {
            format!(
                "Tool '{}' is temporarily out of scope for this task. Use a different tool.",
                ctx.call_name
            )
        };
        PolicyDecision::Deny {
            output: ToolOutput::Text(message),
            collective: false,
        }
    }
}

/// Gate 3: argument schema validation.
pub struct SchemaPolicy;
#[async_trait]
impl PermissionPolicy for SchemaPolicy {
    fn name(&self) -> &'static str {
        "schema"
    }
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        match neenee_core::tool_validation::validate_tool_arguments(
            &ctx.tool.parameters(),
            ctx.arguments,
        ) {
            Ok(()) => PolicyDecision::Pass,
            Err(message) => PolicyDecision::Deny {
                output: ToolOutput::Error {
                    message: format!("Error executing {}: {}", ctx.call_name, message),
                    detail: None,
                },
                collective: false,
            },
        }
    }
}

/// Gate 4: operation-scope gate (ADR-0028). Hard capability limit before broker.
pub struct ScopeGatePolicy;
#[async_trait]
impl PermissionPolicy for ScopeGatePolicy {
    fn name(&self) -> &'static str {
        "scope-gate"
    }
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        if matches!(ctx.scope_target, ScopeTarget::Unspecified) {
            return PolicyDecision::Pass;
        }
        if ctx.operation_scope.allows(&ctx.scope_target) {
            PolicyDecision::Pass
        } else {
            PolicyDecision::Deny {
                output: ToolOutput::Text(format!(
                    "[operation scope] Tool '{}' is blocked outside its granted scope.",
                    ctx.call_name
                )),
                collective: false,
            }
        }
    }
}

/// Gate 5: bash command policy (Deny / unattended-Confirm only). The
/// interactive Confirm path (non-unattended) stays in `execute_tool` because it
/// needs an event channel to park for user approval — a mini-broker that
/// doesn't fit the event-less policy signature. Here we only short-circuit the
/// cases that need no interaction: an outright `Deny`, or a `Confirm` while
/// unattended (resolved per `unattended_confirm_action`).
pub struct BashPolicy;
#[async_trait]
impl PermissionPolicy for BashPolicy {
    fn name(&self) -> &'static str {
        "bash-policy"
    }
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        if ctx.call_name != "bash" {
            return PolicyDecision::Pass;
        }
        let command = match &ctx.scope_target {
            ScopeTarget::Command(c) => c.clone(),
            _ => return PolicyDecision::Pass,
        };
        // Delegate to the context's bash-policy check, but only the
        // non-interactive resolution matters here. A `Some(output)` means the
        // policy produced a terminal decision (Deny, or unattended-Confirm →
        // Deny); a `None` means either Allow or "needs interactive Confirm"
        // (the latter falls through to execute_tool's full check_bash_policy).
        match ctx.ctx.check_bash_policy(&command, ctx.arguments).await {
            Some(output) => {
                let collective = matches!(output, ToolOutput::PermissionDenied { .. });
                PolicyDecision::Deny { output, collective }
            }
            None => PolicyDecision::Pass,
        }
    }
}

/// Gate 6: ask_user shortcut. Under unattended, refuse (no human to answer).
pub struct AskUserPolicy;
#[async_trait]
impl PermissionPolicy for AskUserPolicy {
    fn name(&self) -> &'static str {
        "ask-user"
    }
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        if ctx.call_name == "ask_user" && ctx.unattended {
            return PolicyDecision::Deny {
                output: ToolOutput::Text(
                    "ask_user is unavailable: this session is running unattended and no human \
                     is reachable to answer. Resolve the ambiguity yourself — pick the most \
                     reasonable default and proceed."
                        .to_string(),
                ),
                collective: false,
            };
        }
        PolicyDecision::Pass
    }
}

/// Gate 7: the permission broker. A non-`Unspecified` target not already
/// always-allowed (and not unattended) yields `Ask`; the chain caller parks.
pub struct BrokerPolicy;
#[async_trait]
impl PermissionPolicy for BrokerPolicy {
    fn name(&self) -> &'static str {
        "broker"
    }
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        if matches!(ctx.scope_target, ScopeTarget::Unspecified) {
            return PolicyDecision::Pass;
        }
        if ctx.unattended {
            return PolicyDecision::Approve;
        }
        let rule = scope_target_to_rule(ctx.call_name, &ctx.scope_target);
        if ctx.ctx.permissions().is_always_allowed(&rule) {
            return PolicyDecision::Approve;
        }
        PolicyDecision::Ask {
            request: neenee_core::PermissionRequest {
                id: String::new(), // caller fills the generated id
                tool: ctx.call_name.to_string(),
                label: ctx.tool.permission_label(),
                description: ctx.tool.permission_description(),
                arguments: ctx.arguments.to_string(),
                scope: rule.scope.clone(),
            },
            rule,
        }
    }
}

fn scope_target_to_rule(tool: &str, target: &ScopeTarget) -> PermissionRule {
    let scope = match target {
        ScopeTarget::Unspecified => "*".to_string(),
        ScopeTarget::Path(p) => p.to_string_lossy().into_owned(),
        ScopeTarget::Command(c) => c.clone(),
    };
    PermissionRule {
        tool: tool.to_string(),
        scope,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use async_trait::async_trait;
    use neenee_core::ToolAccesses;
    use std::collections::HashSet;
    use std::path::PathBuf;

    struct StubTool {
        name: String,
        target: ScopeTarget,
    }
    #[async_trait]
    impl Tool for StubTool {
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

    /// A stub PermissionContext for policy tests: all hooks/bash/park are no-ops.
    struct StubCtx {
        unattended: bool,
        perms: PermissionStore,
    }
    #[async_trait]
    impl PermissionContext for StubCtx {
        async fn check_pre_tool_use(
            &self,
            _n: &str,
            _i: &serde_json::Value,
        ) -> PreToolUseVerdict {
            PreToolUseVerdict::default()
        }
        fn apply_scoped_disables(&self, _d: &[(String, RestorePoint)]) {}
        async fn check_bash_policy(&self, _c: &str, _a: &str) -> Option<ToolOutput> {
            None
        }
        fn permissions(&self) -> &PermissionStore {
            &self.perms
        }
        fn unattended(&self) -> bool {
            self.unattended
        }
    }

    fn pctx<'a>(
        tool: &'a Arc<dyn Tool>,
        name: &'a str,
        args: &'a str,
        target: ScopeTarget,
        unattended: bool,
        op: neenee_core::OperationScope,
        disabled: std::collections::HashSet<String>,
        scoped: ScopedToolDisable,
        ctxr: &'a dyn PermissionContext,
    ) -> PolicyContext<'a> {
        PolicyContext {
            tool,
            call_name: name,
            arguments: args,
            scope_target: target,
            unattended,
            operation_scope: op,
            disabled,
            scoped_disabled: scoped,
            ctx: ctxr,
        }
    }

    #[tokio::test]
    async fn disabled_policy_denies() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "bash".into(),
            target: ScopeTarget::Command("ls".into()),
        });
        let disabled: HashSet<String> = ["bash".to_string()].into_iter().collect();
        let scoped = ScopedToolDisable::default();
        let op = neenee_core::OperationScope::unrestricted();
        let ctxr = StubCtx {
            unattended: false,
            perms: PermissionStore::new(),
        };
        let c = pctx(&tool, "bash", "{}", ScopeTarget::Unspecified, false, op.clone(), disabled.clone(), scoped.clone(), &ctxr);
        assert!(matches!(
            DisabledPolicy.evaluate(&c).await,
            PolicyDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn scope_gate_blocks() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/etc/passwd")),
        });
        let op = neenee_core::OperationScope {
            paths: Some(vec![PathBuf::from("/home/user")]),
            commands: None,
        };
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            unattended: false,
            perms: PermissionStore::new(),
        };
        let c = pctx(
            &tool,
            "write_file",
            "{}",
            ScopeTarget::Path(PathBuf::from("/etc/passwd")),
            false,
            op.clone(),
            disabled.clone(),
            scoped.clone(),
            &ctxr,
        );
        assert!(matches!(
            ScopeGatePolicy.evaluate(&c).await,
            PolicyDecision::Deny { collective: false, .. }
        ));
    }

    #[tokio::test]
    async fn broker_unattended_approves() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/anywhere")),
        });
        let op = neenee_core::OperationScope::unrestricted();
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            unattended: true,
            perms: PermissionStore::new(),
        };
        let c = pctx(
            &tool,
            "write_file",
            "{}",
            ScopeTarget::Path(PathBuf::from("/anywhere")),
            true,
            op.clone(),
            disabled.clone(),
            scoped.clone(),
            &ctxr,
        );
        assert!(matches!(BrokerPolicy.evaluate(&c).await, PolicyDecision::Approve));
    }

    #[tokio::test]
    async fn broker_asks_when_not_allowed() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/tmp/x")),
        });
        let op = neenee_core::OperationScope::unrestricted();
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            unattended: false,
            perms: PermissionStore::new(),
        };
        let c = pctx(
            &tool,
            "write_file",
            "{}",
            ScopeTarget::Path(PathBuf::from("/tmp/x")),
            false,
            op.clone(),
            disabled.clone(),
            scoped.clone(),
            &ctxr,
        );
        assert!(matches!(BrokerPolicy.evaluate(&c).await, PolicyDecision::Ask { .. }));
    }

    #[tokio::test]
    async fn chain_short_circuits() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/tmp/x")),
        });
        let op = neenee_core::OperationScope::unrestricted();
        let disabled: HashSet<String> = ["write_file".to_string()].into_iter().collect();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            unattended: false,
            perms: PermissionStore::new(),
        };
        let c = pctx(
            &tool,
            "write_file",
            "{}",
            ScopeTarget::Path(PathBuf::from("/tmp/x")),
            false,
            op.clone(),
            disabled.clone(),
            scoped.clone(),
            &ctxr,
        );
        let chain = PermissionChain::new(vec![Box::new(DisabledPolicy), Box::new(BrokerPolicy)]);
        assert!(matches!(
            chain.evaluate(&c).await,
            PolicyDecision::Deny { collective: false, .. }
        ));
    }

    #[tokio::test]
    async fn chain_falls_back_to_approve() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "read_text".into(),
            target: ScopeTarget::Unspecified,
        });
        let op = neenee_core::OperationScope::unrestricted();
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            unattended: false,
            perms: PermissionStore::new(),
        };
        let c = pctx(&tool, "read_text", "{}", ScopeTarget::Unspecified, false, op.clone(), disabled.clone(), scoped.clone(), &ctxr);
        let chain = PermissionChain::new(vec![Box::new(DisabledPolicy), Box::new(ScopeGatePolicy)]);
        assert!(matches!(chain.evaluate(&c).await, PolicyDecision::Approve));
    }
}
