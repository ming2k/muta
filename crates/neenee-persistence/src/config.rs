//! User configuration schema and persistence.
//!
//! Deserializes/serializes the TOML config file (`principal`, `tui`, providers,
//! channels, MCP servers, hooks, skills, web-search) via [`crate::fsutil`]'s
//! atomic-write helpers, and loads/saves the input history. Config is state
//! (recency-merged under a companion file lock, ADR-0018); the live
//! provider/model selection telemetry lives in [`crate::provider_usage`].

use crate::fsutil;
use crate::paths;
use neenee_contracts::{
    CompactionPolicy, DoomGuardConfig, HookEventKind, McpServerConfig, RemoteModelMetadata,
    SecretString, SkillsConfig, VariantSelection, WebSearchConfig,
};

/// Re-export so server/TUI can use the config-layer path without depending on
/// core's auth module name directly for `AddProvider`.
pub use neenee_contracts::ChannelAuth as ConfigChannelAuth;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;

/// Reserved `[tui.default_expanded]` key that controls reasoning traces.
/// Reasoning isn't a tool, so each frontend addresses it by name.
pub const THINKING_KEY: &str = "thinking";

/// User-tunable principal (top-level agent) behaviour, deserialized from the optional `[principal]`
/// table of `config.toml`. All fields default sensibly, so a
/// `config.toml` with no `[principal]` table (or a partially specified one)
/// is valid.
///
/// ```toml
/// [principal]
/// # Hard-stop a round after this many total ReAct turns. 0 (the default)
/// # means no hard stop — an opt-in execution budget only. This is the sole
/// # per-round turn cap; the loop otherwise runs until the model stops, the user
/// # interrupts, or context compaction cannot relieve pressure (ADR-0009).
/// # hard_stop_turns = 0
///
/// # Never pop the interactive-input panel for a command needing stdin
/// # (sudo/gpg/passwd/…). Instead run it with stdin closed so it fails fast
/// # with a non-interactive remedy hint — like autopilot mode, but without
/// # turning the principal itself autopilot.
/// # skip_interactive_input = false
///
/// # Advanced doom-loop guard. Default disabled; opt in here when deterministic
/// # repeated-call blocking is desired. See [`DoomGuardConfig`]. (TOML key
/// # stays `nudge` for backward compatibility.)
/// # [principal.nudge]
/// # enabled = false
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PrincipalConfig {
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
    /// hint, exactly as it would under autopilot mode. This is the right
    /// setting for users who find the prompt disruptive and prefer to retry
    /// the command themselves (or let the model retry with a non-interactive
    /// form). Wired through `Agent::set_skip_interactive_input`.
    ///
    /// Note: this only governs the *interactive-input* path; it does not turn
    /// the principal autopilot, so ordinary tool confirmations still apply.
    pub skip_interactive_input: bool,
    /// Doom-loop guard configuration (`neenee_agent::doom_guard`). Default
    /// **disabled** — opt in via the advanced `[principal.nudge]` sub-table.
    /// See [`DoomGuardConfig`] for the per-field semantics.
    pub nudge: DoomGuardConfig,
}

// `DoomGuardConfig` is defined in `neenee_contracts::doom_guard_config` and re-exported
// above via `use neenee_contracts::DoomGuardConfig`. It is the `[principal.nudge]`
// TOML table and the wire type for `AgentRequest::UpdateDoomGuardConfig`. See
// `neenee_contracts::DoomGuardConfig` for the per-field semantics and defaults.

/// User-tunable frontend presentation, deserialized from the optional `[tui]`
/// table of `config.toml`. This is the **pure-data** form shared by every
/// frontend (TUI, future GUI); frontend-specific presenter logic (e.g. the
/// TUI's per-tool default-expand lookup against its render presenters) lives
/// in the frontend crate and reads this struct as input.
///
/// All fields default sensibly, so a `config.toml` with no `[tui]` table (or
/// a partially specified one) is valid.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    /// Per-step-kind default expand state. Keys are tool names (`edit_file`,
    /// `bash`, …) or [`THINKING_KEY`] for reasoning traces.
    ///
    /// ```toml
    /// [tui.default_expanded]
    /// edit_file = true
    /// bash = true
    /// thinking = false
    /// ```
    pub default_expanded: HashMap<String, bool>,
    /// How the transcript message stream is arranged. Recognized values
    /// (case-insensitive): `"turn_band"` (default — each tool-bearing ReAct turn is grouped
    /// into a labelled band with a header row). Unknown / empty values fall back to default.
    ///
    /// ```toml
    /// [tui]
    /// transcript_layout = "turn_band"
    /// ```
    pub transcript_layout: String,
    /// Active color scheme id. Built-in values are `zen`, `midnight`, `nord`,
    /// `catppuccin`, and `paper`; `custom` uses `custom_color_scheme` below.
    /// Unknown / empty values fall back to `zen`.
    pub color_scheme: String,
    /// User-editable semantic palette retained even when a built-in scheme is
    /// active, so it can be revisited from `/config` without losing changes.
    pub custom_color_scheme: neenee_contracts::ColorSchemeConfig,
    /// Whether clicking outside a dismissable modal closes it (mirroring Esc).
    ///
    /// Defaults to `true`: clicking the backdrop of a dismissable overlay (Help,
    /// Tools, Sessions, Config, …) closes it, exactly like Esc — the composer
    /// draft is safely parked so nothing is lost. Modals that hold precious
    /// in-progress input (API-key editor, permission/question sheets, …) are
    /// never click-dismissable regardless of this flag (see
    /// `Modal::dismissable_by_outside_click`), and the `neenee resume` startup
    /// picker is a special case whose click-outside still quits. Set `false` to
    /// disable click-outside-to-dismiss entirely (Esc / Ctrl+C always work).
    ///
    /// ```toml
    /// [tui]
    /// click_outside_dismiss = true
    /// ```
    #[serde(default = "default_click_outside_dismiss")]
    pub click_outside_dismiss: bool,
    /// Whether expanding or collapsing a disclosure (tool step, command
    /// result, thinking, provider-retry, or notice card) auto-scrolls the
    /// transcript so the toggled card's summary stays well-placed in the
    /// viewport (expanded: the header moves toward the top to reveal the body;
    /// collapsed: the summary is kept from scrolling off the top).
    ///
    /// Defaults to `false` — the toggle leaves the scroll offset untouched, so
    /// the view never moves as a side effect of a click. Users who want the
    /// content-maximizing behavior back can enable it. When enabled, the
    /// scroll is settled through a staged measure-then-paint frame so no
    /// intermediate viewport is ever shown (no flicker).
    ///
    /// ```toml
    /// [tui]
    /// expand_auto_scroll = false
    /// ```
    #[serde(default = "default_expand_auto_scroll")]
    pub expand_auto_scroll: bool,
}

