//! User configuration schema and persistence.
//!
//! Deserializes/serializes the TOML config file (`master`, `tui`, providers,
//! channels, MCP servers, hooks, skills, web-search) via [`crate::fsutil`]'s
//! atomic-write helpers, and loads/saves the input history. Config is state
//! (recency-merged under a companion file lock, ADR-0018); the live
//! provider/model selection telemetry lives in [`crate::provider_usage`].

use crate::fsutil;
use crate::paths;
use muta_contracts::{
    CompactionPolicy, DoomGuardConfig, HookEventKind, McpServerConfig, RemoteModelMetadata,
    SecretString, SkillsConfig, VariantSelection, WebSearchConfig,
};

/// Re-export so server/TUI can use the config-layer path without depending on
/// core's auth module name directly for `AddProvider`.
pub use muta_contracts::ChannelAuth as ConfigChannelAuth;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;

/// Reserved `[tui.default_expanded]` key that controls reasoning traces.
/// Reasoning isn't a tool, so each frontend addresses it by name.
pub const THINKING_KEY: &str = "thinking";

/// User-tunable master (top-level agent) behaviour, deserialized from the optional `[master]`
/// table of `config.toml`. All fields default sensibly, so a
/// `config.toml` with no `[master]` table (or a partially specified one)
/// is valid.
///
/// ```toml
/// [master]
/// # Hard-stop a round after this many total ReAct turns. 0 (the default)
/// # means no hard stop — an opt-in execution budget only. This is the sole
/// # per-round turn cap; the loop otherwise runs until the model stops, the user
/// # interrupts, or context compaction cannot relieve pressure (ADR-0009).
/// # hard_stop_turns = 0
///
/// # Never pop the interactive-input panel for a command needing stdin
/// # (sudo/gpg/passwd/…). Instead run it with stdin closed so it fails fast
/// # with a non-interactive remedy hint — like delegated autonomous mode, but without
/// # turning the master itself delegated.
/// # skip_interactive_input = false
///
/// # Doom-loop guard (variant-loop defense). On by default; one
/// same-signature re-run is tolerated before a block (ADR-0148). Opt out
/// here, or restore the strict first-repeat block with `threshold = 2`.
/// See [`DoomGuardConfig`]. The historical `nudge` key spelling still
/// loads; saves write `doom_guard`.
/// # [master.doom_guard]
/// # enabled = false
/// # threshold = 2
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MasterConfig {
    /// Opt-in hard-stop budget: abort a round after this many ReAct turns.
    /// `0` (the default) means uncapped. Mutated at runtime via
    /// `Agent::set_hard_stop_turns`.
    pub hard_stop_turns: usize,
    /// Whether the model may supply stdin bytes for a `bash` command it emits
    /// (the opt-in "automatic flow" path, L3.5 α). Default `false`: the bash
    /// tool schema exposes no `stdin` parameter and a command that needs input
    /// either gets it from a human (interactive-classifier → input panel) or
    /// fails fast with a non-interactive remedy hint. When `true`, the bash
    /// schema **dynamically** adds a `stdin` field the model can fill, and the
    /// dispatch layer threads it through as `StdinPolicy::Prefilled`. This
    /// is the explicit authorization that "input may come from the model" —
    /// without it, stdin is structurally unreachable from the model's
    /// arguments. Wired through `Agent::set_allow_model_stdin`.
    pub allow_model_stdin: bool,
    /// Whether an interactive `bash` command (one the interactive classifier
    /// matches: `sudo`/`gpg`/`passwd`/TUI editors/`read`/…) should **never**
    /// pop the inline input panel and instead run with stdin closed.
    ///
    /// Default `false`: a command needing input prompts the operator via the
    /// input-injection panel (with the command + a masked/plain field). When
    /// `true`, the panel is skipped — the command runs non-interactively,
    /// reads EOF immediately, and fails fast with a non-interactive remedy
    /// hint, exactly as it would in delegated autonomous mode. This is the right
    /// setting for users who find the prompt disruptive and prefer to retry
    /// the command themselves (or let the model retry with a non-interactive
    /// form). Wired through `Agent::set_skip_interactive_input`.
    ///
    /// Note: this only governs the *interactive-input* path; it does not turn
    /// the master delegated, so ordinary tool confirmations still apply.
    pub skip_interactive_input: bool,
    /// ADR-0141: how an autonomous session (no human channel attached —
    /// piped headless, CI, cron) settles an `ask_user` question. Wire
    /// format: `"fail_closed"` (default) or `"recommended_labeled"`.
    /// Fail-closed refuses the question and tells the model to resolve the
    /// ambiguity itself; recommended-labeled answers each question with
    /// its first (recommended) option, with an explicit
    /// `[answered by policy, not by user]` label so the model cannot
    /// mistake the recommendation for a human decision.
    #[serde(default)]
    pub ask_user_fallback: muta_contracts::human_request::AutonomousFallbackPolicy,
    /// Doom-loop guard configuration (`muta_agent::doom_guard`). Default
    /// **enabled** (`window: 16`, `threshold: 3` — ADR-0113 §5 flipped it
    /// on, ADR-0148 relaxed the trip point) — opt out via
    /// `[master.doom_guard] enabled = false`, or restore the strict
    /// first-repeat block with `threshold = 2`. See [`DoomGuardConfig`]
    /// for the per-field semantics.
    #[serde(default, alias = "nudge")]
    pub doom_guard: DoomGuardConfig,
}

// `DoomGuardConfig` is defined in `muta_contracts::doom_guard_config` and re-exported
// above via `use muta_contracts::DoomGuardConfig`. It is the `[master.doom_guard]`
// TOML table and the wire type for `AgentRequest::UpdateDoomGuardConfig`. See
// `muta_contracts::DoomGuardConfig` for the per-field semantics and defaults.

/// Declarative permission configuration — the `[permissions]` table. Lets users
/// pre-declare "always allow" rules in `config.toml` so default policies are
/// data-driven, not purely interactive:
///
/// ```toml
/// [[permissions.allow]]
/// tool = "execute_command"
/// scope = "*"
///
/// [[permissions.allow]]
/// tool = "read_text"
/// scope = "*"
/// ```
///
/// These seed the allowlist at startup; runtime "Always" decisions still write
/// to the persisted `permissions.json`. A config rule with scope `"*"` allows
/// every call to that tool without prompting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionConfig {
    /// Rules to pre-seed the "always allow" allowlist at startup.
    pub allow: Vec<PermissionRuleConfig>,
}

/// User-owned filesystem admission policy for the active workspace.
///
/// This table lives in the global `config.toml`, not in project-authored
/// `.muta/config.toml`: repository content must never be able to widen its own
/// filesystem boundary. Relative entries are resolved from the active
/// workspace root.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    /// Additional directory roots that native file tools may access.
    pub additional_roots: Vec<String>,
}

/// Safety policy for model-issued `bash` commands. Built-in dangerous-command
/// rules are compiled into the agent so the config only contains user choices:
/// toggles and project-local overrides/additions.
///
/// ```toml
/// [bash_policy]
/// enabled = true
///
/// [[bash_policy.rules]]
/// name = "deny git reset hard"
/// match = "regex"
/// pattern = '(?i)\bgit\s+reset\s+--hard\b'
/// action = "deny"
/// reason = "This project never allows git reset --hard from the agent."
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BashPolicyConfig {
    /// Master switch for the bash policy guard. Defaults to `true` so dangerous
    /// built-in commands are protected even when the user has broadly allowed
    /// the `bash` tool.
    pub enabled: bool,
    /// Whether an explicit user `allow` rule may override a compiled-in `deny`
    /// rule. Defaults to `false`; user `allow` rules can still override
    /// compiled-in `confirm` rules.
    pub allow_user_override_builtin_deny: bool,
    /// Project/user-defined rules. Evaluated before built-in `confirm` rules,
    /// but built-in `deny` rules remain a hard floor unless
    /// `allow_user_override_builtin_deny` is set.
    pub rules: Vec<BashPolicyRuleConfig>,
}

impl Default for BashPolicyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_user_override_builtin_deny: false,
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BashPolicyRuleConfig {
    /// Human-readable rule name shown in policy decisions.
    pub name: String,
    /// Matcher type. TOML uses `match = "regex"`; `matcher` is accepted as an
    /// alias for callers that avoid the keyword-like field name.
    #[serde(rename = "match", alias = "matcher")]
    pub matcher: BashPolicyMatcherConfig,
    /// Pattern consumed by the selected matcher.
    pub pattern: String,
    /// Decision to apply when this rule matches.
    pub action: BashPolicyActionConfig,
    /// Optional user-facing explanation.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashPolicyMatcherConfig {
    /// Rust `regex` matched against the full shell command string.
    #[default]
    Regex,
    /// Case-sensitive substring match against the full command string.
    Contains,
    /// Case-sensitive prefix match after trimming leading whitespace.
    StartsWith,
    /// Match the leading program name (after leading env assignments), e.g.
    /// `git`, `cargo`, `kubectl`.
    Program,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashPolicyActionConfig {
    /// Let the command proceed to the normal permission broker.
    Allow,
    /// Require a one-off human confirmation, ignoring any broad `bash *` allow.
    Confirm,
    /// Refuse the command before spawning a shell.
    #[default]
    Deny,
}

/// One declarative permission rule from `[permissions]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRuleConfig {
    /// Tool name (e.g. `"execute_command"`, `"read_text"`, `"mcp__fs__read"`).
    pub tool: String,
    /// Permission scope. `"*"` matches every call to the tool. Any other value
    /// must match the call's scope *exactly* (e.g. a full path, or the exact
    /// command string for `bash`) — there is no prefix/substring matching.
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "*".to_string()
}

