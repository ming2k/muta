//! Shared domain and wire contracts for the neenee agent stack: the `Provider`
//! and `Tool` capability traits, conversation and tool-output types, the
//! context-pressure model, repeat/todo values, envoy profiles,
//! skills/MCP config schemas, and the events exchanged by sessions and
//! frontends.
//!
//! This crate is **pure domain, zero I/O** (ADR-0005): no filesystem, no
//! network. It keeps only contracts shared by independent layers: domain
//! values
//! (`TokenUsage`, `ScheduledJob`, `TodoList`, …), wire DTOs, and
//! capability traits (`Provider`, `Tool`, `Hook`, `SessionReview`). Pure logic
//! owned only by the agent belongs in `neenee-agent` (ADR-0057).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub use async_trait::async_trait;

pub mod cron;
pub use cron::CronExpr;
pub mod colorschemeconfig;
pub use colorschemeconfig::ColorSchemeConfig;
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

pub mod tool_output;
pub use tool_output::{PatchOp, ShellTermination, StdinPolicy, ToolOutput, ToolStream};

pub mod tool_access;
pub use tool_access::{ToolAccess, ToolAccesses, ToolFileAccessOperation};

pub mod tool_validation;

pub mod capability;
pub mod catalog;
pub mod channelauth;
pub mod effort;
pub use effort::{
    EFFORT_CLAUDE_FULL, EFFORT_CLAUDE_NO_XHIGH, EFFORT_COMMON, EFFORT_OPENAI_GPT, Effort,
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
pub mod envoy;
pub mod history;
pub use history::{HISTORY_CAP, HistoryEntry, merge_history};
pub mod identity;
pub mod pressure;
pub mod principal;
pub mod token_ledger;
pub use token_ledger::{
    RequestUsageKey, RequestUsageRecord, RequestUsageSource, RequestUsageStatus, TokenSourceLedger,
    TokenSourceReport, TokenSourceRow, TokenSourceTotals, TokenTurn,
};
pub mod doomguardconfig;
pub mod secret;
pub mod session_review;
pub mod session_title;
pub mod skillsconfig;
pub mod tool_registry;
pub mod webconfig;
pub use capability::{
    CommandScope, ModelRequest, OperationScope, Provider, ProviderPromptHints, ProviderStreamEvent,
    ScopeTarget, Tool, ToolSpec, VariantSelection, empty_variant_selection,
};
pub use catalog::{Channel, ProviderEntry, Transport};
pub use channelauth::{ChannelAuth, LoginMethod};
pub use doomguardconfig::DoomGuardConfig;
pub use dynamic::{DynamicCatalog, DynamicToolSink};
pub use envoy::{CODE, EXPLORE, EnvoyProfile, INTERACTIVE, QUANT, REVIEW, TITLE, ToolPolicy};
pub use events::{
    AgentEvent, AgentNotice, AgentOp, AgentRequest, AgentResponse, ConnectStatus,
    ContextTokenSnapshot, ContextTokenSource, EnvoyEvent, HarnessSnapshot, InputReply,
    InputRequest, LoopStatus, McpServerInfo, ModelInfo, NoticeKind, NoticeSeverity, NoticeSource,
    NoticeSurface, ParentStatus, PermissionDecision, PermissionRequest, PermissionRuleInfo,
    ProviderModelInfo, ProviderPickerRow, ProviderPickerSnapshot, QueuedUserInput, RoundEvent,
    RoundSummary, SessionContextSnapshot, SessionDetail, SessionOverview, SkillInfo, ToolInfo,
    UserQuestion, UserQuestionOption, UserQuestionReply, UserQuestionRequest,
};
pub use hooks::{
    Hook, HookContext, HookEvent, HookEventKind, HookOutcome, RestorePoint, SessionSource,
};
pub use identity::AgentIdentity;
pub use mcp::{McpConnectionStatus, McpServerConfig};
pub use model::{
    BaselineModels, FittedModel, Model, ModelCapabilities, RemoteModelEndpoint,
    RemoteModelMetadata, WireFormat, baseline_models, model_by_id, register_fitted_models,
    resolve as resolve_model,
};
pub use pressure::{
    CHARS_PER_TOKEN, CLEARED_TOOL_PREFIX, CompactionPolicy, ContextBudget, PRUNED_TOOL_PLACEHOLDER,
    PruneOutcome, count_tokens, estimate_bytes, estimate_message_tokens,
    estimate_semantic_json_tokens, estimate_tokens, prune_tool_results,
};
pub use principal::{PrincipalProfile, PrincipalRole, PrincipalRuntimeConfig};
pub use secret::SecretString;
pub use session_review::{DEFAULT_REVIEWER_HARD_STOP, ReviewStatus, ReviewVerdict, SessionReview};
pub use session_title::{TITLE_MAX_LEN, clean_title};
pub use skillsconfig::SkillsConfig;
pub use tool_output::truncate_utf8;
pub use tool_registry::{
    Capability, ToolContext, ToolContextBuilder, ToolFactory, ToolScope, ToolSelection, ToolSet,
    collect_toolset,
};
pub use webconfig::WebSearchConfig;
