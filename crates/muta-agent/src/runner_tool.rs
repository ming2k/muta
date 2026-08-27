//! `RunnerTool` — spawns a read-only exploration runner for research subtasks.
//!
//! Lives in `muta-agent` proper (not the [`crate::tools`] module) because it
//! constructs an
//! [`crate::Agent`] internally: spawning an runner is an orchestration
//! concern, not a domain-tool concern. The other tools (Bash/Read/Web/…)
//! stay in [`crate::tools`] and remain pure trait implementations.
//!
//! Admission of tools to the runner is driven by [`muta_contracts::RUNNER_EXPLORE`]
//! — the single source of truth for the read-only / non-interactive /
//! non-recursive policy. See ADR-0011.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;

use muta_contracts::{RunnerPreset, Tool};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, RunnerHandle};

/// Description of the default `spawn_runner` dispatch tool, surfaced to the model.
pub const RUNNER_TOOL_DESCRIPTION: &str = "\
Spawn an isolated Tier-2 Runner agent to perform a focused subtask in a separate \
context window and return a consolidated summary. Set 'role' to 'explore' for read-only \
research (default), 'code' for full implementation and testing, or 'mcp' for specialized \
tool integrations.";

/// Description of the write-capable `runner_code` dispatch tool (bound to the
/// [`muta_contracts::RUNNER_CODE`] profile). Distinct from `RUNNER_TOOL_DESCRIPTION` so
/// the model understands this is the delegation path for *implementation*
/// work, not exploration, and that every write/command the runner makes is
/// user-approved. Paired with the code-profile system prompt, it frames the
/// coder-subagent role (the analogue of kimi-code's `coder` subagent).
pub const RUNNER_CODE_TOOL_DESCRIPTION: &str = "\
Delegate a well-scoped software-engineering task to a coding runner that \
implements the change end to end — it reads the relevant code, edits files, \
and runs builds/tests/git, then returns a technically complete summary of what \
it changed and how it verified the change. Use it for substantial, \
self-contained implementation work you want isolated in its own context \
window. Unlike the read-only `runner`, this one CAN modify files and run \
commands — but every write and command it attempts is presented to the user \
for approval before it executes, just like a top-level call. Do not use it \
for trivial edits you can make directly, and once it is running, leave the \
scope to it (do not redo its work in parallel).";

/// Description of the MCP-specialist dispatch tool (bound to the [`muta_contracts::RUNNER_MCP_SPECIALIST`] profile).
pub const RUNNER_MCP_TOOL_DESCRIPTION: &str = "\
Delegate a specialized integration task (e.g. database operations, external API calls, \
or third-party MCP tool interactions) to a dedicated runner. The runner runs in an isolated \
sandbox with access to dynamic/MCP tools and returns a high-signal summary of the results.";

/// Retry settings for an runner subagent, inherited from the session's provider retry configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerRetryConfig {
    pub max_attempts: usize,
    pub base_ms: u64,
    pub max_ms: u64,
}

impl Default for RunnerRetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 30,
            base_ms: 1_000,
            max_ms: 10_000,
        }
    }
}

/// Live runner handles keyed by the parent tool-call id — the lookup table
/// that lets the harness route a down-direction reply (a permission decision
/// or `ask_user` answer the user gave in the TUI) back into the specific
/// running runner that surfaced the request. Full-duplex (ADR-0029).
///
/// The `task` tool populates this when it spawns a child (and clears the entry
/// when the child finishes); the harness reads it when it needs to reply to a
/// `RunnerEvent::PermissionRequest` / `UserQuestionRequest` that arrived
/// nested under a given `parent_call_id`. Entries are best-effort: a late reply
/// after the child already finished finds no entry (or a dead handle) and
/// degrades to a no-op rather than erroring.
#[derive(Default)]
pub struct RunnerRegistry {
    map: std::sync::Mutex<std::collections::HashMap<String, RunnerHandle>>,
}

impl RunnerRegistry {
    /// Register a steering handle for the runner spawned by the
    /// `parent_call_id` tool call. Replaces any prior entry for that id.
    pub fn register(&self, parent_call_id: &str, handle: RunnerHandle) {
        // Poison-recovery idiom (codebase convention): a panic in another
        // holder poisoned the lock; recover the inner data rather than
        // panicking on a second, downstream error.
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(parent_call_id.to_string(), handle);
    }

    /// Look up the handle for a live runner by its parent tool-call id.
    /// Returns a cloned handle (cheap) so the caller can reply without holding
    /// the lock.
    pub fn get(&self, parent_call_id: &str) -> Option<RunnerHandle> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(parent_call_id)
            .cloned()
    }

    /// Remove the entry for a finished runner. Called when the `task` tool
    /// returns, so the registry never accumulates dead handles for completed
    /// calls (a handle whose `Weak` already expired is harmless but useless).
    pub fn remove(&self, parent_call_id: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(parent_call_id);
    }
}

/// Spawn a read-only exploration runner to handle a research sub-task.
///
/// The runner runs the same provider with the tools admitted by the bound
/// [`RunnerPreset`] (today always [`muta_contracts::RUNNER_EXPLORE`]): read-only, non-interactive,
/// non-recursive. Its final answer is returned to the calling agent, which
/// stays in control of any write operations and any questions for the user.
pub struct RunnerTool {
    provider: Arc<dyn muta_contracts::Provider>,
    toolset: muta_contracts::ToolSet,
    profile: &'static RunnerPreset,
    /// The tool name the model calls this dispatch tool by. The default (set by
    /// [`RunnerTool::new`]) is `"runner"` for the read-only research role; a
    /// second instance bound to a write-capable profile (e.g. [`muta_contracts::RUNNER_CODE`]) takes a
    /// distinct name like `"runner_code"` so it registers as its own capability
    /// alongside the read-only `runner`, instead of colliding on the name.
    tool_name: &'static str,
    /// Human-facing description surfaced to the model as the tool's purpose.
    /// Defaults to the read-only research framing; a write-capable instance
    /// passes its own so the model knows it is the delegation path for
    /// implementation work, not exploration.
    tool_description: &'static str,
    /// Shared handle to the parent agent's variant selection (the **override**
    /// axis). Bound after the parent agent is built (see
    /// [`RunnerTool::bind_variant_selection`]). At spawn the child resolves
    /// its scoped capabilities to the model's chosen variants by snapshotting
    /// this, so an runner — an agent on the same model — inherits the parent's
    /// overrides. `None` (the default, e.g. in tests) means default variants.
    parent_variants:
        std::sync::Mutex<Option<Arc<std::sync::Mutex<muta_contracts::VariantSelection>>>>,
    /// Live workspace authority inherited from the parent. Delegation may
    /// narrow this through the runner's operation scope, never widen it.
    parent_workspace_security:
        std::sync::Mutex<Option<Arc<std::sync::Mutex<muta_contracts::WorkspaceSecuritySnapshot>>>>,
    /// ADR-0141: the parent's human-channel accountant, inherited by every
    /// spawned runner so a child's posture tracks the session's live OR over
    /// attached clients (an interactive session's runner can ask the user;
    /// an autonomous one's child never parks on a missing human).
    parent_human_channel:
        std::sync::Mutex<Option<Arc<muta_contracts::human_request::HumanChannelAccountant>>>,
    /// Full-duplex handle registry (ADR-0029): each spawned runner's
    /// [`RunnerHandle`] is lodged here keyed by the parent tool-call id, so
    /// the harness can route a user's permission / `ask_user` reply back down
    /// into the exact child that surfaced the request. Owned by the tool and
    /// exposed via [`RunnerTool::registry`] so the binary that constructs the
    /// tool (and drives the harness) can hand the same `Arc` to the harness.
    registry: Arc<RunnerRegistry>,
    accounting: std::sync::Mutex<Option<RunnerAccountingContext>>,
    /// Live child cancellation tokens keyed by the parent tool-call id — the
    /// cooperative-cancel arm of interruption (the counterpoint to dropping
    /// the child future). `call_structured_with_events` stores the token each
    /// spawned runner runs under; the harness's executor calls
    /// [`Tool::request_cancel`] when the user interrupts the turn, which
    /// cancels the stored token. The child's round loop observes it at its
    /// next safe boundary, returns its partial transcript through
    /// `run_runner_outcome`, and the parent records it instead of losing it.
    /// Entries are removed when the child's run ends, so a late
    /// `request_cancel` for a finished call degrades to a no-op.
    active_cancels: std::sync::Mutex<std::collections::HashMap<String, CancellationToken>>,
    /// Master preset delegation policy gating which runner presets may be dispatched.
    parent_delegation: std::sync::Mutex<Option<muta_contracts::MasterPresetDelegation>>,
    /// Live source of MCP tools published to the parent's dynamic sink (ADR-0138).
    /// `None` (tests, MCP-less sessions) means the mcp_specialist runner sees no
    /// MCP tools. Bound by bootstrap after the MCP runtime publishes its first
    /// snapshot; consulted when the runner spawns so the child always sees the
    /// *current* toolset, not a stale bootstrap-time copy.
    mcp_tool_source: std::sync::Mutex<Option<Arc<dyn muta_contracts::DynamicToolSource>>>,
    /// The session's workspace root, captured at bootstrap so the child's
    /// operation scope resolves relative `write_paths` against the session's
    /// project — not the daemon process's cwd (ADR-0096). `None` falls back
    /// to the process cwd (tests, single-project processes).
    workspace_root: std::sync::Mutex<Option<std::path::PathBuf>>,
    retry_config: std::sync::Mutex<RunnerRetryConfig>,
}