/// Default for [`TuiConfig::click_outside_dismiss`]: **on**. Kept as a named
/// function so it can be referenced from the manual `Default` impl and the
/// `#[serde(default = …)]` attribute in lockstep.
fn default_click_outside_dismiss() -> bool {
    true
}

/// Default for [`TuiConfig::expand_auto_scroll`]: **off**. A disclosure
/// toggle is a read interaction, not a navigation command — by default the
/// scroll position is the user's and stays put. Kept as a named function so
/// the manual `Default` impl and the `#[serde(default = …)]` attribute stay
/// in lockstep.
fn default_expand_auto_scroll() -> bool {
    false
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            default_expanded: HashMap::new(),
            transcript_layout: String::new(),
            color_scheme: String::new(),
            custom_color_scheme: neenee_contracts::ColorSchemeConfig::default(),
            click_outside_dismiss: default_click_outside_dismiss(),
            expand_auto_scroll: default_expand_auto_scroll(),
        }
    }
}

/// Input-history behaviour (`[input_history]` table): how the persisted
/// prompt history (`history.json`) and the Ctrl+R picker treat repeated
/// prompts and slash-command invocations.
///
/// ```toml
/// [input_history]
/// dedup = true              # one row per prompt text, across sessions
/// record_commands = false   # don't persist `/model`, `/new`, …
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InputHistoryConfig {
    /// Collapse repeated identical prompts into a single history entry, keyed
    /// on the prompt **text alone** (across sessions and workspaces).
    ///
    /// With the default `true`, sending the same prompt twice — even in
    /// different sessions — shows one row; re-sending refreshes the entry's
    /// timestamp so it bubbles to the top of the newest-first picker. With
    /// `false`, identity is `(text, session_id)`: the same words typed in two
    /// sessions stay as two entries, each with its own origin.
    pub dedup: bool,
    /// Record `/slash` command invocations (`/model`, `/new`, `/repeat`, …)
    /// into the input history.
    ///
    /// Default `false`: commands are UI gestures, not prompts — they are
    /// already visible in the transcript, and most users don't want `/model`
    /// noise cluttering the prompt picker. Set `true` to make them recallable
    /// from Ctrl+R again.
    pub record_commands: bool,
}

impl Default for InputHistoryConfig {
    fn default() -> Self {
        Self {
            dedup: true,
            record_commands: false,
        }
    }
}

/// Declarative permission configuration — the `[permissions]` table. Lets users
/// pre-declare "always allow" rules in `config.toml` so default policies are
/// data-driven, not purely interactive:
///
/// ```toml
/// [[permissions.allow]]
/// tool = "bash"
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

/// Safety policy for model-issued `bash` commands. Built-in dangerous-command
/// rules are compiled into the agent so the config only contains user choices:
/// toggles and project-local overrides/additions.
///
/// ```toml
/// [bash_policy]
/// enabled = true
/// autopilot_confirm = "deny"
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
    /// What to do with a `confirm` decision while autopilot/no-human mode is
    /// active. Defaults to `deny`.
    pub autopilot_confirm: BashPolicyAutopilotAction,
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
            autopilot_confirm: BashPolicyAutopilotAction::Deny,
            allow_user_override_builtin_deny: false,
            rules: Vec::new(),
        }
    }
}

