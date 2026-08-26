//! Shared domain and wire contracts for the muta agent stack: the `Provider`
//! and `Tool` capability traits, conversation and tool-output types, the
//! context-pressure model, repeat/todo values, runner profiles,
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

pub mod cron;
pub use cron::CronExpr;
pub mod color_scheme_config;
pub use color_scheme_config::{
    ColorSchemeConfig, CommandThemeConfig, ComponentThemesConfig, CrateThemeConfig,
    DiffThemeConfig, InputThemeConfig, ThemeFile,
};
pub mod cache;
pub use cache::CachePolicy;
pub mod repeat;
pub use repeat::{
    DEFAULT_MAX_AGE_DAYS, RepeatJob, Schedule, ScheduleAt, ScheduledJob, parse_schedule_arg,
};
pub mod usage;
pub use usage::TokenUsage;

pub mod error;
pub use error::{
    HarnessError, RetryableError, is_context_overflow, parse_retryable_error, public_error_message,
    retryable_error,
};

pub mod message;
pub use message::{ImagePart, InjectionKind, InjectionOrigin, Message, Role, ToolCall, ToolResult};

pub mod command;
pub use command::{CommandRecord, CommandResult, CommandStatus, SearchHit};

pub mod completion;
pub use completion::{
    CommandAlias, CommandCatalog, CommandExample, CommandSpec, CommandSuggestion, InputCompletion,
    InputCompletionKind,
};

pub mod tool_output;
pub use tool_output::{PatchOp, ShellTermination, StdinPolicy, ToolOutput, ToolStream};

pub mod tool_access;
pub use tool_access::{ToolAccess, ToolAccesses, ToolFileAccessOperation};

pub mod tool_validation;

pub mod capability;
pub mod catalog;
pub mod channel_auth;
pub mod client_identity;
pub use client_identity::ClientIdentity;
pub mod effort;
pub use effort::{
    EFFORT_CLAUDE_FULL, EFFORT_CLAUDE_NO_XHIGH, EFFORT_COMMON, EFFORT_OPENAI_GPT, Effort,
    EffortLevel,
};
pub mod thinking;
pub use thinking::{ThinkingMode, ThinkingSupport};
pub mod dynamic;
pub mod events;
pub mod hooks;
pub mod mcp;
pub mod model;
pub mod todos;
pub use todos::{MAX_TODOS, TodoId, TodoItem, TodoList, TodoStatus};
pub mod hazard;
pub use hazard::*;
pub mod master;
pub mod mesh;
pub mod runner;
pub mod tier;
pub use mesh::{MeshAddress, MeshEnvelope, MeshMessage, MeshRoute, mesh_ids};
pub use tier::AgentTier;
pub mod history;
pub mod human_request;
pub use history::{HISTORY_CAP, HistoryEntry, merge_history};
pub mod identity;
pub mod pressure;
pub mod token_ledger;
pub mod tokenizer;
pub use token_ledger::{
    RequestUsageKey, RequestUsageRecord, RequestUsageSource, RequestUsageStatus, TokenSourceLedger,
    TokenSourceReport, TokenSourceRow, TokenSourceTotals, TokenTurn, UsageStatSink,
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
pub use execution::{
    DirEntry, ExecutionEnvironment, FsError, FsMetadata, FsProvider, ProcessOutput, ProcessRunner,
    ShellIsolation, ToolMiddleware,
};
pub use security::{TrustDomain, WorkspaceSecuritySnapshot, WorkspaceTrustState};

pub mod session_title;

pub mod session_tree;
pub use session_tree::{SessionEntry, SessionEntryId, SessionEntryKind, SessionTree};
pub mod skills_config;
pub mod tool_registry;
pub mod web_config;
pub use capability::{
    CommandScope, ModelRequest, OperationScope, Provider, ProviderPromptHints, ProviderStreamEvent,
    ScopeTarget, Tool, ToolSpec, VariantSelection, empty_variant_selection,
};
pub use catalog::{Channel, ProviderEntry, Transport};
pub use channel_auth::{ChannelAuth, LoginMethod};
pub use doom_guard_config::DoomGuardConfig;
pub use dynamic::{DynamicCatalog, DynamicToolSink};
pub use events::{
    AgentEvent, AgentNotice, AgentOp, AgentRequest, AgentResponse, BtwAsideSummary, ConnectStatus,
    ConnectionPickerRow, ConnectionPickerSnapshot, ContextTokenSnapshot, ContextTokenSource,
    HarnessSnapshot, InputReply, InputRequest, LoopStatus, McpServerInfo, ModelInfo, NoticeKind,
    NoticeSeverity, NoticeSource, NoticeSurface, ParentStatus, PermissionDecision,
    PermissionRequest, PermissionRuleInfo, ProviderModelInfo, ProviderPickerRow,
    ProviderPickerSnapshot, QueueMode, QueuedMessage, RetryPoint, RoundEvent, RoundInterrupt,
    RoundInterruptReason, RoundSummary, RunnerEvent, SessionContextSnapshot, SessionDetail,
    SessionForkKind, SessionOverview, SessionSnapshot, SkillInfo, ToolInfo, UserQuestion,
    UserQuestionOption, UserQuestionReply, UserQuestionRequest, WebSearchConfigUpdate,
    WebSearchConfigView,
};
pub use runner::{
    RUNNER_CODE, RUNNER_EXPLORE, RUNNER_MCP_SPECIALIST, RUNNER_TITLE, RunnerPreset,
    RunnerPresetPool, ToolPolicy,
};
pub mod monitor;
pub use hooks::{
    Hook, HookContext, HookEvent, HookEventKind, HookOutcome, RestorePoint, SessionSource,
};
pub use identity::AgentIdentity;
pub use master::{
    MASTER_CODE_ANALYST, MASTER_DEVELOPER, MasterPreset, MasterPresetDelegation, MasterPresetId,
    MasterRuntimeConfig,
};
pub use mcp::{McpConnectionStatus, McpServerConfig};
pub use model::{
    BaselineModels, CapabilityOverrides, FittedModel, Model, ModelCapabilities,
    RemoteModelEndpoint, RemoteModelMetadata, WireFormat, baseline_models, model_by_id,
    register_fitted_models, resolve as resolve_model, sanitize_model_id,
};
pub use monitor::{
    MonitorAction, MonitorEvent, MonitorSnapshot, MonitoredSession, SessionHosting, SessionStatus,
};
pub use pressure::{
    CLEARED_TOOL_PREFIX, CompactionPolicy, ContextBudget, PruneOutcome, RequestTokenEstimate,
    estimate_bytes, estimate_draft_tokens, estimate_message_tokens, estimate_semantic_json_tokens,
    estimate_tokens, prune_tool_results,
};
pub use secret::SecretString;
pub use session_title::{TITLE_MAX_LEN, clean_title};
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
pub use web_config::{SharedWebSearchConfig, WebSearchConfig};
pub use wire::{
    AttachAction, ControlRequest, ERR_PROTOCOL_MISMATCH, ERR_VERSION_MISMATCH,
    MIN_PROTOCOL_VERSION, PROTOCOL_VERSION, Wire, protocol_accepts,
};
