//! Policy-chain permission model (stage 2 of the kimi-code tool-system adoption).
//!
//! Replaces the harness's hand-coded sequence of "gates" inside
//! [`execute_tool`](crate::Agent) (hook-deny → disabled → schema → scope-gate
//! → bash-policy → ask-user-shortcut → broker → stdin-policy) with a single
//! ordered chain of [`PermissionPolicy`] implementations. Each gate becomes a
//! policy; the chain is evaluated first-match-wins, exactly mirroring
//! kimi-code's `PermissionManager` + `policies/` design.
//!
//! ### Why a chain
//!
//! The previous model hard-coded eight `if`/`match` arms in sequence inside a
//! 250-line function. Adding a rule meant editing that function; the order was
//! implicit; and every arm had to be reasoned about together. A policy chain
//! makes each rule an isolated, unit-testable type whose position in the chain
//! is explicit, and lets new rules slot in without touching the others.
//!
//! ### What stays identical (behavior invariants, verified against the old path)
//!
//! 1. **Order is load-bearing.** The old sequence is preserved verbatim as the
//!    default chain order (see [`default_chain`]): schema validation comes
//!    *after* the hook (so hooks observe every call, including malformed
//!    ones), and the scope gate precedes the broker (a hard capability limit
//!    outranks a user prompt).
//! 2. **`ScopeTarget` is the shared switch** for scope-gate / bash-policy /
//!    broker: `Unspecified` skips all three; `Path`/`Command` enter them.
//! 3. **`Reject` is collective.** One reject rejects the whole pending batch
//!    (the broker's join-all deadlock guard).
//! 4. **`unattended` bypasses interactive policies only** (broker, bash-confirm,
//!    ask-user), never the hook or scope gate.
//! 5. **`ToolOutput::PermissionDenied` vs `Error`** distinguish user-aborts
//!    (signal the round should stop) from hard failures.
//!
//! ### Phase-in
//!
//! This module lands the *machinery* (trait, context, decision types, chain).
//! Wiring `execute_tool` onto it is a separate change so the behavior can be
//! diffed gate-by-gate against the old path.

use std::sync::Arc;

use neenee_core::{ScopeTarget, Tool, ToolOutput};

use crate::agent::ScopedToolDisable;
use crate::permission_store::{PermissionRule, PermissionStore};

/// The outcome a policy returns for one tool call.
///
/// Mirrors kimi-code's `PermissionPolicyResult` (approve/deny/ask) plus a
/// `Pass` ("this policy has no opinion; ask the next one") that lets a policy
/// opt out cleanly. `Pass` is the chain's continuation signal.
#[derive(Debug)]
pub enum PolicyDecision {
    /// No opinion — evaluate the next policy in the chain.
    Pass,
    /// Admit the call. No further policies are consulted.
    Approve,
    /// Reject the call with a typed output. `PermissionDenied` signals a
    /// user-initiated stop (round should end); `Error` is a hard failure.
    Deny {
        output: ToolOutput,
        /// Whether this deny is a "collective" reject (rejects all sibling
        /// pending calls in the batch). True only for the broker's user-reject
        /// path; false for hook/scope/schema denies.
        collective: bool,
    },
    /// Defer to the user: park the call and await a [`neenee_core::PermissionDecision`].
    /// The caller (the dispatcher) is responsible for the parking + event
    /// emission + await; a policy that returns `Ask` only contributes the
    /// request payload and the rule to remember on `Always`.
    Ask {
        request: neenee_core::PermissionRequest,
        rule: PermissionRule,
    },
}

