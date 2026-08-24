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
//! 3. **`Reject` is collective** — one reject rejects the whole pending batch
//!    (owned by the permission store's `reply`, keyed on a reject decision).
//! 4. **`autopilot` bypasses interactive policies only** (broker, bash-confirm,
//!    ask-user), never the hook. The scope gate *reads* `autopilot` (to decide
//!    whether an out-of-scope call can be elevated by the user or must be
//!    blocked), but it never opens an interactive modal of its own.
//! 5. **`PermissionDenied` vs `Error`** distinguish user-aborts from hard
//!    failures.
//! 6. **One prompt per call.** Both the bash confirm gate and the broker emit
//!    [`PolicyDecision::Ask`]; the caller parks once, emits one prompt, and
//!    awaits one decision. A bash command is never prompted twice (the old
//!    chain-external re-evaluation is gone). An `Ask`'s [`muta_contracts::PermissionRequest`]
//!    carries `elevation` (out-of-scope, ADR-0028) and `one_off` (the bash
//!    dangerous-command confirm: an `Always` reply is honoured but not
//!    persisted) so the caller and the TUI handle both uniformly.

use std::sync::Arc;

use async_trait::async_trait;
use muta_contracts::{RestorePoint, ScopeTarget, Tool, ToolOutput};

use crate::agent::ScopedToolDisable;
use crate::bash_policy::BashPolicyMatch;
use crate::hooks::PreToolUseVerdict;
use crate::permission_store::{PermissionRule, PermissionStore};

/// The non-interactive verdict the bash policy returns for one command.
///
/// Replaces the old ambiguous `Option<ToolOutput>` whose `None` conflated "the
/// command is allowed" with "it needs interactive confirmation but I have no
/// event channel" — a load-bearing lie that forced a second full bash-policy
/// re-evaluation outside the chain. The three variants are now disjoint and
/// self-describing, so [`BashPolicy`] can decide everything itself and return a
/// real [`PolicyDecision::Ask`] for the confirm path (no more chain-external
/// re-run, no more double evaluation, no more double prompt).
pub enum BashVerdict {
    /// The command fell through to the normal permission broker.
    Allow,
    /// The command matches a `Confirm` rule and a human must approve it
    /// one-off. Carries the rule match so the gate can build the prompt
    /// payload (label/description/detail) without re-evaluating.
    Confirm { match_: BashPolicyMatch },
    /// A hard refusal (`Deny`, or a `Confirm` resolved under autopilot).
    Deny { output: ToolOutput },
}

