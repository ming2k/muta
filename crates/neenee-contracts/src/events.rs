//! Wire types for the harness ↔ driver protocol: requests ([`AgentRequest`]),
//! responses ([`AgentResponse`]), live agent events ([`AgentEvent`]), and the
//! small data records they carry.

use crate::{ImagePart, Message, ToolOutput, ToolStream};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum AgentRequest {
    Chat {
        text: String,
        images: Vec<ImagePart>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        // Skipped when `None`: the key is absent on the wire, never explicit `null`.
        #[ts(optional)]
        sent_at_ms: Option<u64>,
    },
    /// Queue user-authored input into a round that is already running. The
    /// target session is explicit so a frontend can keep composing in a side
    /// view (or switch views) without accidentally steering the wrong agent.
    /// The input is admitted atomically at the next safe turn boundary.
    InsertUserInput {
        session_id: String,
        input: QueuedUserInput,
    },
    /// Cancel a not-yet-admitted [`AgentRequest::InsertUserInput`]. The agent
    /// linearizes cancellation against boundary admission: exactly one of
    /// `UserInputInserted` or `UserInputCancelled` is emitted for the id.
    CancelInsertedInput {
        session_id: String,
        input_id: String,
    },
    /// Start a fresh round against an explicit live session. Ordinary compose
    /// sends continue to use [`AgentRequest::Chat`]; outbox follow-ups use this
    /// variant so their session affinity survives view changes.
    ChatToSession {
        session_id: String,
        input: QueuedUserInput,
    },
    SlashCommand(String),
    Interrupt,
    /// The client declares this session over (ADR-0112). Sent on the paths
    /// where the operator's intent is "I am done with this session", not
    /// "I am detaching": the TUI's `/exit` and double-Ctrl+C quit, a
    /// headless run's terminal round, and the web panel's end-session
    /// action. The server intercepts it at the attach connection (it never
    /// reaches the driver queue) and tears the hosted session down through
    /// the same path as `ControlRequest::KillSession`: cancel the driver,
    /// fire SessionEnd hooks, drop it from the registry, and publish
    /// `SessionRemoved` so every dashboard drops the row. Disk history is
    /// kept — ending a session is not deleting it.
    EndSession,
    PermissionReply {
        request_id: String,
        decision: PermissionDecision,
        /// Full-duplex (ADR-0029): when the reply targets a permission
        /// request surfaced by a *envoy* (carried up as a
        /// [`RoundEvent::Envoy`] / [`EnvoyEvent::PermissionRequest`]),
        /// this is the parent tool-call id the request was nested under. The
        /// harness looks up the live child's `crate::EnvoyHandle` in the
        /// task registry by this id and resolves its parked oneshot directly.
        /// `None` means the request came from the top-level (or `/btw` side)
        /// agent and is resolved on `context.agent` as before.
        parent_call_id: Option<String>,
    },
    UserQuestionReply {
        request_id: String,
        answers: Vec<Vec<String>>,
        /// Full-duplex (ADR-0029): the parent tool-call id when the answered
        /// question came from an envoy's `ask_user`
        /// ([`EnvoyEvent::UserQuestionRequest`]); `None` for a top-level /
        /// side agent question. See [`AgentRequest::PermissionReply`] for the
        /// routing contract.
        parent_call_id: Option<String>,
    },
    /// Reply to an [`AgentEvent::InputRequest`] (L3.5 β): the operator's input
    /// for an interactive `bash` command, routed back to the parked oneshot.
    /// `parent_call_id` mirrors the question/permission replies for envoy
    /// routing.
    InputReply {
        request_id: String,
        text: String,
        parent_call_id: Option<String>,
    },
    SwitchProvider {
        provider_type: String,
        model: String,
        api_key: Option<crate::SecretString>,
        base_url: Option<String>,
    },
    /// Add a user-defined provider from a TUI template, persist it to config,
    /// then activate it. `protocol` is one of `"openai"` | `"anthropic"` |
    /// `"google"`; `api_key` may be empty (a keyless OpenAI-compatible relay
    /// suppresses the auth header). The harness derives a stable id from `name`.
    /// `models` is the provider's seeded model list — one channel per model,
    /// the first becoming the default/active model. A template that seeds the
    /// whole Claude family lands all of them in the picker's stage-2 list.
    ///
    /// Per ADR-0046, reasoning (effort/thinking) is no longer set at provider
    /// creation — it is opted in per model via the stage-2 model `e` editor
    /// (`EditProviderModel`). New channels start with thinking off.
    AddProvider {
        name: String,
        protocol: String,
        base_url: String,
        api_key: crate::SecretString,
        user_agent: Option<String>,
        models: Vec<String>,
        /// How the seeded channels authenticate. Default (`ApiKey`) keeps the
        /// historical behavior; `XaiOAuth` marks SuperGrok channels whose live
        /// access token is resolved from `auth.toml`.
        auth: crate::ChannelAuth,
        /// The stable template id this instance is created from, when it came
        /// from a template. `None` for a pure-custom provider. When set to a
        /// known template, the catalog re-seeds this instance's channels from
        /// the template's current model list at startup, so a template edit
        /// propagates to the instance. See
        /// `neenee_agent::catalog::reconcile_provider_models`.
        template_id: Option<String>,
    },
    /// Connect (authenticate) an OAuth provider — currently xAI SuperGrok. Runs
    /// the browser-loopback or device-code flow, persists tokens to `auth.toml`,
    /// then activates `id`. Progress streams via [`AgentResponse::ConnectStatus`].
    ConnectProvider {
        id: String,
        method: crate::LoginMethod,
    },
    /// Run an OAuth login **before** a provider instance exists ("+ Add
    /// provider → xAI OAuth / ChatGPT OAuth"). `auth` selects which OAuth
    /// provider to authenticate against. Persists tokens and streams
    /// [`AgentResponse::ConnectStatus`]; the TUI then prompts for instance name.
    AuthorizeOAuth {
        method: crate::LoginMethod,
        auth: crate::ChannelAuth,
    },
    /// Edit a user-defined provider's metadata in place (display name, protocol,
    /// base URL, API key) without touching its model list — every channel keeps
    /// its model id, so a multi-model custom provider is not collapsed. An empty
    /// `api_key` leaves the existing key untouched. Built-in providers are not
    /// editable this way (their `e` editor only sets the API key).
    ///
    /// Per ADR-0046, this no longer carries reasoning knobs — effort/thinking
    /// are per-model (`EditProviderModel`), not provider-wide.
    EditProvider {
        id: String,
        name: String,
        protocol: String,
        base_url: String,
        api_key: crate::SecretString,
    },
    /// Remove a model (channel) from a user-defined provider, persist, and push a
    /// fresh picker snapshot. The last remaining model is kept (a provider must
    /// serve at least one model).
    RemoveProviderModel {
        provider_id: String,
        model: String,
    },
    /// Edit settings for one model/channel of a user-defined provider. This is
    /// intentionally channel-scoped: OpenAI effort and Anthropic
    /// effort/thinking can vary by model even when the provider endpoint/key are
    /// shared.
    EditProviderModel {
        provider_id: String,
        model: String,
        effort: Option<String>,
        thinking: Option<bool>,
    },
    /// Edit the per-model reasoning settings (Anthropic effort/thinking) for a
    /// **built-in** model, persisted into the `[model_reasoning."<model-id>"]`
    /// table. This is the model-level counterpart to `EditProviderModel`:
    /// built-in providers (e.g. `anthropic`) have no user-editable channels, so
    /// their per-model reasoning knobs live in this shared table keyed by model
    /// id rather than on a channel. ADR-0045.
    EditModelReasoning {
        model: String,
        effort: Option<String>,
        thinking: Option<bool>,
    },
    /// Delete a user-defined provider entirely: drop the entry from
    /// `config.providers`, remove it from `favorites`, and persist. If the
    /// deleted provider was active (`default_provider`), fall back to the
    /// default built-in provider (`"kimi-code"`) and activate it so the live
    /// provider never points at a removed entry. Built-in providers are not
    /// deletable this way; the handler ignores unknown / built-in ids.
    DeleteProvider {
        id: String,
    },
    /// Toggle the favorite flag on a model in the **Models** picker. `id` is the
    /// model wire id. Favorite is model-level (a daily-driver model is starred
    /// wherever it is served), so the Connections list has no favorite concept.
    ToggleFavorite {
        id: String,
    },
    /// Make `id` the default model and activate it. Equivalent to selecting it
    /// in the picker and pressing `d`: it both sets the persisted default and
    /// switches the live provider.
    SetDefaultModel {
        id: String,
    },
    /// Refresh / rediscover available models for discovery-enabled providers from upstream.
    RefreshProviderModels {
        #[serde(default)]
        user_initiated: bool,
    },
    /// Delete a session (active or archived) by id or short id prefix.
    DeleteSession {
        id: String,
    },
    /// Set (or clear) a session's display title — the manual title the
    /// monitor row and session pickers show (ADR-0022's AI title fills it
    /// only while no manual title exists). `title: None` clears the manual
    /// title back to the AI/first-prompt fallback. The harness republishes
    /// the monitor row so every client sees the rename.
    RenameSession {
        id: String,
        title: Option<String>,
    },
    /// Request full detail for one session (the `i` session-info sub-view).
    /// The harness replies with [`AgentResponse::SessionDetail`].
    QuerySessionDetail {
        id: String,
    },
    /// Request the token-source report (per-round / per-turn request usage,
    /// reported vs. estimated) for one session. The harness replies with
    /// [`AgentResponse::TokenUsageReport`] carrying a snapshot of its
    /// server-side ledger. Attached frontends have no local ledger, so the
    /// context-usage modal (click on the hint-bar meter) issues this on
    /// demand — mirroring [`AgentRequest::QuerySessionDetail`].
    QueryTokenUsage {
        session_id: String,
    },
    /// Request the cross-session usage-statistics report (ADR-0122): daily
    /// token totals, per-model breakdown, and the recent terminal-request
    /// event log, aggregated over the durable day-partitioned store that
    /// survives session cleanup. The harness replies with
    /// [`AgentResponse::UsageStatsReport`]. Sent by the TUI when the
    /// `/usage` overlay opens.
    QueryUsageStats {
        /// How many recent events to include in the event-log tail.
        event_cap: usize,
    },
    /// Request a fresh session-context snapshot (model / tools / permissions /
    /// skills / mcp). The harness replies with [`AgentResponse::SessionContext`].
    /// Sent by the TUI when a manager modal opens.
    QuerySessionContext,
    /// Revoke a single cached "always allow" permission rule. The harness
    /// removes it from the in-memory allowlist and replies with an updated
    /// [`AgentResponse::SessionContext`] so the modal reflects the change.
    RevokePermission {
        tool: String,
        scope: String,
    },
    /// Clear every cached "always allow" permission rule for this process.
    /// The harness drops the whole in-memory allowlist and replies with an
    /// updated [`AgentResponse::SessionContext`] so the permissions manager
    /// modal reflects the now-empty list.
    ClearAllPermissions,
    /// Enable or disable a tool for the current session. Disabled tools are
    /// hidden from the model (their schemas are not sent) and rejected if the
    /// model still tries to call them. The harness replies with an updated
    /// [`AgentResponse::SessionContext`].
    ToggleTool {
        name: String,
        enabled: bool,
    },
    /// Enable or disable a configured MCP server for the live session. Unlike
    /// [`AgentRequest::ToggleTool`] (which only flips a session flag on an
    /// already-installed tool), this connects/disconnects the server: disabling
    /// drops its tools from the live tool list and closes the connection;
    /// enabling reconnects it from `[mcp.<name>]` config and re-discovers its
    /// tools. Session-scoped — config.toml is not rewritten, so a restart
    /// restores the configured state. The harness replies with an updated
    /// [`AgentResponse::SessionContext`].
    ToggleMcpServer {
        name: String,
        enabled: bool,
    },
    /// Reset and re-establish one MCP server's connection, re-discovering its
    /// tools (the per-server analogue of the periodic catalog refresh). Used by
    /// the `/mcp` modal's `r` action to recover a crashed/failed server on
    /// demand. The harness replies with an updated
    /// [`AgentResponse::SessionContext`].
    ReconnectMcpServer {
        name: String,
    },
    /// Run a shell command directly through the `bash` tool, bypassing the
    /// LLM. Triggered by the TUI's `!` prefix (e.g. `!git status`). The
    /// harness emits a synthetic `ToolCall`, live `ToolStream` events, and a
    /// final `ToolResult`, mirroring a normal bash step's lifecycle.
    ShellCommand {
        command: String,
    },
    /// Detach from the `/btw` aside view and return to the primary transcript
    /// (ADR-0103). The aside **keeps running**: its in-flight round is left
    /// alone and its session stays registered so it can be re-entered via
    /// [`AgentRequest::FocusSide`] or the asides list. The harness emits
    /// [`AgentResponse::SideViewClosed`]. Sent by the TUI when the user
    /// presses `Ctrl+C` inside an aside view. A pristine aside (no round ever
    /// started) is discarded outright instead of lingering in the list.
    ExitSideView,
    /// Jump the view into a live `/btw` aside (ADR-0103): open it if it was
    /// closed, make the composer target it, and emit
    /// [`AgentResponse::SideViewOpened`] with the aside's full transcript so
    /// the frontend rebuilds its side buffer (the inherited parent context
    /// included). Sent when the user re-enters an aside from the asides list.
    FocusSide {
        side_id: String,
    },
    /// Interrupt the in-flight round of one `/btw` aside (ADR-0103). Esc
    /// inside an aside view resolves to this — interrupting an aside never
    /// closes it. The aside's round unwinds with its own `[Interrupted]`
    /// cleanup, mirroring [`AgentRequest::Interrupt`] for the primary.
    InterruptSide {
        side_id: String,
    },
    /// Close one `/btw` aside for real: cancel any in-flight round, drop the
    /// registry entry, **and delete its session files** (ADR-0103 §4). The
    /// aside disappears from the asides list and `/sessions`. If the aside
    /// was the focused view, the harness also emits
    /// [`AgentResponse::SideViewClosed`]. Sent by the asides modal's `D`
    /// action.
    CloseSide {
        side_id: String,
    },
    /// Request the `/btw` asides list (ADR-0103). The harness replies with
    /// [`AgentResponse::BtwList`]. Sent when the asides modal opens or is
    /// refreshed, and by the event loop to keep the header's aside count
    /// truthful.
    QueryBtwList,
    /// Update the transcript layout preference.
    /// The harness writes the new value to `config.toml`'s `[tui]
    /// transcript_layout` and replies with [`AgentResponse::TuiLayoutUpdated`]
    /// carrying the persisted string so the renderer updates its state. The value
    /// is a raw config string (e.g. "turn_band"); interpretation into a [`crate`] layout
    /// `Strategy` happens in the renderer, keeping the core free of render types.
    UpdateTuiLayout(String),
    /// Update the TUI color scheme preference (from the `/config` modal).
    /// The harness persists the selected preset id and the custom semantic
    /// palette together so switching away from Custom does not discard it.
    UpdateTuiColorScheme {
        name: String,
        custom: crate::ColorSchemeConfig,
    },
}