impl BashPolicyConfig {
    /// Return a copy hardened for an **untrusted** project (codex
    /// `UnlessTrusted` analogue). Applied only when the project is not yet
    /// trusted; `/trust` re-seeds with the raw config.
    ///
    /// Two adjustments, both reversibly additive:
    /// 1. Lock `autopilot_confirm` to `Deny`. A confirmation-gated command
    ///    must not auto-proceed when no human is reachable — a cloned/vendored
    ///    repo is exactly the case a human should eyeball.
    /// 2. Prepend a `confirm` rule matching common prompt-injection payloads:
    ///    package-manager installs (`npm i`, `pip install`, `cargo add`,
    ///    `go get`, …) and pipe-to-shell execution (`curl … | sh`, `wget … |
    ///    bash`). The built-in destructive-command rules already cover `rm`/
    ///    `git reset`; this layer covers *fetch-and-execute*, the other classic
    ///    untrusted-repo hazard.
    ///
    /// This is a lint (see `bash_policy.rs`), not a capability boundary — the
    /// envoy `OperationScope` remains the real wall — but it surfaces the
    /// decision to the human in the loop instead of letting it run silently.
    pub fn with_untrusted_hardening(mut self) -> Self {
        self.autopilot_confirm = BashPolicyAutopilotAction::Deny;
        let hardening_rule = BashPolicyRuleConfig {
            name: "untrusted-project confirm".to_string(),
            matcher: BashPolicyMatcherConfig::Regex,
            pattern: r"(?i)(^|[;&|]\s*|\s\|\s)*(?:npm\s+(?:install|i|ci|exec)\b|npx\s+-y\b|pnpm\s+(?:install|add|exec)\b|yarn\s+(?:add|install)\b|pip3?\s+install\b|pipx\s+install\b|uv\s+(?:pip\s+)?install\b|poetry\s+add\b|cargo\s+(?:add|install)\b|go\s+get\b|gem\s+install\b|brew\s+install\b|apt(?:-get)?\s+install\b|yum\s+install\b|dnf\s+install\b|pacman\s+-S\b|\b(?:curl|wget)\b[^|]*\|\s*(?:sh|bash|zsh|python3?|ruby|perl)\b)".to_string(),
            action: BashPolicyActionConfig::Confirm,
            reason: Some(
                "Project is not trusted: confirm fetch/install/pipe-to-shell commands."
                    .to_string(),
            ),
        };
        // Prepend so it is evaluated first (a later user `allow` rule for the
        // same command still wins, matching the override semantics).
        let mut rules = Vec::with_capacity(self.rules.len() + 1);
        rules.push(hardening_rule);
        rules.append(&mut self.rules);
        self.rules = rules;
        self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashPolicyAutopilotAction {
    /// Refuse commands that require confirmation when no human is reachable.
    #[default]
    Deny,
    /// Allow confirmation-gated commands to proceed autopilot. Useful only for
    /// highly controlled automation; not recommended for normal agent use.
    Allow,
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
    /// Tool name (e.g. `"bash"`, `"read_text"`, `"mcp__fs__read"`).
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
/// `neenee_contracts::catalog::Transport` variants but stays a plain serializable
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
    /// [`neenee_contracts::ChannelAuth`] instead.
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
/// See `neenee_contracts::model::FittedModel`.
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

/// Per-(instance, model) reasoning overrides, persisted in the discovery cache
/// keyed `route_settings[<instance_id>][<model_id>]`. Unlike the *derived*
/// capability fields ([`FittedModelInfo`]), `effort` / `thinking` are the
/// user's own per-route choices (set from the model `e` editor) — the entry's
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
}

impl RouteSettings {
    /// Whether the entry carries any explicit knob. An entry with neither
    /// field set still opts the model in to thinking on Anthropic routes.
    pub fn is_empty(&self) -> bool {
        self.effort.is_none() && self.thinking.is_none()
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
/// is a derived model path, not a security principal. One instance has
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Credentials {
    /// API keys keyed by provider instance id.
    #[serde(default)]
    pub providers: BTreeMap<String, SecretString>,
}

impl Credentials {
    fn path() -> PathBuf {
        paths::get().credentials_file()
    }

    /// Read `credentials.toml`, returning an empty (not erroring) value when
    /// the file is missing or unparseable. A missing secrets file is a normal
    /// first-run condition; a corrupt one must never block startup, so it is
    /// best-effort and only logs a warning.
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
    /// [`crate::fsutil::atomic_write_bytes`]. An empty `Credentials` writes an
    /// empty file (still valid TOML). Errors propagate to the caller.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let bytes = toml::to_string_pretty(self)?.into_bytes();
        fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    /// The credential for `instance_id`, if set and non-empty.
    pub fn api_key(&self, instance_id: &str) -> Option<&SecretString> {
        self.providers
            .get(instance_id)
            .filter(|k| !k.expose_secret().trim().is_empty())
    }

    /// Set (or clear) the credential for `instance_id`.
    pub fn set_api_key(&mut self, instance_id: &str, key: Option<SecretString>) {
        match key {
            Some(key) if !key.expose_secret().trim().is_empty() => {
                self.providers.insert(instance_id.to_string(), key);
            }
            _ => {
                self.providers.remove(instance_id);
            }
        }
    }

    /// Remove the credential for `instance_id`, if any.
    pub fn remove_api_key(&mut self, instance_id: &str) {
        self.providers.remove(instance_id);
    }
}

/// Discovered model lists, fitted capabilities, and per-route reasoning
/// overrides, cached under `$XDG_CACHE_HOME/neenee/models_discovery.json`.
///
/// Everything here is keyed by **provider instance** then model id — the
/// per-route facts the catalog needs to derive channels at runtime. The
/// derived fields (`provider_models`, `fitted_models`) are rebuildable by live
/// discovery; `route_settings` holds the user's own per-(instance, model)
/// reasoning choices and is written by the model `e` editor. Keeping all
/// per-route facts in one store (rather than config.toml) means `config.toml`
/// stays behavior-only and two instances of the same template never duplicate
/// or drift a route set.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveryCache {
    /// Cached discovered model lists, keyed by provider instance id:
    /// instance_id -> model ids (in discovery order).
    #[serde(default)]
    pub provider_models: BTreeMap<String, Vec<String>>,
    /// Fitted capability metadata, keyed by instance id then model id.
    #[serde(default)]
    pub fitted_models: BTreeMap<String, BTreeMap<String, FittedModelInfo>>,
    /// Per-(instance, model) reasoning overrides (the user's choices, not the
    /// endpoint's): `route_settings[instance_id][model_id]`.
    #[serde(default)]
    pub route_settings: BTreeMap<String, BTreeMap<String, RouteSettings>>,
    /// Trusted per-(instance, model) capability metadata advertised by the
    /// provider's live `GET /models` (endpoint, thinking, effort tiers …),
    /// mirror of `DiscoveredModel::remote_metadata`. Keyed instance id then
    /// model id; re-derived by every successful live discovery.
    #[serde(default)]
    pub remote_metadata: BTreeMap<String, BTreeMap<String, RemoteModelMetadata>>,
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

    /// Persist atomically to `$XDG_CACHE_HOME/neenee/models_discovery.json`.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let bytes = serde_json::to_vec_pretty(self)?;
        fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    /// The reasoning override for one route, if set.
    pub fn route_settings_for(&self, instance_id: &str, model_id: &str) -> Option<&RouteSettings> {
        self.route_settings
            .get(instance_id)
            .and_then(|models| models.get(model_id))
    }

    /// Borrow a route's reasoning settings mutably, inserting a default entry
    /// when absent, so a caller can set one field without rebuilding the store.
    pub fn route_settings_for_mut(
        &mut self,
        instance_id: &str,
        model_id: &str,
    ) -> &mut RouteSettings {
        self.route_settings
            .entry(instance_id.to_string())
            .or_default()
            .entry(model_id.to_string())
            .or_default()
    }

    /// Remove the per-instance records for `instance_id` (used on instance
    /// deletion).
    pub fn remove_instance(&mut self, instance_id: &str) {
        self.provider_models.remove(instance_id);
        self.fitted_models.remove(instance_id);
        self.route_settings.remove(instance_id);
        self.remote_metadata.remove(instance_id);
    }

    /// The trusted per-(instance, model) metadata, if set.
    pub fn remote_metadata_for(
        &self,
        instance_id: &str,
        model_id: &str,
    ) -> Option<&RemoteModelMetadata> {
        self.remote_metadata
            .get(instance_id)
            .and_then(|models| models.get(model_id))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub default_provider: String,
    pub mcp: HashMap<String, McpServerConfig>,
    /// Context-compaction thresholds expressed as fractions of the active
    /// model's context window, plus a fallback window for unknown models. See
    /// [`CompactionPolicy`] for the per-field semantics.
    pub compaction: CompactionPolicy,
    /// Number of recent complete user rounds preserved verbatim by full
    /// compaction.
    pub compaction_preserve_rounds: usize,
    /// Use the active model to produce an anchored, structured summary when
    /// compacting. When `false` (or when the summarization call fails) compaction
    /// falls back to the deterministic message-excerpt summary.
    pub compaction_summarize: bool,
    /// Enable cheap tool-result pruning (pre-turn and mid-turn) that clears old
    /// tool outputs in place to relieve context pressure before a full
    /// compaction is needed.
    pub compaction_prune: bool,
    /// Token budget of the most recent tool results protected from pruning.
    pub compaction_prune_protect_tokens: usize,
    /// Maximum number of attempts for a single model request when the provider returns a
    /// transient error (HTTP 408/429/5xx, connection, timeout). The initial try
    /// counts as the first attempt, so this is the *total* attempts, not extra
    /// retries. Clamped to `[1, 60]` at the call site.
    pub provider_retry_max_attempts: usize,
    /// Base delay (ms) for the bounded exponential backoff between retries:
    /// `base_ms * 2^(attempt-1)`, capped by `provider_retry_max_ms`.
    pub provider_retry_base_ms: u64,
    /// Hard cap (ms) on a single backoff delay, including the exponential growth.
    /// A server-supplied `Retry-After`/`retry-after-ms` header still wins but is
    /// itself capped at this value.
    pub provider_retry_max_ms: u64,
    /// The model id to use within the active provider. For single-model
    /// providers this mirrors the provider's pinned model; for multi-model
    /// providers (opencode-go) it selects which of the provider's models is
    /// active. `None` falls back to the provider's default model.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Favorite model ids for quick access in the Models picker (ADR-0046
    /// moved favorite from provider-level to per-model). Stored as a flat list
    /// of model wire ids; a starred daily-driver model sorts to the top of the
    /// flat list wherever it is served.
    #[serde(default)]
    pub favorites: Vec<String>,
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
    /// Bash command safety policy (`[bash_policy]` table). Built-in dangerous
    /// command rules are compiled into the agent; this config supplies only
    /// user overrides/additional rules and guard toggles.
    #[serde(default)]
    pub bash_policy: BashPolicyConfig,
    /// Web tool configuration (`[websearch]` table): search backend, proxy, timeout.
    #[serde(default)]
    pub websearch: WebSearchConfig,
    /// TUI presentation (`[tui]` table): per-step-kind default expand state.
    #[serde(default)]
    pub tui: TuiConfig,
    /// Input-history behaviour (`[input_history]` table): prompt dedup and
    /// slash-command recording. See [`InputHistoryConfig`].
    #[serde(default)]
    pub input_history: InputHistoryConfig,
    /// Principal behaviour (`[principal]` table): opt-in hard-stop budget and the
    /// doom-loop guard toggle. See [`PrincipalConfig`] for the per-field
    /// semantics and TOML examples.
    #[serde(default)]
    pub principal: PrincipalConfig,
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
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            shutdown_grace_secs: 10,
            idle_exit_minutes: 5,
            local_auth: true,
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
/// read_text = "terse"            # capability = variant id
/// bash      = "strict"
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
            .unwrap_or_else(|| neenee_contracts::empty_variant_selection())
    }
}

/// One lifecycle event hook entry (ADR-0025). Deserialized from a `[[hooks]]`
/// table in `config.toml`:
///
/// ```toml
/// [[hooks]]
/// event   = "PostToolUse"          # a [`HookEventKind`] variant
/// matcher = "Write|Edit"           # optional; tool-name `|`-list or regex
/// command = ".neenee/hooks/lint.sh"
/// ```
///
/// The command receives the [`neenee_contracts::HookContext`] as JSON on stdin and
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_provider: String::new(),
            mcp: HashMap::new(),
            compaction: CompactionPolicy::default(),
            compaction_preserve_rounds: 6,
            compaction_summarize: true,
            compaction_prune: true,
            compaction_prune_protect_tokens: 6_000,
            provider_retry_max_attempts: 30,
            provider_retry_base_ms: 1_000,
            provider_retry_max_ms: 10_000,
            default_model: None,
            favorites: Vec::new(),
            skills: SkillsConfig::default(),
            permissions: PermissionConfig::default(),
            bash_policy: BashPolicyConfig::default(),
            websearch: WebSearchConfig::default(),
            tui: TuiConfig::default(),
            input_history: InputHistoryConfig::default(),
            principal: PrincipalConfig::default(),
            hooks: Vec::new(),
            tool_variants: ToolVariantsConfig::default(),
            daemon: DaemonConfig::default(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let config_path = Self::config_file_path();
        match fs::read_to_string(&config_path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(parsed) => parsed,
                Err(error) => {
                    // A corrupt config must never block startup, but falling
                    // back to defaults *silently* would discard the user's
                    // entire setup with no trace of why. Warn loudly (the
                    // log carries the file and the error) so a typo'd
                    // config.toml is diagnosable instead of reading as
                    // "neenee forgot my settings".
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
        }
    }

    /// Load only the `[mcp.*]` table from a project-local `.neenee/config.toml`
    /// (ADR-0085 §2/§3). Returns an empty map when the file or table is absent.
    ///
    /// This reads a *narrow* projection — just the mcp table — so a project
    /// config that also carries unrelated keys (or partial/incomplete TOML)
    /// does not fail the whole load. Project-scope MCP is untrusted until a
    /// trust grant exists (§5); this function is pure parsing, the trust gate
    /// is applied by the caller (bootstrap / `/reload`).
    pub fn load_project_mcp(project_root: &std::path::Path) -> HashMap<String, McpServerConfig> {
        let path = project_root.join(".neenee/config.toml");
        let Some(content) = fs::read_to_string(&path).ok() else {
            return HashMap::new();
        };
        // Deserialize into a struct that only declares `mcp`, ignoring every
        // other key the project file may carry (deny_unknown_fields off).
        #[derive(Deserialize)]
        struct ProjectMcpProjection {
            #[serde(default)]
            mcp: HashMap<String, McpServerConfig>,
        }
        match toml::from_str::<ProjectMcpProjection>(&content) {
            Ok(parsed) => parsed.mcp,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "project .neenee/config.toml has invalid [mcp.*]; ignoring project MCP"
                );
                HashMap::new()
            }
        }
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

    /// Load only the `[[hooks]]` array from a project-local
    /// `.neenee/config.toml`. Returns an empty vec when the file or table is
    /// absent. Like [`Self::load_project_mcp`], this is a *narrow* projection
    /// (just the hooks array) so an unrelated key in the project file does not
    /// fail the whole load. Project-scope hooks are untrusted until a trust
    /// grant exists; the caller applies the gate.
    ///
    /// A project `[[hooks]]` entry whose `command` points at a project-supplied
    /// script (e.g. `.neenee/hooks/lint.sh`) is the same class of hazard as a
    /// project `[mcp.*]` server: a cloned/vendored repo must not gain shell
    /// execution merely because the user opened it.
    pub fn load_project_hooks(project_root: &std::path::Path) -> Vec<HookSpec> {
        let path = project_root.join(".neenee/config.toml");
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
            Ok(parsed) => parsed.hooks,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "project .neenee/config.toml has invalid [[hooks]]; ignoring project hooks"
                );
                Vec::new()
            }
        }
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
    /// default is its whole point.
    ///
    /// The lock + disk read makes this cross-process safe: another `neenee`
    /// writing its own selection concurrently is not clobbered, and this
    /// process's latest non-selection fields still land on disk.
    pub fn save_preserving_provider_selection(&self) -> Result<(), Box<dyn std::error::Error>> {
        Self::save_inner(self, true)
    }