#[derive(Clone)]
struct RunnerAccountingContext {
    ledger: Arc<muta_contracts::TokenSourceLedger>,
    session_id: Arc<std::sync::Mutex<Option<String>>>,
    round_counter: Arc<std::sync::Mutex<u64>>,
}

impl RunnerTool {
    /// `toolset` should be the parent agent's full capability set; `profile`
    /// declares what the spawned runner may actually use (admission + variant
    /// pins + framing). The caller binds the role explicitly — `&RUNNER_EXPLORE` for
    /// the `spawn_runner` tool.
    pub fn new(
        provider: Arc<dyn muta_contracts::Provider>,
        toolset: muta_contracts::ToolSet,
        profile: &'static RunnerPreset,
    ) -> Self {
        Self::named(
            provider,
            toolset,
            profile,
            "spawn_runner",
            RUNNER_TOOL_DESCRIPTION,
        )
    }

    /// Like [`new`](Self::new) but shares an existing [`RunnerRegistry`] instead
    /// of creating a fresh one.
    pub fn with_registry(
        provider: Arc<dyn muta_contracts::Provider>,
        toolset: muta_contracts::ToolSet,
        profile: &'static RunnerPreset,
        registry: Arc<RunnerRegistry>,
    ) -> Self {
        Self::named_with_registry(
            provider,
            toolset,
            profile,
            "spawn_runner",
            RUNNER_TOOL_DESCRIPTION,
            registry,
        )
    }

    /// Build an MCP-specialist dispatch tool for running dynamic/MCP integrations in an isolated sandbox.
    pub fn mcp_specialist(
        provider: Arc<dyn muta_contracts::Provider>,
        toolset: muta_contracts::ToolSet,
        registry: Arc<RunnerRegistry>,
    ) -> Self {
        Self::named_with_registry(
            provider,
            toolset,
            &muta_contracts::RUNNER_MCP_SPECIALIST,
            "runner_mcp",
            RUNNER_MCP_TOOL_DESCRIPTION,
            registry,
        )
    }

    /// Build a dispatch tool under an explicit name and description. This is
    /// how a second, write-capable runner dispatch tool is constructed: a
    /// profile like [`muta_contracts::RUNNER_CODE`] is paired with a distinct tool name
    /// (e.g. `"runner_code"`) and a description that tells the model this is the
    /// delegation path for implementation work. The read-only `runner` tool and
    /// a named variant coexist as separate capabilities in the parent toolset.
    pub fn named(
        provider: Arc<dyn muta_contracts::Provider>,
        toolset: muta_contracts::ToolSet,
        profile: &'static RunnerPreset,
        tool_name: &'static str,
        tool_description: &'static str,
    ) -> Self {
        Self {
            provider,
            toolset,
            profile,
            tool_name,
            tool_description,
            parent_variants: std::sync::Mutex::new(None),
            parent_workspace_security: std::sync::Mutex::new(None),
            parent_human_channel: std::sync::Mutex::new(None),
            registry: Arc::new(RunnerRegistry::default()),
            accounting: std::sync::Mutex::new(None),
            active_cancels: std::sync::Mutex::new(std::collections::HashMap::new()),
            parent_delegation: std::sync::Mutex::new(None),
            mcp_tool_source: std::sync::Mutex::new(None),
            workspace_root: std::sync::Mutex::new(None),
            retry_config: std::sync::Mutex::new(RunnerRetryConfig::default()),
        }
    }

    /// [`Self::named`] sharing an existing registry. The companion of
    /// [`Self::with_registry`]: a named dispatch tool whose children are
    /// reachable through the same harness reply path as its sibling.
    pub fn named_with_registry(
        provider: Arc<dyn muta_contracts::Provider>,
        toolset: muta_contracts::ToolSet,
        profile: &'static RunnerPreset,
        tool_name: &'static str,
        tool_description: &'static str,
        registry: Arc<RunnerRegistry>,
    ) -> Self {
        Self {
            provider,
            toolset,
            profile,
            tool_name,
            tool_description,
            parent_variants: std::sync::Mutex::new(None),
            parent_workspace_security: std::sync::Mutex::new(None),
            parent_human_channel: std::sync::Mutex::new(None),
            registry,
            accounting: std::sync::Mutex::new(None),
            active_cancels: std::sync::Mutex::new(std::collections::HashMap::new()),
            parent_delegation: std::sync::Mutex::new(None),
            mcp_tool_source: std::sync::Mutex::new(None),
            workspace_root: std::sync::Mutex::new(None),
            retry_config: std::sync::Mutex::new(RunnerRetryConfig::default()),
        }
    }

    /// Bind the master preset delegation policy to enforce allowed runner profiles.
    pub fn bind_master_delegation(&self, delegation: muta_contracts::MasterPresetDelegation) {
        *self
            .parent_delegation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(delegation);
    }

    /// Bind a live source of the parent's dynamic (MCP) tools, consulted when
    /// an mcp_specialist runner spawns (ADR-0138 §2). The snapshot is read at
    /// spawn — not at bind — so periodic MCP re-discovery (McpCatalog's 10-min
    /// refresh, `/mcp` reconnects) reaches subsequent children without
    /// re-binding. `None`-bound (the default) also leaves the mcp_specialist
    /// runner with no MCP tools.
    pub fn bind_dynamic_tool_source(&self, source: Arc<dyn muta_contracts::DynamicToolSource>) {
        *self
            .mcp_tool_source
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(source);
    }

    /// Pin the session's workspace root so spawned runners resolve relative
    /// `write_paths` (ADR-0028) against the session's project rather than the
    /// daemon process's cwd (ADR-0096). Called by the bootstrap right after
    /// construction; `None` (the default) keeps the process-cwd fallback.
    pub fn set_workspace_root(&self, root: Option<std::path::PathBuf>) {
        self.workspace_root
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone_from(&root);
    }

    /// Bind the parent's session-scoped accounting handles. Each spawned
    /// runner gets its own actor id while sharing the session ledger, so nested
    /// provider requests are visible without colliding with the principal's
    /// round/turn numbers.
    pub fn bind_accounting(
        &self,
        ledger: Arc<muta_contracts::TokenSourceLedger>,
        session_id: Arc<std::sync::Mutex<Option<String>>>,
        round_counter: Arc<std::sync::Mutex<u64>>,
    ) {
        *self.accounting.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(RunnerAccountingContext {
                ledger,
                session_id,
                round_counter,
            });
    }