/// User-authored input waiting to be inserted into a running round.
///
/// `text` is the provider-facing payload. `display_text` preserves compact
/// attachment chips for the transcript when the provider-facing form expanded
/// a large paste. The stable id is generated by the submitting frontend and
/// makes admission/cancellation races deterministic.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct QueuedUserInput {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    // Skipped when `None`: absent on the wire, never an explicit `null`.
    #[ts(optional)]
    pub display_text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImagePart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    // Skipped when `None`: absent on the wire, never an explicit `null`.
    #[ts(optional)]
    pub sent_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentResponse {
    /// A per-round event tagged with the session it belongs to (ADR-0017). The
    /// TUI keys its transcript buffers by `session_id` and routes `event` to
    /// the matching one, so a primary round and a live `/btw` side round can
    /// stream concurrently over the single harness↔TUI channel without
    /// clobbering each other's transcript.
    ///
    /// Global (non-session-scoped) responses — command replies, modal
    /// snapshots, provider switches — stay as dedicated top-level variants so
    /// they are handled once, regardless of which view is focused.
    Round {
        session_id: String,
        event: RoundEvent,
    },
    /// Coarse status of the primary session, surfaced to a side view's banner
    /// while the user is inside a `/btw`. Emitted by the session registry's
    /// parent-status watcher; the primary round is deliberately left running, so
    /// this is how the user learns the main session hit an approval/input wall.
    ParentStatus(ParentStatus),
    /// The user entered a `/btw` aside view (ADR-0017, extended by ADR-0103).
    /// The TUI records `side_id` as the routing key for per-round events and
    /// rebuilds its side transcript buffer from `messages` + `commands` — the
    /// aside's full persisted transcript at open time, inherited parent
    /// context included — so the viewed pixels match the model's actual
    /// context window. Emitted by the harness on `/btw` (new aside) and on
    /// [`AgentRequest::FocusSide`] (re-entry).
    SideViewOpened {
        side_id: String,
        primary_id: String,
        /// The aside's persisted transcript at open time. One-shot back-fill:
        /// after this, per-round `Round` events stream into the same buffer.
        #[serde(default)]
        messages: Vec<Message>,
        /// Command-ledger rows for the aside (ADR-0091), same as
        /// [`AgentResponse::ConversationReplaced`].
        #[serde(default)]
        commands: Vec<crate::command::CommandRecord>,
    },
    /// The user left the `/btw` aside view (ADR-0103). The TUI returns to the
    /// primary transcript. Detach is non-destructive by default: the aside
    /// keeps running unless it was pristine (no round ever started), in which
    /// case it was discarded. Emitted by the harness in reply to
    /// [`AgentRequest::ExitSideView`] / [`AgentRequest::CloseSide`].
    SideViewClosed,
    /// The `/btw` asides list (ADR-0103), newest first. Drives the asides
    /// modal (`F5` / `/btw list`) and the main view's header aside count.
    /// Pushed on every list mutation (open, detach-with-discard, close) and
    /// in reply to [`AgentRequest::QueryBtwList`].
    BtwList(Vec<BtwAsideSummary>),
    PermissionsCleared,
    /// Lowercase provider name → whether a usable API key is configured.
    ProviderKeys(Vec<(String, bool)>),
    /// Full provider-picker state (default id + one row per provider) for the
    /// provider picker. Supersedes `ProviderKeys` for the picker's needs;
    /// `ProviderKeys` is retained for the header key-readiness summary.
    ProviderPicker(ProviderPickerSnapshot),
    /// Blank the visible transcript and zero the round counter: the harness
    /// switched to a brand-new empty session (`/new`, `/session new`). The
    /// previous session is untouched on disk — nothing was deleted.
    /// `session_id` is the freshly minted id, mirroring
    /// [`AgentResponse::ConversationReplaced`]'s post-switch id: attached
    /// frontends track it so session-scoped state (the inline ↑/↓ prompt
    /// recall, on-demand queries) follows the switch instead of lingering on
    /// the retired session.
    ConversationCleared {
        session_id: String,
    },
    /// Replace the visible transcript (dialogue messages) AND the command
    /// ledger (ADR-0091) with another session's state, after `/session open`,
    /// `/resume`, `/session resume`. The frontend rebuilds the whole document
    /// from these two sources: pure dialogue from `messages`, command rows
    /// from `commands`. `session_id` identifies the session that produced the
    /// replacement (the harness emits this only as a session switch, so it is
    /// the post-switch id); attached frontends track it to keep on-demand
    /// queries (e.g. [`AgentRequest::QueryTokenUsage`]) session-correct.
    ConversationReplaced {
        session_id: String,
        messages: Vec<Message>,
        #[serde(default)]
        commands: Vec<crate::command::CommandRecord>,
    },
    /// Replace the sessions picker contents (and open the picker).
    SessionsOverview(Vec<SessionOverview>),
    /// Open the session dashboard (`/dashboard`, formerly `/host`; ADR-0096).
    /// The TUI renders the monitor stream it maintains independently; this is
    /// only the open signal, carrying no data.
    OpenHostPanel,
    /// Reply to [`AgentRequest::QuerySessionDetail`]: full detail for one
    /// session (complete last prompt, title, timestamps). Consumed by the
    /// session-info sub-view.
    SessionDetail(SessionDetail),
    /// Reply to [`AgentRequest::QueryTokenUsage`]: the daemon-side token-source
    /// report for one session (per-round request usage, reported vs.
    /// estimated). Attached frontends hold no local ledger, so the
    /// context-usage modal renders this snapshot; the session id lets the
    /// frontend discard a reply that raced a session switch.
    TokenUsageReport {
        session_id: String,
        report: crate::token_ledger::TokenSourceReport,
    },
    /// Reply to [`AgentRequest::QueryUsageStats`]: the cross-session usage
    /// report (per-day / per-model totals + recent event log) aggregated from
    /// the durable usage store. Unlike [`Self::TokenUsageReport`] this data is
    /// session-independent — it outlives session deletion by design
    /// (ADR-0122) — so no session id accompanies it.
    UsageStatsReport {
        report: crate::usage_stats::UsageStatsReport,
    },
    Error(String),
    Exit,
    ProviderSwitched {
        provider: String,
        model: String,
    },
    /// Progress of an OAuth connect/authorize flow (xAI SuperGrok).
    ConnectStatus(ConnectStatus),
    /// Full session-context snapshot (model + tools + permissions + skills +
    /// mcp) for the session modal. Sent in reply to [`AgentRequest::QuerySessionContext`]
    /// and re-sent after any mutation handled by the harness
    /// ([`AgentRequest::RevokePermission`] / [`AgentRequest::ToggleTool`]).
    SessionContext(SessionContextSnapshot),
    /// The transcript layout preference was updated (from the `/config` modal
    /// via [`AgentRequest::UpdateTuiLayout`]). Carries the persisted config
    /// string so the modal re-renders from the authoritative state — the TOML
    /// write is the source of truth, not the TUI's optimistic local edit.
    TuiLayoutUpdated(String),
    /// The TUI color scheme and custom palette were persisted successfully.
    TuiColorSchemeUpdated {
        name: String,
        custom: crate::ColorSchemeConfig,
    },
}