    fn save_inner(
        &self,
        preserve_provider_selection: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Serialise against other `neenee` instances so concurrent config
        // writes do not lost-update each other (ADR-0018 pattern). The lock is
        // held on the companion `.lock` file (not the data file, which is
        // rewritten via temp + rename and swaps inodes) for the whole RMW.
        let config_path = Self::config_file_path();
        let _lock = fsutil::FileLock::acquire(&config_path)
            .map_err(|e| format!("could not lock config file: {e}"))?;

        // The effective selection to write back. When preserving, re-read the
        // on-disk value under the lock so another process's write survives.
        let (default_provider, default_model) = if preserve_provider_selection {
            let on_disk: Config = fs::read_to_string(&config_path)
                .ok()
                .and_then(|content| toml::from_str(&content).ok())
                .unwrap_or_default();
            let provider = if on_disk.default_provider.is_empty() {
                // On-disk default is gone (or never set): keep this writer's
                // selection so the file never silently loses it.
                self.default_provider.clone()
            } else {
                on_disk.default_provider
            };
            (provider, on_disk.default_model)
        } else {
            (self.default_provider.clone(), self.default_model.clone())
        };

        // ── config.toml = behavior only ─────────────────────────────────────
        // Secrets live in `credentials.toml`, provider instances in
        // `providers.toml`; neither is touched here. `default_provider` /
        // `default_model` only *reference* instance ids.
        let mut out = self.clone();
        out.default_provider = default_provider;
        out.default_model = default_model;
        let bytes = toml::to_string_pretty(&out)?.into_bytes();
        fsutil::atomic_write_bytes(&config_path, &bytes)?;
        Ok(())
    }