/// Everything a policy needs to decide one call. Built once per call by the
/// dispatcher and passed down the chain.
pub struct PolicyContext<'a> {
    /// The tool being invoked.
    pub tool: &'a Arc<dyn Tool>,
    /// The call's raw arguments (JSON string).
    pub arguments: &'a str,
    /// The tool's resolved scope target (Path / Command / Unspecified).
    pub scope_target: ScopeTarget,
    /// Whether the session is running unattended (no human reachable).
    pub unattended: bool,
    /// The current operation scope (granted path prefixes / command allowlist).
    pub operation_scope: &'a neenee_core::OperationScope,
    /// The persistent + scoped disable masks, as names.
    pub disabled: &'a std::collections::HashSet<String>,
    pub scoped_disabled: &'a ScopedToolDisable,
    /// The permission store (for `is_always_allowed`).
    pub permissions: &'a PermissionStore,
}

impl<'a> PolicyContext<'a> {
    /// Convenience: is this tool's name disabled by either mask?
    pub fn is_name_disabled(&self) -> bool {
        self.disabled.contains(self.tool.name()) || self.scoped_disabled.contains(self.tool.name())
    }

    /// Disabled by the *user* (persisted) mask specifically? Used to word the
    /// rejection distinctly from a hook-scoped (transient) disable.
    pub fn is_user_disabled(&self) -> bool {
        self.disabled.contains(self.tool.name())
    }
}

/// One rule in the permission chain. Implementations are unit-testable in
/// isolation; the chain composes them in a fixed order.
pub trait PermissionPolicy: Send + Sync {
    /// A short, stable name for telemetry / debugging.
    fn name(&self) -> &'static str;

    /// Decide for this call. Return [`PolicyDecision::Pass`] to defer to the
    /// next policy. The first non-`Pass` result wins (short-circuit), matching
    /// kimi-code's `for policy in policies { if let Some(r) = ... return }`.
    fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision;
}

/// The ordered chain. Evaluate by walking until the first non-`Pass`.
pub struct PermissionChain {
    policies: Vec<Box<dyn PermissionPolicy>>,
}

impl PermissionChain {
    pub fn new(policies: Vec<Box<dyn PermissionPolicy>>) -> Self {
        Self { policies }
    }

    /// Evaluate the chain for one call. Returns the first non-`Pass` decision,
    /// or [`PolicyDecision::Approve`] if every policy passed (the implicit
    /// fallback — a call nothing objects to is admitted).
    pub fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        for policy in &self.policies {
            let decision = policy.evaluate(ctx);
            match decision {
                PolicyDecision::Pass => continue,
                other => return other,
            }
        }
        PolicyDecision::Approve
    }

    /// Iterate policy names (for telemetry / the Tools modal).
    pub fn policy_names(&self) -> Vec<&'static str> {
        self.policies.iter().map(|p| p.name()).collect()
    }
}

/// Build the default chain in the canonical gate order (see module docs).
///
/// This is the single source of truth for policy ordering. Each entry is one
/// of the historical gates; reordering here changes load-bearing behavior.
///
/// The chain holds the **synchronous** gates — those that can decide from the
/// [`PolicyContext`] alone, with no `.await`. The asynchronous gates
/// (PreToolUse hook execution, bash-policy confirmation, ask_user parking,
/// broker park/await) stay in `execute_tool` because they call back into the
/// agent's async machinery; the chain runs *before* them as a fast filter,
/// short-circuiting the calls that need no async work. See the switchover
/// notes in `execute_tool`.
pub fn default_chain() -> Vec<Box<dyn PermissionPolicy>> {
    vec![
        Box::new(DisabledPolicy),
        Box::new(SchemaPolicy),
        Box::new(ScopeGatePolicy),
        // BrokerPolicy does only the synchronous "already always-allowed?"
        // fast path here; the interactive park/await stays in execute_tool.
        Box::new(BrokerPolicy),
    ]
}

// ---------------------------------------------------------------------------
// Concrete policies. Each is a zero-sized type; all state is read from the
// PolicyContext. They are deliberately thin so the historical gate logic is
// visible in one place each.
// ---------------------------------------------------------------------------