/// A user-visible notice emitted by the agent or harness.
///
/// This is distinct from state-sync events such as [`RoundEvent::TodosUpdated`]
/// and blocking interaction events such as [`RoundEvent::PermissionRequest`]:
/// those events update UI state or require a reply, while a notice means
/// "surface this fact to the user".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
// `body` skips serialization when `None`: absent on the wire, never `null`.
#[ts(optional_fields, export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct AgentNotice {
    pub id: String,
    pub kind: NoticeKind,
    pub severity: NoticeSeverity,
    /// Preferred UI surface. Frontends may degrade this when a surface is not
    /// available, e.g. render a toast as an inline notice.
    pub surface: NoticeSurface,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub source: NoticeSource,
}

impl AgentNotice {
    pub fn new(
        kind: NoticeKind,
        severity: NoticeSeverity,
        title: impl Into<String>,
        source: NoticeSource,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            severity,
            surface: NoticeSurface::Inline,
            title: title.into(),
            body: None,
            source,
        }
    }

    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn with_surface(mut self, surface: NoticeSurface) -> Self {
        self.surface = surface;
        self
    }

    /// Build an ephemeral, toast-surfaced acknowledgment of a slash command or
    /// configuration change (the *reply* to a command, not its invocation).
    ///
    /// `title` is the one-line confirmation (e.g. `"Autopilot ON: …"`). The
    /// notice is `Info` severity and routed to the [`NoticeSurface::Toast`]
    /// surface so frontends show it as a transient bubble and do **not** append
    /// it to the transcript — it carries no conversational content. ADR-0050
    /// keeps the command *invocation* durable; this reply is deliberately
    /// ephemeral.
    ///
    /// Prefer this constructor over `AgentNotice::new(...).with_surface(Toast)`
    /// so the `CommandAck` kind is stamped uniformly and frontends can branch
    /// on `kind == CommandAck` (e.g. to suppress re-surfacing on reconnect).
    ///
    /// A command acknowledgment is a *session-scoped* notice: emit it wrapped
    /// in [`crate::RoundEvent::Notice`] (via `round_response`), not as a
    /// top-level `AgentResponse::Notice`. Wrapping it routes the toast to the
    /// frontend over the session's broadcast tap so every attached client (the
    /// in-process TUI, `neenee attach`, `/serve`) sees the same confirmation,
    /// and it is what the TUI's toast drain actually listens for.
    pub fn command_ack(title: impl Into<String>) -> Self {
        Self::new(
            NoticeKind::CommandAck,
            NoticeSeverity::Info,
            title,
            NoticeSource::Harness,
        )
        .with_surface(NoticeSurface::Toast)
    }

    pub fn render_text(&self) -> String {
        match self.body.as_deref().filter(|body| !body.trim().is_empty()) {
            Some(body) => format!("{}\n{}", self.title, body),
            None => self.title.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum NoticeKind {
    ProviderRetry,
    NudgeInjected,
    ReviewAlert,
    /// A harness-level acknowledgment of a slash command / configuration change
    /// (e.g. `/autopilot on`, `--autopilot`, `/permissions clear`). These are
    /// status confirmations, not model output: they carry no conversational
    /// content, so frontends should surface them as a transient notification
    /// (toast) rather than appending them to the transcript as if the model
    /// had spoken. See ADR-0050 for the durable-vs-ephemeral boundary — the
    /// command *invocation* stays durable; this *reply* is ephemeral.
    CommandAck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum NoticeSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum NoticeSurface {
    /// Render inline in the current conversation or event feed.
    Inline,
    /// Show as a transient bubble/toast.
    Toast,
    /// Show in a retained alert area until the related condition clears or is
    /// superseded.
    Banner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum NoticeSource {
    Agent,
    TurnGuard,
    Todo,
    Review,
    Harness,
}

/// Session-scoped events emitted while a user round runs, carried under an
/// [`AgentResponse::Round`] envelope (ADR-0017). Splitting these off
/// `AgentResponse` makes "which session does this belong to" a first-class
/// question: every event — whether from the primary or a `/btw` side — arrives
/// tagged with its `session_id`, and global/command responses stay top-level.
/// Origin of the current model-context token count shown by frontends.
///
/// This describes the AI-visible request context, never the durable session or
/// rendered transcript size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum ContextTokenSource {
    /// Count reported by the provider for the completed request, plus that
    /// request's completion (which becomes history for the next request).
    Api,
    /// Local estimate of the provider-visible projection of `model_window`.
    Projection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct ContextTokenSnapshot {
    pub tokens: usize,
    pub source: ContextTokenSource,
}

/// A compact per-round accounting handed to frontends when a user round
/// completes naturally. The "active" generation time is
/// `duration_ms.saturating_sub(paused_ms)`; dividing `output_tokens` by it
/// yields an honest tokens/sec that reflects the server's real throughput,
/// unaffected by how long the user deliberated on a permission prompt or
/// `ask_user` question.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct RoundSummary {
    /// 1-based user-round index, mirroring the transcript's round counter.
    pub round: u64,
    /// Total output (completion) tokens the model generated this round.
    pub output_tokens: u64,
    /// Full wall-clock duration of the round, including any human-decision
    /// pause time (`paused_ms`) and all tool execution.
    pub duration_ms: u64,
    /// Time within `duration_ms` the round spent parked on a human decision
    /// (a permission request or an `ask_user`). `0` when nothing blocked.
    pub paused_ms: u64,
    /// Time the model actually spent *generating* — summed across every
    /// completed provider request in the round, measured from request start
    /// to a validated response and therefore **excluding** tool execution,
    /// hooks, and human-decision pauses. This is the honest denominator for
    /// tokens/sec: `tps = output_tokens / generation_ms`. Falls back to the
    /// round `active_ms()` only when no request completed measurably.
    pub generation_ms: u64,
}

impl RoundSummary {
    /// Net-active generation time: the wall-clock minus the human-decision
    /// pause. Saturates at 0 so a round that somehow paused longer than it ran
    /// still yields a finite (large) TPS rather than dividing by a negative.
    pub fn active_ms(&self) -> u64 {
        self.duration_ms.saturating_sub(self.paused_ms)
    }

    /// The time `tps()` divides by: `generation_ms` when at least one request
    /// completed measurably, otherwise the round `active_ms()` fallback.
    /// Exposed so a UI can render *exactly* the denominator the throughput
    /// figure was computed from, instead of a coincidentally-larger span.
    pub fn denominator_ms(&self) -> u64 {
        if self.generation_ms > 0 {
            self.generation_ms
        } else {
            self.active_ms()
        }
    }

    /// Output tokens per second of *generation* time — the time the model
    /// actually spent streaming a response, excluding tool execution and
    /// human-decision pauses. Falls back to round `active_ms()` (wall-clock
    /// minus human pause) when no provider request completed measurably, and
    /// returns `0.0` when there is no usable denominator so the UI renders `–`
    /// rather than `inf`.
    pub fn tps(&self) -> f64 {
        let denominator_ms = self.denominator_ms();
        if denominator_ms == 0 {
            0.0
        } else {
            (self.output_tokens as f64) * 1000.0 / (denominator_ms as f64)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum RoundEvent {
    Notice(AgentNotice),
    /// Current AI-visible context size for this session. Frontends must use
    /// this instead of estimating from the persisted/rendered transcript.
    ContextTokens(ContextTokenSnapshot),
    /// A queued insert crossed the agent's safe turn boundary and is now part
    /// of the live conversation (the agent persists that boundary before the
    /// provider observes it). Frontends append it to the transcript at this
    /// event, never when it was merely queued.
    UserInputInserted(QueuedUserInput),
    /// The addressed round stopped accepting inserts before this id could be
    /// admitted. A frontend may safely retain it as a paused next-round item.
    UserInputUnavailable {
        input_id: String,
    },
    /// A pending insert was cancelled before admission.
    UserInputCancelled {
        input_id: String,
    },
    /// Cancellation lost the race with admission (or the id was unknown).
    /// The subsequent inserted/unavailable event remains authoritative.
    UserInputCancelFailed {
        input_id: String,
    },
    /// A next-round outbox item was accepted by its exact live session and a
    /// fresh round was started. Like insertion, this is the transcript commit
    /// point for frontends.
    NextRoundStarted(QueuedUserInput),
    /// The user-driven round reached its natural, successful terminal path.
    /// Interruptions, blocked prompts, and errors deliberately emit no such
    /// event, so next-round outbox items pause instead of auto-running.
    /// Carries a small per-round summary so frontends can show an honest
    /// generation throughput (tokens/sec) that excludes the time the round
    /// spent parked on human decisions (permission prompts / ask_user).
    RoundCompleted(RoundSummary),
    Text(String),
    /// A typed slash-command result (ADR-0091). Replaces the `Text` replies
    /// commands used to emit: the TUI renders it as a distinct command block
    /// (dimmed header + expandable result), never as assistant prose, and the
    /// same value is recorded in the session's command ledger for resume/
    /// export/audit.
    /// A typed slash-command result (ADR-0091). Replaces the `Text` replies
    /// commands used to emit: the TUI renders it as a distinct command block
    /// (dimmed invocation header + expandable result), never as assistant
    /// prose, and the same value is recorded in the session's command ledger
    /// for resume/export/audit.
    CommandResult {
        /// Command word without the leading slash (e.g. `"search"`), or
        /// `"shell"` for a `!command` passthrough.
        name: String,
        /// Raw argument remainder after the command word.
        args: String,
        result: crate::command::CommandResult,
    },
    /// Turn-level error (e.g. a provider failure mid-turn). Distinct from the
    /// global [`AgentResponse::Error`] only in that it belongs to a specific
    /// session's transcript and is therefore carried under the [`Round`]
    /// envelope.
    ///
    /// [`Round`]: AgentResponse::Round
    Error(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        id: String,
        name: String,
        output: String,
        structured: ToolOutput,
        duration_ms: u64,
    },
    /// Incremental output streamed by a running tool (see [`ToolStream`]).
    ToolStream {
        id: String,
        stream: ToolStream,
    },
    ToolCancelled {
        id: String,
        name: String,
    },
    PermissionRequest(PermissionRequest),
    UserQuestionRequest(UserQuestionRequest),
    /// Mirrors [`AgentEvent::InputRequest`]: an interactive `bash` command
    /// needs operator input (L3.5 β).
    InputRequest(InputRequest),
    /// A context projection (compaction or prune) was committed. Token
    /// samples of the active window around the projection (ADR-0120).
    Compacted {
        archived_messages: usize,
        window_tokens_before: usize,
        window_tokens_after: usize,
    },
    HarnessState(HarnessSnapshot),
    /// The task list changed (full-replace via `todo`, surgical update via
    /// `todo_update`). Mirrors [`AgentEvent::TodosUpdated`]. An empty list
    /// means "no active task list" and hides the sticky panel.
    TodosUpdated(crate::todos::TodoList),
    /// The autopilot toggle changed. `autopilot` = the agent runs without
    /// human intervention (no confirmations, no questions). Emitted by
    /// `/autopilot` so the TUI can refresh its badge without waiting for the
    /// next harness snapshot.
    AutopilotChanged(bool),
    RetryScheduled {
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        message: String,
    },
    Activity(String),
    /// A new ReAct turn started within the current user round. `turn` is the
    /// 0-indexed model-request index within the round (0 = the first request).
    /// Surfaced as structured data so the activity bar can render
    /// `round N · turn M · <status>` without parsing the turn back out of the
    /// `Activity` status string. Emitted just before the matching
    /// `Activity("waiting for model")`.
    TurnStarted {
        /// 1-indexed enclosing user round.
        round: u64,
        /// 0-indexed model-request position within `round`.
        turn: usize,
    },
    StreamStart,
    StreamDelta(String),
    StreamReasoningDelta(String),
    StreamReasoningEnd(String),
    StreamEnd(String),
    StreamDiscard,
    /// The user interrupted the round before any model output reached the
    /// client (Phase 1: request in-flight, no response bytes yet). The round's
    /// user message has been removed from the conversation context and session
    /// store, and the TUI should restore `prompt` (and any `images`) into the
    /// input box for re-editing — the conversation is back to its pre-send
    /// state. The cancelled network request may still bill its input tokens
    /// on the provider side, but no assistant message, tool calls, or output
    /// tokens are produced or recorded.
    UnsentInput {
        prompt: String,
        images: Vec<crate::ImagePart>,
    },
    /// An envoy event to render nested inside the parent tool step.
    Envoy {
        parent_call_id: String,
        event: EnvoyEvent,
    },
}

/// Coarse status of the primary session, reported to a `/btw` side view's
/// banner (ADR-0017). This is the codex `SideParentStatus` equivalent: the
/// whole reason the parent round is left running instead of cancelled is so the
/// user can see the main session hit an approval or input wall and jump back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParentStatus {
    Idle,
    Running,
    NeedsApproval,
    NeedsInput,
    Failed,
    Interrupted,
}

/// One row of the `/btw` asides list (ADR-0103): a live aside conversation
/// forked from the primary session. Rows are ordered newest-first (most
/// recently opened or re-entered first) by the harness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BtwAsideSummary {
    /// The aside's session id — the same key per-round `Round` events carry.
    pub id: String,
    /// Short label: the aside's first user prompt when it has one, else a
    /// neutral placeholder. Already truncated for one-line display.
    pub title: String,
    /// Whether the aside has an in-flight round right now.
    pub running: bool,
    /// Epoch seconds of the aside's last activity (creation or last write).
    pub updated_at: u64,
}

/// Coarse, display-level status of a session's round lifecycle, mirrored to
/// the TUI activity bar. This is a badge, not the protocol state: the round
/// lifecycle itself (`RoundLifecycle` in neenee-agent) is binary — no active
/// round, or an active round identified by a generation.
/// Awaiting-permission / awaiting-input are overlays derived from the
/// parked-request tables (see [`ParentStatus`]), not values here: they carry
/// no lifecycle meaning (interrupt behaves identically) and there is no
/// user-level pause/resume for them to describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum LoopStatus {
    Idle,
    Running,
}

impl LoopStatus {
    pub fn is_idle(self) -> bool {
        matches!(self, Self::Idle)
    }

    /// The wire string, also used directly by the TUI's activity bar.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
        }
    }
}

impl std::fmt::Display for LoopStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct HarnessSnapshot {
    pub loop_status: LoopStatus,
    /// Monotonic session round counter. For a running snapshot this is the
    /// admitted active round; for an idle snapshot it is the most recently
    /// admitted round. Frontends must use this instead of counting visible
    /// transcript messages, which may have been compacted.
    #[serde(default)]
    pub round_counter: u64,
    /// Whether write-tool permission prompts are bypassed this session
    /// (`--autopilot` / `/autopilot on`). The TUI mirrors this into a
    /// visible badge so the elevated state is never silent.
    pub autopilot: bool,
}

/// A row in the sessions picker: enough to identify, describe and order a past
/// session without leaking the full transcript to the TUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOverview {
    pub id: String,
    pub overview: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    pub active: bool,
    /// The session this one was forked from, when it is a branch rather
    /// than a trunk (`/fork`, `/btw` aside). `None` for a trunk session
    /// (`/new` or the very first session). Lineage is what the dashboard
    /// groups by: one trunk row per conversation, its branches nested
    /// beneath — there is always exactly one *main* line, the trunk the
    /// user is driving; branches are derived views that never replace it.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// How this session came to exist, so the dashboard can badge rows
    /// without string-matching ids: `fork` (an explicit `/fork` branch that
    /// *replaced* the active pointer), `aside` (a `/btw` background
    /// conversation forked off the trunk), or `trunk` (no parent).
    #[serde(default)]
    pub fork_kind: SessionForkKind,
}