    pub fn config_file_path() -> PathBuf {
        paths::get().config_file()
    }

    pub fn history_file_path() -> PathBuf {
        paths::get().history_file()
    }

    pub fn load_history() -> Vec<neenee_contracts::HistoryEntry> {
        let path = Self::history_file_path();
        if let Ok(content) = fs::read_to_string(path) {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    /// Persist `history` into the global history file, locking and merging
    /// against what is already on disk so a concurrent process's recent
    /// prompts survive this write (ADR-0018).
    ///
    /// `dedup` selects the merge identity (see
    /// [`neenee_contracts::merge_history`]): `true` collapses identical prompt
    /// text into one entry across sessions, `false` keeps `(text, session_id)`
    /// entries distinct. Callers pass the live `[input_history] dedup` value
    /// rather than having this method re-read `config.toml` on every send.
    pub fn save_history(
        history: &[neenee_contracts::HistoryEntry],
        dedup: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let path = Self::history_file_path();
        // Serialise against other `neenee` instances and merge so a concurrent
        // process's recent commands survive this write (ADR-0018). Without the
        // lock + reload the last writer would erase the other's history; the
        // merge keeps the newest recorded timestamp for each survivor and
        // re-orders newest-first.
        let _lock = fsutil::FileLock::acquire(&path)
            .map_err(|e| format!("could not lock history file: {e}"))?;
        let existing: Vec<neenee_contracts::HistoryEntry> = fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default();
        let merged = neenee_contracts::merge_history(&existing, history, dedup);
        fsutil::atomic_write_json(&path, &merged).map_err(Box::<dyn std::error::Error>::from)?;
        Ok(())
    }

    /// Truncate the global history file (the Ctrl+R picker's "clear history"
    /// action). Locked like [`Self::save_history`] so a concurrent write does
    /// not race the truncation; a concurrent *record* can still append a new
    /// entry immediately after — clearing is inherently last-writer-wins for
    /// the on-disk union, exactly like the rest of the history writes.
    pub fn clear_history() -> Result<(), Box<dyn std::error::Error>> {
        let path = Self::history_file_path();
        let _lock = fsutil::FileLock::acquire(&path)
            .map_err(|e| format!("could not lock history file: {e}"))?;
        fsutil::atomic_write_json(&path, &Vec::<neenee_contracts::HistoryEntry>::new())
            .map_err(Box::<dyn std::error::Error>::from)?;
        Ok(())
    }
}

/// Load all valid custom theme files (`*.toml`) from the given themes directory.
///
/// Each file defines a named theme with metadata (`name`, `description`, etc.)
/// and full or partial color definitions. Errors reading individual files are
/// ignored so a single malformed file does not break theme discovery.
pub fn load_theme_files(themes_dir: &std::path::Path) -> Vec<neenee_contracts::ThemeFile> {
    let mut themes = Vec::new();
    let Ok(entries) = std::fs::read_dir(themes_dir) else {
        return themes;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("toml")
            && let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(mut theme) = toml::from_str::<neenee_contracts::ThemeFile>(&content)
        {
            if theme.id.is_empty()
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                theme.id = stem.to_string();
            }
            themes.push(theme);
        }
    }
    themes.sort_by(|a, b| a.name.cmp(&b.name));
    themes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untrusted_hardening_locks_autopilot_confirm_and_prepends_rule() {
        // A default (trusted-shape) policy allows autopilot-confirm to be
        // configured; hardening for an untrusted project forces Deny and
        // prepends exactly one `confirm` rule for fetch/install/pipe-to-shell.
        let base = BashPolicyConfig {
            autopilot_confirm: BashPolicyAutopilotAction::Allow, // user opted in
            ..BashPolicyConfig::default()
        };
        let hardened = base.clone().with_untrusted_hardening();

        assert_eq!(
            hardened.autopilot_confirm,
            BashPolicyAutopilotAction::Deny,
            "untrusted project must not auto-proceed confirm-gated commands"
        );
        // Exactly one hardening rule prepended.
        assert!(
            hardened
                .rules
                .iter()
                .any(|r| r.name == "untrusted-project confirm"),
            "hardening rule present"
        );
        assert_eq!(
            hardened.rules.len(),
            base.rules.len() + 1,
            "only the hardening rule was added"
        );
        // The hardening rule is first (prepended) so it is evaluated before any
        // user rule.
        assert_eq!(hardened.rules[0].name, "untrusted-project confirm");
        assert_eq!(hardened.rules[0].action, BashPolicyActionConfig::Confirm);
    }

    #[test]
    fn untrusted_hardening_matches_common_injection_payloads() {
        // The hardening rule's pattern must target the classic untrusted-repo
        // payloads. The regex itself is compiled and exercised in the agent
        // crate (which owns `regex`); here we assert the pattern text covers
        // the expected command families so a refactor cannot silently narrow it.
        let hardened = BashPolicyConfig::default().with_untrusted_hardening();
        let rule = hardened
            .rules
            .iter()
            .find(|r| r.name == "untrusted-project confirm")
            .expect("hardening rule");
        let pattern = rule.pattern.as_str();

        // Each token anchors one payload family the rule must catch.
        for needle in [
            "npm\\s+(?:install",
            "npx\\s+-y",
            "pip3?\\s+install",
            "uv\\s+(?:pip\\s+)?install",
            "cargo\\s+(?:add|install)",
            "go\\s+get",
            "brew\\s+install",
            "apt",
            "curl",
            "wget",
            "\\|\\s*(?:sh|bash|zsh|python3?|ruby|perl)",
        ] {
            assert!(
                pattern.contains(needle),
                "hardening pattern missing {needle:?} (got: {pattern})"
            );
        }
    }

    #[test]
    fn agent_table_round_trips_through_toml() {
        // The `[principal]` table must round-trip: partial TOML keeps defaults,
        // full TOML preserves explicit overrides. Legacy `[agent.review]`
        // sub-tables (ADR-0016) are accepted but ignored — `hard_stop_turns`
        // now lives directly under `[principal]` (ADR-0018).
        let toml_full = r#"
            [principal]
            hard_stop_turns = 40
        "#;
        let cfg: Config = toml::from_str(toml_full).unwrap();
        assert_eq!(cfg.principal.hard_stop_turns, 40);

        // Missing `[principal]` table → defaults match the documented values.
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.principal.hard_stop_turns, 0);

        // A legacy `[agent.review]` block no longer maps to anything; it must
        // not break parsing (unknown sub-tables are ignored) and the new
        // direct field still round-trips.
        let toml_legacy = r#"
            [agent.review]
            review_start_turn = 64
            hard_stop_turns = 99
        "#;
        let cfg: Config = toml::from_str(toml_legacy).unwrap();
        assert_eq!(cfg.principal.hard_stop_turns, 0);

        // Round-trip through save+load format (serialize then parse).
        let mut cfg = Config::default();
        cfg.principal.hard_stop_turns = 99;
        let serialised = toml::to_string(&cfg).unwrap();
        let parsed: Config = toml::from_str(&serialised).unwrap();
        assert_eq!(parsed.principal.hard_stop_turns, 99);
    }