/// `Provider` implementation the catalog builds. Mirrors the built-in
/// `muta_contracts::catalog::Transport` variants but stays a plain serializable
/// enum so it round-trips through TOML.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum UserTransport {
    #[default]
    #[serde(alias = "OpenAiCompat", alias = "openai")]
    OpenAi,
    /// OpenAI **Responses** API (`/responses` endpoint) over an ordinary API
    /// key — e.g. DeepSeek V4's native surface. Distinct from
    /// [`OpenAi`](Self::OpenAi) (chat completions) in transport only. OAuth
    /// Responses channels (ChatGPT) resolve their transport from
    /// [`muta_contracts::ChannelAuth`] instead.
    #[serde(alias = "openai-responses", alias = "openai_responses")]
    OpenAiResponses,
    /// Anthropic-compatible `/messages` endpoint. Used by opencode-go's
    /// MiniMax/Qwen models and any Anthropic-format relay.
    Anthropic,
    /// Google native API — speaks the official `/v1beta` REST surface
    /// (`generateContent`/`streamGenerateContent`). Use for Google's own API or
    /// a relay that forwards model ids verbatim.
    #[serde(alias = "GeminiNative", alias = "gemini")]
    Google,
}

/// Capability metadata fitted from a provider's live `GET /models` response
/// for one model id the client registry does not know. Persisted in the
/// discovery cache (`models_discovery.json`) so the metadata survives
/// restarts: live discovery refreshes it in the background, and a failed
/// fetch leaves the last good values in place. Only instances created from a
/// fitting-enabled template (trusted official endpoints) ever carry this.
/// See `muta_contracts::model::FittedModel`.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct FittedModelInfo {
    /// Advertised context window in tokens (`0` = the endpoint did not say).
    #[serde(default)]
    pub context_window: usize,
    /// The endpoint advertises reasoning (e.g. a `reasoning_content` stream).
    #[serde(default)]
    pub reasoning: bool,
    /// The endpoint advertises image inputs.
    #[serde(default)]
    pub vision: bool,
    /// Advertised reasoning-effort tiers, as named by the provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
}

/// Per-(instance, model) reasoning overrides — the user's own per-route
/// choices (set from the model `e` editor), persisted in **state** via
/// [`crate::route_settings::RouteSettingsStore`] (keyed
/// `providers[<instance_id>][<model_id>]`). Unlike the *derived* capability
/// fields ([`FittedModelInfo`]), these are not rebuildable: the entry's
/// presence opts the model in to reasoning on Anthropic-protocol routes,
/// `thinking` defaulting **on** unless explicitly `false` (ADR-0046).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteSettings {
    /// Reasoning depth: `"none"`/`"minimal"`/`"low"`/`"medium"`/`"high"`/
    /// `"xhigh"`/`"max"`, clamped at request time to the model's levels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Whether extended thinking is on (`true`) or off (`false`) once the
    /// route is opted in. Defaults to on when the entry exists; set `false`
    /// to reason with depth only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    /// Explicit capability overrides -- the **top layer** of the capability
    /// resolution order (ADR-0149). `None`/empty means "no opinion": the
    /// effective capabilities fall through to remote metadata, then the
    /// static baseline. Unlike the derived `FittedModelInfo`, these are the
    /// user's own per-route choices and are never rebuilt from an endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_overrides: Option<muta_contracts::CapabilityOverrides>,
}

impl RouteSettings {
    /// Whether the entry carries any explicit knob. An entry with neither
    /// field set still opts the model in to thinking on Anthropic routes.
    /// Capability overrides (ADR-0149 layer 1) count as a knob: a record that
    /// only carries them must not be pruned as empty.
    pub fn is_empty(&self) -> bool {
        self.effort.is_none()
            && self.thinking.is_none()
            && self
                .capability_overrides
                .as_ref()
                .is_none_or(muta_contracts::CapabilityOverrides::is_empty)
    }
}

/// Provider API keys split out of `config.toml` into their own
/// `credentials.toml` (written `rw-------` via [`crate::fsutil`]). This is the
/// **secret** half of provider configuration: `config.toml` holds the
/// *behavior*, `providers.toml` the instance *declarations*, and this file the
/// *keys* — so the other two files can be shared, screenshotted, or
/// version-controlled without leaking credentials.
///
/// Credentials are keyed by **provider instance**, never by route — a route
/// is a derived model path, not a security master. One instance has
/// exactly one API-key credential:
///
/// ```toml
/// [providers]
/// deepseek = "sk-..."
/// ```
///
/// OAuth logins do **not** live here; their access/refresh token sets are
/// runtime state in `auth.toml` (`[tokens.<provider>]`), also keyed by
/// provider instance.
///
/// Resolution precedence is **`api_key_env` env var > credentials.toml**: the
/// instance declares an optional `api_key_env` (a variable *name*) in
/// `providers.toml`; the catalog resolves env-first, then this file. The map
/// is a `BTreeMap` for stable, diff-friendly serialisation. Unknown tables
/// (e.g. the pre-refactor `[builtins.<id>]` / `[user.<id>]` sections) are
/// tolerated and ignored so a not-yet-migrated file keeps loading.
/// Credentials split out of `config.toml` into their own `credentials.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Credentials {
    /// API keys keyed by connection id.
    #[serde(default, alias = "providers")]
    pub connections: BTreeMap<String, SecretString>,
    /// The web-tool API keys (`[websearch]`): search backends + the Jina
    /// reader. Kept here — not in `config.toml`'s `[websearch]` — so
    /// `config.toml` stays behavior-only and shareable. Merged into
    /// [`Config::websearch`] at load time by [`Config::load`].
    #[serde(default, skip_serializing_if = "WebSearchKeys::is_empty")]
    pub websearch: WebSearchKeys,
}

/// The six web-tool API keys, persisted as `credentials.toml [websearch]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebSearchKeys {
    pub exa_api_key: Option<SecretString>,
    pub parallel_api_key: Option<SecretString>,
    pub tavily_api_key: Option<SecretString>,
    pub bocha_api_key: Option<SecretString>,
    pub jina_api_key: Option<SecretString>,
}

impl WebSearchKeys {
    /// Whether any key is set — drives `skip_serializing_if` so a clean
    /// credentials file does not grow an empty table.
    pub fn is_empty(&self) -> bool {
        self.exa_api_key.is_none()
            && self.parallel_api_key.is_none()
            && self.tavily_api_key.is_none()
            && self.bocha_api_key.is_none()
            && self.jina_api_key.is_none()
    }

    /// Overlay onto a `[websearch]` config table: a key set here wins, an
    /// absent one leaves whatever the config already carries (which after
    /// migration is always `None`).
    fn merge_into(self, websearch: &mut muta_contracts::WebSearchConfig) {
        if self.exa_api_key.is_some() {
            websearch.exa_api_key = self.exa_api_key;
        }
        if self.parallel_api_key.is_some() {
            websearch.parallel_api_key = self.parallel_api_key;
        }
        if self.tavily_api_key.is_some() {
            websearch.tavily_api_key = self.tavily_api_key;
        }
        if self.bocha_api_key.is_some() {
            websearch.bocha_api_key = self.bocha_api_key;
        }
        if self.jina_api_key.is_some() {
            websearch.jina_api_key = self.jina_api_key;
        }
    }

    /// Fill any unset key from `other` (used when folding the historical
    /// `config.toml` location into the credentials file).
    fn absorb(&mut self, other: WebSearchKeys) {
        if self.exa_api_key.is_none() {
            self.exa_api_key = other.exa_api_key;
        }
        if self.parallel_api_key.is_none() {
            self.parallel_api_key = other.parallel_api_key;
        }
        if self.tavily_api_key.is_none() {
            self.tavily_api_key = other.tavily_api_key;
        }
        if self.bocha_api_key.is_none() {
            self.bocha_api_key = other.bocha_api_key;
        }
        if self.jina_api_key.is_none() {
            self.jina_api_key = other.jina_api_key;
        }
    }
}

impl Credentials {
    fn path() -> PathBuf {
        paths::get().credentials_file()
    }

    /// Read `credentials.toml`, returning an empty (not erroring) value when
    /// the file is missing or unparseable.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(content) = fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str(&content) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not parse credentials file; ignoring",
                );
                Self::default()
            }
        }
    }

    /// Persist atomically with owner-only permissions (0600) via
    /// [`crate::fsutil::atomic_write_bytes`].
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let bytes = toml::to_string_pretty(self)?.into_bytes();
        fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    /// The credential for `connection_id`, if set and non-empty.
    pub fn api_key(&self, connection_id: &str) -> Option<&SecretString> {
        self.connections
            .get(connection_id)
            .filter(|k| !k.expose_secret().trim().is_empty())
    }

    /// Replace the whole `[websearch]` key table. Serialization already
    /// skips an empty table, so a cleared configuration never grows an empty
    /// `[websearch]` section in `credentials.toml`.
    pub fn set_websearch_keys(&mut self, keys: WebSearchKeys) {
        self.websearch = keys;
    }

    /// Set (or clear) the credential for `connection_id`.
    pub fn set_api_key(&mut self, connection_id: &str, key: Option<SecretString>) {
        match key {
            Some(key) if !key.expose_secret().trim().is_empty() => {
                self.connections.insert(connection_id.to_string(), key);
            }
            _ => {
                self.connections.remove(connection_id);
            }
        }
    }

    /// Remove the credential for `connection_id`, if any.
    pub fn remove_api_key(&mut self, connection_id: &str) {
        self.connections.remove(connection_id);
    }
}