/// The provenance of a session relative to its lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum SessionForkKind {
    /// A root session: `/new` or the first-ever session. The main line.
    #[default]
    Trunk,
    /// An explicit `/fork` branch (the active pointer moved here).
    Fork,
    /// A `/btw` aside: forked from the trunk, running alongside it.
    Aside,
}

/// Full detail for one session, requested on demand (the session-info
/// sub-view, `i` from the sessions picker). Unlike [`SessionOverview`], which
/// carries a truncated preview, this carries the *complete* last effective user
/// prompt so the info view can show it in full. Built from the same deferred
/// header parse as the picker rows (no full-transcript deserialize).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionDetail {
    pub id: String,
    /// Stored title (AI or manual), if any.
    pub title: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    pub active: bool,
    /// The complete, untruncated text of the last non-echo user prompt, or
    /// `None` when the session has no real user turn yet.
    pub last_prompt: Option<String>,
}

/// One row of provider-picker state sent from the harness to the TUI. Carries
/// everything the picker renders for a provider — display name, the served model
/// ids and the active one, plus the dynamic signals (key readiness, favorite,
/// last-used) — keyed by canonical provider id. The TUI renders directly from
/// these rows (built-in and user-defined providers share one path), so no static
/// per-provider table is consulted. See ADR-0002.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct ProviderPickerRow {
    pub id: String,
    /// Display name (e.g. `"OpenAI"`, `"Anthropic"`, or a custom provider's name).
    pub name: String,
    /// Wire id of the currently-active model on this provider.
    pub model: String,
    /// Every model id this provider serves, in catalog order. A single-model
    /// provider lists exactly one; multi-model providers list all of them.
    pub models: Vec<String>,
    /// Per-model/channel settings in the same order as `models`. Newer TUIs use
    /// this to render and edit model-specific controls such as Anthropic
    /// effort/thinking. `models` stays as the simple compatibility list.
    #[serde(default)]
    pub model_info: Vec<ProviderModelInfo>,
    /// `true` for built-in presets, `false` for user-defined providers. The TUI
    /// only offers add/remove-model (and full meta editing) on user-defined
    /// providers.
    pub builtin: bool,
    /// Wire protocol id of the default channel (`"openai"` | `"anthropic"` |
    /// `"google"`), used to pre-fill the edit form for a user-defined provider.
    /// Empty for built-ins (their `e` editor only changes the API key).
    pub protocol: String,
    /// Base URL of the default channel, used to pre-fill the edit form. Empty
    /// for built-ins and keyless/native transports.
    pub base_url: String,
    pub key_ready: bool,
    /// The add-provider template that birthed this instance (`"openai"`,
    /// `"anthropic"`, `"openai-sub2api"`, …), when known. Surfaced to the TUI
    /// so the **Connections** list can show the provider *type* beside the
    /// instance name — distinct from the user-given instance name. Empty for
    /// instances with no recorded template (legacy configs).
    #[serde(default)]
    pub template_id: String,
    /// Unix epoch milliseconds of the last activation. `None` if the provider
    /// has never been activated, which the picker sorts as "oldest".
    pub last_used_ms: Option<u64>,
    /// How the default channel authenticates. Surfaced so the TUI can route an
    /// OAuth provider with no stored token to the connect flow rather than the
    /// API-key editor.
    #[serde(default)]
    pub auth: crate::ChannelAuth,
}