/// The outcome a policy returns for one tool call. `Pass` is the chain's
/// continuation signal; the first non-`Pass` wins.
#[derive(Debug)]
pub enum PolicyDecision {
    /// No opinion — evaluate the next policy.
    Pass,
    /// Admit the call; no further policies consulted.
    Approve,
    /// Reject the call with a typed output.
    ///
    /// A reject of one parked request also rejects the rest of its concurrent
    /// batch, but that collective-abort is owned by the permission store's
    /// `reply` (keyed on a `PermissionDecision::Reject`), not signalled here:
    /// synchronous chain denies are per-call, and a `ToolOutput::PermissionDenied`
    /// is enough for the caller to treat the outcome as a user-style abort.
    Deny { output: ToolOutput },
    /// Defer to the user: park and await a [`muta_contracts::PermissionDecision`].
    /// The chain caller parks, emits the request, awaits; the policy only
    /// contributes the request payload + the rule to remember on `Always`.
    ///
    /// The request's `one_off` flag tells the caller **not to persist** an
    /// `Always` reply (the bash dangerous-command confirm is one-off); its
    /// `elevation` flag tells the TUI the call is out-of-scope (ADR-0028).
    Ask {
        request: muta_contracts::PermissionRequest,
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

    /// The bash command policy for `command`, as a disjoint three-way
    /// [`BashVerdict`] (Allow / Confirm / Deny). The chain's [`BashPolicy`]
    /// gate maps each variant to its terminal decision, so **every** bash
    /// outcome — including the interactive confirm — is resolved inside the
    /// chain. There is no longer a chain-external re-evaluation.
    ///
    /// Autopilot sessions never yield [`BashVerdict::Confirm`]: a confirm is
    /// resolved silently to `Allow` or `Deny` per `autopilot_confirm_action`,
    /// so the gate produces no [`PolicyDecision::Ask`] under autopilot (the
    /// broker's autopilot bypass therefore sees a `Pass`, as before).
    async fn check_bash_policy(&self, command: &str, arguments: &str) -> BashVerdict;

    /// The permission store, for synchronous `is_always_allowed` checks.
    fn permissions(&self) -> &PermissionStore;
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
    pub autopilot: bool,
    pub operation_scope: muta_contracts::OperationScope,
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
    /// Stable rule name. Not read by the dispatch path itself; exists so the
    /// chain-order regression test can pin the load-bearing sequence.
    #[allow(dead_code)]
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
    /// The policy names in chain order. Consumed by the chain-order regression
    /// test; not part of the dispatch path.
    #[allow(dead_code)]
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
        match muta_contracts::tool_validation::validate_tool_arguments(
            &ctx.tool.parameters(),
            ctx.arguments,
        ) {
            Ok(()) => PolicyDecision::Pass,
            Err(message) => PolicyDecision::Deny {
                output: ToolOutput::Error {
                    message: format!("Error executing {}: {}", ctx.call_name, message),
                    detail: None,
                },
            },
        }
    }
}

/// Gate 4: operation-scope gate (ADR-0028). A *soft* capability limit before
/// the broker: the right to decide out-of-scope calls is handed to the user.
///
/// A call whose [`ScopeTarget`] falls outside the agent's granted
/// [`muta_contracts::OperationScope`] is not hard-blocked. Instead:
/// - **Attended** (a human is reachable, `ctx.autopilot == false`) → `Pass`, so
///   the call falls through to the next gate — the [`BrokerPolicy`], which
///   surfaces the standard approve / always-allow / reject prompt. The user, not
///   a builtin limit, decides whether the elevation is granted.
/// - **Autopilot** (no human reachable, `ctx.autopilot == true`) → hard `Deny`.
///   With no user to answer a prompt, blocking outright is the only safe
///   resolution; auto-allowing would remove the safety floor entirely. The
///   message is a `ToolOutput::Text` (not `PermissionDenied`) so the model can
///   retry with a different path, preserving the pre-softening retry semantics.
///
/// A target that *is* inside scope, or a tool with `ScopeTarget::Unspecified`
/// (no locatable target), always `Pass`es — the broker then applies as usual.
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
            return PolicyDecision::Pass;
        }
        // Out of scope. Attended → hand the decision to the user (the broker);
        // autopilot → block outright, since no human can answer an elevation.
        if ctx.autopilot {
            PolicyDecision::Deny {
                output: ToolOutput::Text(format!(
                    "[operation scope] Tool '{}' targets '{}' outside its granted scope, and no \
                     user is reachable to approve the elevation. Use a path or command inside the \
                     granted scope.",
                    ctx.call_name,
                    ScopeTargetDisplay(&ctx.scope_target)
                )),
            }
        } else {
            PolicyDecision::Pass
        }
    }
}

/// Tiny helper to render a [`ScopeTarget`] for denial messages without forcing
/// a `Display` impl onto the core type.
struct ScopeTargetDisplay<'a>(&'a ScopeTarget);

impl std::fmt::Display for ScopeTargetDisplay<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            ScopeTarget::Path(p) => write!(f, "path {}", p.display()),
            ScopeTarget::Command(c) => write!(f, "command {:?}", c),
            ScopeTarget::Unspecified => f.write_str("no target"),
        }
    }
}

