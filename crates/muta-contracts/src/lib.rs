//! Shared domain and wire contracts for the muta agent stack: the `Provider`
//! and `Tool` capability traits, conversation and tool-output types, the
//! context-pressure model, repeat/todo values, subagent profiles,
//! skills/MCP config schemas, and the events exchanged by sessions and
//! frontends.
//!
//! This crate is **pure domain, zero I/O** (ADR-0005): no filesystem, no
//! network. It keeps only contracts shared by independent layers: domain
//! values
//! (`TokenUsage`, `ScheduledJob`, `TodoList`, …), wire DTOs, and
//! capability traits (`Provider`, `Tool`, `Hook`). Pure logic
//! owned only by the agent belongs in `muta-agent` (ADR-0057).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub use async_trait::async_trait;

pub mod color_scheme_config;
pub use color_scheme_config::{
    ColorSchemeConfig, CommandThemeConfig, ComponentThemesConfig, CrateThemeConfig,
    DialogThemeConfig, DiffThemeConfig, FeedbackThemeConfig, FeedbackToneConfig, InputThemeConfig,
    KeycapThemeConfig, OverlayThemeConfig, SheetThemeConfig, SurfacesThemeConfig, ThemeFile,
    ViewThemeConfig,
};
pub mod cache;
pub use cache::{
    CachePlan, CacheResolutionError, CacheRetention, PromptCacheCapabilities, PromptCacheMode,
    PromptCacheModePreference, PromptCachePreference, PromptCacheSpec, PromptCacheUsage,
    ResolvedCachePolicy, read_prompt_cache_usage,
};
pub mod request_projection;
pub use request_projection::RequestProjection;
pub mod usage;
pub use usage::TokenUsage;

pub mod error;
pub use error::{
    HarnessError, ProviderError, ProviderErrorKind, RetryDisposition, ToolError, ToolErrorKind,
};

pub mod message;
pub use message::{ImagePart, InjectionKind, InjectionOrigin, Message, Role, SubagentMeta, ToolCall, ToolResult};

pub mod transcript;
pub use transcript::{
    DirectiveKind, DirectivePayload, EntryKind, EntryOrigin, EntryPayload, MessagePayload,
    ProjectionDirective, PrunedToolOutput, StatePayload, SubagentRef, Transcript, TranscriptEntry,
};

pub mod instructions;
pub use instructions::{InstructionBundle, InstructionSlice, InstructionTier};

pub mod command;
pub use command::{CommandRecord, CommandResult, CommandStatus, SearchHit};

pub mod completion;
pub use completion::{
    CommandAlias, CommandCatalog, CommandExample, CommandSpec, CommandSubcommandSpec,
    CommandSuggestion, ComposerCompletion, ComposerCompletionKind, InputCompletion,
    InputCompletionKind,
};

pub mod tool_output;
pub use tool_output::{
    PatchOp, ShellTermination, StdinPolicy, ToolOutput, ToolStream, WebSearchHit,
};

pub mod tool_access;
pub use tool_access::{ToolAccess, ToolAccesses, ToolFileAccessOperation};

pub mod auth;
pub use auth::{CredentialSource, ResolvedAuth, StaticCredentialSource, static_credential};

pub mod tool_validation;