/// Progress / outcome of an OAuth connect flow (xAI SuperGrok).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectStatus {
    /// User must complete authorization out-of-band. `url` is the authorize /
    /// verification URL; `user_code` is set for device-code (empty for browser).
    Pending {
        provider: String,
        url: String,
        user_code: String,
        message: String,
    },
    /// Authorization succeeded; tokens persisted (and provider activated when
    /// this followed [`AgentRequest::ConnectProvider`]).
    Done { provider: String },
    /// Authorization succeeded but the follow-up live model discovery failed,
    /// so the provider keeps its previous (often seed-only) model list. The
    /// UI surfaces this as a warning so the user does not mistake a stale list
    /// for the account's real entitlements.
    DiscoveryWarning { provider: String, message: String },
    /// Authorization failed or was denied.
    Failed { provider: String, message: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct ProviderModelInfo {
    /// Wire model id. Mirrors an entry in [`ProviderPickerRow::models`].
    pub model: String,
    /// Wire protocol id of the channel serving this model (`"openai"` |
    /// `"anthropic"` | `"google"`).
    pub protocol: String,
    /// Effective reasoning effort for channels whose model exposes an effort
    /// knob. `None` for protocols/models that do not expose one.
    pub effort: Option<String>,
    /// Effective extended-thinking state for channels that expose a separate
    /// thinking on/off knob. `None` for protocols that do not expose one.
    pub thinking: Option<bool>,
    /// Whether this model is favorited in the **Models** picker (ADR-0046 moved
    /// favorite from provider-level to per-model). A starred daily-driver model
    /// sorts into the second priority tier of the flat list wherever it is
    /// served. Added late, so it defaults to `false` on deserialize for older
    /// snapshots.
    #[serde(default)]
    pub favorite: bool,
}

/// Full snapshot of provider-picker state: which provider is the current
/// default plus one row per known provider. Sent on startup and after any
/// mutation (favorite toggle, default change, provider switch) so the TUI
/// always renders from a fresh, consistent picture rather than merging
/// incremental updates.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct ProviderPickerSnapshot {
    /// Canonical id of the active/default provider. Matches
    /// `config.default_provider`.
    pub default_id: String,
    pub rows: Vec<ProviderPickerRow>,
}