/// Gate 5: bash command policy. A **complete** gate — every bash outcome,
/// including the interactive confirm, is decided here. The old design left the
/// attended `Confirm` path in `execute_tool` (it "needed an event channel to
/// park"), which forced three corollary hacks: a chain-external re-evaluation
/// of the same policy, an ambiguous `Option<ToolOutput>` return whose `None`
/// conflated Allow with "needs confirm", and a *second* prompt on top of the
/// broker's own `Ask` for the same command.
///
/// All of that is gone. The gate now maps the [`BashVerdict`] directly:
/// - [`BashVerdict::Allow`] → `Pass` (fall through to the broker, which may
///   still `Ask` for a not-yet-approved command — the *single* prompt).
/// - [`BashVerdict::Deny`] → `Deny` (hard refusal, or a confirm resolved under
///   autopilot).
/// - [`BashVerdict::Confirm`] → `Ask` with `one_off: true`, carrying the rule
///   match. The caller parks through the **same** `Ask` path the broker uses,
///   so there is one park/emit/await block, one prompt, no re-evaluation.
///
/// Because the confirm is `one_off`, an `Always` reply is honoured for this one
/// call but **not persisted** — a dangerous-command confirmation is sharper
/// than ordinary tool permission and stays one-off unless the user writes an
/// explicit `[bash_policy.rules] action = "allow"` override.
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
        match ctx.ctx.check_bash_policy(&command, ctx.arguments).await {
            BashVerdict::Allow => PolicyDecision::Pass,
            BashVerdict::Deny { output } => PolicyDecision::Deny { output },
            BashVerdict::Confirm { match_ } => {
                // Build the one-off dangerous-command prompt. `one_off: true`
                // tells the caller (and the TUI) that an `Always` reply is not
                // persisted and the option should be de-emphasised.
                PolicyDecision::Ask {
                    request: muta_contracts::PermissionRequest {
                        id: String::new(), // caller fills the generated id
                        tool: "bash".to_string(),
                        label: "Dangerous bash command".to_string(),
                        description: format!(
                            "Bash policy requires one-off confirmation before running this \
                             command.\n\nRule: {}{}\nReason: {}\n\nA broad bash allowlist entry \
                             does not bypass this safety check.",
                            match_.name,
                            if match_.builtin { " (built-in)" } else { "" },
                            match_.reason,
                        ),
                        arguments: ctx.arguments.to_string(),
                        scope: command.clone(),
                        elevation: false,
                        one_off: true,
                    },
                    // A well-formed rule that is never persisted: `one_off`
                    // short-circuits persistence in the caller. Carried only to
                    // satisfy the `Ask` shape uniformly.
                    rule: PermissionRule {
                        tool: "bash".to_string(),
                        scope: command,
                    },
                }
            }
        }
    }
}

/// Gate 6: ask_user shortcut. Under autopilot, refuse (no human to answer).
pub struct AskUserPolicy;
#[async_trait]
impl PermissionPolicy for AskUserPolicy {
    fn name(&self) -> &'static str {
        "ask-user"
    }
    async fn evaluate(&self, ctx: &PolicyContext<'_>) -> PolicyDecision {
        if ctx.call_name == "ask_user" && ctx.autopilot {
            return PolicyDecision::Deny {
                output: ToolOutput::Text(
                    "ask_user is unavailable: this session is running on autopilot and no human \
                     is reachable to answer. Resolve the ambiguity yourself — pick the most \
                     reasonable default and proceed."
                        .to_string(),
                ),
            };
        }
        PolicyDecision::Pass
    }
}