/// Discovered model lists and fitted capabilities, cached under
/// `$XDG_CACHE_HOME/muta/models_discovery.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveryCache {
    /// Cached discovered model lists, keyed by connection id:
    /// connection_id -> model ids (in discovery order).
    #[serde(default, alias = "provider_models")]
    pub connection_models: BTreeMap<String, Vec<String>>,
    /// Fitted capability metadata, keyed by connection id then model id.
    #[serde(default)]
    pub fitted_models: BTreeMap<String, BTreeMap<String, FittedModelInfo>>,
    /// Trusted per-(connection, model) capability metadata advertised by the
    /// connection's live `GET /models` (endpoint, thinking, effort tiers …).
    #[serde(default)]
    pub remote_metadata: BTreeMap<String, BTreeMap<String, RemoteModelMetadata>>,
    /// Revalidation metadata for live catalogs, keyed by connection id. The
    /// actual model/capability payload remains in the maps above so older cache
    /// files continue to deserialize without migration.
    #[serde(default)]
    pub model_lists: BTreeMap<String, ModelListCacheState>,
}

/// Freshness and validator state for one connection's remote model catalog.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelListCacheState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default)]
    pub client_version: String,
    #[serde(default)]
    pub refreshed_at_ms: i64,
}

impl DiscoveryCache {
    fn path() -> PathBuf {
        paths::get().discovery_cache_file()
    }

    /// Read `models_discovery.json`, returning an empty value if missing or unparseable.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(content) = fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    /// Persist atomically to `$XDG_CACHE_HOME/muta/models_discovery.json`.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let bytes = serde_json::to_vec_pretty(self)?;
        fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    /// Remove the per-connection records for `connection_id` (used on connection deletion).
    pub fn remove_connection(&mut self, connection_id: &str) {
        self.connection_models.remove(connection_id);
        self.fitted_models.remove(connection_id);
        self.remote_metadata.remove(connection_id);
        self.model_lists.remove(connection_id);
    }

    /// The trusted per-(connection, model) metadata, if set.
    pub fn remote_metadata_for(
        &self,
        connection_id: &str,
        model_id: &str,
    ) -> Option<&RemoteModelMetadata> {
        self.remote_metadata
            .get(connection_id)
            .and_then(|models| models.get(model_id))
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct Config {
    #[serde(alias = "default_provider")]
    pub default_connection: String,
    pub mcp: HashMap<String, McpServerConfig>,
    /// Context-compaction thresholds and relief policies. See
    /// [`CompactionPolicy`] for the per-field semantics.
    pub compaction: CompactionPolicy,
    /// Maximum number of attempts for a single model request when the connection returns a
    /// transient error (HTTP 408/429/5xx, connection, timeout). The initial try
    /// counts as the first attempt, so this is the *total* attempts, not extra
    /// retries. Clamped to `[1, 60]` at the call site.
    #[serde(alias = "provider_retry_max_attempts")]
    pub connection_retry_max_attempts: usize,
    /// Base delay (ms) for the bounded exponential backoff between retries:
    /// `base_ms * 2^(attempt-1)`, capped by `connection_retry_max_ms`.
    #[serde(alias = "provider_retry_base_ms")]
    pub connection_retry_base_ms: u64,
    /// Hard cap (ms) on a single backoff delay, including the exponential growth.
    /// A server-supplied `Retry-After`/`retry-after-ms` header still wins but is
    /// itself capped at this value.
    #[serde(alias = "provider_retry_max_ms")]
    pub connection_retry_max_ms: u64,
    /// The model id to use within the active connection. For single-model
    /// connections this mirrors the connection's pinned model; for multi-model
    /// connections (opencode-go) it selects which of the connection's models is
    /// active. `None` falls back to the connection's default model.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Favorite model ids for quick access in the Models picker (ADR-0046
    /// moved favorite from provider-level to per-model). Stored as a flat list
    /// of model wire ids; a starred daily-driver model sorts to the top of the
    /// flat list wherever it is served.
    #[serde(default)]
    pub favorites: Vec<String>,
    /// Model ids or glob patterns (e.g. `"gemini-3.6-flash*"`) to hide from
    /// model pickers across all connections.
    #[serde(default)]
    pub hidden_models: Vec<String>,
    /// Skill configuration (`[skills]` table).
    #[serde(default)]
    pub skills: SkillsConfig,
    /// Declarative permission rules (`[permissions]` table). Each entry is a
    /// `[[permissions.allow]]` rule (`tool` + `scope`) pre-seeded into the
    /// allowlist at startup, so default policies are data-driven rather than
    /// only interactive. Runtime "Always" decisions still add to the persisted
    /// `permissions.json`; these config rules are re-applied on every start.
    #[serde(default)]
    pub permissions: PermissionConfig,
    /// Filesystem roots admitted in addition to the active project root.
    /// This is user-owned global policy and is independent of project asset
    /// trust.
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    /// Bash command safety policy (`[bash_policy]` table). Built-in dangerous
    /// command rules are compiled into the agent; this config supplies only
    /// user overrides/additional rules and guard toggles.
    #[serde(default)]
    pub bash_policy: BashPolicyConfig,
    /// Web tool configuration (`[websearch]` table): search backend, proxy, timeout.
    #[serde(default)]
    pub websearch: WebSearchConfig,
    /// Master behaviour (`[master]` table): opt-in hard-stop budget and the
    /// doom-loop guard toggle. See [`MasterConfig`] for the per-field
    /// semantics and TOML examples.
    #[serde(default)]
    pub master: MasterConfig,
    /// Lifecycle event hooks (`[[hooks]]` array, ADR-0025). Each entry fires a
    /// shell command at one lifecycle point; see [`HookSpec`].
    #[serde(default)]
    pub hooks: Vec<HookSpec>,
    /// Per-model tool-variant selection (`[tool_variants."<model-id>"]`
    /// table). When talking to the named model, each listed capability is
    /// realized by the named variant instead of its default. See
    /// [`ToolVariantsConfig`].
    #[serde(default)]
    pub tool_variants: ToolVariantsConfig,
    /// Daemon lifecycle knobs (ADR-0101): the `[daemon]` table of
    /// `config.toml`. Controls how the session daemon exits — its shutdown
    /// grace budget and its idle-empty auto-exit.
    #[serde(default)]
    pub daemon: DaemonConfig,
}

/// Daemon lifecycle configuration, deserialized from the `[daemon]` table of
/// `config.toml` (ADR-0101).
///
/// ```toml
/// [daemon]
/// shutdown_grace_secs = 10      # graceful-teardown budget before forced exit
/// idle_exit_minutes = 5         # auto-exit after N minutes of zero sessions
///                                # and zero attached clients; 0 = never
/// local_auth = true             # bearer-token the loopback listener too
///                                # (ADR-0105); false = trust local processes
/// rehost_armed_schedules = true # rehost sessions with armed /schedule jobs
///                                # at boot (ADR-0125); false = cold start
/// ```
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct DaemonConfig {
    /// Total budget for graceful shutdown (listeners close → connections
    /// drain → sessions tear down with `SessionEnd` hooks). When the budget
    /// expires — or a second signal arrives — remaining tasks are aborted and
    /// the process exits anyway, so a hung external hook can never pin the
    /// daemon open. A always-on/service deployment should set this at or
    /// above the supervisor's stop timeout (e.g. systemd's `TimeoutStopSec`).
    pub shutdown_grace_secs: u64,
    /// Auto-exit after this many continuous minutes hosting **zero sessions
    /// with zero attached clients** (ADR-0100 rule 3): the daemon becomes
    /// born-on-demand, gone-when-useless. `0` disables idle exit for
    /// always-on deployments.
    pub idle_exit_minutes: u64,
    /// Require a bearer token even on the loopback TCP listener (ADR-0105).
    /// The token is generated per daemon start and published in the
    /// owner-only (0600) discovery record, so co-located CLI/TUI clients
    /// authenticate transparently while other local processes, other users
    /// on a shared machine, and drive-by browser pages cannot drive the
    /// control plane. `false` restores the pre-0105 trust-the-loopback
    /// posture; the UDS listener is always exempt (filesystem permissions
    /// are its boundary). Default: true.
    pub local_auth: bool,
    /// Rehost autonomous sessions at daemon boot (ADR-0125): scan every
    /// project's persisted sessions and re-assemble a hosted harness for
    /// each one that still has armed `/schedule` jobs, so a scheduled
    /// prompt keeps firing across daemon restarts (crash, upgrade, reboot)
    /// instead of waiting for a human to attach. Default: true. Set `false`
    /// to start cold every time (the pre-0125 behavior).
    pub rehost_armed_schedules: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            shutdown_grace_secs: 10,
            idle_exit_minutes: 5,
            local_auth: true,
            rehost_armed_schedules: true,
        }
    }
}