pub mod capability;
pub mod catalog;
pub mod channel_auth;
pub mod client_identity;
pub mod connection_auth;
pub mod connection_detail;
pub mod model_providers;
pub mod provider_auth;
pub mod provider_state;
pub use client_identity::{
    ClientCapabilities, ClientIdentity, ClientPreset, ClientProfile, ClientProfileSpec,
    MUTA_USER_AGENT, OPENCODE_CLIENT_HEADERS, OPENCODE_USER_AGENT, OPENCODE_VERSION,
};
pub mod effort;
pub use effort::{
    EFFORT_CLAUDE_FULL, EFFORT_CLAUDE_NO_XHIGH, EFFORT_COMMON, EFFORT_OPENAI_GPT, Effort,
    EffortLevel,
};
pub mod reasoning;
pub use reasoning::{ReasoningMode, ReasoningSupport};
pub mod dynamic;
pub mod events;
pub mod hooks;
pub mod mcp;
pub mod model;
pub mod todos;
pub use todos::{MAX_TODOS, TodoId, TodoItem, TodoList, TodoStatus};
pub mod agent_kind;
pub mod agent_role;
pub mod aspects;
pub mod cognitive;
pub mod execution_policy;
pub mod extension;
pub use extension::{Extension, HookPhase, ToolExtension};
pub mod hazard;
pub use hazard::*;
pub mod job;
pub mod mesh;
pub mod subagent;
pub use agent_kind::{AgentKind, MeshStation};
pub use agent_role::{
    Agent, AgentRole, AgentRoleDelegation, AgentRoleProfile, AgentRuntimeConfig, DelegationPolicy,
    MainAgent, MainAgentRole, SubAgent, SubAgentRole,
};
pub use aspects::{AspectHook, AspectPhase, AspectVerdict};
pub use cognitive::{
    CognitiveModelPreference, CognitiveTask, EnvironmentReminderOutput, EnvironmentSensorInput,
    EnvironmentSensorTask, ExecutionTier, HarnessTask, HarnessTaskModelPreference,
    PreFlightRouteInput, PreFlightRouteOutput, PreFlightRouterTask, SessionDigest,
    SessionTitleInput, SessionTitleTask, StreamLoopChannel, StreamLoopReviewInput,
    StreamLoopReviewerTask, StreamLoopVerdict,
};
pub use execution_policy::{ContextLifecycle, ExecutionPolicy, PolicyViolation};
pub use mesh::{MeshAddress, MeshEnvelope, MeshMessage, MeshRoute, mesh_ids};
pub mod history;
pub mod human_request;
pub use history::{HISTORY_CAP, HistoryEntry, HistorySearchHit, merge_history};
pub mod identity;
pub mod pressure;
pub mod token_ledger;
pub mod tokenizer;
pub use token_ledger::{
    BeginRequestParams, MAX_PLAUSIBLE_STREAM_TPS, MIN_DEFENSIBLE_STREAM_SPAN_US,
    PerformanceTimingSource, RequestPerformance, RequestUsageKey, RequestUsageRecord,
    RequestUsageSource, RequestUsageStatus, StreamTokenSource, TokenSourceLedger,
    TokenSourceReport, TokenSourceRow, TokenSourceTotals, TokenTurn, TransportObservation,
    TransportTelemetry,
    TransportTimings,
    TurnPerformanceSnapshot, UsageStatSink, latest_turn_performance,
};
pub mod usage_stats;
pub use usage_stats::{
    UsageDayTotals, UsageModelRow, UsageModelTotals, UsageStatRecord, UsageStatsReport,
    aggregate_usage_records, day_key_from_epoch_ms,
};
pub mod doom_guard_config;
pub mod execution;
pub mod secret;
pub mod security;
pub mod shared_roots;
pub use execution::{
    DirEntry, ExecutionEnvironment, FsError, FsMetadata, FsProvider, ProcessOutput, ProcessRunner,
    ShellIsolation, ToolMiddleware,
};
pub use security::{TrustDomain, WorkspaceSecuritySnapshot, WorkspaceTrustState};

pub mod workspace;
pub use workspace::{WorkspaceBinding, WorkspaceFilter};

pub mod session_title;

pub mod session_ir;
pub use session_ir::{
    compile_session_request, BudgetPolicy, CacheBoundary, CapabilityPolicy, CausalGraph,
    CausalNode, CompilationArtifact, CompilationStats, CompilerError, CompilerOptions,
    ExecutionStatus, GuardrailPolicy, NodeId, NodeKind, NodePayload, RuleSet, SessionDelta,
    SessionIR, SessionPolicy, SessionState, StateUpdate, SuspensionReason, SystemNoticePayload,
    TerminationReason,
};