/// Complete state snapshot of a live or persisted session for client hydration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub round_counter: u64,
    pub messages: Vec<Message>,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub todos: Vec<crate::todos::TodoItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<ContextTokenSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_picker: Option<ProviderPickerSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_keys: Option<Vec<(String, bool)>>,
}

/// Events emitted by an envoy spawned through the `task` tool.
///
/// These are forwarded from the child agent back to the parent harness so that
/// the TUI can render nested tool steps and streaming output inside the parent
/// tool step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum EnvoyEvent {
    /// Emitted once at envoy start, carrying the bound profile's name
    /// (e.g. `"explore"`, `"plan"`, `"verify"`). Lets the TUI label the
    /// envoy by its role rather than a generic "Envoy", so a user can
    /// tell a planning envoy from a research one at a glance.
    Started { profile: String },
    /// A user-visible notice from the envoy.
    Notice(AgentNotice),
    /// The envoy started a new response stream. `round`/`turn` carry the
    /// envoy's own ReAct position (1-indexed user round, 0-indexed
    /// model-request position within it, mirroring
    /// [`AgentEvent::ModelRequestStarted`]) so the TUI can stamp the child
    /// message and group the zoomed envoy view into turn bands exactly like
    /// the main session view.
    StreamStart { round: u64, turn: usize },
    /// New text token from the envoy.
    StreamDelta(String),
    /// The envoy response stream finished with the final accumulated text.
    StreamEnd(String),
    /// The envoy started a reasoning (thinking) stream. `round`/`turn`
    /// identify the envoy's own ReAct position (see
    /// [`EnvoyEvent::StreamStart`]) so the child thinking trace joins the
    /// same turn band as its sibling assistant text and tool calls. Emitted
    /// before the first [`EnvoyEvent::StreamReasoningDelta`] of a trace, so
    /// frontends can place the trace without waiting for content.
    ///
    /// This closes a visibility gap, not a new capability: the envoy's
    /// reasoning is already captured in its persisted transcript
    /// (`Message::reasoning_content`) and renders after a session reload —
    /// but before these events it was invisible while the envoy was actually
    /// running, because the child's `AgentEvent::ReasoningDelta` had no
    /// forwarding arm. The design principle is that no agent behaviour is
    /// hidden from the user: what the principal discloses live, an envoy
    /// discloses live too.
    StreamReasoningStart { round: u64, turn: usize },
    /// New reasoning token from the envoy (a disclosed chain only — the
    /// sender gates hidden-chain models out at the source; see
    /// [`crate::ThinkingSupport::chain_disclosed`]).
    StreamReasoningDelta(String),
    /// The envoy's reasoning stream finished with the final accumulated
    /// reasoning text.
    StreamReasoningEnd(String),
    /// The envoy invoked a tool. `round`/`turn` identify the envoy's own
    /// ReAct position (see [`EnvoyEvent::StreamStart`]) so the child tool
    /// step joins the same turn band as its sibling calls.
    ToolCall {
        id: String,
        name: String,
        arguments: String,
        round: u64,
        turn: usize,
    },
    /// A tool invoked by the envoy returned a result.
    ToolResult {
        id: String,
        name: String,
        output: String,
        duration_ms: u64,
    },
    /// A status update from the envoy.
    Activity(String),
    /// The envoy's permission broker surfaced a write/execute tool call
    /// that needs a human decision. Full-duplex (ADR-0029): this carries the
    /// request *up* to the parent harness so the user can answer it; the
    /// reply travels back *down* through the envoy handle's
    /// `reply_permission` (resolving the parked oneshot directly), unblocking
    /// the envoy's pending tool. Only fires when
    /// the envoy's profile does not suppress the broker (e.g. via
    /// `autopilot`) — a read-only profile never produces one.
    PermissionRequest(PermissionRequest),
    /// The envoy called `ask_user` and is blocked awaiting answers.
    /// Full-duplex (ADR-0029): carries the questions *up*; the reply travels
    /// back *down* through the envoy handle's `reply_user_question`. Only
    /// fires for profiles with `allow_user_interaction: true`.
    UserQuestionRequest(UserQuestionRequest),
    /// The envoy's `bash` tool classified a command interactive and needs
    /// operator input (L3.5 β). Carries the request *up*; the reply travels
    /// back *down* through the envoy handle's `reply_input`.
    InputRequest(InputRequest),
}