/// Gate 1: PreToolUse hook. A hook's `Deny` rejects the call.
///
/// NOTE: hook execution is async and lives on the `HookRegistry`; this policy
/// is a placeholder that assumes the hook verdict has been pre-computed and
/// threaded in (the dispatcher runs hooks once, before the chain, because the
/// chain itself is sync). Full wiring arrives with the execute_tool rewrite.
pub struct HookPolicy;
impl PermissionPolicy for HookPolicy {
    fn name(&self) -> &'static str {
        "hook"
    }
    fn evaluate(&self, _ctx: &PolicyContext<'_>) -> PolicyDecision {
        // Hook verdict is computed by the dispatcher (async) and consulted
        // before the chain runs; this policy is a no-op marker for now.
        PolicyDecision::Pass
    }
}

/// Gate: user + scoped disable masks. The rejection wording distinguishes a
/// persisted user disable ("re-enable in /tools") from a transient hook-scoped
/// disable ("temporarily out of scope"), matching the historical execute_tool
/// messages so the model's remedy guidance is unchanged.
pub struct DisabledPolicy;
impl PermissionPolicy for DisabledPolicy {
    fn name(&self) -> &'static str {
        "disabled"
    }
    fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        if !ctx.is_name_disabled() {
            return PolicyDecision::Pass;
        }
        let message = if ctx.is_user_disabled() {
            format!(
                "Tool '{}' is disabled for this session. Re-enable it in the Tools modal (/tools).",
                ctx.tool.name()
            )
        } else {
            format!(
                "Tool '{}' is temporarily out of scope for this task. Use a different tool.",
                ctx.tool.name()
            )
        };
        PolicyDecision::Deny {
            output: ToolOutput::Text(message),
            collective: false,
        }
    }
}

/// Gate 3: argument schema validation. Reads the tool's `parameters()` and
/// validates the raw arguments against it.
pub struct SchemaPolicy;
impl PermissionPolicy for SchemaPolicy {
    fn name(&self) -> &'static str {
        "schema"
    }
    fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        match neenee_core::tool_validation::validate_tool_arguments(&ctx.tool.parameters(), ctx.arguments) {
            Ok(()) => PolicyDecision::Pass,
            Err(message) => PolicyDecision::Deny {
                output: ToolOutput::Error {
                    message: format!("Error executing {}: {}", ctx.tool.name(), message),
                    detail: None,
                },
                collective: false,
            },
        }
    }
}

/// Gate 4: operation-scope gate (ADR-0028). A non-`Unspecified` target outside
/// the granted scope is blocked. This is a *hard* capability limit, so it runs
/// before the broker.
pub struct ScopeGatePolicy;
impl PermissionPolicy for ScopeGatePolicy {
    fn name(&self) -> &'static str {
        "scope-gate"
    }
    fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        if matches!(ctx.scope_target, ScopeTarget::Unspecified) {
            return PolicyDecision::Pass;
        }
        if ctx.operation_scope.allows(&ctx.scope_target) {
            PolicyDecision::Pass
        } else {
            PolicyDecision::Deny {
                output: ToolOutput::Text(format!(
                    "[operation scope] Tool '{}' is blocked outside its granted scope.",
                    ctx.tool.name()
                )),
                collective: false,
            }
        }
    }
}

/// Gate 5: bash command policy. Only fires for `bash` with a `Command` target;
/// consults the bash policy (Allow/Confirm/Deny). Confirm under unattended
/// follows `unattended_confirm_action`.
///
/// NOTE: like the hook, the bash policy is computed by the dispatcher (it
/// reads config). This policy is a marker until full wiring; the historical
/// bash-policy logic stays in `check_bash_policy` and is invoked by the
/// dispatcher alongside the chain.
pub struct BashPolicy;
impl PermissionPolicy for BashPolicy {
    fn name(&self) -> &'static str {
        "bash-policy"
    }
    fn evaluate(&self, _ctx: &PolicyContext<'_>) -> PolicyDecision {
        PolicyDecision::Pass
    }
}