    /// Bind the parent agent's variant-selection handle (the **override** axis)
    /// so spawned runners inherit the model's tool overrides. Called once,
    /// after the parent agent is constructed (the agent owns the handle). When
    /// unbound, runners use each capability's default variant.
    pub fn bind_variant_selection(
        &self,
        handle: Arc<std::sync::Mutex<muta_contracts::VariantSelection>>,
    ) {
        *self
            .parent_variants
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    /// ADR-0141: bind the session's channel accountant; spawned runners
    /// inherit it (see [`Self::spawn`] wiring of
    /// `Agent::set_human_channel_accountant`).
    pub fn bind_human_channel(
        &self,
        handle: Arc<muta_contracts::human_request::HumanChannelAccountant>,
    ) {
        *self
            .parent_human_channel
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(handle);
    }

    fn parent_human_channel(
        &self,
    ) -> Option<Arc<muta_contracts::human_request::HumanChannelAccountant>> {
        self.parent_human_channel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn bind_workspace_security(
        &self,
        handle: Arc<std::sync::Mutex<muta_contracts::WorkspaceSecuritySnapshot>>,
    ) {
        *self
            .parent_workspace_security
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(handle);
    }

    /// Snapshot the parent's current variant selection (empty when unbound).
    fn variant_snapshot(&self) -> muta_contracts::VariantSelection {
        self.parent_variants
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|h| h.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .unwrap_or_default()
    }

    /// Bind the parent's provider retry settings so spawned runners inherit
    /// the session's retry budget and backoff parameters.
    pub fn bind_retry_policy(&self, max_attempts: usize, base_ms: u64, max_ms: u64) {
        *self.retry_config.lock().unwrap_or_else(|e| e.into_inner()) = RunnerRetryConfig {
            max_attempts: max_attempts.clamp(1, 60),
            base_ms,
            max_ms,
        };
    }

    /// The shared handle registry for runners spawned by this tool. The
    /// binary passes this `Arc` to the harness so a user reply in the TUI can
    /// be routed back into the live child (ADR-0029). Each `RunnerTool` instance
    /// owns its own registry (children of different dispatch tools are
    /// disjoint), which is fine because the harness that needs to reply is the
    /// same one that constructed the tool.
    pub fn registry(&self) -> Arc<RunnerRegistry> {
        self.registry.clone()
    }
}

#[async_trait]
impl Tool for RunnerTool {
    fn name(&self) -> &str {
        self.tool_name
    }
    fn description(&self) -> &str {
        self.tool_description
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "Short label for the sub-task (<=60 chars)" },
                "prompt": { "type": "string", "description": "The full, self-contained instructions for the runner" },
                "role": {
                    "type": "string",
                    "enum": ["explore", "code", "mcp"],
                    "description": "Optional runner role: 'explore' (default, read-only research), 'code' (coding, file edits, testing), or 'mcp' (specialized tool integration). Defaults to 'explore'."
                },
                "preset": { "type": "string", "description": "Legacy alias for role." }
            },
            "required": ["description", "prompt"]
        })
    }

    /// `task` spawns an runner; runner profiles exclude it to prevent
    /// unbounded recursion.
    fn spawns_runner(&self) -> bool {
        true
    }

    /// The runner's in-flight call owns a partial transcript worth preserving,
    /// so the harness routes turn cancellation through
    /// [`Tool::request_cancel`] instead of dropping the future: the child
    /// stops at its next safe boundary, returns its partial work, and the
    /// parent records it as an interrupted result.
    fn supports_cooperative_cancel(&self) -> bool {
        true
    }

    /// Cancel the live child spawned by the `call_id` call. The child's round
    /// loop observes its token at the next safe boundary and returns its
    /// partial transcript through `run_runner_outcome`, so the parent executor
    /// can drain it instead of dropping it. Returns `false` for an unknown or
    /// already-finished call — the harness then falls back to the drop path.
    fn request_cancel(&self, call_id: &str) -> bool {
        let Some(token) = self
            .active_cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(call_id)
            .cloned()
        else {
            return false;
        };
        token.cancel();
        true
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.run_runner(None, arguments, Box::new(|_| {})).await
    }

    async fn call_with_events<'a>(
        &self,
        call_id: &str,
        arguments: &str,
        on_event: Box<dyn FnMut(muta_contracts::RunnerEvent) + Send + 'a>,
    ) -> Result<String, String> {
        self.run_runner(Some(call_id), arguments, on_event).await
    }

    async fn call_structured_with_events<'a>(
        &self,
        call_id: &str,
        arguments: &str,
        on_event: Box<dyn FnMut(muta_contracts::RunnerEvent) + Send + 'a>,
        _on_stream: &mut (dyn FnMut(muta_contracts::ToolStream) + Send + 'a),
        _stdin: muta_contracts::StdinPolicy,
    ) -> Result<muta_contracts::ToolOutput, String> {
        // Run the runner, streaming its lifecycle as RunnerEvents to the
        // parent harness (so the live TUI builds the nested view in real
        // time), then return a structured payload carrying the full transcript
        // + real token usage so the parent can persist children and account
        // cost truthfully.
        //
        // `call_id` is now used (not discarded): it keys the child's duplex
        // handle in the registry (ADR-0029) so a user reply can flow back down
        // into this exact child while it runs.
        //
        // Failure path: an runner that hit the 32-turn limit, repeated-call
        // guard, or a provider error returns an Runner payload too — the
        // structured `failed` flag is set so the UI classifies it as Failed
        // without text-sniffing, and the partial transcript is preserved so
        // the user can resume into the half-finished work and the real token
        // cost is accounted. The summary still carries an `Error:` prefix so
        // the parent *model* understands the sub-task did not succeed. Only
        // input-validation errors (bad JSON, missing fields) propagate as
        // `Err`, because they have no partial transcript worth keeping.
        let outcome = self
            .run_runner_outcome(Some(call_id), arguments, on_event)
            .await?;
        let summary = if outcome.final_content.trim().is_empty() {
            if outcome.failed {
                "(runner failed before producing an answer)".to_string()
            } else {
                "(runner returned no answer)".to_string()
            }
        } else {
            outcome.final_content.trim().to_string()
        };
        Ok(muta_contracts::ToolOutput::Runner {
            summary,
            messages: outcome.messages,
            usage: outcome.token_usage,
            generation_ms: outcome.generation_ms,
            failed: outcome.failed,
            interrupted: outcome.interrupted,
        })
    }
}

/// Internal result of running an runner. Bundles everything the parent
/// harness needs to persist the nested transcript and account for real cost.
struct RunnerOutcome {
    messages: Vec<muta_contracts::Message>,
    token_usage: muta_contracts::TokenUsage,
    /// Final assistant content, mirrored for convenience so the parent doesn't
    /// have to scan `messages` for the last Assistant turn.
    final_content: String,
    /// Whether the runner terminated abnormally (hit its turn cap,
    /// repeated-call guard, or a provider error). Drives the structured
    /// `failed` flag on the returned [`muta_contracts::ToolOutput::Runner`]
    /// instead of the old `summary.starts_with("Error")` text sniff.
    failed: bool,
    /// Whether the runner was stopped by the parent before finishing (the turn
    /// was cancelled). Distinct from `failed`: the partial transcript is
    /// preserved either way, but interruption is a user-initiated stop that
    /// the model should treat as resumable work, not a sub-task error.
    interrupted: bool,
    /// The runner's own generation time (summed across its completed provider
    /// requests). Folded into the parent round's `generation_ms` so the
    /// throughput denominator matches its numerator scope (runner output
    /// tokens already reach the parent via `token_usage`).
    generation_ms: u64,
}