/// Steering operations a parent can submit into a running agent's inbox — the
/// down-direction of full-duplex (ADR-0029). Distinct from the request/reply
/// class ([`PermissionRequest`] / [`UserQuestionRequest`]), which resolve
/// instantly via the agent's shared-state oneshots (`reply_permission` /
/// `reply_user_question`) and therefore do **not** flow through this queue: a
/// reply must unblock a tool that is parked mid-turn, so it cannot wait for
/// the driver loop to drain. This enum covers only the "new input / control"
/// class that is safe to apply at the next ReAct-turn boundary.
///
/// Modeled on codex's `Op` (`codex-rs/protocol/src/protocol.rs`), trimmed to
/// neenee's driver shape: the agent owns an `mpsc` inbox whose receiver is
/// drained at the top of every ReAct turn (and, for `Interrupt`, raced against
/// the live stream). The top-level agent and spawned envoys share the same
/// `Op` vocabulary — an envoy is just an agent whose inbox sender the
/// parent holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentOp {
    /// Append a visible user message to the live transcript before the next
    /// model request, as if the user typed it. Lets a parent (or, for a
    /// envoy, the orchestrating agent) steer a running round with new
    /// information without restarting it. codex `inject_if_running` analogue.
    InjectUserMessage(String),
    /// Append a hidden (system-level) steering note — like
    /// [`AgentOp::InjectUserMessage`] but recorded as a hidden user message so
    /// it informs the model without polluting the visible transcript. codex
    /// `InterAgentCommunication` analogue.
    InterAgentMessage { msg: String },
    /// Abort the current round at the next boundary. Coarser than the parent's
    /// `CancellationToken` (which cancels instantly): this is the
    /// handle-addressable path for a caller that owns the inbox but not the
    /// cancel token. codex `Op::Interrupt` analogue.
    Interrupt,
    /// Tear the agent down (interrupt + signal that the shutdown was
    /// requested rather than cancelled). codex `Op::Shutdown` analogue.
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    Notice(AgentNotice),
    ModelRequestStarted {
        /// 1-indexed enclosing user round.
        round: u64,
        /// 0-indexed model-request position within `round`.
        turn: usize,
        /// Semantic estimate of the exact request projection after turn-start
        /// hooks and immediately before it is sent to the provider.
        context_tokens: usize,
    },
    /// Provider-reported context after a completed request. This supersedes the
    /// pre-request projection for that session until its context mutates again.
    ContextTokens(ContextTokenSnapshot),
    /// A user-authored insert was atomically admitted at a safe turn boundary.
    UserInputInserted(QueuedUserInput),
    AssistantDelta {
        delta: String,
        start: bool,
    },
    AssistantEnd(String),
    AssistantDiscard,
    ReasoningDelta {
        delta: String,
        start: bool,
    },
    ReasoningEnd(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        id: String,
        name: String,
        output: String,
        structured: ToolOutput,
        duration_ms: u64,
    },
    /// Incremental output streamed by a running tool (see [`ToolStream`]).
    ToolStream {
        id: String,
        stream: ToolStream,
    },
    ToolCancelled {
        id: String,
        name: String,
    },
    /// The task list changed (`todo` / `todo_update`). The TUI uses this to refresh the
    /// unified sticky panel above the input box.
    TodosUpdated(crate::todos::TodoList),
    /// The autopilot toggle changed (via `/autopilot`).
    AutopilotChanged(bool),
    PermissionRequest(PermissionRequest),
    UserQuestionRequest(UserQuestionRequest),
    /// An interactive `bash` command needs a line of input from the operator
    /// (L3.5 β). The TUI shows an inline input panel; the reply travels back
    /// as [`AgentRequest::InputReply`].
    InputRequest(InputRequest),
    /// An envoy spawned by a tool (e.g. `task`) emitted an event.
    Envoy {
        parent_call_id: String,
        event: EnvoyEvent,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum PermissionDecision {
    Once,
    Always,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct PermissionRequest {
    pub id: String,
    pub tool: String,
    /// Short human-friendly title for the prompt (e.g. `"Run tests"`).
    /// Falls back to [`tool`](Self::tool) when a tool does not override
    /// `Tool::permission_label`. The TUI renders this as the header.
    #[serde(default)]
    pub label: String,
    /// User-facing description shown in the prompt's "Details" section.
    /// Populated from `Tool::permission_description`, distinct from the
    /// model-facing `Tool::description`.
    pub description: String,
    pub arguments: String,
    pub scope: String,
    /// Whether this call is **outside** the agent's granted `OperationScope`
    /// — an elevation the user, not a builtin limit, is being asked to grant
    /// (the soft scope-gate, ADR-0028). The TUI renders such prompts with a
    /// distinct ⚠ treatment so the operator understands they are authorising
    /// access *beyond* the configured boundary, not a routine in-scope call.
    /// `false` for ordinary broker prompts and bash-policy confirms.
    #[serde(default)]
    pub elevation: bool,
    /// Whether the decision is **one-off only**: an `Always` reply is *not*
    /// persisted, and the TUI suppresses the "Always allow" option for such
    /// prompts. Set by the bash-policy confirm gate — a dangerous-command
    /// confirmation must stay one-off unless the user writes an explicit
    /// `[bash_policy.rules] action = "allow"` override. `false` (i.e.
    /// "Always" is honoured) for ordinary broker prompts.
    #[serde(default)]
    pub one_off: bool,
}

/// One option offered to the user inside an `ask_user` question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
// `description` skips serialization when `None`: absent on the wire, never `null`.
#[ts(optional_fields, export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct UserQuestionOption {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A single question inside an `ask_user` tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
// `header` skips serialization when `None`: absent on the wire, never `null`.
#[ts(optional_fields, export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct UserQuestion {
    /// Short label shown as a chip/tag above the question (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// The full question text.
    pub question: String,
    /// Available choices. Must contain at least one option.
    pub options: Vec<UserQuestionOption>,
    /// Whether the user may select more than one option.
    #[serde(default)]
    pub multi_select: bool,
}

/// Request sent from the agent to the TUI when the model calls `ask_user`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct UserQuestionRequest {
    pub id: String,
    pub questions: Vec<UserQuestion>,
}