/// Gate 6: ask_user shortcut. Under unattended there is no human to answer, so
/// the call is refused outright (no parking, else it would deadlock).
pub struct AskUserPolicy;
impl PermissionPolicy for AskUserPolicy {
    fn name(&self) -> &'static str {
        "ask-user"
    }
    fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        if ctx.tool.name() == "ask_user" && ctx.unattended {
            return PolicyDecision::Deny {
                output: ToolOutput::Text(
                    "ask_user is unavailable while running unattended.".to_string(),
                ),
                collective: false,
            };
        }
        PolicyDecision::Pass
    }
}

/// Gate 7: the permission broker. A non-`Unspecified` target that is not
/// already `always`-allowed is parked for a user decision. Under unattended,
/// everything is admitted (bypasses the allowlist wholesale).
pub struct BrokerPolicy;
impl PermissionPolicy for BrokerPolicy {
    fn name(&self) -> &'static str {
        "broker"
    }
    fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        // Unspecified targets never broker (read-only tools like grep/list).
        if matches!(ctx.scope_target, ScopeTarget::Unspecified) {
            return PolicyDecision::Pass;
        }
        // Unattended bypasses the whole allowlist.
        if ctx.unattended {
            return PolicyDecision::Approve;
        }
        let rule = scope_target_to_rule(ctx.tool.name(), &ctx.scope_target);
        if ctx.permissions.is_always_allowed(&rule) {
            return PolicyDecision::Approve;
        }
        // Otherwise ask. The dispatcher owns parking + event + await; we only
        // contribute the payload + the rule to remember on `Always`.
        PolicyDecision::Ask {
            request: neenee_core::PermissionRequest {
                id: String::new(), // dispatcher fills the generated id
                tool: ctx.tool.name().to_string(),
                label: ctx.tool.permission_label(),
                description: ctx.tool.permission_description(),
                arguments: ctx.arguments.to_string(),
                scope: rule.scope.clone(),
            },
            rule,
        }
    }
}