pub mod session_tree;
pub use session_tree::{
    CompactionPayload, SessionEntry, SessionEntryId, SessionEntryKind, SessionTree,
};
pub mod skills_config;
pub use shared_roots::{SharedAdditionalRoots, SharedConfinement};
pub mod tool_registry;
pub mod web_config;
pub use capability::{
    ModelRequest, Provider, ProviderEventStream, ProviderPromptHints, ProviderStreamEvent,
    ProviderTextStream, ProviderTurnContext, ScopeTarget, Tool, ToolSpec, VariantSelection,
    empty_variant_selection,
};
pub use catalog::{
    AnthropicMessagesDialect, Channel, GoogleGeminiDialect, GoogleGenerateContentDialect,
    OpenAiChatDialect, OpenAiResponsesDialect, ProviderEntry, Transport,
};
pub use connection_auth::{ChannelAuth, ConnectionAuth, LoginMethod};
pub use connection_detail::{
    BalanceQuota, ConnectionDetail, ConnectionUsageState, PeriodicQuota, ProviderQuotaData,
    ProviderUsage, QuotaWindowBucket, QuotaWindowKind, RateLimitSpec, UsageMetric,
};
pub use doom_guard_config::DoomGuardConfig;
pub use dynamic::{DynamicCatalog, DynamicToolSink};
pub use events::{
    AgentEvent, AgentNotice, AgentOp, AgentRequest, AgentResponse, BtwAsideSummary, ConnectStatus,
    ConnectionPickerRow, ConnectionPickerSnapshot, ContextTokenSnapshot, ContextTokenSource,
    HarnessSnapshot, InputReply, InputRequest, LoopStatus, McpServerInfo, ModelInfo, NoticeKind,
    NoticeSeverity, NoticeSource, NoticeSurface, ParentStatus, PermissionDecision,
    PermissionRequest, PermissionRuleInfo, ProviderModelInfo, ProviderPickerRow,
    ProviderPickerSnapshot, QueueMode, QueuedMessage, RetryPoint, RetryResolution, RoundEvent,
    RoundInterrupt, RoundInterruptReason, RoundSummary, SessionContextSnapshot, SessionDetail,
    SessionForkKind, SessionOverview, SessionSnapshot, SkillInfo, StdinReply, StdinRequest,
    SubagentEvent, ToolInfo, UserQuestion, UserQuestionOption, UserQuestionReply,
    UserQuestionRequest, WebConfigUpdate, WebConfigView, WebCredentialUpdate,
    WebSearchConfigUpdate, WebSearchConfigView,
};
pub use provider_state::{
    CONTINUATION_ARTIFACT_KEY, ContextRelation, ContextRevision, ContinuationCursor,
    ContinuationMode, CursorInvalidationReason, EnvelopeRevision, OPENAI_RESPONSE_ID_ARTIFACT_KEY,
    OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY, ProviderArtifacts, ProviderCompletion,
    ProviderCompletionMeta, ProviderCursorState, RequestDelivery, RouteFingerprint,
    read_continuation_cursor, request_envelope_fingerprint, request_prefix_fingerprint,
    select_request_delivery, semantic_context_head, write_continuation_cursor,
};
pub use subagent::{
    SUBAGENT_CODE, SUBAGENT_EXPLORE, SUBAGENT_SKILL, SUBAGENT_TITLE, SubagentPreset,
    SubagentPresetPool, ToolPolicy,
};
pub mod monitor;
pub use hooks::{
    Hook, HookContext, HookEvent, HookEventKind, HookOutcome, RestorePoint, SessionSource,
};
pub use identity::AgentIdentity;
pub use job::{
    AdoptionInfo, BackgroundJobInfo, BackgroundJobOutcome, BackgroundJobService, CrateChildBridge,
    JobId, JobKind, JobSpec, JobState, Readiness, RestartPolicy,
};
pub use mcp::{McpConnectionStatus, McpServerConfig};
pub use model::{
    BaselineModels, CapabilityOverrides, ConnectionFilterPolicy, DeclaredModel, FittedModel, Model,
    ModelCapabilities, ModelCapabilityPatch, ModelScopeConfig, ModelTargetScope, NamedFilterPolicy,
    RemoteCatalogEndpoint, RemoteCatalogSourceOverride, RemoteModelMetadata, RouteCapabilities,
    WireProtocol, baseline_models, model_by_id, register_fitted_models, resolve as resolve_model,
    sanitize_model_id, simple_glob_matches,
};
pub use monitor::{
    MonitorAction, MonitorEvent, MonitorSnapshot, MonitoredSession, MonitoredTask, SessionHosting,
    SessionStatus,
};
pub use pressure::{
    CLEARED_TOOL_PREFIX, CompactionPolicy, ContextBudget, LayeredRequestWeights,
    MessageContentFingerprint, MessageTokenWeights, PruneOutcome, RequestTokenEstimate,
    ToolSchemaWeights, estimate_bytes, estimate_draft_tokens, estimate_message_tokens,
    estimate_semantic_json_tokens, estimate_tokens, estimate_tokens_weighted, freeze_tool_output,
    layered_request_weights, prune_tool_results,
};
pub use secret::SecretString;
pub use session_title::{SessionTitle, TITLE_MAX_LEN, clean_title};
pub use skills_config::SkillsConfig;
/// The BPE token counter ([`crate::tokenizer`], ADR-0117) under the name the
/// heuristic estimator used to own: token prediction is BPE now, and callers
/// that imported `count_tokens` for budget-fitting (summary truncation)
/// must measure in the same unit as the projection thresholds.
pub use tokenizer::{StreamingCounter, Tokenizer, count_tokens, truncate_to_tokens};
pub use tool_output::truncate_utf8;
pub use tool_registry::{
    Capability, ToolCapabilityAudit, ToolContext, ToolContextBuilder, ToolDeclaration, ToolFactory,
    ToolPool, ToolPoolSnapshot, ToolScope, ToolSelection, ToolSet, WorkspaceRoot, WorkspaceRoots,
    collect_toolset,
};
pub mod wire;
pub use web_config::{
    BOCHA_SEARCH_ENDPOINT, DUCKDUCKGO_HTML_ENDPOINT, DUCKDUCKGO_LITE_ENDPOINT, EXA_SEARCH_ENDPOINT,
    JINA_READER_ENDPOINT, PARALLEL_SEARCH_ENDPOINT, SharedWebConfig, TAVILY_SEARCH_ENDPOINT,
    WebConfig, WebCredentialRequirement, WebCredentialStatus, WebEndpointRequirement,
    WebProviderAxis, WebProviderCapability, WebReaderProvider, WebRuntimeConfig, WebSearchConfig,
    WebSearchProvider, web_provider_capabilities,
};
pub use wire::{
    AttachAction, ControlRequest, ERR_PROTOCOL_MISMATCH, ERR_VERSION_MISMATCH,
    MIN_PROTOCOL_VERSION, PROTOCOL_VERSION, SessionInitOptions, Wire, protocol_accepts,
};