/// Gate 7: the permission broker. A non-`Unspecified` target not already
/// always-allowed (and not autopilot) yields `Ask`; the chain caller parks.
///
/// **Autopilot bypass:** when `ctx.autopilot` is true this gate auto-approves
/// every call that survived scope-gate (gate 4) and bash-policy (gate 5). The
/// broker therefore does *no* work for the common read-only envoy profiles,
/// which are `autopilot: true` — their real safety floor is admission
/// (`resolve_tools`, which filters the toolset at spawn) plus the scope-gate.
/// The broker is the live layer only for the principal agent and for the
/// `INTERACTIVE` profile (`autopilot: false`), which forwards prompts up to
/// the parent (ADR-0029).
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
        if ctx.autopilot {
            return PolicyDecision::Approve;
        }
        let rule = scope_target_to_rule(ctx.call_name, &ctx.scope_target);
        if ctx.ctx.permissions().is_always_allowed(&rule) {
            return PolicyDecision::Approve;
        }
        // #10: an attended call whose target is OUTSIDE the granted scope is an
        // *elevation* — the soft scope-gate (gate 4) deliberately let it reach
        // here so the user, not a builtin limit, decides. Mark the prompt so the
        // TUI can render a distinct ⚠ treatment; the operator must understand
        // they are authorising access beyond the configured boundary.
        let elevation = !ctx.operation_scope.allows(&ctx.scope_target);
        PolicyDecision::Ask {
            request: muta_contracts::PermissionRequest {
                id: String::new(), // caller fills the generated id
                tool: ctx.call_name.to_string(),
                label: ctx.tool.permission_label(),
                description: ctx.tool.permission_description(),
                arguments: ctx.arguments.to_string(),
                scope: rule.scope.clone(),
                elevation,
                one_off: false,
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
    use muta_contracts::ToolAccesses;
    use std::collections::HashSet;
    use std::path::PathBuf;

    struct StubTool {
        name: String,
        target: ScopeTarget,
    }

    /// The chain order is load-bearing (see [`default_chain`]): hooks observe
    /// every call, schema validation runs after the hook, the scope gate
    /// precedes the broker. Pin it so a careless reorder fails loudly.
    #[test]
    fn default_chain_order_is_load_bearing() {
        let chain = PermissionChain::new(default_chain());
        assert_eq!(
            chain.policy_names(),
            vec![
                "hook",
                "disabled",
                "schema",
                "scope-gate",
                "bash-policy",
                "ask-user",
                "broker",
            ]
        );
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
        perms: PermissionStore,
    }
    #[async_trait]
    impl PermissionContext for StubCtx {
        async fn check_pre_tool_use(&self, _n: &str, _i: &serde_json::Value) -> PreToolUseVerdict {
            PreToolUseVerdict::default()
        }
        fn apply_scoped_disables(&self, _d: &[(String, RestorePoint)]) {}
        async fn check_bash_policy(&self, _c: &str, _a: &str) -> BashVerdict {
            BashVerdict::Allow
        }
        fn permissions(&self) -> &PermissionStore {
            &self.perms
        }
    }

    /// Build a [`PolicyContext`] for one policy evaluation. The argument count
    /// mirrors the struct's fields one-to-one so each call site reads as a
    /// literal context; grouping them would only obscure that mapping.
    #[allow(clippy::too_many_arguments)]
    fn pctx<'a>(
        tool: &'a Arc<dyn Tool>,
        name: &'a str,
        args: &'a str,
        target: ScopeTarget,
        autopilot: bool,
        op: muta_contracts::OperationScope,
        disabled: std::collections::HashSet<String>,
        scoped: ScopedToolDisable,
        ctxr: &'a dyn PermissionContext,
    ) -> PolicyContext<'a> {
        PolicyContext {
            tool,
            call_name: name,
            arguments: args,
            scope_target: target,
            autopilot,
            operation_scope: op,
            disabled,
            scoped_disabled: scoped,
            ctx: ctxr,
        }
    }

    /// Native absolute paths for scope-policy tests. Unix-looking paths such
    /// as `/home/user` are drive-relative on Windows and therefore test a
    /// different rule than intended.
    fn scoped_test_paths() -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|_| std::env::temp_dir());
        let granted = base.join("muta-policy-granted");
        let inside = granted.join("notes.md");
        let outside = base.join("muta-policy-outside").join("secret.txt");
        (granted, inside, outside)
    }

    #[tokio::test]
    async fn disabled_policy_denies() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "bash".into(),
            target: ScopeTarget::Command("ls".into()),
        });
        let disabled: HashSet<String> = ["bash".to_string()].into_iter().collect();
        let scoped = ScopedToolDisable::default();
        let op = muta_contracts::OperationScope::unrestricted();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(
            &tool,
            "bash",
            "{}",
            ScopeTarget::Unspecified,
            false,
            op.clone(),
            disabled.clone(),
            scoped.clone(),
            &ctxr,
        );
        assert!(matches!(
            DisabledPolicy.evaluate(&c).await,
            PolicyDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn scope_gate_autopilot_blocks_out_of_scope() {
        // No user reachable → an out-of-scope call is hard-denied (safety floor).
        let (granted, _inside, outside) = scoped_test_paths();
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(outside.clone()),
        });
        let op = muta_contracts::OperationScope {
            paths: Some(vec![granted]),
            commands: None,
        };
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(
            &tool,
            "write_file",
            "{}",
            ScopeTarget::Path(outside),
            true, // autopilot
            op.clone(),
            disabled.clone(),
            scoped.clone(),
            &ctxr,
        );
        assert!(matches!(
            ScopeGatePolicy.evaluate(&c).await,
            PolicyDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn scope_gate_attended_hands_out_of_scope_to_user() {
        // A human is reachable → the gate passes; the broker (next gate) asks.
        let (granted, _inside, outside) = scoped_test_paths();
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(outside.clone()),
        });
        let op = muta_contracts::OperationScope {
            paths: Some(vec![granted]),
            commands: None,
        };
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(
            &tool,
            "write_file",
            "{}",
            ScopeTarget::Path(outside),
            false, // attended
            op.clone(),
            disabled.clone(),
            scoped.clone(),
            &ctxr,
        );
        assert!(matches!(
            ScopeGatePolicy.evaluate(&c).await,
            PolicyDecision::Pass
        ));
    }

    #[tokio::test]
    async fn scope_gate_in_scope_passes_regardless_of_autopilot() {
        // Inside the granted scope → always passes (broker applies as usual).
        let (granted, inside, _outside) = scoped_test_paths();
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(inside.clone()),
        });
        let op = muta_contracts::OperationScope {
            paths: Some(vec![granted]),
            commands: None,
        };
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(
            &tool,
            "write_file",
            "{}",
            ScopeTarget::Path(inside),
            true,
            op.clone(),
            disabled.clone(),
            scoped.clone(),
            &ctxr,
        );
        assert!(matches!(
            ScopeGatePolicy.evaluate(&c).await,
            PolicyDecision::Pass
        ));
    }

    #[tokio::test]
    async fn broker_autopilot_approves() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/anywhere")),
        });
        let op = muta_contracts::OperationScope::unrestricted();
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
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
        assert!(matches!(
            BrokerPolicy.evaluate(&c).await,
            PolicyDecision::Approve
        ));
    }

    #[tokio::test]
    async fn broker_asks_when_not_allowed() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/tmp/x")),
        });
        let op = muta_contracts::OperationScope::unrestricted();
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
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
        assert!(matches!(
            BrokerPolicy.evaluate(&c).await,
            PolicyDecision::Ask { .. }
        ));
    }

    #[tokio::test]
    async fn broker_marks_out_of_scope_as_elevation() {
        // #10: an attended out-of-scope call reaches the broker (the soft gate
        // passes it), and the broker's Ask request must carry elevation: true so
        // the TUI renders the distinct ⚠ treatment.
        let (granted, _inside, outside) = scoped_test_paths();
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(outside.clone()),
        });
        let op = muta_contracts::OperationScope {
            paths: Some(vec![granted]),
            commands: None,
        };
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(
            &tool,
            "write_file",
            "{}",
            ScopeTarget::Path(outside),
            false,
            op.clone(),
            disabled.clone(),
            scoped.clone(),
            &ctxr,
        );
        match BrokerPolicy.evaluate(&c).await {
            PolicyDecision::Ask { request, .. } => assert!(request.elevation),
            other => panic!("expected Ask, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn broker_in_scope_ask_is_not_elevation() {
        // An in-scope call's Ask must carry elevation: false (routine prompt).
        let (granted, inside, _outside) = scoped_test_paths();
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(inside.clone()),
        });
        let op = muta_contracts::OperationScope {
            paths: Some(vec![granted]),
            commands: None,
        };
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(
            &tool,
            "write_file",
            "{}",
            ScopeTarget::Path(inside),
            false,
            op.clone(),
            disabled.clone(),
            scoped.clone(),
            &ctxr,
        );
        match BrokerPolicy.evaluate(&c).await {
            PolicyDecision::Ask { request, .. } => assert!(!request.elevation),
            other => panic!("expected Ask, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chain_short_circuits() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/tmp/x")),
        });
        let op = muta_contracts::OperationScope::unrestricted();
        let disabled: HashSet<String> = ["write_file".to_string()].into_iter().collect();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
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
            PolicyDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn chain_falls_back_to_approve() {
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "read_text".into(),
            target: ScopeTarget::Unspecified,
        });
        let op = muta_contracts::OperationScope::unrestricted();
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(
            &tool,
            "read_text",
            "{}",
            ScopeTarget::Unspecified,
            false,
            op.clone(),
            disabled.clone(),
            scoped.clone(),
            &ctxr,
        );
        let chain = PermissionChain::new(vec![Box::new(DisabledPolicy), Box::new(ScopeGatePolicy)]);
        assert!(matches!(chain.evaluate(&c).await, PolicyDecision::Approve));
    }

    #[tokio::test]
    async fn chain_out_of_scope_attended_reaches_broker() {
        // The behaviour the soft gate exists for: an attended out-of-scope call
        // is not hard-blocked — it flows scope-gate (Pass) → broker (Ask), so the
        // user, not a builtin limit, decides the elevation.
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/etc/passwd")),
        });
        let op = muta_contracts::OperationScope {
            paths: Some(vec![PathBuf::from("/home/user")]),
            commands: None,
        };
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(
            &tool,
            "write_file",
            "{}",
            ScopeTarget::Path(PathBuf::from("/etc/passwd")),
            false, // attended
            op.clone(),
            disabled.clone(),
            scoped.clone(),
            &ctxr,
        );
        let chain = PermissionChain::new(vec![Box::new(ScopeGatePolicy), Box::new(BrokerPolicy)]);
        assert!(matches!(
            chain.evaluate(&c).await,
            PolicyDecision::Ask { .. }
        ));
    }

    #[tokio::test]
    async fn chain_out_of_scope_autopilot_blocks() {
        // No human → the soft gate must still block out-of-scope calls; it never
        // reaches a broker that would silently auto-approve under autopilot.
        let tool: Arc<dyn Tool> = Arc::new(StubTool {
            name: "write_file".into(),
            target: ScopeTarget::Path(PathBuf::from("/etc/passwd")),
        });
        let op = muta_contracts::OperationScope {
            paths: Some(vec![PathBuf::from("/home/user")]),
            commands: None,
        };
        let disabled = HashSet::new();
        let scoped = ScopedToolDisable::default();
        let ctxr = StubCtx {
            perms: PermissionStore::new(),
        };
        let c = pctx(
            &tool,
            "write_file",
            "{}",
            ScopeTarget::Path(PathBuf::from("/etc/passwd")),
            true, // autopilot
            op.clone(),
            disabled.clone(),
            scoped.clone(),
            &ctxr,
        );
        let chain = PermissionChain::new(vec![Box::new(ScopeGatePolicy), Box::new(BrokerPolicy)]);
        assert!(matches!(
            chain.evaluate(&c).await,
            PolicyDecision::Deny { .. }
        ));
    }
}