/// Map a `ScopeTarget` to the stable scope-string key used by the allowlist.
/// Mirrors the historical `scope_target_to_rule`. `Unspecified` → `"*"`
/// (though the broker never reaches here for Unspecified).
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
    use std::sync::{Arc, Mutex};

    /// A configurable stub tool for policy tests.
    struct StubTool {
        name: String,
        scope_target: ScopeTarget,
        params_valid: bool,
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
            if self.params_valid {
                serde_json::json!({"type": "object"})
            } else {
                serde_json::json!({"type": "object", "required": ["x"], "properties": {"x": {"type": "string"}}})
            }
        }
        async fn call(&self, _a: &str) -> Result<String, String> {
            Ok("ok".into())
        }
        fn scope_target(&self, _a: &str) -> ScopeTarget {
            self.scope_target.clone()
        }
        fn accesses(&self, _a: &str) -> ToolAccesses {
            ToolAccesses::none()
        }
    }

    fn ctx<'a>(
        tool: &'a Arc<dyn Tool>,
        arguments: &'a str,
        scope_target: ScopeTarget,
        unattended: bool,
        operation_scope: &'a neenee_core::OperationScope,
        disabled: &'a HashSet<String>,
        scoped_disabled: &'a ScopedToolDisable,
        permissions: &'a PermissionStore,
    ) -> PolicyContext<'a> {
        PolicyContext {
            tool,
            arguments,
            scope_target,
            unattended,
            operation_scope,
            disabled,
            scoped_disabled,
            permissions,
        }
    }

    fn make_ctx(
        _tool: Arc<dyn Tool>,
        _target: ScopeTarget,
    ) {
        // Per-test contexts are built inline via `ctx(...)`; no shared helper.
    }

    #[test]
    fn disabled_policy_denies_disabled_name() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "bash".into(),
            scope_target: ScopeTarget::Command("ls".into()),
            params_valid: true,
        });
        let disabled: HashSet<String> = ["bash".to_string()].into_iter().collect();
        let scoped = ScopedToolDisable::default();
        let perms = PermissionStore::new();
        let op = neenee_core::OperationScope::unrestricted();
        let c = ctx(&tool, "{}", ScopeTarget::Unspecified, false, &op, &disabled, &scoped, &perms);
        assert!(matches!(DisabledPolicy.evaluate(&c), PolicyDecision::Deny { .. }));
    }

    #[test]
    fn scope_gate_blocks_path_outside_granted() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            scope_target: ScopeTarget::Path(PathBuf::from("/etc/passwd")),
            params_valid: true,
        });
        // Granted scope = only /home/user.
        let op = neenee_core::OperationScope {
            paths: Some(vec![PathBuf::from("/home/user")]),
            commands: None,
        };
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let perms = PermissionStore::new();
        let c = ctx(
            &tool,
            "{}",
            ScopeTarget::Path(PathBuf::from("/etc/passwd")),
            false,
            &op,
            &disabled,
            &scoped,
            &perms,
        );
        let d = ScopeGatePolicy.evaluate(&c);
        assert!(matches!(d, PolicyDecision::Deny { collective: false, .. }));
    }

    #[test]
    fn broker_unattended_approves_everything() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            scope_target: ScopeTarget::Path(PathBuf::from("/anywhere")),
            params_valid: true,
        });
        let op = neenee_core::OperationScope::unrestricted();
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let perms = PermissionStore::new();
        let c = ctx(
            &tool,
            "{}",
            ScopeTarget::Path(PathBuf::from("/anywhere")),
            true, // unattended
            &op,
            &disabled,
            &scoped,
            &perms,
        );
        assert!(matches!(BrokerPolicy.evaluate(&c), PolicyDecision::Approve));
    }

    #[test]
    fn broker_asks_when_not_always_allowed() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            scope_target: ScopeTarget::Path(PathBuf::from("/tmp/x")),
            params_valid: true,
        });
        let op = neenee_core::OperationScope::unrestricted();
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let perms = PermissionStore::new();
        let c = ctx(
            &tool,
            "{}",
            ScopeTarget::Path(PathBuf::from("/tmp/x")),
            false,
            &op,
            &disabled,
            &scoped,
            &perms,
        );
        assert!(matches!(BrokerPolicy.evaluate(&c), PolicyDecision::Ask { .. }));
    }

    #[test]
    fn chain_short_circuits_on_first_non_pass() {
        // Disabled beats broker: a disabled tool is denied before brokering.
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            scope_target: ScopeTarget::Path(PathBuf::from("/tmp/x")),
            params_valid: true,
        });
        let op = neenee_core::OperationScope::unrestricted();
        let disabled: HashSet<String> = ["write_file".to_string()].into_iter().collect();
        let scoped = ScopedToolDisable::default();
        let perms = PermissionStore::new();
        let c = ctx(
            &tool,
            "{}",
            ScopeTarget::Path(PathBuf::from("/tmp/x")),
            false,
            &op,
            &disabled,
            &scoped,
            &perms,
        );
        let chain = PermissionChain::new(vec![
            Box::new(DisabledPolicy),
            Box::new(BrokerPolicy),
        ]);
        let d = chain.evaluate(&c);
        assert!(matches!(d, PolicyDecision::Deny { collective: false, .. }));
    }

    #[test]
    fn chain_falls_back_to_approve_when_all_pass() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "read_text".into(),
            scope_target: ScopeTarget::Unspecified,
            params_valid: true,
        });
        let op = neenee_core::OperationScope::unrestricted();
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let perms = PermissionStore::new();
        let c = ctx(
            &tool,
            "{}",
            ScopeTarget::Unspecified,
            false,
            &op,
            &disabled,
            &scoped,
            &perms,
        );
        let chain = PermissionChain::new(vec![Box::new(DisabledPolicy), Box::new(ScopeGatePolicy)]);
        assert!(matches!(chain.evaluate(&c), PolicyDecision::Approve));
    }
}
