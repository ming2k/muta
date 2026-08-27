//! Configuration, budget, and hook-fire methods on [`Agent`].
//!
//! Everything an embedder sets up before the first round: tool variant
//! selection, context budgets, the doom guard, bash policy, hook registries,
//! todo lists, and the identity/preset accessors.

use super::*;

impl Agent {
    /// Start configuring an agent from a flat tool list.
    pub fn builder(
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        identity: AgentIdentity,
    ) -> AgentBuilder {
        AgentBuilder::new(
            provider,
            muta_contracts::ToolSet::from_tools(tools),
            identity,
        )
    }

    /// Start configuring an agent from a full multi-variant tool set.
    pub fn builder_from_toolset(
        provider: Arc<dyn Provider>,
        toolset: muta_contracts::ToolSet,
        identity: AgentIdentity,
    ) -> AgentBuilder {
        AgentBuilder::new(provider, toolset, identity)
    }

    /// Construct an agent from a flat tool list. The tools are grouped into a
    /// [`muta_contracts::ToolSet`] (one capability per [`Tool::name`], one variant
    /// per [`Tool::variant`]) — the common case for a single-variant toolset or
    /// an already-resolved runner toolset. Use [`Agent::from_toolset`] to
    /// preserve a multi-variant set so per-model variant selection can switch
    /// between variants at runtime.
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        identity: AgentIdentity,
    ) -> Self {
        Self::from_toolset(
            provider,
            muta_contracts::ToolSet::from_tools(tools),
            identity,
        )
    }

    /// Construct an agent from a full [`muta_contracts::ToolSet`], preserving every
    /// capability's variants so [`Agent::set_variant_selection`] can swap the
    /// model-visible variant at runtime.
    pub fn from_toolset(
        provider: Arc<dyn Provider>,
        toolset: muta_contracts::ToolSet,
        identity: AgentIdentity,
    ) -> Self {
        Self::builder_from_toolset(provider, toolset, identity).build()
    }

    pub(super) fn from_toolset_with_model_request_assembler(
        provider: Arc<dyn Provider>,
        toolset: muta_contracts::ToolSet,
        skills_registry: skills::SkillRegistry,
        identity: AgentIdentity,
        model_request_assembler: crate::model_request::ModelRequestAssembler,
    ) -> Self {
        let thread_id = Arc::new(std::sync::Mutex::new(None));

        let mut toolset = toolset;
        let round_counter = Arc::new(std::sync::Mutex::new(0u64));
        let todos = Arc::new(std::sync::Mutex::new(muta_contracts::TodoList::default()));
        crate::tool_integration::install_agent_owned_tools(
            &mut toolset,
            Arc::clone(&todos),
            Arc::clone(&round_counter),
        );

        // Seed the model-visible view by resolving the pool for the live model
        // with no role restriction and no model variant overrides yet: the
        // master's identity selection (unrestricted) composed with the
        // model's capability limits. `set_variant_selection` re-resolves once
        // the model's `[tool_variants]` selection is known and on every switch.
        let agent_selection = muta_contracts::ToolSelection::unrestricted();
        let seed_model = muta_contracts::resolve_model(&provider.model());
        let resolved_tools = Arc::new(std::sync::RwLock::new(toolset.resolve_for(
            &seed_model,
            &agent_selection,
            &muta_contracts::ToolSelection::unrestricted(),
        )));
        let dynamic_tools = Arc::new(crate::dynamic_tools::DynamicToolRegistry::default());
        let disabled_tools = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let scoped_disabled_tools = Arc::new(std::sync::Mutex::new(ScopedToolDisable::default()));
        // The unified ToolManager view (kimi-code port) owns the single
        // authority for classification, per-turn schema, and dispatch lookup.
        // It shares the storage Arcs with the agent so both reach the same
        // live state. The `user` bucket (empty today) has no other owner:
        // the manager is its sole authority. See `tool_manager`.
        let tool_manager = crate::tool_manager::ToolManager::new(
            Arc::clone(&resolved_tools),
            Arc::clone(&dynamic_tools),
            Arc::new(std::sync::RwLock::new(Vec::new())),
            Arc::clone(&disabled_tools),
            Arc::clone(&scoped_disabled_tools),
        );

        let pool = Arc::new(std::sync::RwLock::new(muta_contracts::ToolPool::new(
            toolset.clone(),
        )));

        Self {
            provider,
            tier: std::sync::RwLock::new(muta_contracts::AgentTier::Master),
            pool,
            toolset,
            resolved_tools,

            dynamic_tools,
            disabled_tools,
            scoped_disabled_tools,
            tool_manager,
            todos,
            round_counter,
            permissions: crate::permission_store::PermissionStore::new(),
            additional_workspace_roots: Vec::new(),
            workspace_security: Arc::new(std::sync::Mutex::new(
                muta_contracts::WorkspaceSecuritySnapshot::default(),
            )),
            project_rules: Arc::new(std::sync::RwLock::new(String::new())),
            skills_registry,
            thread_id,
            accounting_actor_id: std::sync::Mutex::new("master".to_string()),
            context_prune_threshold_tokens: Arc::new(std::sync::Mutex::new(0)),
            context_projection_gate: Arc::new(std::sync::Mutex::new(None)),
            hard_stop_turns: Arc::new(std::sync::Mutex::new(0)),
            doom_guard_config: Arc::new(std::sync::RwLock::new(
                muta_contracts::DoomGuardConfig::default(),
            )),
            interaction: Arc::new(crate::interaction::InteractionController::default()),
            human_broker: crate::human_broker::HumanRequestBroker::new(),
            bash_policy: std::sync::RwLock::new(crate::bash_policy::BashPolicy::default()),

            operation_scope: std::sync::Mutex::new(muta_contracts::OperationScope::unrestricted()),
            hooks: crate::hook_runner::HookRunner::new(),
            inbox_tx: std::sync::Mutex::new(None),
            inbox_rx: std::sync::Mutex::new(None),
            session_queues: std::sync::Mutex::new(None),
            steering_mode: std::sync::RwLock::new(muta_contracts::QueueMode::default()),
            follow_up_mode: std::sync::RwLock::new(muta_contracts::QueueMode::default()),
            round_paused_ms: std::sync::atomic::AtomicU64::new(0),
            identity: std::sync::RwLock::new(identity),
            turn_persist: std::sync::Mutex::new(None),
            model_request_assembler,
            variant_selection: Arc::new(std::sync::Mutex::new(
                muta_contracts::VariantSelection::new(),
            )),
            agent_selection: std::sync::Mutex::new(agent_selection),
            token_ledger: std::sync::Mutex::new(None),
        }
    }

    /// Context-pressure threshold (in tokens) for mid-turn relief. `0` (the
    /// default) disables the mid-turn [`ContextProjectionGate`]. Re-seed on provider
    /// switch so the threshold tracks the new model's context window.
    pub fn set_context_prune_threshold(&self, budget_tokens: usize) {
        *self
            .context_prune_threshold_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = budget_tokens;
    }

    /// Replace the per-model tool-variant selection and re-resolve the
    /// model-visible toolset to match. Seeded from `[tool_variants."<model-id>"]`
    /// config and re-applied on model switch so the resolved variants — and the
    /// live model's hard capability limits (e.g. vision) — always track the
    /// live model. An empty map (the default) realizes every capability with its
    /// model-chosen / default variant.
    pub fn set_variant_selection(&self, selection: muta_contracts::VariantSelection) {
        self.reresolve_tools(&selection);
        *self
            .variant_selection
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = selection;
    }

    /// Replace this agent's identity-side selection (capability scope + variant
    /// pins) and re-resolve the model-visible toolset. The master is
    /// unrestricted by default; this narrows it (e.g. confining a role-bound
    /// master to a capability subset). The current per-model variant
    /// selection is preserved and re-composed.
    pub fn set_agent_selection(&self, selection: muta_contracts::ToolSelection) {
        *self
            .agent_selection
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = selection;
        let model_variants = self
            .variant_selection
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.reresolve_tools(&model_variants);
    }

    /// Re-resolve [`resolved_tools`](Self::resolved_tools) from the pool for the
    /// live model, composing this agent's identity selection with the model's
    /// selection (`model_variants` overrides + the model's hard capability
    /// limits). The single choke point through which both the master seed and
    /// every model/selection switch flow, so the schema sent to the provider and
    /// the dispatch table always reflect `agent_scope ∩ model_caps`.
    fn reresolve_tools(&self, model_variants: &muta_contracts::VariantSelection) {
        let model = muta_contracts::resolve_model(&self.provider.model());
        let agent_selection = self
            .agent_selection
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let model_selection =
            muta_contracts::ToolSelection::unrestricted().with_variants(model_variants.clone());
        *self
            .resolved_tools
            .write()
            .unwrap_or_else(|e| e.into_inner()) =
            self.toolset
                .resolve_for(&model, &agent_selection, &model_selection);
    }

    /// Every currently installed tool, including dynamic sources. Static
    /// capabilities win name collisions; dynamic source order is deterministic.
    pub fn installed_tools(&self) -> Vec<Arc<dyn Tool>> {
        // Delegate to the unified ToolManager — the single authority for the
        // three-bucket classification (builtin/user/mcp) and name-clash priority.
        self.tool_manager
            .installed()
            .into_iter()
            .map(|s| s.tool)
            .collect()
    }

    /// The unified tool manager (kimi-code port). Exposed so the dispatcher /
    /// model-request assembly can call its authoritative methods directly.
    pub(crate) fn tool_manager(&self) -> &crate::tool_manager::ToolManager {
        &self.tool_manager
    }

    /// The permission policy chain for this agent. Built fresh per call.
    /// Holds the complete authority policy chain. Interaction-only behavior
    /// (`ask_user`, stdin, and missing-authority prompting) stays in
    /// `execute_tool`, outside the authority context by construction.
    pub(crate) fn permission_chain(&self) -> crate::permission_policy::PermissionChain {
        crate::permission_policy::PermissionChain::new(crate::permission_policy::default_chain())
    }

    /// Snapshot the live state available to declarative system-prompt policy.
    fn system_prompt_context(&self, tools: &[Arc<dyn Tool>]) -> crate::SystemPromptContext {
        let mut tool_names: Vec<String> =
            tools.iter().map(|tool| tool.name().to_string()).collect();
        tool_names.sort();
        let model_guidance = muta_contracts::resolve_model(&self.provider.model()).model_guidance;
        let provider_guidance = self.provider.prompt_hints().system_guidance;

        let available_skills = {
            let list = self.skills_registry.lock().list();
            muta_skills::render::format_skills_for_prompt(&list)
        };

        crate::SystemPromptContext {
            identity_preamble: self
                .identity
                .read()
                .map(|guard| guard.preamble())
                .unwrap_or_default(),
            tool_names,
            model_guidance,
            provider_guidance,
            yolo: self.get_yolo(),
            available_skills,
            project_rules: self
                .project_rules
                .read()
                .map(|rules| rules.clone())
                .unwrap_or_default(),
            additional_workspace_roots: self
                .additional_workspace_roots
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            workspace_root: self.workspace_root().map(|p| p.display().to_string()),
        }
    }

    /// Build one immutable provider request from a borrowed conversation window.
    /// Implicit skill loading is evaluated on a private copy so estimates and
    /// debug previews use the same projection without mutating durable state.
    pub(super) fn model_request(&self, messages: &[Message]) -> muta_contracts::ModelRequest {
        let mut enriched = messages.to_vec();
        crate::conversation_context::inject_mentioned_skills(&self.skills_registry, &mut enriched);
        crate::conversation_context::inject_mentioned_files(
            self.workspace_root().as_deref(),
            &mut enriched,
        );
        let tools = self.visible_tools();
        let context = self.system_prompt_context(&tools);
        self.model_request_assembler
            .assemble(&enriched, &context, &tools)
    }

    pub(super) fn estimate_model_request(
        request: &muta_contracts::ModelRequest,
    ) -> RequestTokenEstimate {
        // Per-message wire weight (not `estimate_tokens`, which intentionally
        // includes persisted runner children the provider never sees), with
        // each message tokenized exactly once — a prior version tokenized the
        // non-system subset a second time (and cloned the whole message list
        // to build it), doubling the dominant cost of every estimate.
        let per_message: Vec<i64> = request
            .messages
            .iter()
            .map(muta_contracts::estimate_message_tokens)
            .collect();
        let history_tokens = per_message
            .iter()
            .zip(&request.messages)
            .filter(|(_, message)| message.role != Role::System)
            .map(|(tokens, _)| *tokens)
            .sum::<i64>()
            .max(0) as usize;
        let prepared_message_tokens = per_message.iter().sum::<i64>().max(0) as usize;
        let tool_schema_tokens = request
            .tool_specs
            .iter()
            .map(|spec| {
                // Estimate over the full spec (name + description + the JSON
                // Schema parameters), matching the old whole-Value estimate.
                let val = serde_json::to_value(spec).unwrap_or(serde_json::Value::Null);
                muta_contracts::estimate_semantic_json_tokens(&val).max(0) as usize
            })
            .sum::<usize>();
        let total_tokens = prepared_message_tokens.saturating_add(tool_schema_tokens);

        RequestTokenEstimate {
            history_tokens,
            overhead_tokens: total_tokens.saturating_sub(history_tokens),
            total_tokens,
        }
    }

    /// Estimate the complete next request at the same immutable request
    /// boundary the provider call uses.
    pub fn estimate_next_request_tokens(&self, messages: &[Message]) -> RequestTokenEstimate {
        Self::estimate_model_request(&self.model_request(messages))
    }

    /// Dev-only dry run: rebuild the head system message and auto-load any
    /// skills mentioned in the latest visible user round against a borrowed
    /// message list, exactly as the next turn would, but with no provider call
    /// and no mutation of live round history. Powers
    /// the `/debug preview` so it captures the *real* request shape —
    /// including the freshly composed system prompt and injected skills —
    /// rather than a degenerate reconstruction.
    pub fn prepare_request_messages_debug(&self, messages: &mut Vec<Message>) {
        *messages = self.model_request(messages).messages;
    }

    /// A shared handle to this agent's live variant selection (the **override**
    /// axis). Handed to a spawned runner's dispatch tool so the runner — an
    /// agent on the same model — resolves its admitted capabilities to the same
    /// variants the parent uses, tracking model switches live. The profile still
    /// owns the orthogonal **scope** axis.
    pub fn variant_selection_handle(
        &self,
    ) -> Arc<std::sync::Mutex<muta_contracts::VariantSelection>> {
        Arc::clone(&self.variant_selection)
    }

    /// Override the opt-in hard-stop budget. Mirrors `[master] hard_stop_turns`
    /// in `config.toml` but can be flipped at runtime. `0` (the default) leaves
    /// the round uncapped, matching ADR-0009. The reviewer runner gets a
    /// tight non-zero bound so a runaway diagnostic cannot loop.
    pub fn set_hard_stop_turns(&self, turns: usize) {
        *self
            .hard_stop_turns
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = turns;
    }

    /// Current hard-stop budget. Read by the `/hard-stop` slash command (if
    /// present) and by `check_hard_stop` at each ReAct-turn boundary.
    pub fn get_hard_stop_turns(&self) -> usize {
        *self
            .hard_stop_turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Replace the live doom-guard configuration atomically. The next round
    /// reconstructs its per-round guard from the new settings; the current
    /// round, if any, keeps its already-built guard state.
    ///
    /// Wired from `[master.doom_guard]` in `config.toml` at startup and forced to
    /// [`muta_contracts::DoomGuardConfig::disabled`] on runners and the review
    /// diagnostic so they run unobstructed regardless of user settings.
    pub fn set_doom_guard_config(&self, config: muta_contracts::DoomGuardConfig) {
        *self
            .doom_guard_config
            .write()
            .unwrap_or_else(|e| e.into_inner()) = config;
    }

    /// Snapshot of the live doom-guard configuration. The turn boundary reads
    /// `enabled` to gate the pre-dispatch doom check.
    pub fn doom_guard_config(&self) -> muta_contracts::DoomGuardConfig {
        *self
            .doom_guard_config
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the doom guard is currently armed (allowed to block). Convenience
    /// wrapper over [`Self::doom_guard_config`] for the turn-boundary fast path.
    pub fn doom_guard_enabled(&self) -> bool {
        self.doom_guard_config().enabled
    }

    /// Enable or disable the model-supplied-stdin path for `bash` (L3.5 α).
    /// Mirrors `[master] allow_model_stdin` in `config.toml`. When off
    /// (the default), the bash schema exposes no `stdin` parameter and a
    /// command needing input either gets it from a human (interactive
    /// classifier → input panel) or fails fast. When on, the model may feed
    /// a command's stdin directly — for autopilot/automatic flows.
    pub fn set_allow_model_stdin(&self, enabled: bool) {
        self.interaction.set_allow_model_stdin(enabled);
    }

    /// Replace the command-aware bash safety policy from `[bash_policy]` config.
    /// Built-in dangerous-command rules remain compiled into the policy; config
    /// only supplies toggles and user-defined overrides/additions.
    pub fn set_bash_policy(&self, config: &muta_persistence::config::BashPolicyConfig) {
        let policy = crate::bash_policy::BashPolicy::from_config(config);
        for error in policy.invalid_rules() {
            tracing::warn!(error = %error, "ignoring invalid bash policy rule");
        }
        *self.bash_policy.write().unwrap_or_else(|e| e.into_inner()) = policy;
    }

    /// Whether the model may supply stdin for a `bash` call. Read at the
    /// dispatch site to decide the [`StdinPolicy`] and whether the bash schema
    /// exposes a `stdin` parameter.
    pub fn allow_model_stdin(&self) -> bool {
        self.interaction.allow_model_stdin()
    }

    /// Mirrors `[master] skip_interactive_input` in `config.toml`. When on,
    /// an interactive `bash` command (matched by the interactive classifier)
    /// never pops the inline input panel and instead runs with stdin closed —
    /// fast failure with a non-interactive remedy, as under autopilot mode.
    /// Lets an operator who finds the prompt disruptive opt out of it without
    /// turning the master itself autopilot.
    pub fn set_skip_interactive_input(&self, enabled: bool) {
        self.interaction.set_skip_interactive_input(enabled);
    }

    /// Whether an interactive `bash` command should skip the operator input
    /// panel and run with stdin closed instead. Read at the bash dispatch site
    /// to decide the [`StdinPolicy`].
    pub fn skip_interactive_input(&self) -> bool {
        self.interaction.skip_interactive_input()
    }

    /// Install (or clear with `None`) the mid-turn model-context projection gate.
    pub fn set_context_projection_gate(&self, gate: Option<Arc<dyn ContextProjectionGate>>) {
        *self
            .context_projection_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = gate;
    }

    /// Install the shared token-source ledger so this agent books each turn's
    /// token counts (reported vs. estimated) into it. The embedding shares the
    /// same `Arc` with the TUI so the token-source report modal reads live.
    /// No-op for runners/tests that never call this (the ledger stays `None`
    /// and booking is skipped).
    pub fn install_token_ledger(&self, ledger: Arc<muta_contracts::TokenSourceLedger>) {
        *self.token_ledger.lock().unwrap_or_else(|e| e.into_inner()) = Some(ledger);
    }

    /// A handle to the token-source ledger, if one was installed. The TUI uses
    /// this to snapshot the report for the modal.
    pub fn token_ledger(&self) -> Option<Arc<muta_contracts::TokenSourceLedger>> {
        self.token_ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Book one turn's token usage into [`RoundState::token_usage`] and, when a
    /// ledger is installed, into the token-source ledger.
    ///
    /// `streamed_usage` is the usage reported mid-stream (OpenAI
    /// `include_usage` / Anthropic `message_delta`), if any. When absent, we
    /// fall back to [`Provider::take_last_usage`] (usage reported out-of-band
    /// instead of as a mid-stream `Usage` event) and finally to the local
    /// char-class estimator.
    ///
    /// This is the single point that decides whether a turn counts as
    /// **reported** (authoritative) or **estimated** (heuristic), and records
    /// that classification so the token-source report modal can render it.
    pub(super) fn book_turn_usage(
        &self,
        state: &mut RoundState,
        response: &Message,
        streamed_usage: Option<TokenUsage>,
        request: &mut RequestAccountingGuard,
    ) {
        // Seal the generation clock now, while we hold a validated assistant
        // response and before any tool dispatch, so tool execution never
        // inflates the measured generation span.
        request.seal_generation();
        state.generation_ms = state.generation_ms.saturating_add(request.generation_ms);
        // Prefer the usage the provider reported (streamed, then drained).
        let reported = streamed_usage.or_else(|| self.provider.take_last_usage());
        // Any streamed-but-unfinalized tail (the last open pretoken) belongs
        // to the completion count too: close the incremental counter before
        // settling so the estimate matches what a whole-text count would say.
        // (This is a maximum, never a downgrade: `finish_output` keeps the
        // larger of the finalized total and the already-observed count.)
        request.finish_output();
        if let Some(usage) = reported {
            state.token_usage.total_tokens += usage.total_tokens;
            state.token_usage.prompt_tokens += usage.prompt_tokens;
            state.token_usage.completion_tokens += usage.completion_tokens;
            state.token_usage.cache_creation_input_tokens += usage.cache_creation_input_tokens;
            state.token_usage.cache_read_input_tokens += usage.cache_read_input_tokens;
            request.settle(
                muta_contracts::RequestUsageStatus::Completed,
                Some(usage),
                0,
            );
        } else {
            // Estimate both sides of the request. The old fallback counted
            // only the assistant response while the reported path counted
            // prompt + completion, making mixed-source totals incomparable.
            let completion = pressure::estimate_message_tokens(response).max(0);
            let prompt = request.projected_prompt_tokens.max(0);
            let estimated = prompt.saturating_add(completion);
            state.token_usage.total_tokens += estimated;
            state.token_usage.prompt_tokens += prompt;
            state.token_usage.completion_tokens += completion;
            request.settle(
                muta_contracts::RequestUsageStatus::Completed,
                None,
                completion,
            );
        }
    }

    /// Install the lifecycle hook registry (ADR-0025). Replaces any prior
    /// registry; intended to be called once at startup after the `[hooks]`
    /// config is parsed. Runners and tests leave the default empty registry.
    pub fn set_hooks(&self, registry: crate::hooks::HookRegistry) {
        self.hooks.set(registry);
    }

    /// Install the mid-round save point fired at every ReAct-turn boundary
    /// (ADR-0048). The closure receives the current full round history and
    /// should durably append only the new tail (see
    /// `SessionStore::append_turn`). Called once by orchestration after the
    /// agent is built and the session is open; runners and the review
    /// diagnostic never call this, so the default `None` keeps their turn
    /// boundaries no-ops.
    pub fn set_turn_persist(&self, f: TurnPersistFn) {
        *self.turn_persist.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    /// Fire the mid-round save point if installed. Returns `Ok(())` when no
    /// closure is set (the runner / review / test path) so the call site
    /// stays unconditional. Invoked at the turn boundary — after a turn's
    /// tool results are in `messages` and before the next model request.
    pub(super) async fn fire_turn_persist(&self, messages: &[Message]) -> Result<(), HarnessError> {
        let f = self
            .turn_persist
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match f {
            Some(f) => f(messages).await.map_err(|error| {
                HarnessError::Other(format!("could not persist mid-round turn: {error}"))
            }),
            None => Ok(()),
        }
    }

    /// Snapshot the hook registry as a cheap `Arc` clone, so insertion points
    /// fire hooks without holding the swap lock across the async `fire`.
    pub(super) fn hooks(&self) -> Arc<crate::hooks::HookRegistry> {
        self.hooks.get()
    }

    /// The session id hooks see (the live thread id, if any).
    pub(super) fn hook_session_id(&self) -> String {
        self.thread_id().unwrap_or_default()
    }

    /// The cwd hooks run under (the persisted project root, if any).
    pub(super) fn hook_cwd(&self) -> Option<std::path::PathBuf> {
        self.workspace_root()
    }

    /// The persisted project root — the workspace sandbox for `@file:` injection
    /// and the base relative file-tool paths resolve against. `None` when no
    /// project was designated (runners, tests, or a detached session), in which
    /// case file injection is disabled.
    /// Record the session's additional workspace roots (ADR-0142). Called
    /// once by the assembling bootstrap after they validate; they surface to
    /// the model through the `WorkspaceRootsGuidance` system-prompt section.
    pub fn set_additional_workspace_roots(&mut self, roots: Vec<std::path::PathBuf>) {
        self.additional_workspace_roots = roots;
    }

    /// The session's additional workspace roots, if any (ADR-0142).
    pub fn additional_workspace_roots(&self) -> &[std::path::PathBuf] {
        &self.additional_workspace_roots
    }

    pub(crate) fn workspace_root(&self) -> Option<std::path::PathBuf> {
        self.permissions.project_root()
    }

    // --- Public hook entry points (ADR-0025) ---------------------------------
    // The PreToolUse / PostToolUse / Stop insertion points are inline in the
    // loop above (they need local control flow); the lifecycle entry points
    // below are called by the driver / orchestration at the session, turn, and
    // compaction boundaries.

    /// `UserPromptSubmit` gate. Called by `execute_round` before the prompt
    /// enters the transcript: a `Deny` drops it, a `Prepend` prefixes context.
    pub async fn fire_user_prompt_submit(&self, prompt: &str) -> crate::hooks::UserPromptVerdict {
        self.hooks()
            .check_user_prompt_submit(prompt, &self.hook_session_id(), self.hook_cwd().as_deref())
            .await
    }

    /// `PreCompact` observers. Returns any injected context to fold into the
    /// upcoming summarization (ADR-0025).
    pub async fn fire_pre_compact(&self) -> Vec<String> {
        self.hooks()
            .pre_compact(&self.hook_session_id(), self.hook_cwd().as_deref())
            .await
    }

    /// `PostCompact` observers. Informational only.
    pub async fn fire_post_compact(&self) {
        self.hooks()
            .post_compact(&self.hook_session_id(), self.hook_cwd().as_deref())
            .await
    }

    /// `SessionStart` observers; injected context becomes hidden setup messages.
    pub async fn fire_session_start(
        &self,
        source: muta_contracts::SessionSource,
        messages: &mut Vec<Message>,
    ) {
        self.hooks()
            .session_start(
                source,
                &self.hook_session_id(),
                self.hook_cwd().as_deref(),
                messages,
            )
            .await
    }

    /// `SessionEnd` observers. Informational only.
    pub async fn fire_session_end(&self) {
        self.hooks()
            .session_end(&self.hook_session_id(), self.hook_cwd().as_deref())
            .await
    }

    /// Between ReAct turns, if context pressure exceeds the configured budget,
    /// hand the live message list to the [`ContextProjectionGate`] so it can
    /// produce and persist the next model-visible window.
    pub(super) async fn project_context_if_needed(
        &self,
        messages: &mut Vec<Message>,
        cancel: &CancellationToken,
    ) -> Result<(), HarnessError> {
        let budget = *self
            .context_prune_threshold_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if budget == 0 || self.estimate_next_request_tokens(messages).total_tokens <= budget {
            return Ok(());
        }
        let gate = self
            .context_projection_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(gate) = gate else {
            return Ok(());
        };
        let replacement = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(HarnessError::Interrupted),
            replacement = gate.project_context(messages.clone()) => replacement,
        };
        if let Some(replacement) = replacement
            && !replacement.is_empty()
        {
            *messages = replacement;
        }
        Ok(())
    }

    pub fn set_thread_id(&self, thread_id: impl Into<String>) {
        *self.thread_id.lock().unwrap_or_else(|e| e.into_inner()) = Some(thread_id.into());
    }

    pub fn thread_id_handle(&self) -> Arc<std::sync::Mutex<Option<String>>> {
        Arc::clone(&self.thread_id)
    }

    pub fn round_counter_handle(&self) -> Arc<std::sync::Mutex<u64>> {
        Arc::clone(&self.round_counter)
    }

    pub fn set_accounting_actor_id(&self, actor_id: impl Into<String>) {
        *self
            .accounting_actor_id
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = actor_id.into();
    }

    pub(super) fn accounting_actor_id(&self) -> String {
        self.accounting_actor_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn clear_thread_id(&self) {
        *self.thread_id.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Current task list snapshot. Read by the harness to mirror into the
    /// session and by the TUI to render the sticky panel.
    pub fn todos(&self) -> muta_contracts::TodoList {
        self.todos.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Replace the task list. Used by session-restore paths on resume.
    pub fn set_todos(&self, todos: muta_contracts::TodoList) {
        *self.todos.lock().unwrap_or_else(|e| e.into_inner()) = todos;
    }

    /// Drop the task list.
    pub fn clear_todos(&self) {
        *self.todos.lock().unwrap_or_else(|e| e.into_inner()) = muta_contracts::TodoList::default();
    }
}