impl RunnerTool {
    async fn run_runner_outcome<'a>(
        &self,
        call_id: Option<&str>,
        arguments: &str,
        mut on_event: Box<dyn FnMut(muta_contracts::RunnerEvent) + Send + 'a>,
    ) -> Result<RunnerOutcome, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let description = args["description"]
            .as_str()
            .ok_or("Missing 'description'")?
            .trim();
        let prompt = args["prompt"].as_str().ok_or("Missing 'prompt'")?;
        if description.is_empty() {
            return Err("'description' must not be empty.".to_string());
        }
        if prompt.trim().is_empty() {
            return Err("'prompt' must not be empty.".to_string());
        }

        let requested_preset = args
            .get("role")
            .or_else(|| args.get("preset"))
            .and_then(|p| p.as_str())
            .unwrap_or(self.profile.name);
        let profile =
            muta_contracts::RunnerPresetPool::find(requested_preset).unwrap_or(self.profile);

        if let Some(delegation) = self
            .parent_delegation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            && !delegation.admits_runner(profile.name)
        {
            return Err(format!(
                "Master preset '{}' does not admit runner preset '{}'. Admitted presets: {:?}",
                delegation.preset_id, profile.name, delegation.runner_presets
            ));
        }

        // Announce the bound profile name first so the parent harness / TUI
        // can label this runner by its role (explore / plan / verify / …)
        // rather than a generic "Runner". Emitted before the child runs.
        on_event(muta_contracts::RunnerEvent::Started {
            profile: profile.name.to_string(),
        });

        // Resolve the pool for this runner: profile selection ⊓ model selection.
        // The runner is an agent on the *same* model as the parent, so it carries
        // the parent's model (capability limits + variant overrides). The profile
        // contributes the role scope and any variant pins; the model contributes
        // its variant overrides (snapshotted from the parent) and its hard
        // capability limits. `resolve_tools` composes both and applies the runner
        // runtime hard rules (no recursion / control-flow / blocking-on-user).
        let model = muta_contracts::resolve_model(&self.provider.model());
        let model_sel =
            muta_contracts::ToolSelection::unrestricted().with_variants(self.variant_snapshot());
        let mut sub_tools = profile.resolve_tools(&self.toolset, &model, &model_sel);

        // ADR-0138 §2: the mcp_specialist envoy receives the session's live
        // dynamic (MCP) toolset on top of the static snapshot. Reading the
        // source at spawn (not at bind) means McpCatalog's periodic
        // re-discovery and `/mcp` reconnects reach later children without
        // re-binding. The admission filter still applies: a recursive or
        // control-flow dynamic tool would be silently dropped here, mirroring
        // `resolve_tools`'s hard rules for static tools.
        if profile.name == "mcp_specialist"
            && let Some(source) = self
                .mcp_tool_source
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .cloned()
        {
            let dynamic = source.snapshot_tools();
            if !dynamic.is_empty() {
                let static_names: HashSet<String> =
                    sub_tools.iter().map(|t| t.name().to_string()).collect();
                sub_tools.extend(dynamic.into_iter().filter(|tool| {
                    !static_names.contains(tool.name())
                        && profile.tool_policy.admits_runtime(tool.as_ref())
                }));
            }
        }

        // The runner's identity *is* its profile's system prompt — that is the
        // persona/mission framing for this role (e.g. RUNNER_EXPLORE's research
        let identity = crate::AgentIdentity::from_persona(profile.system_prompt);
        let mut runner = Agent::new(self.provider.clone(), sub_tools, identity);
        runner.set_tier(muta_contracts::AgentTier::Runner);

        if let Some(handle) = self
            .parent_workspace_security
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .cloned()
        {
            runner.bind_workspace_security_handle(handle);
        }
        let runner = Arc::new(runner);
        if let Some(accounting) = self
            .accounting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            let session_id = accounting
                .session_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap_or_default();
            let round = *accounting
                .round_counter
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let actor = call_id
                .map(|id| format!("runner:{id}"))
                .unwrap_or_else(|| format!("runner:{}", uuid::Uuid::new_v4()));
            runner.set_thread_id(session_id);
            runner.restore_round_count(round);
            runner.set_accounting_actor_id(actor);
            runner.install_token_ledger(accounting.ledger);
        }
        // A `task` runner runs unobstructed: disable the deterministic
        // read-loop guard's nudge (ADR-0034) so a short-lived, parent-supervised
        // runner is never steered by it. The parent and `abort` remain its
        // backstops.
        runner.set_doom_guard_config(muta_contracts::DoomGuardConfig::disabled());
        // Full-duplex (ADR-0029): install the child's steering inbox and lodge
        // its handle in the registry keyed by the parent tool-call id. Now any
        // permission / `ask_user` request the child surfaces travels *up* via
        // `forward_event`, and the user's reply can travel *down* via the
        // registry → handle → `reply_permission` / `reply_user_question`,
        // resolving the child's parked oneshot. A `None` call_id (the bare
        // `call` path, no harness involvement) skips registration — there is no
        // one to reply, so the child must stay self-contained.
        let _handle = runner.install_inbox();
        if let Some(id) = call_id {
            self.registry.register(id, _handle.clone());
        }
        // Full-duplex (ADR-0029): the broker gate is now profile-driven. The
        // built-in profiles keep `autopilot: true` to preserve the legacy
        // autonomous contract, but a profile with `autopilot: false` lets a
        // runner's write/execute tool calls surface as
        // `RunnerEvent::PermissionRequest` up to the parent, with the user's
        // reply routed back down via the registry → handle →
        // `reply_permission` (the parked oneshot resolves directly, no inbox
        // drain needed).
        runner.set_yolo(profile.yolo);
        // ADR-0141: the child inherits the parent's human-channel posture
        // source. An interactive session's runners can ask the user through
        // the parent's channel (permission requests flow up via
        // RunnerEvent); an autonomous parent's children never park on a
        // human that is not there — they fail closed / settle by labeled
        // policy in the child, instead of deadlocking the parent's tool
        // call on an unanswerable question.
        if let Some(accountant) = self.parent_human_channel() {
            runner.set_human_channel_accountant(accountant);
        }
        // Resolve the bound profile's write grant (ADR-0028) against the
        // session's workspace root (falling back to the process cwd when no
        // root was captured) and set it on the child. All built-in profiles
        // (RUNNER_EXPLORE/RUNNER_TITLE: empty `write_paths`) resolve to
        // `WriteScope::None`, consistent with their admission (no write tools
        // admitted anyway).
        let cwd = self
            .workspace_root
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        runner.set_operation_scope(profile.resolve_operation_scope(&cwd));
        // Runners are short-lived and read-only by profile, and session
        // review is on-demand (`/review`) with no automatic firing — so a
        // research runner never pays for a diagnostic and review can never
        // recurse. No setup needed here. ADR-0018.

        // The runner's durable transcript opens with just the task as the user
        // message. Request assembly composes a fresh head system message every
        // round from the profile persona (carried via `AgentIdentity`, set
        // above) and mission-neutral system-prompt policy — see ADR-0061.
        //
        // An earlier `Task: {description}` system message here was dead code:
        // request assembly projects legacy system messages out before adding
        // the profile composition, so the task wrapper never reached the
        // model. Dropping it makes the single-message path honest. The task
        // itself is the user message; `description` remains a required label
        // arg (validated above) for the parent / TUI.
        let mut messages = vec![crate::conversation_context::visible_user(
            muta_contracts::InjectionKind::RunnerTask,
            prompt,
        )];
        // The runner runs under its own cancellation token. When the parent
        // turn is interrupted, the harness's executor calls
        // [`Tool::request_cancel`] on this tool with the parent tool-call id,
        // which cancels the stored token below; the child's round loop
        // observes it at its next safe boundary and returns its partial
        // transcript through the error arm of this function — so the parent
        // records the half-finished work instead of dropping it. A `None`
        // call_id (the bare `call` path, no harness involvement) means nothing
        // can cancel the child, so it keeps a fresh never-cancelled token.
        let child_cancel = CancellationToken::new();
        if let Some(id) = call_id {
            self.active_cancels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.to_string(), child_cancel.clone());
        }
        // Track the runner's own ReAct position as `ModelRequestStarted`
        // events arrive so the streamed `StreamStart` / `ToolCall` events can
        // carry it (mirroring the main session's `(round, turn)` stamping).
        let mut position: (u64, usize) = (1, 0);
        // Transient-provider-retry loop. The top-level interactive round
        // retries `HarnessError::Retryable` in `orchestration::execute_round`
        // (config: `provider_retry_max_attempts`), but an runner runs through
        // `run_streaming_with_events` directly and had *no* retry at all —
        // one flaky long SSE generation (the GLM `xhigh`-effort stream that
        // gets cut mid-body) killed the whole sub-task after minutes of
        // work. Mirror the top-level contract here, bounded and simpler:
        // reuse the same round state across attempts so completed turns are
        // not replayed, back off exponentially, and never retry an
        // interruption, a hard terminal error, or a non-retryable one.
        let retry_config = *self.retry_config.lock().unwrap_or_else(|e| e.into_inner());
        let retry_limit = retry_config.max_attempts.clamp(1, 60);
        // Hidden-chain gate, computed once at the source. A hidden-chain model
        // (GPT-5.x, `ReasoningSummary`) surfaces only a reasoning *summary*,
        // never its full chain, so streaming it upward would disclose text the
        // principal's live path also refuses to show (the TUI drops
        // `StreamReasoningDelta` for such models at message creation). The
        // runner shares the session's provider, so the parent's model is the
        // runner's model. Unknown ids default to disclosed — mirroring the
        // `model_by_id` (not `resolve`) rule of both TUI gates — so local and
        // user-defined models that reason still stream their chains.
        let hidden_chain = !muta_contracts::model_by_id(&self.provider.model())
            .map(|model| model.thinking.chain_disclosed())
            .unwrap_or(true);
        let short_id = call_id
            .map(|id| {
                let clean = id.trim_start_matches("call_");
                &clean[..clean.len().min(6)]
            })
            .unwrap_or("subagent");
        let origin_label = format!("runner #{} · {}", short_id, profile.name);

        let mut round = runner.begin_streaming_round();
        let mut attempt: usize = 0;
        let result = loop {
            attempt += 1;
            let run = runner
                .resume_streaming_with_events(&mut messages, &child_cancel, &mut round, |event| {
                    if let muta_contracts::AgentEvent::ModelRequestStarted { round, turn, .. } =
                        &event
                    {
                        position = (*round, *turn);
                    }
                    Self::forward_event(
                        event,
                        position,
                        hidden_chain,
                        Some(&origin_label),
                        &mut on_event,
                    )
                })
                .await;
            match run {
                Ok(outcome) => break Ok(outcome),
                Err(muta_contracts::HarnessError::Retryable {
                    message,
                    retry_after_ms,
                }) if attempt < retry_limit => {
                    let base_ms = crate::orchestration::retry_delay_ms(
                        attempt,
                        retry_after_ms,
                        retry_config.base_ms,
                        retry_config.max_ms,
                    );
                    let delay_ms = crate::orchestration::apply_jitter_ms(base_ms, |_| {
                        fastrand::u64(0..base_ms)
                    });
                    tracing::warn!(
                        attempt,
                        max_attempts = retry_limit,
                        delay_ms,
                        error = %message,
                        "runner hit a transient provider error; retrying"
                    );
                    on_event(muta_contracts::RunnerEvent::Notice(
                        muta_contracts::AgentNotice::new(
                            muta_contracts::NoticeKind::ProviderRetry,
                            muta_contracts::NoticeSeverity::Warning,
                            format!(
                                "Runner retrying after transient provider error \
                                 ({attempt}/{retry_limit})"
                            ),
                            muta_contracts::NoticeSource::Harness,
                        )
                        .with_body(format!(
                            "Waiting {}s before retrying: {}",
                            delay_ms.div_ceil(1_000),
                            crate::orchestration::public_retry_reason(&message),
                        )),
                    ));
                    on_event(muta_contracts::RunnerEvent::Activity(format!(
                        "waiting to retry ({}s)",
                        delay_ms.div_ceil(1_000)
                    )));
                    tokio::select! {
                        _ = child_cancel.cancelled() => {
                            break Err(muta_contracts::HarnessError::Interrupted)
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
                    }
                }
                Err(error) => break Err(error),
            }
        };
        // Drop the registry entry for this call regardless of outcome so it
        // never holds a dead handle. The child `Arc` is also dropped here
        // (the last strong ref besides the registry's `Weak`), so any late
        // reply via the handle degrades to a no-op. The cancellation entry is
        // dropped alongside, so a late `request_cancel` finds nothing to
        // cancel and the harness falls back to dropping a finished call.
        if let Some(id) = call_id {
            self.registry.remove(id);
            self.active_cancels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(id);
        }
        match result {
            Ok(result) => {
                let final_content = result.message.content.clone();
                Ok(RunnerOutcome {
                    messages,
                    token_usage: result.token_usage,
                    final_content,
                    failed: false,
                    interrupted: false,
                    generation_ms: result.generation_ms,
                })
            }
            Err(error) => {
                // Interruption (the parent cancelled the turn): preserve the
                // partial transcript as an *interrupted* outcome — not an
                // error. The model must understand the sub-task was stopped by
                // the user (resumable work), not that it failed. On a genuine
                // failure we surface the partial transcript too — both so the
                // parent's tool-result message carries the runner's
                // work-in-progress `children` and so the real token cost
                // reaches the parent round's accounting; the `final_content`
                // is prefixed `Error: …` so the failure classifier and the
                // TUI's Failed badge both trigger.
                if matches!(error, muta_contracts::HarnessError::Interrupted) {
                    let tool_calls = messages
                        .iter()
                        .filter(|m| m.role == muta_contracts::Role::Tool)
                        .count();
                    let partial = messages.iter().rev().find_map(|m| {
                        (m.role == muta_contracts::Role::Assistant && !m.content.trim().is_empty())
                            .then(|| m.content.trim().to_string())
                    });
                    let final_content = match partial {
                        Some(text) => format!(
                            "Interrupted: the runner was stopped by the user before completing. \
                             It ran {tool_calls} tool call(s) and produced the following partial \
                             findings:\n{text}"
                        ),
                        None => format!(
                            "Interrupted: the runner was stopped by the user before producing any \
                             findings (it ran {tool_calls} tool call(s))."
                        ),
                    };
                    tracing::info!(
                        tool_calls,
                        "runner interrupted by parent; preserving partial transcript"
                    );
                    return Ok(RunnerOutcome {
                        messages,
                        token_usage: muta_contracts::TokenUsage::default(),
                        final_content,
                        failed: false,
                        interrupted: true,
                        generation_ms: 0,
                    });
                }
                let error_string = error.to_string();
                tracing::warn!(error = %error_string, "runner failed; preserving partial transcript");
                Ok(RunnerOutcome {
                    messages,
                    token_usage: muta_contracts::TokenUsage::default(),
                    final_content: format!("Error: {error_string}"),
                    failed: true,
                    interrupted: false,
                    generation_ms: 0,
                })
            }
        }
    }

    async fn run_runner<'a>(
        &self,
        call_id: Option<&str>,
        arguments: &str,
        on_event: Box<dyn FnMut(muta_contracts::RunnerEvent) + Send + 'a>,
    ) -> Result<String, String> {
        let outcome = self
            .run_runner_outcome(call_id, arguments, on_event)
            .await?;
        let content = outcome.final_content.trim().to_string();
        if content.is_empty() {
            Ok("(runner returned no answer)".to_string())
        } else {
            Ok(content)
        }
    }

    fn forward_event(
        event: muta_contracts::AgentEvent,
        position: (u64, usize),
        hidden_chain: bool,
        origin: Option<&str>,
        on_event: &mut dyn FnMut(muta_contracts::RunnerEvent),
    ) {
        match event {
            muta_contracts::AgentEvent::Notice(notice) => {
                on_event(muta_contracts::RunnerEvent::Notice(notice));
            }
            muta_contracts::AgentEvent::ModelRequestStarted { turn, .. } => {
                let status = if turn == 0 {
                    "waiting for model".to_string()
                } else {
                    format!("waiting for model (turn {})", turn + 1)
                };
                on_event(muta_contracts::RunnerEvent::Activity(status));
            }
            muta_contracts::AgentEvent::AssistantDelta { delta, start } => {
                if start {
                    on_event(muta_contracts::RunnerEvent::StreamStart {
                        round: position.0,
                        turn: position.1,
                    });
                }
                on_event(muta_contracts::RunnerEvent::StreamDelta(delta));
            }
            muta_contracts::AgentEvent::AssistantEnd(content) => {
                on_event(muta_contracts::RunnerEvent::StreamEnd(content));
            }
            // The runner's reasoning chain, streamed live instead of surfacing
            // only after a session reload. Without these arms the child's
            // thinking fell into the catch-all `_ => {}` below — the durable
            // transcript kept it (`Message::reasoning_content`) and a resumed
            // session rendered it, but the run itself showed nothing: a live
            // drill-in and a reloaded one disagreed about what the runner did.
            // Gated at the source for hidden-chain models so the summary-only
            // chain is never disclosed (the caller computed `hidden_chain`
            // once; the principal's live path applies the same gate).
            muta_contracts::AgentEvent::ReasoningDelta { delta, start } => {
                if hidden_chain {
                    return;
                }
                if start {
                    on_event(muta_contracts::RunnerEvent::StreamReasoningStart {
                        round: position.0,
                        turn: position.1,
                    });
                }
                on_event(muta_contracts::RunnerEvent::StreamReasoningDelta(delta));
            }
            muta_contracts::AgentEvent::ReasoningEnd(content) => {
                if hidden_chain {
                    return;
                }
                on_event(muta_contracts::RunnerEvent::StreamReasoningEnd(content));
            }
            muta_contracts::AgentEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                on_event(muta_contracts::RunnerEvent::ToolCall {
                    id,
                    name,
                    arguments,
                    round: position.0,
                    turn: position.1,
                });
            }
            muta_contracts::AgentEvent::ToolResult {
                id,
                name,
                output,
                duration_ms,
                ..
            } => {
                on_event(muta_contracts::RunnerEvent::ToolResult {
                    id,
                    name,
                    output,
                    duration_ms,
                });
            }
            // Full-duplex (ADR-0029 / ADR-0138): a permission broker request from the
            // child travels *up* stamped with the child's short hash and profile origin.
            muta_contracts::AgentEvent::PermissionRequest(mut request) => {
                if request.origin.is_none() {
                    request.origin = origin.map(str::to_string);
                }
                on_event(muta_contracts::RunnerEvent::PermissionRequest(request));
            }
            // Same full-duplex contract as the permission arm above.
            muta_contracts::AgentEvent::UserQuestionRequest(mut request) => {
                if request.origin.is_none() {
                    request.origin = origin.map(str::to_string);
                }
                on_event(muta_contracts::RunnerEvent::UserQuestionRequest(request));
            }
            // An interactive `bash` inside the runner needs operator
            // stdin; forward the request up so the parent harness can surface
            // it, with the reply routed back down via `reply_input`.
            muta_contracts::AgentEvent::StdinRequest(request) => {
                on_event(muta_contracts::RunnerEvent::StdinRequest(request));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::{self, BoxStream};
    use muta_contracts::{Message, Provider, ProviderStreamEvent, RUNNER_EXPLORE, Role};

    struct CannedProvider;

    #[async_trait::async_trait]
    impl Provider for CannedProvider {
        async fn chat(&self, _request: muta_contracts::ModelRequest) -> Result<Message, String> {
            Ok(Message::new(Role::Assistant, "found 3 relevant files"))
        }
        async fn stream_chat(
            &self,
            _request: muta_contracts::ModelRequest,
        ) -> Result<BoxStream<'static, Result<String, String>>, String> {
            Ok(Box::pin(stream::once(async {
                Ok("found 3 relevant files".to_string())
            })))
        }
    }

    #[derive(Default)]
    struct RecordingProvider {
        request: std::sync::Mutex<Option<muta_contracts::ModelRequest>>,
    }

    /// Fails the first `stream_chat_events` call with a retryable transport
    /// error, succeeds afterwards — the exact shape of a GLM long SSE stream
    /// cut off mid-body (`Kind::Decode` → `[MUTA_RETRYABLE]`), which before
    /// the runner retry loop killed the sub-task outright.
    struct FlakyThenOkProvider {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FlakyThenOkProvider {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for FlakyThenOkProvider {
        async fn chat(&self, _request: muta_contracts::ModelRequest) -> Result<Message, String> {
            Ok(Message::new(Role::Assistant, "recovered"))
        }

        async fn stream_chat(
            &self,
            _request: muta_contracts::ModelRequest,
        ) -> Result<BoxStream<'static, Result<String, String>>, String> {
            Ok(Box::pin(stream::once(async {
                Ok("recovered".to_string())
            })))
        }

        async fn stream_chat_events(
            &self,
            _request: muta_contracts::ModelRequest,
        ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, String>>, String> {
            let seen = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if seen == 0 {
                // Retryable mid-stream failure (what `transport_error` now
                // produces for `Kind::Decode` stream truncation).
                return Err(muta_contracts::retryable_error(
                    "OpenAI transport error: error decoding response body \
                     (connection closed before message completed)",
                    None,
                ));
            }
            Ok(Box::pin(stream::iter(vec![Ok(
                ProviderStreamEvent::TextDelta("recovered".to_string()),
            )])))
        }
    }
    #[async_trait::async_trait]
    impl Provider for RecordingProvider {
        async fn chat(&self, request: muta_contracts::ModelRequest) -> Result<Message, String> {
            *self.request.lock().unwrap() = Some(request);
            Ok(Message::new(Role::Assistant, "found 3 relevant files"))
        }

        async fn stream_chat(
            &self,
            request: muta_contracts::ModelRequest,
        ) -> Result<BoxStream<'static, Result<String, String>>, String> {
            *self.request.lock().unwrap() = Some(request);
            Ok(Box::pin(stream::once(async {
                Ok("found 3 relevant files".to_string())
            })))
        }
    }

    struct EchoReadTool;

    #[async_trait::async_trait]
    impl Tool for EchoReadTool {
        fn name(&self) -> &str {
            "read_text"
        }
        fn description(&self) -> &str {
            "test read tool"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("echo".to_string())
        }
    }

    /// A terse `read_text` variant and a write tool, to prove an runner
    /// resolves the *model's* variant (override axis) and then narrows to the
    /// *profile's* scope (scope axis) — the two are orthogonal.
    struct TerseReadTool;
    #[async_trait::async_trait]
    impl Tool for TerseReadTool {
        fn name(&self) -> &str {
            "read_text"
        }
        fn variant(&self) -> &str {
            "terse"
        }
        fn description(&self) -> &str {
            "terse read tool"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("terse".to_string())
        }
    }
    #[test]
    fn runner_inherits_model_variant_then_applies_profile_scope() {
        // `StubWriteTool` (name "stub_write") is not in RUNNER_EXPLORE's read-only
        // scope, so it is always excluded; `read_text` has two variants.
        let toolset = muta_contracts::ToolSet::from_tools([
            std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>,
            std::sync::Arc::new(TerseReadTool) as std::sync::Arc<dyn Tool>,
            std::sync::Arc::new(StubWriteTool) as std::sync::Arc<dyn Tool>,
        ]);
        let tool = RunnerTool::new(
            std::sync::Arc::new(CannedProvider),
            toolset,
            &RUNNER_EXPLORE,
        );

        let resolve = |tool: &RunnerTool| {
            let model = muta_contracts::resolve_model(&CannedProvider.model());
            let model_sel = muta_contracts::ToolSelection::unrestricted()
                .with_variants(tool.variant_snapshot());
            tool.profile
                .resolve_tools(&tool.toolset, &model, &model_sel)
        };

        // Unbound (no model override) → read_text resolves to its default
        // variant; the out-of-scope write tool is excluded regardless.
        let scoped = resolve(&tool);
        let read = scoped.iter().find(|t| t.name() == "read_text");
        assert_eq!(read.map(|t| t.variant()), Some("default"));
        assert!(scoped.iter().all(|t| t.name() != "stub_write"));

        // Bind a model selection pinning read_text=terse: the runner inherits
        // the override (terse), while scope is still profile-driven.
        let mut sel = muta_contracts::VariantSelection::new();
        sel.insert("read_text".to_string(), "terse".to_string());
        tool.bind_variant_selection(std::sync::Arc::new(std::sync::Mutex::new(sel)));
        let scoped = resolve(&tool);
        let read = scoped.iter().find(|t| t.name() == "read_text");
        assert_eq!(read.map(|t| t.variant()), Some("terse"));
        assert!(scoped.iter().all(|t| t.name() != "stub_write"));
    }

    #[tokio::test]
    async fn runner_retries_after_transient_stream_failure() {
        let provider = std::sync::Arc::new(FlakyThenOkProvider::new());
        let tool = RunnerTool::new(
            std::sync::Arc::clone(&provider) as std::sync::Arc<dyn Provider>,
            muta_contracts::ToolSet::from_tools([
                std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
            ]),
            &RUNNER_EXPLORE,
        );

        let output = tool
            .call(r#"{"description":"find files","prompt":"where are the handlers?"}"#)
            .await
            .expect("the runner must recover from one transient stream failure");

        assert_eq!(
            output, "recovered",
            "the retry must reach the successful attempt's answer"
        );
        assert_eq!(
            provider.calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "exactly one transient failure then one success"
        );
    }

    #[tokio::test]
    async fn runner_inherits_and_respects_custom_retry_policy() {
        let provider = std::sync::Arc::new(FlakyThenOkProvider::new());
        let tool = RunnerTool::new(
            std::sync::Arc::clone(&provider) as std::sync::Arc<dyn Provider>,
            muta_contracts::ToolSet::from_tools([
                std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
            ]),
            &RUNNER_EXPLORE,
        );
        tool.bind_retry_policy(1, 10, 10); // only 1 attempt

        let output = tool
            .call(r#"{"description":"find files","prompt":"where are the handlers?"}"#)
            .await
            .expect("tool call returns error string in outcome");

        assert!(
            output.starts_with("Error:"),
            "should return error string when retry limit is 1 and first call fails: {output}"
        );
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn task_tool_runs_read_only_runner_and_returns_answer() {
        let tool = RunnerTool::new(
            std::sync::Arc::new(CannedProvider),
            muta_contracts::ToolSet::from_tools([
                std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
            ]),
            &RUNNER_EXPLORE,
        );

        let output = tool
            .call(r#"{"description":"find files","prompt":"where are the handlers?"}"#)
            .await
            .unwrap();

        assert_eq!(output, "found 3 relevant files");
    }

    /// A provider that lets the test control when the *second* model request
    /// is in flight: the first request returns a `read_text` tool call (which
    /// the runner executes), the second flips `second_request_started` and
    /// then never produces a stream event — so the runner is parked mid-flight
    /// until its cancellation token fires.
    struct GatedProvider {
        requests: std::sync::atomic::AtomicUsize,
        second_request_started: tokio::sync::watch::Sender<bool>,
    }

    #[async_trait]
    impl Provider for GatedProvider {
        async fn chat(&self, _request: muta_contracts::ModelRequest) -> Result<Message, String> {
            Ok(Message::new(Role::Assistant, "gated"))
        }
        async fn stream_chat(
            &self,
            _request: muta_contracts::ModelRequest,
        ) -> Result<BoxStream<'static, Result<String, String>>, String> {
            Ok(Box::pin(stream::empty()))
        }
        async fn stream_chat_events(
            &self,
            _request: muta_contracts::ModelRequest,
        ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, String>>, String> {
            if self
                .requests
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                // First request: ask the runner to run its `read_text` tool.
                Ok(Box::pin(stream::iter(vec![Ok(
                    ProviderStreamEvent::ToolCallDelta {
                        index: 0,
                        id: Some("runner_inner_1".to_string()),
                        name: Some("read_text".to_string()),
                        arguments: "{}".to_string(),
                    },
                )])))
            } else {
                // Second request: tell the test the runner is mid-flight, then
                // stall forever. The runner's streaming loop races its
                // cancellation token against `stream.next()`, so cancelling
                // the child token resolves this immediately.
                let _ = self.second_request_started.send(true);
                Ok(Box::pin(stream::pending()))
            }
        }
    }

    /// Regression for cooperative interruption: when the parent cancels a
    /// running runner, the partial transcript is preserved as an *interrupted*
    /// outcome (not dropped, not a failure). The child's completed tool call,
    /// the task message, and a model-facing "Interrupted:" summary all survive
    /// so the parent can record them and the user can resume.
    #[tokio::test]
    async fn interrupting_runner_preserves_partial_transcript() {
        let (started_tx, started_rx) = tokio::sync::watch::channel(false);
        let provider = std::sync::Arc::new(GatedProvider {
            requests: std::sync::atomic::AtomicUsize::new(0),
            second_request_started: started_tx,
        });
        let tool = std::sync::Arc::new(RunnerTool::new(
            provider,
            muta_contracts::ToolSet::from_tools([
                std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
            ]),
            &RUNNER_EXPLORE,
        ));

        let tool_for_run = tool.clone();
        let run = tokio::spawn(async move {
            tool_for_run
                .run_runner_outcome(
                    Some("call_interrupt"),
                    r#"{"description":"interrupt me","prompt":"find the handlers"}"#,
                    Box::new(|_event: muta_contracts::RunnerEvent| {}),
                )
                .await
        });

        // Wait until the runner is genuinely mid-flight (its second model
        // request is in the air), then interrupt it the way the harness's
        // executor does: via `Tool::request_cancel` keyed by the call id.
        let mut started_rx = started_rx;
        started_rx
            .changed()
            .await
            .expect("runner reached its second request");
        assert!(
            tool.request_cancel("call_interrupt"),
            "an in-flight runner must accept the cancel request"
        );

        let outcome = run.await.expect("runner run task").expect("outcome");
        assert!(outcome.interrupted, "interruption must be flagged");
        assert!(!outcome.failed, "interruption is not a failure");
        assert!(
            outcome.final_content.starts_with("Interrupted:"),
            "model-facing summary should say Interrupted, got: {}",
            outcome.final_content
        );
        // The partial transcript must contain the completed read_text round.
        let tool_result_msgs = outcome
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .count();
        assert_eq!(
            tool_result_msgs, 1,
            "the completed child tool call must survive in the partial transcript"
        );
        assert!(outcome.messages[0].role == Role::User);
        // A late cancel for a finished call degrades to a no-op, not an error.
        assert!(
            !tool.request_cancel("call_interrupt"),
            "a finished call must reject a late cancel"
        );
    }

    /// The runner persona belongs to the immutable provider request, not its
    /// durable child transcript. The delegated task remains a user message.
    #[tokio::test]
    async fn runner_head_system_message_has_no_dead_task_line() {
        let provider = std::sync::Arc::new(RecordingProvider::default());
        let tool = RunnerTool::new(
            provider.clone(),
            muta_contracts::ToolSet::from_tools([
                std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>
            ]),
            &RUNNER_EXPLORE,
        );
        let outcome = tool
            .run_runner_outcome(
                None,
                r#"{"description":"find files","prompt":"where are the handlers?"}"#,
                Box::new(|_event: muta_contracts::RunnerEvent| {}),
            )
            .await
            .unwrap();

        let request = provider
            .request
            .lock()
            .unwrap()
            .clone()
            .expect("runner request captured");
        let system = &request.messages[0];
        assert_eq!(system.role, muta_contracts::Role::System);
        assert!(
            system
                .content
                .starts_with("You are a focused research runner"),
            "system message should open with the RUNNER_EXPLORE persona"
        );
        assert!(
            !system.content.contains("Task: find files"),
            "the dead `Task: {{description}}` line must not appear (ADR-0039)"
        );

        assert!(
            outcome
                .messages
                .iter()
                .all(|message| message.role != muta_contracts::Role::System),
            "request-scoped policy must not be persisted in the child transcript"
        );

        // The task is the first durable user message.
        assert_eq!(outcome.messages[0].role, muta_contracts::Role::User);
        assert_eq!(outcome.messages[0].content, "where are the handlers?");
        assert_eq!(
            outcome.messages[0]
                .origin
                .as_ref()
                .map(|origin| origin.kind),
            Some(muta_contracts::InjectionKind::RunnerTask)
        );
    }

    #[tokio::test]
    async fn task_tool_rejects_missing_fields() {
        let tool = RunnerTool::new(
            std::sync::Arc::new(CannedProvider),
            muta_contracts::ToolSet::default(),
            &RUNNER_EXPLORE,
        );
        assert!(tool.call(r#"{"description":"x"}"#).await.is_err());
        assert!(tool.call(r#"{"prompt":"x"}"#).await.is_err());
    }

    /// A non-whitelisted stub, used to prove the explore profile rejects tools
    /// by name (it is not in READ_ONLY_TOOLS).
    struct StubWriteTool;

    /// A dynamic-source stub shaped like an MCP tool the master's sink
    /// published — used to prove the mcp_specialist child receives live
    /// dynamic tools (ADR-0138 §2) while other profiles do not.
    struct DynamicMcpTool(&'static str);

    #[async_trait::async_trait]
    impl Tool for DynamicMcpTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "dynamic MCP test tool"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("mcp".to_string())
        }
    }

    /// A minimal [`muta_contracts::DynamicToolSource`] serving a fixed list —
    /// stands in for the master's `DynamicToolRegistry` read port.
    struct FixedSource(Vec<std::sync::Arc<dyn Tool>>);

    impl muta_contracts::DynamicToolSource for FixedSource {
        fn snapshot_tools(&self) -> Vec<std::sync::Arc<dyn Tool>> {
            self.0.clone()
        }
    }

    /// ADR-0138 §2: an mcp_specialist runner spawned with a bound dynamic
    /// source receives its MCP tools alongside the static capability set, and
    /// the injection reads the source *at spawn* — a tool published after
    /// binding still reaches a later child. Static-name collisions are
    /// resolved first-wins (static wins), matching sink-side advertisement.
    #[tokio::test]
    async fn mcp_specialist_runner_receives_live_dynamic_tools() {
        // A provider that records every ModelRequest it serves, so the test can
        // assert on the tool_specs the *child* actually received.
        #[derive(Default)]
        struct RecordingProvider {
            specs: std::sync::Mutex<Vec<String>>,
        }
        #[async_trait::async_trait]
        impl Provider for RecordingProvider {
            async fn chat(&self, request: muta_contracts::ModelRequest) -> Result<Message, String> {
                self.record(&request);
                Ok(Message::new(Role::Assistant, "done"))
            }
            async fn stream_chat(
                &self,
                request: muta_contracts::ModelRequest,
            ) -> Result<BoxStream<'static, Result<String, String>>, String> {
                self.record(&request);
                Ok(Box::pin(stream::once(async { Ok("done".to_string()) })))
            }
        }
        impl RecordingProvider {
            fn record(&self, request: &muta_contracts::ModelRequest) {
                self.specs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend(request.tool_specs.iter().map(|s| s.name.clone()));
            }
        }

        let provider = std::sync::Arc::new(RecordingProvider::default());
        let live = std::sync::Arc::new(FixedSource(vec![
            std::sync::Arc::new(DynamicMcpTool("mcp__github__create_issue")) as Arc<dyn Tool>,
            // Collides with a static tool name: static must win.
            std::sync::Arc::new(DynamicMcpTool("read_text")) as Arc<dyn Tool>,
        ]));
        let tool = RunnerTool::new(
            provider.clone(),
            muta_contracts::ToolSet::default(),
            &muta_contracts::RUNNER_MCP_SPECIALIST,
        );
        tool.bind_dynamic_tool_source(live as Arc<dyn muta_contracts::DynamicToolSource>);

        let _summary = tool
            .call(r#"{"description":"mcp","prompt":"use the dynamic tools","role":"mcp"}"#)
            .await
            .expect("runner dispatch succeeds");

        let specs = provider.specs.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            specs.iter().any(|name| name == "mcp__github__create_issue"),
            "dynamic MCP tool must reach the child's tool_specs, got: {specs:?}"
        );
    }

    #[async_trait::async_trait]
    impl Tool for StubWriteTool {
        fn name(&self) -> &str {
            "stub_write"
        }
        fn description(&self) -> &str {
            "test write tool"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("write".to_string())
        }
    }

    /// Regression for the deadlock fixed in ADR-0011: the explore profile
    /// must exclude (a) the real `ask_user` tool — Read but interactive,
    /// (b) any write tool, and (c) `task` itself — Read but a dispatch tool
    /// that would recurse. Built with the real tool instances the harness
    /// registers, not stubs, so a future capability-bit regression on either
    /// side is caught here.
    #[test]
    fn explore_profile_excludes_user_write_and_recursion_using_real_tools() {
        let provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(CannedProvider);
        let runner_tool = RunnerTool::new(
            provider.clone(),
            muta_contracts::ToolSet::default(),
            &RUNNER_EXPLORE,
        );

        let toolset = muta_contracts::ToolSet::from_tools(vec![
            std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>,
            std::sync::Arc::new(crate::tools::AskUserTool),
            std::sync::Arc::new(StubWriteTool),
            std::sync::Arc::new(runner_tool),
        ]);

        let model = muta_contracts::resolve_model(&CannedProvider.model());
        let model_sel = muta_contracts::ToolSelection::unrestricted();
        let admitted = RUNNER_EXPLORE.resolve_tools(&toolset, &model, &model_sel);
        let admitted_names: Vec<&str> = admitted.iter().map(|t| t.name()).collect();

        assert_eq!(admitted_names, vec!["read_text"]);
    }

    /// Cross-cut regression: `RUNNER_EXPLORE` admits only its whitelisted read tools —
    /// `ask_user`, the non-whitelisted write stub, and recursion are all
    /// excluded. The read stub is admitted because it is named `read_text`,
    /// which is in [`READ_ONLY_TOOLS`].
    #[test]
    fn explore_profile_excludes_bash_writes_user_and_recursion() {
        let provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(CannedProvider);
        let runner_tool = RunnerTool::new(
            provider.clone(),
            muta_contracts::ToolSet::default(),
            &RUNNER_EXPLORE,
        );

        let toolset = muta_contracts::ToolSet::from_tools(vec![
            std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>,
            std::sync::Arc::new(crate::tools::ExecuteCommandTool::new(None)),
            std::sync::Arc::new(crate::tools::AskUserTool),
            std::sync::Arc::new(StubWriteTool),
            std::sync::Arc::new(runner_tool),
        ]);

        // RUNNER_EXPLORE: only the whitelisted read tool survives (bash, ask_user,
        // the write stub, and recursion are all excluded).
        let model = muta_contracts::resolve_model(&CannedProvider.model());
        let model_sel = muta_contracts::ToolSelection::unrestricted();
        let explore_selected = RUNNER_EXPLORE.resolve_tools(&toolset, &model, &model_sel);
        let explore_names: Vec<&str> = explore_selected.iter().map(|t| t.name()).collect();
        assert_eq!(explore_names, vec!["read_text"]);
    }

    /// A write-capable `runner_code` tool (bound to [`muta_contracts::RUNNER_CODE`])
    /// admits the edit surface a coder needs. Built with the real tools the
    /// harness registers so a future capability regression is caught here —
    /// mirrors `explore_profile_excludes_bash_writes_user_and_recursion` for
    /// the inverse contract.
    #[test]
    fn code_profile_admits_edit_surface_using_real_tools() {
        let provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(CannedProvider);
        let runner_code_arc = std::sync::Arc::new(RunnerTool::named(
            provider.clone(),
            muta_contracts::ToolSet::default(),
            &muta_contracts::RUNNER_CODE,
            "runner_code",
            "coding runner",
        ));

        let toolset = muta_contracts::ToolSet::from_tools(vec![
            std::sync::Arc::new(EchoReadTool) as std::sync::Arc<dyn Tool>,
            std::sync::Arc::new(crate::tools::ExecuteCommandTool::new(None)),
            std::sync::Arc::new(crate::tools::WriteFileTool::new(None)),
            std::sync::Arc::new(crate::tools::EditFileTool::new(None)),
            std::sync::Arc::new(crate::tools::AskUserTool),
            runner_code_arc.clone() as std::sync::Arc<dyn Tool>,
        ]);

        // RUNNER_CODE admits bash, write_file, edit_file, and the read tools; it
        // excludes the runner dispatch tool itself (recursion).
        let model = muta_contracts::resolve_model(&CannedProvider.model());
        let model_sel = muta_contracts::ToolSelection::unrestricted();
        let selected = muta_contracts::RUNNER_CODE.resolve_tools(&toolset, &model, &model_sel);
        let names: std::collections::HashSet<&str> = selected.iter().map(|t| t.name()).collect();
        assert!(names.contains("read_text"));
        assert!(names.contains("run_command"));
        assert!(names.contains("write_file"));
        assert!(names.contains("edit_file"));
        assert!(!names.contains("runner_code"), "recursion must be excluded");
        assert!(!names.contains("spawn_runner"));

        // The tool surfaces under its own name.
        assert_eq!(runner_code_arc.name(), "runner_code");
    }

    /// Two dispatch tools sharing one registry is the load-bearing property
    /// that lets the harness route a reply to the correct child regardless of
    /// which tool spawned it. A child registered under one tool's call id is
    /// reachable via the shared registry, and neither tool's `registry()` is
    /// a distinct allocation.
    #[test]
    fn named_with_registry_shares_the_registry_across_tools() {
        let provider: std::sync::Arc<dyn Provider> = std::sync::Arc::new(CannedProvider);
        let explore = RunnerTool::new(
            provider.clone(),
            muta_contracts::ToolSet::default(),
            &RUNNER_EXPLORE,
        );
        let shared = explore.registry();
        let code = RunnerTool::named_with_registry(
            provider.clone(),
            muta_contracts::ToolSet::default(),
            &muta_contracts::RUNNER_CODE,
            "runner_code",
            "coding runner",
            shared.clone(),
        );
        // Same Arc<RunnerRegistry> — the driver hands one to the harness, and
        // children of either tool land in the same table.
        assert!(
            std::sync::Arc::ptr_eq(&explore.registry(), &code.registry()),
            "named_with_registry must share the registry, not clone-allocate"
        );
        // The two tools are distinct capabilities (different names) so they
        // coexist in a parent toolset without one shadowing the other.
        assert_ne!(explore.name(), code.name());

        let mcp = RunnerTool::mcp_specialist(provider, muta_contracts::ToolSet::default(), shared);
        assert_eq!(mcp.name(), "runner_mcp");
    }
}