/// Per-model tool-variant selection, deserialized from the `[tool_variants]`
/// section of `config.toml`. Maps a model id to a `capability → variant_id`
/// map. A capability is realized by the named variant (a genuinely different
/// implementation/schema/description), not a re-worded copy of one impl.
///
/// ```toml
/// [tool_variants."glm-5.2"]       # model id (quoted: has dots)
/// read_text        = "terse"    # capability = variant id
/// execute_command  = "workspace"
/// ```
///
/// Capabilities and models not listed use their default variant.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolVariantsConfig(pub HashMap<String, ModelToolVariants>);

/// One model's variant selection: a transparent wrapper around the
/// `capability → variant_id` map so it serializes directly as a TOML table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelToolVariants(pub VariantSelection);

impl ToolVariantsConfig {
    /// Look up the variant selection for `model_id`, if any. Returns an empty
    /// map (not `None`) for unknown models so callers can always borrow
    /// `&VariantSelection`.
    pub fn for_model(&self, model_id: &str) -> &VariantSelection {
        self.0
            .get(model_id)
            .map(|m| &m.0)
            .unwrap_or_else(|| muta_contracts::empty_variant_selection())
    }
}

/// One lifecycle event hook entry (ADR-0025). Deserialized from a `[[hooks]]`
/// table in `config.toml`:
///
/// ```toml
/// [[hooks]]
/// event   = "PostToolUse"          # a [`HookEventKind`] variant
/// matcher = "Write|Edit"           # optional; tool-name `|`-list or regex
/// command = ".muta/hooks/lint.sh"
/// ```
///
/// The command receives the [`muta_contracts::HookContext`] as JSON on stdin and
/// communicates a decision via exit code / stdout JSON (see the CLI runner).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookSpec {
    /// When this hook fires.
    pub event: HookEventKind,
    /// Tool-name filter. `None` (or unset) matches every event; only tool
    /// events (`PreToolUse` / `PostToolUse` / `PostToolUseFailure`) honour it.
    #[serde(default)]
    pub matcher: Option<String>,
    /// Shell command run when the event matches. Executed with the project
    /// root as cwd and the hook context as JSON on stdin.
    pub command: String,
    /// Runtime-only origin marker. Project-defined hooks carry their exact
    /// workspace root and execute read-only/offline inside the workspace
    /// sandbox. Global user hooks leave this unset.
    #[serde(skip)]
    pub sandbox_root: Option<std::path::PathBuf>,
}

#[derive(Deserialize)]
struct RawConfig {
    #[serde(default, alias = "default_provider")]
    default_connection: Option<String>,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    mcp: Option<HashMap<String, McpServerConfig>>,
    #[serde(default)]
    compaction: Option<CompactionPolicy>,
    #[serde(default)]
    compaction_preserve_rounds: Option<usize>,
    #[serde(default)]
    compaction_summarize: Option<bool>,
    #[serde(default)]
    compaction_prune: Option<bool>,
    #[serde(default)]
    compaction_prune_protect_tokens: Option<usize>,
    #[serde(default, alias = "provider_retry_max_attempts")]
    connection_retry_max_attempts: Option<usize>,
    #[serde(default, alias = "provider_retry_base_ms")]
    connection_retry_base_ms: Option<u64>,
    #[serde(default, alias = "provider_retry_max_ms")]
    connection_retry_max_ms: Option<u64>,
    #[serde(default)]
    favorites: Option<Vec<String>>,
    #[serde(default)]
    hidden_models: Option<Vec<String>>,
    #[serde(default)]
    skills: Option<SkillsConfig>,
    #[serde(default)]
    permissions: Option<PermissionConfig>,
    #[serde(default)]
    workspace: Option<WorkspaceConfig>,
    #[serde(default)]
    bash_policy: Option<BashPolicyConfig>,
    #[serde(default)]
    websearch: Option<WebSearchConfig>,
    #[serde(default)]
    master: Option<MasterConfig>,
    #[serde(default)]
    hooks: Option<Vec<HookSpec>>,
    #[serde(default)]
    tool_variants: Option<ToolVariantsConfig>,
    #[serde(default)]
    daemon: Option<DaemonConfig>,
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawConfig::deserialize(deserializer)?;
        let mut cfg = Config::default();
        if let Some(c) = raw.default_connection {
            cfg.default_connection = c;
        }
        if let Some(m) = raw.default_model {
            cfg.default_model = Some(m);
        }
        if let Some(mcp) = raw.mcp {
            cfg.mcp = mcp;
        }
        if let Some(mut comp) = raw.compaction {
            if let Some(r) = raw.compaction_preserve_rounds {
                comp.preserve_rounds = r;
            }
            if let Some(s) = raw.compaction_summarize {
                comp.summarize = s;
            }
            if let Some(p) = raw.compaction_prune {
                comp.prune = p;
            }
            if let Some(pt) = raw.compaction_prune_protect_tokens {
                comp.prune_protect_tokens = pt;
            }
            cfg.compaction = comp;
        } else {
            if let Some(r) = raw.compaction_preserve_rounds {
                cfg.compaction.preserve_rounds = r;
            }
            if let Some(s) = raw.compaction_summarize {
                cfg.compaction.summarize = s;
            }
            if let Some(p) = raw.compaction_prune {
                cfg.compaction.prune = p;
            }
            if let Some(pt) = raw.compaction_prune_protect_tokens {
                cfg.compaction.prune_protect_tokens = pt;
            }
        }
        if let Some(a) = raw.connection_retry_max_attempts {
            cfg.connection_retry_max_attempts = a;
        }
        if let Some(b) = raw.connection_retry_base_ms {
            cfg.connection_retry_base_ms = b;
        }
        if let Some(m) = raw.connection_retry_max_ms {
            cfg.connection_retry_max_ms = m;
        }
        if let Some(f) = raw.favorites {
            cfg.favorites = f;
        }
        if let Some(h) = raw.hidden_models {
            cfg.hidden_models = h;
        }
        if let Some(s) = raw.skills {
            cfg.skills = s;
        }
        if let Some(p) = raw.permissions {
            cfg.permissions = p;
        }
        if let Some(w) = raw.workspace {
            cfg.workspace = w;
        }
        if let Some(b) = raw.bash_policy {
            cfg.bash_policy = b;
        }
        if let Some(w) = raw.websearch {
            cfg.websearch = w;
        }
        if let Some(m) = raw.master {
            cfg.master = m;
        }
        if let Some(h) = raw.hooks {
            cfg.hooks = h;
        }
        if let Some(tv) = raw.tool_variants {
            cfg.tool_variants = tv;
        }
        if let Some(d) = raw.daemon {
            cfg.daemon = d;
        }
        Ok(cfg)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_connection: String::new(),
            mcp: HashMap::new(),
            compaction: CompactionPolicy::default(),
            connection_retry_max_attempts: 30,
            connection_retry_base_ms: 1_000,
            connection_retry_max_ms: 10_000,
            default_model: None,
            favorites: Vec::new(),
            hidden_models: Vec::new(),
            skills: SkillsConfig::default(),
            permissions: PermissionConfig::default(),
            workspace: WorkspaceConfig::default(),
            bash_policy: BashPolicyConfig::default(),
            websearch: WebSearchConfig::default(),
            master: MasterConfig::default(),
            hooks: Vec::new(),
            tool_variants: ToolVariantsConfig::default(),
            daemon: DaemonConfig::default(),
        }
    }
}

/// Whether none of the six web-tool keys is set (helper for
/// [`Config::merge_websearch_keys`]).
fn keys_eq_none(keys: &muta_contracts::WebSearchConfig) -> bool {
    keys.exa_api_key.is_none()
        && keys.parallel_api_key.is_none()
        && keys.tavily_api_key.is_none()
        && keys.bocha_api_key.is_none()
        && keys.jina_api_key.is_none()
}