    #[test]
    fn compaction_round_count_writes_canonical_key_and_drops_legacy_key() {
        // ADR-0120 policy: the pre-ADR-0047 key is not aliased. It parses as
        // an unknown key (warned and ignored) and the field stays at its
        // default — the stale value must not carry through.
        let legacy: Config = toml::from_str("compaction_preserve_turns = 9").unwrap();
        assert_eq!(
            legacy.compaction_preserve_rounds,
            Config::default().compaction_preserve_rounds
        );

        let serialized = toml::to_string(&legacy).unwrap();
        assert!(serialized.contains("compaction_preserve_rounds ="));
        assert!(!serialized.contains("compaction_preserve_turns ="));
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
            bash = "strict"

            [tool_variants."glm-5.2"]
            read_text = "verbose"
        "#;
        let cfg: Config = toml::from_str(toml_src).unwrap();

        // Known model → its map; unlisted capability within a known model → absent.
        let kimi = cfg.tool_variants.for_model("kimi-k2.7-code");
        assert_eq!(kimi.get("read_text").map(String::as_str), Some("terse"));
        assert_eq!(kimi.get("bash").map(String::as_str), Some("strict"));
        assert!(kimi.get("grep").is_none());

        // A different model gets its own independent map.
        let glm = cfg.tool_variants.for_model("glm-5.2");
        assert_eq!(glm.get("read_text").map(String::as_str), Some("verbose"));
        assert!(glm.get("bash").is_none());

        // Unknown model → empty (but borrowable without an Option).
        assert!(cfg.tool_variants.for_model("does-not-exist").is_empty());

        // Absent table entirely → empty config, every lookup is empty.
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.tool_variants.for_model("kimi-k2.7-code").is_empty());
    }

    #[test]
    fn tool_variants_round_trip_through_serialise() {
        let mut cfg = Config::default();
        let mut sel = neenee_contracts::VariantSelection::new();
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
        let tmp = std::env::temp_dir().join(format!("neenee-creds-{}", uuid::Uuid::new_v4()));
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
    fn discovery_cache_route_settings_and_remote_round_trip() {
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        let mut cache = DiscoveryCache::default();
        cache.provider_models.insert(
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
        cache
            .route_settings_for_mut("deepseek", "deepseek-v4-flash")
            .effort = Some("high".to_string());
        cache.save().unwrap();

        let mut reloaded = DiscoveryCache::load();
        assert_eq!(
            reloaded
                .route_settings_for("deepseek", "deepseek-v4-flash")
                .and_then(|r| r.effort.as_deref()),
            Some("high")
        );
        assert_eq!(
            reloaded.fitted_models["kimi"]["kimi-for-coding"].context_window,
            262_144
        );
        assert!(reloaded.route_settings_for("deepseek", "nope").is_none());

        reloaded.remove_instance("deepseek");
        assert!(reloaded.provider_models.is_empty());
        assert!(reloaded.route_settings.is_empty());
        reloaded.save().unwrap();
        assert!(DiscoveryCache::load().provider_models.is_empty());

        paths::set_test_default(None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn config_save_is_behavior_only_and_tolerates_legacy_provider_tables() {
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        // A pre-refactor config.toml still carrying `[[providers]]` and legacy
        // key fields loads fine (unknown keys are ignored) and re-saves as
        // behavior-only — the legacy tables are not re-emitted.
        std::fs::write(
            tmp.join("config.toml"),
            r#"default_provider = "deepseek"
deepseek_api_key = "legacy-key"
[[providers]]
id = "deepseek"
name = "DeepSeek"
"#,
        )
        .unwrap();
        let loaded = Config::load();
        assert_eq!(loaded.default_provider, "deepseek");
        let mut cfg = loaded;
        cfg.default_provider = "zai".to_string();
        cfg.save().unwrap();
        let on_disk = std::fs::read_to_string(tmp.join("config.toml")).unwrap();
        assert!(on_disk.contains("default_provider = \"zai\""));
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
        };
        assert!(bare.is_empty());
        let with_effort = RouteSettings {
            effort: Some("high".to_string()),
            thinking: None,
        };
        assert!(!with_effort.is_empty());
        let with_thinking = RouteSettings {
            effort: None,
            thinking: Some(false),
        };
        assert!(!with_thinking.is_empty());
    }

    #[test]
    fn tui_click_outside_dismiss_defaults_on_and_overrides() {
        // The click-outside-to-dismiss pref is ON by default: clicking the
        // backdrop of a dismissable overlay closes it like Esc (the draft is
        // parked, so nothing is lost). The startup-picker quit path and the
        // never-dismissable "precious input" modals are gated elsewhere
        // (`Modal::dismissable_by_outside_click`), not by this flag.
        let cfg: Config = toml::from_str("").unwrap();
        assert!(
            cfg.tui.click_outside_dismiss,
            "click_outside_dismiss defaults to true"
        );

        // Explicit opt-out round-trips.
        let toml = r#"
            [tui]
            click_outside_dismiss = false
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(!cfg.tui.click_outside_dismiss);
    }

    #[test]
    fn tui_expand_auto_scroll_defaults_off_and_overrides() {
        // The disclosure auto-scroll pref is OFF by default: toggling a
        // card's expansion is a read interaction, so the scroll offset stays
        // where the user put it. Users who want the content-maximizing
        // behavior (expanded header moves toward the viewport top) opt in.
        let cfg: Config = toml::from_str("").unwrap();
        assert!(
            !cfg.tui.expand_auto_scroll,
            "expand_auto_scroll defaults to false"
        );

        // Explicit opt-in round-trips.
        let toml = r#"
            [tui]
            expand_auto_scroll = true
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(cfg.tui.expand_auto_scroll);
    }

    #[test]
    fn input_history_defaults_dedup_on_and_commands_off() {
        // A config with no `[input_history]` table gets the sensible defaults:
        // dedup on (one row per prompt text) and slash-command recording off.
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.input_history.dedup, "dedup defaults to true");
        assert!(
            !cfg.input_history.record_commands,
            "record_commands defaults to false"
        );

        // Both keys parse and round-trip.
        let toml = r#"
            [input_history]
            dedup = false
            record_commands = true
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(!cfg.input_history.dedup);
        assert!(cfg.input_history.record_commands);

        // Serialising back keeps the explicit table.
        let out = toml::to_string(&cfg).unwrap();
        assert!(out.contains("[input_history]"));
        assert!(out.contains("dedup = false"));
        assert!(out.contains("record_commands = true"));
    }

    // --- project-scope MCP merge (ADR-0085 §2/§3) --------------------------

    fn scratch_project_root() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("neenee-project-mcp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".neenee")).unwrap();
        dir
    }

    #[test]
    fn load_project_mcp_reads_neenee_config_table() {
        let root = scratch_project_root();
        std::fs::write(
            root.join(".neenee/config.toml"),
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

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_project_mcp_is_empty_when_file_absent() {
        let root = scratch_project_root();
        // No config.toml written.
        let mcp = Config::load_project_mcp(&root);
        assert!(mcp.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_project_mcp_ignores_unrelated_keys_and_bad_toml() {
        let root = scratch_project_root();
        // A project file may carry non-mcp tables; only [mcp.*] is projected,
        // and an invalid [mcp.*] makes the whole table drop (warn + empty).
        std::fs::write(
            root.join(".neenee/config.toml"),
            r#"
                [principal]
                hard_stop_turns = 7

                [mcp.ok]
                command = ["x"]
            "#,
        )
        .unwrap();
        let mcp = Config::load_project_mcp(&root);
        assert_eq!(mcp.len(), 1, "principal ignored, mcp.ok projected");
        let _ = std::fs::remove_dir_all(&root);

        // A structurally invalid TOML → empty (never panics).
        let root2 = scratch_project_root();
        std::fs::write(root2.join(".neenee/config.toml"), "this is = = not toml").unwrap();
        assert!(Config::load_project_mcp(&root2).is_empty());
        let _ = std::fs::remove_dir_all(&root2);
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
            root.join(".neenee/config.toml"),
            r#"
                [[hooks]]
                event   = "PostToolUse"
                matcher = "Write|Edit"
                command = ".neenee/hooks/lint.sh"

                [[hooks]]
                event   = "Stop"
                command = ".neenee/hooks/notify.sh"
            "#,
        )
        .unwrap();

        let hooks = Config::load_project_hooks(&root);
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].event, HookEventKind::PostToolUse);
        assert_eq!(hooks[0].command, ".neenee/hooks/lint.sh");
        assert_eq!(hooks[1].event, HookEventKind::Stop);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_project_hooks_is_empty_when_file_absent() {
        let root = scratch_project_root();
        // No config.toml written.
        assert!(Config::load_project_hooks(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_project_hooks_ignores_unrelated_keys_and_bad_toml() {
        let root = scratch_project_root();
        // A project file may carry non-hooks tables; only [[hooks]] projects.
        std::fs::write(
            root.join(".neenee/config.toml"),
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
        let _ = std::fs::remove_dir_all(&root);

        // Structurally invalid TOML → empty (never panics).
        let root2 = scratch_project_root();
        std::fs::write(root2.join(".neenee/config.toml"), "this is = = not toml").unwrap();
        assert!(Config::load_project_hooks(&root2).is_empty());
        let _ = std::fs::remove_dir_all(&root2);
    }

    #[test]
    fn merge_project_hooks_appends_to_global() {
        let mut global = Config::default();
        global.hooks.push(HookSpec {
            event: HookEventKind::Stop,
            matcher: None,
            command: "global-notify.sh".to_string(),
        });
        let project_hooks = vec![HookSpec {
            event: HookEventKind::PostToolUse,
            matcher: Some("Write".to_string()),
            command: ".neenee/hooks/lint.sh".to_string(),
        }];
        global.merge_project_hooks(project_hooks);
        assert_eq!(global.hooks.len(), 2, "global + project appended");
        // Global hook ordering preserved; project hooks come after.
        assert_eq!(global.hooks[0].command, "global-notify.sh");
        assert_eq!(global.hooks[1].command, ".neenee/hooks/lint.sh");
    }

    #[test]
    fn load_theme_files_reads_and_sorts_valid_toml() {
        let root = scratch_project_root();
        let themes_dir = root.join("themes");
        std::fs::create_dir_all(&themes_dir).unwrap();

        let theme_a = r##"
name = "Dracula"
description = "Vampire dark palette"
[colors]
background = "#282a36"
surface = "#44475a"
text = "#f8f8f2"
muted = "#6272a4"
accent = "#bd93f9"
success = "#50fa7b"
warning = "#ffb86c"
error = "#ff5555"
"##;

        let theme_b = r##"
name = "Cyberpunk"
description = "Neon high-contrast"
[colors]
background = "#050505"
surface = "#151515"
text = "#ffffff"
muted = "#808080"
accent = "#00ffff"
success = "#00ff00"
warning = "#ffff00"
error = "#ff0055"

[components.input]
bg_active = "#222222"
caret = "#00ffff"

[components.crate]
fg = "#ff00ff"
"##;

        std::fs::write(themes_dir.join("dracula.toml"), theme_a).unwrap();
        std::fs::write(themes_dir.join("cyberpunk.toml"), theme_b).unwrap();
        std::fs::write(themes_dir.join("corrupt.toml"), "invalid [== toml").unwrap();
        std::fs::write(themes_dir.join("readme.txt"), "not a theme").unwrap();

        let loaded = load_theme_files(&themes_dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "Cyberpunk");
        assert_eq!(loaded[0].id, "cyberpunk");
        assert_eq!(loaded[1].name, "Dracula");
        assert_eq!(loaded[1].id, "dracula");
        let cyberpunk_components = loaded[0].components.as_ref().unwrap();
        assert_eq!(
            cyberpunk_components
                .input
                .as_ref()
                .unwrap()
                .caret
                .as_deref(),
            Some("#00ffff")
        );
        assert_eq!(
            cyberpunk_components
                .crate_component
                .as_ref()
                .unwrap()
                .fg
                .as_deref(),
            Some("#ff00ff")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