/// Reply sent from the TUI back to the agent after the user answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserQuestionReply {
    pub request_id: String,
    /// One array of selected option labels per question.
    pub answers: Vec<Vec<String>>,
}

/// Request sent from the agent to the TUI when a `bash` command is classified
/// interactive and needs a line of input the agent cannot supply itself
/// (L3.5 β — the default human-input path). The TUI shows an inline input
/// panel; the operator's reply is sent back as an [`InputReply`]. If the
/// operator dismisses it (Esc), an empty reply cancels the command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct InputRequest {
    pub id: String,
    /// The command that needs input, shown for context.
    pub command: String,
    /// A human-readable prompt describing what to enter (e.g. "sudo password",
    /// "passphrase", "confirmation").
    pub prompt: String,
    /// Whether to mask the typed input (passwords/passphrases).
    pub secret: bool,
}

/// Reply sent from the TUI back to the agent carrying the operator's input.
/// An empty `text` signals cancellation (the command runs with closed stdin
/// and fails fast with a non-interactive remedy hint).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputReply {
    pub request_id: String,
    pub text: String,
}

/// A complete, render-ready picture of the live session, sent from the harness
/// to the TUI for the session-context modal. Every pane in that modal reads
/// from this one snapshot, so opening the modal and any mutation
/// (revoke / toggle) only needs a single request/response round-trip rather
/// than one per pane. Built by the harness from its own state (provider,
/// tools, permissions, skills) plus the MCP load result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionContextSnapshot {
    pub model: ModelInfo,
    pub tools: Vec<ToolInfo>,
    pub permissions: Vec<PermissionRuleInfo>,
    pub skills: Vec<SkillInfo>,
    pub mcp: Vec<McpServerInfo>,
}

/// Model-side pane of [`SessionContextSnapshot`]. `capabilities` carries
/// heuristic hints (e.g. "tool calling", "reasoning") since per-model
/// capability data is not yet modeled in the catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider: String,
    pub model: String,
    pub display_name: String,
    pub context_window: usize,
    pub api_key_ready: bool,
    pub description: String,
    pub capabilities: Vec<String>,
}

/// One tool in the session, as seen by the modal's Tools pane. `source`
/// classifies origin: `builtin`, `mcp:<server>`, or `plan`. `enabled`
/// reflects the session-level enable/disable flag (toggled via
/// [`AgentRequest::ToggleTool`]); disabled tools stay installed but are hidden
/// from the model and rejected if invoked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub source: String,
}

/// One cached "always allow" permission rule, shown in the modal's Permissions
/// pane where it can be revoked individually via [`AgentRequest::RevokePermission`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PermissionRuleInfo {
    pub tool: String,
    pub scope: String,
}

/// One skill in the registry, shown in the modal's Skills pane. `source` is the
/// [`SkillScope`](../neenee_skills/enum.SkillScope.html) display string
/// (system / remote / user / extra / repo).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub enabled: bool,
    pub source: String,
    pub tags: Vec<String>,
}

/// One MCP server, shown in the modal's MCP pane. The connection tri-state
/// (connected / disabled / failed) is unpacked from
/// [`crate::mcp::McpConnectionStatus`] so the DTO stays decoupled from the
/// enum, and `tool_names` carries the per-server tool list that the hint bar
/// collapses to a mere count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub connected: bool,
    pub disabled: bool,
    pub failure: Option<String>,
    pub tool_names: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_summary_tps_excludes_human_pause() {
        // Fallback path (no generation_ms recorded): 500 output tokens over
        // 10s wall-clock, 8s of which the user spent deliberating on a
        // permission prompt → only 2s of active time. TPS = 250.
        let summary = RoundSummary {
            round: 3,
            output_tokens: 500,
            duration_ms: 10_000,
            paused_ms: 8_000,
            generation_ms: 0,
        };
        assert_eq!(summary.active_ms(), 2_000);
        assert!(
            (summary.tps() - 250.0).abs() < 0.01,
            "got {}",
            summary.tps()
        );
    }

    #[test]
    fn round_summary_tps_uses_generation_time_excluding_tools() {
        // A round that streamed 500 output tokens in 2s of real generation,
        // then spent 30s executing tools, then 8s parked on a permission.
        // Only the 2s of generation counts: TPS = 250, NOT ~12.5 (round
        // active_ms) and NOT ~3.1 (wall-clock).
        let summary = RoundSummary {
            round: 3,
            output_tokens: 500,
            duration_ms: 40_000,
            paused_ms: 8_000,
            generation_ms: 2_000,
        };
        assert_eq!(summary.active_ms(), 32_000);
        assert!(
            (summary.tps() - 250.0).abs() < 0.01,
            "got {}",
            summary.tps()
        );
    }

    #[test]
    fn round_summary_tps_is_zero_when_round_had_no_active_time() {
        let summary = RoundSummary {
            round: 1,
            output_tokens: 0,
            duration_ms: 0,
            paused_ms: 0,
            generation_ms: 0,
        };
        assert_eq!(summary.active_ms(), 0);
        assert_eq!(summary.tps(), 0.0);
    }

    #[test]
    fn round_summary_active_time_saturates_when_pause_exceeds_duration() {
        // Defensive: a round whose recorded pause exceeds its wall-clock
        // (shouldn't happen, but must never panic on subtraction) yields zero
        // active time rather than a negative.
        let summary = RoundSummary {
            round: 1,
            output_tokens: 100,
            duration_ms: 1_000,
            paused_ms: 2_000,
            generation_ms: 0,
        };
        assert_eq!(summary.active_ms(), 0);
        assert_eq!(summary.tps(), 0.0);
    }

    #[test]
    fn command_ack_notice_is_toast_surfaced_info_from_harness() {
        // A slash-command reply (the *acknowledgment*, not the invocation) is
        // stamped uniformly so frontends can branch on kind + surface:
        //   - severity Info (it is a status confirmation, not an error),
        //   - surface Toast (transient bubble, never appended to transcript),
        //   - source Harness (not the agent / a tool).
        let notice = AgentNotice::command_ack("Autopilot ON: …");
        assert_eq!(notice.kind, NoticeKind::CommandAck);
        assert_eq!(notice.severity, NoticeSeverity::Info);
        assert_eq!(notice.surface, NoticeSurface::Toast);
        assert_eq!(notice.source, NoticeSource::Harness);
        assert_eq!(notice.title, "Autopilot ON: …");
        assert!(notice.body.is_none());
    }

    #[test]
    fn command_ack_kind_serialises_as_snake_case() {
        // The closed NoticeKind classifier must serialise the new variant
        // distinctly so frontends (and persisted/forwarded notices) cannot
        // confuse it with the existing kinds.
        let notice = AgentNotice::command_ack("x");
        let json = serde_json::to_string(&notice.kind).expect("serialise");
        assert_eq!(json, "\"command_ack\"");
    }

    #[test]
    fn command_ack_kind_round_trips() {
        let notice = AgentNotice::command_ack("x");
        let json = serde_json::to_string(&notice).expect("serialise");
        let back: AgentNotice = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.kind, NoticeKind::CommandAck);
        assert_eq!(back.surface, NoticeSurface::Toast);
    }
}