impl Config {
    pub fn load() -> Self {
        let config_path = Self::config_file_path();
        let mut config = match fs::read_to_string(&config_path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(parsed) => parsed,
                Err(error) => {
                    // A corrupt config must never block startup, but falling
                    // back to defaults *silently* would discard the user's
                    // entire setup with no trace of why. Warn loudly (the
                    // log carries the file and the error) so a typo'd
                    // config.toml is diagnosable instead of reading as
                    // "muta forgot my settings".
                    tracing::error!(
                        path = %config_path.display(),
                        %error,
                        "config.toml is unparseable; continuing with defaults \
                         (fix the syntax error to restore the saved configuration)"
                    );
                    Config::default()
                }
            },
            // Absent is the normal first-run condition; nothing to report.
            Err(_) => Config::default(),
        };
        Self::merge_websearch_keys(&mut config);
        config
    }

    /// Merge `credentials.toml [websearch]` into the in-memory `[websearch]`
    /// table, and migrate any keys found in `config.toml [websearch]` (the
    /// historical location) into the credentials file — one-shot and
    /// idempotent. `config.toml` is behavior-only and shareable; the six API
    /// keys are secrets and must not live there.
    fn merge_websearch_keys(config: &mut Config) {
        // Pull the secret keys out of the parsed table. Serialization already
        // skips them (they cannot come back through a save); this moves any
        // keys a pre-migration file still carries.
        let keys_in_config = config.websearch.secret_keys_only();
        let from_config = (!keys_eq_none(&keys_in_config)).then_some(WebSearchKeys {
            exa_api_key: keys_in_config.exa_api_key,
            parallel_api_key: keys_in_config.parallel_api_key,
            tavily_api_key: keys_in_config.tavily_api_key,
            bocha_api_key: keys_in_config.bocha_api_key,
            jina_api_key: keys_in_config.jina_api_key,
        });
        let mut creds = Credentials::load();
        if let Some(migrated) = from_config {
            // Fold into the credentials store (an explicit credentials entry
            // wins: it is the location the user edits going forward) and
            // persist both files. A failed save is non-fatal — the keys stay
            // in memory for this run and the migration retries next load.
            creds.websearch.absorb(migrated);
            if let Err(e) = creds.save() {
                tracing::warn!("could not migrate websearch keys into credentials.toml: {e}");
            }
        }
        creds.websearch.clone().merge_into(&mut config.websearch);
    }

    /// Load only the `[mcp.*]` table from a project-local `.muta/config.toml`
    /// (ADR-0085 §2/§3). Returns an empty map when the file or table is absent.
    ///
    /// This reads a *narrow* projection — just the mcp table — so unrelated
    /// well-formed keys do not affect the result. Project-scope MCP stays
    /// quarantined until the current MCP-domain digest is trusted; this
    /// function is pure parsing, and the caller applies the decision.
    pub fn load_project_mcp(project_root: &std::path::Path) -> HashMap<String, McpServerConfig> {
        let path = project_root.join(".muta/config.toml");
        // Deserialize into a struct that only declares `mcp`, ignoring every
        // other key the project file may carry (deny_unknown_fields off).
        #[derive(Deserialize)]
        struct ProjectMcpProjection {
            #[serde(default)]
            mcp: HashMap<String, McpServerConfig>,
        }
        let mut servers = match fs::read_to_string(&path) {
            Ok(content) => match toml::from_str::<ProjectMcpProjection>(&content) {
                Ok(parsed) => parsed.mcp,
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "project .muta/config.toml has invalid [mcp.*]; ignoring that MCP source"
                    );
                    HashMap::new()
                }
            },
            Err(_) => HashMap::new(),
        };

        // `.muta/mcp.json` follows the common MCP client shape while retaining
        // Muta's `read_only` and `enabled` policy fields. A JSON definition with
        // the same name replaces the TOML entry, giving the dedicated file a
        // deterministic precedence.
        #[derive(Deserialize, Default)]
        struct ProjectMcpJson {
            #[serde(default, rename = "mcpServers")]
            mcp_servers: HashMap<String, ProjectMcpJsonServer>,
        }
        #[derive(Deserialize)]
        struct ProjectMcpJsonServer {
            command: String,
            #[serde(default)]
            args: Vec<String>,
            #[serde(default, rename = "env")]
            environment: HashMap<String, String>,
            #[serde(default = "default_true")]
            enabled: bool,
            #[serde(default)]
            read_only: bool,
            /// Config-time tool scoping (ADR-0085 follow-up): original-name
            /// allow/deny lists, same semantics as `[mcp.<name>]` TOML.
            #[serde(default)]
            allow_tools: Vec<String>,
            #[serde(default)]
            deny_tools: Vec<String>,
        }
        fn default_true() -> bool {
            true
        }

        let json_path = project_root.join(".muta/mcp.json");
        if let Ok(content) = fs::read_to_string(&json_path) {
            match serde_json::from_str::<ProjectMcpJson>(&content) {
                Ok(parsed) => {
                    for (name, entry) in parsed.mcp_servers {
                        let mut command = Vec::with_capacity(entry.args.len() + 1);
                        command.push(entry.command);
                        command.extend(entry.args);
                        servers.insert(
                            name,
                            McpServerConfig {
                                url: None,
                                command,
                                environment: entry.environment,
                                enabled: entry.enabled,
                                read_only: entry.read_only,
                                allow_tools: entry.allow_tools,
                                deny_tools: entry.deny_tools,
                                sandbox_root: None,
                            },
                        );
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        path = %json_path.display(),
                        error = %err,
                        "project .muta/mcp.json is invalid; ignoring that MCP source"
                    );
                }
            }
        }

        let root =
            std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
        for config in servers.values_mut() {
            config.sandbox_root = Some(root.clone());
        }
        servers
    }

    /// Merge a project-local MCP server set into this (global-origin) config.
    /// A project entry with the same name as a global entry **replaces** it
    /// wholesale (ADR-0085 §4); project entries with new names are added. The
    /// result is the effective `[mcp.*]` set the runtime connects to.
    pub fn merge_project_mcp(&mut self, project_mcp: HashMap<String, McpServerConfig>) {
        for (name, cfg) in project_mcp {
            self.mcp.insert(name, cfg);
        }
    }

    /// Parse a bare `[mcp.<name>]` TOML document — the same table shape a
    /// project `.muta/config.toml` carries and a server-side `print-config`
    /// command emits — into named server entries. A *narrow* projection like
    /// [`Self::load_project_mcp`]: unrelated well-formed keys are ignored, so
    /// a full user config can be piped through. Used by `muta mcp import`.
    pub fn parse_mcp_toml(content: &str) -> Result<HashMap<String, McpServerConfig>, String> {
        #[derive(Deserialize)]
        struct McpProjection {
            #[serde(default)]
            mcp: HashMap<String, McpServerConfig>,
        }
        toml::from_str::<McpProjection>(content)
            .map(|parsed| parsed.mcp)
            .map_err(|error| format!("input is not valid TOML with [mcp.<name>] tables: {error}"))
    }

    /// Load only the `[[hooks]]` array from a project-local
    /// `.muta/config.toml`. Returns an empty vec when the file or table is
    /// absent. Like [`Self::load_project_mcp`], this is a *narrow* projection
    /// (just the hooks array) so an unrelated key in the project file does not
    /// fail the whole load. Project-scope hooks are quarantined until their
    /// exact extension content is trusted; the caller applies the gate.
    ///
    /// A project `[[hooks]]` entry whose `command` points at a project-supplied
    /// script (e.g. `.muta/hooks/lint.sh`) is the same class of hazard as a
    /// project `[mcp.*]` server: a cloned/vendored repo must not gain shell
    /// execution merely because the user opened it.
    pub fn load_project_hooks(project_root: &std::path::Path) -> Vec<HookSpec> {
        let path = project_root.join(".muta/config.toml");
        let Some(content) = fs::read_to_string(&path).ok() else {
            return Vec::new();
        };
        // Deserialize into a struct that only declares `hooks`, ignoring every
        // other key the project file may carry (deny_unknown_fields off).
        #[derive(Deserialize)]
        struct ProjectHooksProjection {
            #[serde(default)]
            hooks: Vec<HookSpec>,
        }
        match toml::from_str::<ProjectHooksProjection>(&content) {
            Ok(mut parsed) => {
                let root = std::fs::canonicalize(project_root)
                    .unwrap_or_else(|_| project_root.to_path_buf());
                for hook in &mut parsed.hooks {
                    hook.sandbox_root = Some(root.clone());
                }
                parsed.hooks
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "project .muta/config.toml has invalid [[hooks]]; ignoring project hooks"
                );
                Vec::new()
            }
        }
    }

    /// Load only the `[workspace].additional_roots` array from a project-local
    /// `.muta/config.toml`. Returns an empty vec when the file or table is
    /// absent. Like [`Self::load_project_mcp`] and [`Self::load_project_hooks`],
    /// this is a narrow projection (just the workspace table). Project-scope
    /// additional roots remain quarantined until the `roots` domain is trusted.
    pub fn load_project_additional_roots(project_root: &std::path::Path) -> Vec<String> {
        let path = project_root.join(".muta/config.toml");
        let Some(content) = fs::read_to_string(&path).ok() else {
            return Vec::new();
        };
        #[derive(Deserialize)]
        struct ProjectWorkspaceProjection {
            #[serde(default)]
            workspace: ProjectWorkspaceConfig,
        }
        #[derive(Deserialize, Default)]
        struct ProjectWorkspaceConfig {
            #[serde(default)]
            additional_roots: Vec<String>,
        }
        match toml::from_str::<ProjectWorkspaceProjection>(&content) {
            Ok(parsed) => parsed.workspace.additional_roots,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "project .muta/config.toml has invalid [workspace]; ignoring project additional_roots"
                );
                Vec::new()
            }
        }
    }

    /// Append project-local `[workspace].additional_roots` to this config's additional roots.
    pub fn merge_project_additional_roots(&mut self, project_roots: Vec<String>) {
        for root in project_roots {
            if !self.workspace.additional_roots.contains(&root) {
                self.workspace.additional_roots.push(root);
            }
        }
    }

    /// Resolve the `[workspace].additional_roots` policy for an active project.
    /// Global and trusted project-local additional roots are resolved and canonicalized
    /// against `project_root`.
    pub fn resolve_workspace_additional_roots(
        &self,
        project_root: &std::path::Path,
    ) -> Result<Vec<std::path::PathBuf>, String> {
        if self.workspace.additional_roots.is_empty() {
            return Ok(Vec::new());
        }
        let canonical_root =
            std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let mut resolved_roots = Vec::new();
        for raw in &self.workspace.additional_roots {
            let expanded: std::path::PathBuf = if raw == "~" {
                home.clone().ok_or_else(|| {
                    "[workspace].additional_roots: '~' used but HOME is unset".to_string()
                })?
            } else if let Some(rest) = raw.strip_prefix("~/") {
                match &home {
                    Some(h) => h.join(rest),
                    None => {
                        return Err(format!(
                            "[workspace].additional_roots: '{raw}' uses '~' but HOME is unset"
                        ));
                    }
                }
            } else {
                if std::path::Path::new(raw).starts_with("/") {
                    std::path::PathBuf::from(raw)
                } else {
                    canonical_root.join(raw)
                }
            };
            let expanded = if expanded.is_absolute() {
                expanded
            } else {
                canonical_root.join(&expanded)
            };
            let canonical = std::fs::canonicalize(&expanded).map_err(|_| {
                format!(
                    "[workspace].additional_roots: '{}' does not exist",
                    expanded.display()
                )
            })?;
            if !canonical.is_dir() {
                return Err(format!(
                    "[workspace].additional_roots: '{}' is not a directory",
                    canonical.display()
                ));
            }
            if canonical == canonical_root {
                return Err(format!(
                    "[workspace].additional_roots: '{}' is the workspace root itself; it is already admitted",
                    canonical.display()
                ));
            }
            if canonical.starts_with(&canonical_root) {
                return Err(format!(
                    "[workspace].additional_roots: '{}' is inside the workspace and already admitted",
                    canonical.display()
                ));
            }
            // Distinct spellings of the same directory (relative plus
            // absolute, a symlinked twin) collapse silently: admission is a
            // set, and the second mention grants nothing new to reject.
            if !resolved_roots.contains(&canonical) {
                resolved_roots.push(canonical);
            }
        }
        Ok(resolved_roots)
    }

    /// Append project-local `[[hooks]]` to this config's (global-origin) hooks.
    /// Project hooks are appended *after* global ones so the global ordering is
    /// preserved; hook semantics within one event are order-independent (each
    /// hook decides independently), so concatenation is sufficient.
    pub fn merge_project_hooks(&mut self, project_hooks: Vec<HookSpec>) {
        self.hooks.extend(project_hooks);
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        Self::save_inner(self, false)
    }

    /// Persist config while leaving the on-disk `default_provider` /
    /// `default_model` selection untouched. Used by mutations that are not
    /// selection changes (favorites, provider metadata edits, TUI
    /// preferences) so they never leak the in-memory selection — which may
    /// carry a resumed session's provider pin — into `config.toml`. The
    /// `/models` switch itself calls [`Config::save`]: updating the global
    /// Persist config while leaving the on-disk `default_connection` /
    /// `default_model` selection untouched.
    pub fn save_preserving_connection_selection(&self) -> Result<(), Box<dyn std::error::Error>> {
        Self::save_inner(self, true)
    }

    fn save_inner(
        &self,
        preserve_connection_selection: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Serialise against other `muta` instances so concurrent config
        // writes do not lost-update each other (ADR-0018 pattern). The lock is
        // held on the companion `.lock` file (not the data file, which is
        // rewritten via temp + rename and swaps inodes) for the whole RMW.
        let config_path = Self::config_file_path();
        let _lock = fsutil::FileLock::acquire(&config_path)
            .map_err(|e| format!("could not lock config file: {e}"))?;

        // The effective selection to write back. When preserving, re-read the
        // on-disk value under the lock so another process's write survives.
        let (default_connection, default_model) = if preserve_connection_selection {
            let on_disk: Config = fs::read_to_string(&config_path)
                .ok()
                .and_then(|content| toml::from_str(&content).ok())
                .unwrap_or_default();
            let connection = if on_disk.default_connection.is_empty() {
                // On-disk default is gone (or never set): keep this writer's
                // selection so the file never silently loses it.
                self.default_connection.clone()
            } else {
                on_disk.default_connection
            };
            (connection, on_disk.default_model)
        } else {
            (self.default_connection.clone(), self.default_model.clone())
        };

        // ── config.toml = behavior only ─────────────────────────────────────
        // Secrets live in `credentials.toml`, connections in `connections.toml`.
        let mut out = self.clone();
        out.default_connection = default_connection;
        out.default_model = default_model;
        let bytes = toml::to_string_pretty(&out)?.into_bytes();
        fsutil::atomic_write_bytes(&config_path, &bytes)?;
        Ok(())
    }

    pub fn config_file_path() -> PathBuf {
        paths::get().config_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_table_round_trips_through_toml() {
        // The `[master]` table must round-trip: partial TOML keeps defaults,
        // full TOML preserves explicit overrides. Legacy `[agent.review]`
        // sub-tables (ADR-0016) are accepted but ignored — `hard_stop_turns`
        // now lives directly under `[master]` (ADR-0018).
        let toml_full = r#"
            [master]
            hard_stop_turns = 40
        "#;
        let cfg: Config = toml::from_str(toml_full).unwrap();
        assert_eq!(cfg.master.hard_stop_turns, 40);

        // Missing `[master]` table → defaults match the documented values.
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.master.hard_stop_turns, 0);

        // A legacy `[agent.review]` block no longer maps to anything; it must
        // not break parsing (unknown sub-tables are ignored) and the new
        // direct field still round-trips.
        let toml_legacy = r#"
            [agent.review]
            review_start_turn = 64
            hard_stop_turns = 99
        "#;
        let cfg: Config = toml::from_str(toml_legacy).unwrap();
        assert_eq!(cfg.master.hard_stop_turns, 0);

        // Round-trip through save+load format (serialize then parse).
        let mut cfg = Config::default();
        cfg.master.hard_stop_turns = 99;
        let serialised = toml::to_string(&cfg).unwrap();
        let parsed: Config = toml::from_str(&serialised).unwrap();
        assert_eq!(parsed.master.hard_stop_turns, 99);
    }

    #[test]
    fn compaction_round_count_writes_canonical_key_and_drops_legacy_key() {
        // ADR-0120 policy: the pre-ADR-0047 key is not aliased. It parses as
        // an unknown key (warned and ignored) and the field stays at its
        // default — the stale value must not carry through.
        let legacy: Config = toml::from_str("compaction_preserve_turns = 9").unwrap();
        assert_eq!(
            legacy.compaction.preserve_rounds,
            Config::default().compaction.preserve_rounds
        );

        let serialized = toml::to_string(&legacy).unwrap();
        assert!(serialized.contains("preserve_rounds ="));
        assert!(!serialized.contains("compaction_preserve_turns ="));
    }

    #[test]
    fn doom_guard_table_writes_canonical_key_and_accepts_legacy_nudge_alias() {
        // Unlike the ADR-0120 ignore-and-drop policy, the guard's rename is
        // aliased, not ignored: the default is ON, so an explicit
        // `enabled = false` under the old `nudge` key must survive — dropping
        // it would silently flip the user's opt-out back to blocking.
        let legacy: Config =
            toml::from_str("[master.nudge]\nenabled = false\nwindow = 24\n").unwrap();
        let canonical: Config =
            toml::from_str("[master.doom_guard]\nenabled = false\nwindow = 24\n").unwrap();
        assert_eq!(legacy.master.doom_guard, canonical.master.doom_guard);
        assert!(!canonical.master.doom_guard.enabled);
        assert_eq!(canonical.master.doom_guard.window, 24);

        // Save always writes the canonical key; the alias is load-only.
        let serialized = toml::to_string(&canonical).unwrap();
        assert!(
            serialized.contains("[master.doom_guard]"),
            "got: {serialized}"
        );
        assert!(
            !serialized.contains("nudge"),
            "legacy key must not be re-emitted: {serialized}"
        );
    }

    #[test]
    fn tool_variants_table_parses_and_resolves_per_model() {
        // The table name mirrors the Config field name (`tool_variants`), as
        // serde maps struct fields to TOML keys verbatim. The model id is
        // quoted because it contains dots/hyphens. Each entry maps a capability
        // name to the variant id chosen for that model.
        let toml_src = r#"
            [tool_variants."kimi-k2.7-code"]
            read_text = "terse"
            execute_command = "workspace"

            [tool_variants."glm-5.2"]
            read_text = "verbose"
        "#;
        let cfg: Config = toml::from_str(toml_src).unwrap();

        // Known model → its map; unlisted capability within a known model → absent.
        let kimi = cfg.tool_variants.for_model("kimi-k2.7-code");
        assert_eq!(kimi.get("read_text").map(String::as_str), Some("terse"));
        assert_eq!(
            kimi.get("execute_command").map(String::as_str),
            Some("workspace")
        );
        assert!(kimi.get("grep").is_none());

        // A different model gets its own independent map.
        let glm = cfg.tool_variants.for_model("glm-5.2");
        assert_eq!(glm.get("read_text").map(String::as_str), Some("verbose"));
        assert!(glm.get("execute_command").is_none());

        // Unknown model → empty (but borrowable without an Option).
        assert!(cfg.tool_variants.for_model("does-not-exist").is_empty());

        // Absent table entirely → empty config, every lookup is empty.
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.tool_variants.for_model("kimi-k2.7-code").is_empty());
    }

    #[test]
    fn tool_variants_round_trip_through_serialise() {
        let mut cfg = Config::default();
        let mut sel = muta_contracts::VariantSelection::new();
        sel.insert("read_text".to_string(), "terse".to_string());
        sel.insert("bash".to_string(), "strict".to_string());
        cfg.tool_variants
            .0
            .insert("kimi-k2.7-code".to_string(), ModelToolVariants(sel));
        let serialised = toml::to_string(&cfg).unwrap();
        let parsed: Config = toml::from_str(&serialised).unwrap();
        let resolved = parsed.tool_variants.for_model("kimi-k2.7-code");
        assert_eq!(resolved.get("read_text").map(String::as_str), Some("terse"));
        assert_eq!(resolved.get("bash").map(String::as_str), Some("strict"));
    }

    /// Tests that mutate the process-wide paths override (`set_test_default`)
    /// and read/write the throwaway config/credentials/cache files must
    /// serialise against each other so the parallel runner never observes
    /// another test's Dirs. Mirrors the `ENV_GUARD` pattern in `paths.rs`.
    static PATHS_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A fresh throwaway directory + the test paths override installed against
    /// it. Drop the returned guards (both the module lock and the crate-wide
    /// override lock) to restore the default paths so the next test starts
    /// clean.
    ///
    /// The crate-wide `paths::TEST_OVERRIDE_GUARD` is what actually serialises
    /// against `session`'s override-touching tests; the per-module `PATHS_GUARD`
    /// is kept for any intra-module shared state.
    fn sandbox_config_dir() -> (
        std::path::PathBuf,
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let override_guard = paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("muta-creds-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let dirs = paths::Dirs {
            config_dir: tmp.clone(),
            data_dir: tmp.join("data"),
            state_dir: tmp.join("state"),
            cache_dir: tmp.join("cache"),
            runtime_dir: None,
        };
        paths::set_test_default(Some(dirs));
        (tmp, guard, override_guard)
    }

    #[test]
    fn credentials_round_trip_through_toml() {
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        let mut creds = Credentials::default();
        creds.set_api_key("deepseek", Some("sk-ds".into()));
        creds.set_api_key("relay", Some("relay-secret".into()));
        // Empty / whitespace keys never materialise an entry.
        creds.set_api_key("keyless", Some("   ".into()));
        creds.save().unwrap();

        let on_disk = std::fs::read_to_string(tmp.join("credentials.toml")).unwrap();
        assert!(on_disk.contains("sk-ds"));
        assert!(on_disk.contains("relay-secret"));
        assert!(!on_disk.contains("keyless"), "empty key must not persist");

        let mut reloaded = Credentials::load();
        assert_eq!(
            reloaded
                .api_key("deepseek")
                .map(SecretString::expose_secret),
            Some("sk-ds")
        );
        assert_eq!(
            reloaded.api_key("relay").map(SecretString::expose_secret),
            Some("relay-secret")
        );
        assert!(reloaded.api_key("keyless").is_none());
        assert!(reloaded.api_key("missing").is_none());

        reloaded.remove_api_key("deepseek");
        reloaded.save().unwrap();
        assert!(Credentials::load().api_key("deepseek").is_none());

        paths::set_test_default(None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn credentials_ignore_legacy_sections_and_read_providers() {
        // The pre-refactor credentials layout (`[builtins.<id>]` /
        // `[user.<id>]`) is superseded by `[providers.<id>]`. Reading an old
        // file must not fail and must not surface the old sections.
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        std::fs::write(
            tmp.join("credentials.toml"),
            r#"[builtins]
openai = "old-builtin"
[user.my-relay]
api_key = "old-user"
[providers]
deepseek = "new-key"
"#,
        )
        .unwrap();
        let creds = Credentials::load();
        assert!(
            creds.api_key("openai").is_none(),
            "builtins section is gone"
        );
        assert!(creds.api_key("my-relay").is_none(), "user section is gone");
        assert_eq!(
            creds.api_key("deepseek").map(SecretString::expose_secret),
            Some("new-key")
        );

        paths::set_test_default(None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn discovery_cache_and_remote_round_trip() {
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        let mut cache = DiscoveryCache::default();
        cache.connection_models.insert(
            "deepseek".to_string(),
            vec!["deepseek-v4-flash".to_string()],
        );
        cache.fitted_models.insert("kimi".to_string(), {
            let mut m = std::collections::BTreeMap::new();
            m.insert(
                "kimi-for-coding".to_string(),
                FittedModelInfo {
                    context_window: 262_144,
                    reasoning: true,
                    vision: true,
                    efforts: vec!["max".to_string()],
                },
            );
            m
        });
        cache.model_lists.insert(
            "deepseek".to_string(),
            ModelListCacheState {
                etag: Some("\"models-v1\"".to_string()),
                client_version: "0.1.0".to_string(),
                refreshed_at_ms: 1234,
            },
        );
        cache.save().unwrap();

        let mut reloaded = DiscoveryCache::load();
        assert_eq!(
            reloaded.fitted_models["kimi"]["kimi-for-coding"].context_window,
            262_144
        );
        assert!(reloaded.remote_metadata_for("deepseek", "nope").is_none());
        assert_eq!(
            reloaded.model_lists["deepseek"].etag.as_deref(),
            Some("\"models-v1\"")
        );

        reloaded.remove_connection("deepseek");
        assert!(reloaded.connection_models.is_empty());
        assert!(reloaded.model_lists.is_empty());
        reloaded.save().unwrap();
        assert!(DiscoveryCache::load().connection_models.is_empty());

        paths::set_test_default(None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn config_save_is_behavior_only_and_tolerates_legacy_provider_tables() {
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        std::fs::write(
            tmp.join("config.toml"),
            r#"default_connection = "deepseek"
deepseek_api_key = "legacy-key"
[[providers]]
id = "deepseek"
name = "DeepSeek"
"#,
        )
        .unwrap();
        let loaded = Config::load();
        assert_eq!(loaded.default_connection, "deepseek");
        let mut cfg = loaded;
        cfg.default_connection = "zai".to_string();
        cfg.save().unwrap();
        let on_disk = std::fs::read_to_string(tmp.join("config.toml")).unwrap();
        assert!(on_disk.contains("default_connection = \"zai\""));
        assert!(
            !on_disk.contains("[[providers]]"),
            "legacy provider tables must not be re-emitted"
        );
        assert!(
            !on_disk.contains("legacy-key"),
            "legacy key fields must not be re-emitted"
        );
        // The old credentials layout is untouched by a behavior-only save.
        let creds_text =
            std::fs::read_to_string(tmp.join("credentials.toml")).unwrap_or_else(|_| String::new());
        assert!(creds_text.is_empty() || !creds_text.contains("legacy-key"));

        paths::set_test_default(None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn route_settings_presence_opts_in_and_is_empty_semantics() {
        // ADR-0046: entry presence opts in; a bare entry (no knobs) still
        // counts as configured, while an entry that carries no fields after
        // mutation reports empty so callers can prune it.
        let empty = RouteSettings::default();
        assert!(empty.is_empty());
        let bare = RouteSettings {
            effort: None,
            thinking: None,
            capability_overrides: None,
        };
        assert!(bare.is_empty());
        let with_effort = RouteSettings {
            effort: Some("high".to_string()),
            thinking: None,
            capability_overrides: None,
        };
        assert!(!with_effort.is_empty());
        let with_thinking = RouteSettings {
            effort: None,
            thinking: Some(false),
            capability_overrides: None,
        };
        assert!(!with_thinking.is_empty());
    }

    // --- project-scope MCP merge (ADR-0085 §2/§3) --------------------------

    struct ScratchProject(tempfile::TempDir);

    impl std::ops::Deref for ScratchProject {
        type Target = std::path::Path;

        fn deref(&self) -> &Self::Target {
            self.0.path()
        }
    }

    fn scratch_project_root() -> ScratchProject {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".muta")).unwrap();
        ScratchProject(dir)
    }

    #[test]
    fn resolve_workspace_additional_roots_empty_when_table_absent() {
        let root = scratch_project_root();
        assert!(
            Config::default()
                .resolve_workspace_additional_roots(&root)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn resolve_workspace_additional_roots_resolves_relative_and_absolute_entries() {
        let root = scratch_project_root();
        let sibling =
            std::env::temp_dir().join(format!("muta-additional-root-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&sibling).unwrap();
        let mut config = Config::default();
        config.workspace.additional_roots = vec![
            format!("../{}", sibling.file_name().unwrap().to_string_lossy()),
            sibling.canonicalize().unwrap().display().to_string(),
        ];
        let roots = config.resolve_workspace_additional_roots(&root).unwrap();
        // Both spellings resolve to the same canonical sibling directory.
        assert_eq!(roots, vec![sibling.canonicalize().unwrap()]);
    }

    #[test]
    fn resolve_workspace_additional_roots_rejects_missing_and_nested_entries() {
        let root = scratch_project_root();
        // Missing directory.
        let mut config = Config::default();
        config.workspace.additional_roots = vec!["../does-not-exist-anywhere".to_string()];
        let err = config
            .resolve_workspace_additional_roots(&root)
            .unwrap_err();
        assert!(err.contains("does not exist"), "{err}");

        // Nested inside the workspace (already admitted).
        std::fs::create_dir_all(root.join("nested")).unwrap();
        config.workspace.additional_roots = vec!["nested".to_string()];
        let err = config
            .resolve_workspace_additional_roots(&root)
            .unwrap_err();
        assert!(err.contains("already admitted"), "{err}");

        // The workspace root itself.
        let canonical = root.canonicalize().unwrap();
        config.workspace.additional_roots = vec![canonical.display().to_string()];
        let err = config
            .resolve_workspace_additional_roots(&root)
            .unwrap_err();
        assert!(err.contains("workspace root itself"), "{err}");
    }

    #[test]
    fn project_config_cannot_widen_workspace_roots() {
        let root = scratch_project_root();
        let sibling =
            std::env::temp_dir().join(format!("muta-additional-root-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(
            root.join(".muta/config.toml"),
            format!(
                r#"
                    [workspace]
                    additional_roots = ["{}"]
                "#,
                sibling.canonicalize().unwrap().display()
            ),
        )
        .unwrap();
        let roots = Config::default()
            .resolve_workspace_additional_roots(&root)
            .unwrap();
        assert!(roots.is_empty());
    }

    #[test]
    fn load_project_mcp_reads_muta_config_table() {
        let root = scratch_project_root();
        std::fs::write(
            root.join(".muta/config.toml"),
            r#"
                [mcp.project-db]
                command = ["./bin/db-mcp"]
                enabled = true
            "#,
        )
        .unwrap();

        let mcp = Config::load_project_mcp(&root);
        assert_eq!(mcp.len(), 1);
        assert_eq!(mcp["project-db"].command, vec!["./bin/db-mcp".to_string()]);
        assert!(mcp["project-db"].enabled);
        assert_eq!(
            mcp["project-db"].sandbox_root.as_deref(),
            Some(root.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn load_project_mcp_reads_json_and_json_overrides_toml() {
        let root = scratch_project_root();
        std::fs::write(
            root.join(".muta/config.toml"),
            "[mcp.shared]\ncommand = [\"toml-server\"]\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".muta/mcp.json"),
            r#"{
                "mcpServers": {
                    "shared": {
                        "command": "json-server",
                        "args": ["--stdio"],
                        "env": {"MODE": "project"},
                        "read_only": true
                    }
                }
            }"#,
        )
        .unwrap();

        let mcp = Config::load_project_mcp(&root);
        assert_eq!(
            mcp["shared"].command,
            vec!["json-server".to_string(), "--stdio".to_string()]
        );
        assert_eq!(mcp["shared"].environment["MODE"], "project");
        assert!(mcp["shared"].enabled);
        assert!(mcp["shared"].read_only);
        assert_eq!(
            mcp["shared"].sandbox_root.as_deref(),
            Some(root.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn load_project_mcp_is_empty_when_file_absent() {
        let root = scratch_project_root();
        // No config.toml written.
        let mcp = Config::load_project_mcp(&root);
        assert!(mcp.is_empty());
    }

    #[test]
    fn load_project_mcp_ignores_unrelated_keys_and_bad_toml() {
        let root = scratch_project_root();
        // A project file may carry non-mcp tables; only [mcp.*] is projected,
        // and an invalid [mcp.*] makes the whole table drop (warn + empty).
        std::fs::write(
            root.join(".muta/config.toml"),
            r#"
                [master]
                hard_stop_turns = 7

                [mcp.ok]
                command = ["x"]
            "#,
        )
        .unwrap();
        let mcp = Config::load_project_mcp(&root);
        assert_eq!(mcp.len(), 1, "master ignored, mcp.ok projected");

        // A structurally invalid TOML → empty (never panics).
        let root2 = scratch_project_root();
        std::fs::write(root2.join(".muta/config.toml"), "this is = = not toml").unwrap();
        assert!(Config::load_project_mcp(&root2).is_empty());
    }

    #[test]
    fn merge_project_mcp_overrides_same_name_adds_new() {
        let mut global = Config::default();
        global.mcp.insert(
            "shared".to_string(),
            McpServerConfig {
                command: vec!["global-cmd".into()],
                enabled: true,
                ..McpServerConfig::default()
            },
        );
        global
            .mcp
            .insert("only-global".to_string(), McpServerConfig::default());

        let mut project = HashMap::new();
        // Same name → wholesale override (new command).
        project.insert(
            "shared".to_string(),
            McpServerConfig {
                command: vec!["project-cmd".into()],
                enabled: false,
                ..McpServerConfig::default()
            },
        );
        // New name → added.
        project.insert("only-project".to_string(), McpServerConfig::default());

        global.merge_project_mcp(project);

        // Override took effect.
        assert_eq!(
            global.mcp["shared"].command,
            vec!["project-cmd".to_string()]
        );
        assert!(!global.mcp["shared"].enabled);
        // Both pre-existing and added survive.
        assert!(global.mcp.contains_key("only-global"));
        assert!(global.mcp.contains_key("only-project"));
        assert_eq!(global.mcp.len(), 3);
    }

    #[test]
    fn load_project_hooks_reads_hooks_array() {
        let root = scratch_project_root();
        std::fs::write(
            root.join(".muta/config.toml"),
            r#"
                [[hooks]]
                event   = "PostToolUse"
                matcher = "Write|Edit"
                command = ".muta/hooks/lint.sh"

                [[hooks]]
                event   = "Stop"
                command = ".muta/hooks/notify.sh"
            "#,
        )
        .unwrap();

        let hooks = Config::load_project_hooks(&root);
        let canonical = root.0.path().canonicalize().unwrap();
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].event, HookEventKind::PostToolUse);
        assert_eq!(hooks[0].command, ".muta/hooks/lint.sh");
        assert_eq!(hooks[1].event, HookEventKind::Stop);
        assert!(
            hooks
                .iter()
                .all(|hook| hook.sandbox_root.as_deref() == Some(canonical.as_path()))
        );
    }

    #[test]
    fn load_project_hooks_is_empty_when_file_absent() {
        let root = scratch_project_root();
        // No config.toml written.
        assert!(Config::load_project_hooks(&root).is_empty());
    }

    #[test]
    fn load_project_hooks_ignores_unrelated_keys_and_bad_toml() {
        let root = scratch_project_root();
        // A project file may carry non-hooks tables; only [[hooks]] projects.
        std::fs::write(
            root.join(".muta/config.toml"),
            r#"
                [mcp.something]
                command = ["x"]

                [[hooks]]
                event = "Stop"
                command = "echo done"
            "#,
        )
        .unwrap();
        let hooks = Config::load_project_hooks(&root);
        assert_eq!(hooks.len(), 1, "mcp ignored, one hook projected");

        // Structurally invalid TOML → empty (never panics).
        let root2 = scratch_project_root();
        std::fs::write(root2.join(".muta/config.toml"), "this is = = not toml").unwrap();
        assert!(Config::load_project_hooks(&root2).is_empty());
    }

    #[test]
    fn merge_project_hooks_appends_to_global() {
        let mut global = Config::default();
        global.hooks.push(HookSpec {
            event: HookEventKind::Stop,
            matcher: None,
            command: "global-notify.sh".to_string(),
            sandbox_root: None,
        });
        let project_hooks = vec![HookSpec {
            event: HookEventKind::PostToolUse,
            matcher: Some("Write".to_string()),
            command: ".muta/hooks/lint.sh".to_string(),
            sandbox_root: Some(std::path::PathBuf::from("/project")),
        }];
        global.merge_project_hooks(project_hooks);
        assert_eq!(global.hooks.len(), 2, "global + project appended");
        // Global hook ordering preserved; project hooks come after.
        assert_eq!(global.hooks[0].command, "global-notify.sh");
        assert_eq!(global.hooks[1].command, ".muta/hooks/lint.sh");
    }

    #[test]
    fn parse_mcp_toml_projects_only_the_mcp_table() {
        // The `aegis-mcp print-config` shape: unrelated keys may appear.
        let input = r#"
            title = "unrelated scalar"

            [mcp.aegis]
            command = ["/usr/bin/aegis-mcp"]
            enabled = true
            read_only = false
            environment = { AEGIS_MCP_INSTANCE_ID = "8d1b62d6" }

            [mcp.docs]
            url = "https://example.com/mcp"
        "#;
        let servers = Config::parse_mcp_toml(input).unwrap();
        assert_eq!(servers.len(), 2);
        let aegis_command = vec!["/usr/bin/aegis-mcp".to_string()];
        assert_eq!(servers["aegis"].command, aegis_command);
        let instance_id = servers["aegis"].environment.get("AEGIS_MCP_INSTANCE_ID");
        assert_eq!(instance_id.map(String::as_str), Some("8d1b62d6"));
        assert_eq!(
            servers["docs"].url.as_deref(),
            Some("https://example.com/mcp")
        );
        // Runtime-only marker never leaks in from serialized input.
        assert!(servers["aegis"].sandbox_root.is_none());
    }

    #[test]
    fn parse_mcp_toml_rejects_invalid_toml_and_ignores_empty_tables() {
        assert!(Config::parse_mcp_toml("not = = toml").is_err());
        // A document without [mcp.*] parses to an empty map, not an error.
        let empty = Config::parse_mcp_toml("[other]\nx = 1\n").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn load_project_additional_roots_reads_workspace_table() {
        let root = scratch_project_root();
        std::fs::write(
            root.join(".muta/config.toml"),
            r#"
                [workspace]
                additional_roots = ["../optics", "~/shared/design"]
            "#,
        )
        .unwrap();

        let roots = Config::load_project_additional_roots(&root);
        assert_eq!(roots, vec!["../optics", "~/shared/design"]);
    }

    #[test]
    fn load_project_additional_roots_empty_when_absent() {
        let root = scratch_project_root();
        assert!(Config::load_project_additional_roots(&root).is_empty());
    }

    #[test]
    fn merge_project_additional_roots_deduplicates_and_appends() {
        let mut global = Config::default();
        global.workspace.additional_roots = vec!["../optics".to_string()];
        global.merge_project_additional_roots(vec![
            "../optics".to_string(),
            "../backend".to_string(),
        ]);
        assert_eq!(
            global.workspace.additional_roots,
            vec!["../optics", "../backend"]
        );
    }
}
