//! Snapshot-driven provider/model picker filter & sort logic.
//!
//! The pickers render directly from [`neenee_core::ProviderPickerSnapshot`] — one
//! [`neenee_core::ProviderPickerRow`] per provider the harness knows how to
//! drive, carrying the display name, the served model ids, the active model, and
//! the live per-user signals (favorite, key-ready, last-used). Built-in and
//! user-defined providers share this single path, so a custom provider added via
//! the editor shows up like any built-in (there is no separate static table).
//!
//! Two surfaces read the same snapshot:
//!
//! - **Connections** (`/connections`): [`providers_filtered_from`] builds the
//!   provider-instance list — the management surface (favorite, edit, delete,
//!   add). Activating a provider activates its current model.
//! - **Models** (`/models`, `Ctrl+M`): [`models_flat_filtered_from`] builds a
//!   **flat** list of every (provider, model) pair — the daily-driver switch
//!   surface. There is no drilling: one row per pair, Enter activates.

use neenee_core::{
    ChannelAuth, ProviderModelInfo, ProviderPickerSnapshot, WireFormat, baseline_models,
    resolve_model,
};

use crate::tui::fuzzy;

/// One editable field of the provider editor. The visible set is chosen by the
/// active [`ProviderTemplate`] (create) or the edited provider's protocol (edit),
/// rather than a fixed five-field form. Provider-owned model collections are
/// imported from `neenee_providers`; this view layer only selects and renders
/// those curated values.
///
/// Reasoning (effort/thinking) is intentionally NOT a provider-editor field —
/// ADR-0046 moved it to the per-model `e` editor in the Models picker, so a
/// provider is
/// created/authed here and its models are reasoned with (or not) individually.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CustomField {
    Name,
    BaseUrl,
    Token,
    Model,
}

/// A curated starting point for adding a user-defined provider. The protocol is
/// pre-locked (no protocol picker), the relay's model list is seeded, and the
/// URL placeholder shows the expected endpoint shape. Modelled as *data* — one
/// table entry per template — mirroring `neenee_providers::OPENAI_PROVIDER_SPECS`.
pub struct ProviderTemplate {
    /// Stable identifier shared with the matching entry in
    /// `neenee_providers::PROVIDER_TEMPLATE_SPECS`. Persisted on the created
    /// instance as `template_id` so the catalog can re-seed the instance from
    /// this template's *current* model list on later startups. MUST match the
    /// spec's `id` 1:1 and never change once shipped.
    pub id: &'static str,
    /// List label, e.g. `"Custom Anthropic (Claude relay)"`.
    pub label: &'static str,
    /// One-line description shown under the label in the chooser.
    pub description: &'static str,
    /// Wire protocol sent in `AgentRequest::AddProvider`: `"openai"` |
    /// `"anthropic"` | `"gemini"`.
    pub protocol: &'static str,
    /// Models seeded as channels. Empty means the user enters one via the Model
    /// field (templates can opt in when they need one).
    pub models: &'static [&'static str],
    /// Whether the editor shows a Base URL field (false for native Gemini).
    pub needs_url: bool,
    /// Placeholder shown in the Base URL field — the full endpoint shape.
    pub url_hint: &'static str,
    /// Whether the editor exposes a free-text Model field. Most templates seed
    /// `models`; open protocols can still add arbitrary model ids later.
    pub needs_model: bool,
    /// A concrete relay endpoint pre-filled into the Base URL field on open
    /// (create mode), so a relay-specific template works without the user
    /// typing the host. `None` for the generic templates — their `url_hint` is
    /// a placeholder only and the field starts empty, since the user supplies
    /// their own relay host. When set, the user can still edit the value.
    pub default_url: Option<&'static str>,
    /// Template-specific user agent. Most providers use the default agent, but
    /// the coding-plan endpoints validate this header.
    pub user_agent: Option<&'static str>,
    /// How seeded channels authenticate. `XaiOAuth` starts browser OAuth
    /// before the name editor (OAuth-first add flow).
    pub auth: neenee_core::ChannelAuth,
}

impl ProviderTemplate {
    /// The ordered, visible editor fields for this template (create mode).
    /// OAuth templates only ask for the instance name (auth already completed).
    pub fn fields(&self) -> Vec<CustomField> {
        if self.auth.is_oauth() {
            return vec![CustomField::Name];
        }
        let mut fields = vec![CustomField::Name];
        if self.needs_url {
            fields.push(CustomField::BaseUrl);
        }
        fields.push(CustomField::Token);
        if self.needs_model {
            fields.push(CustomField::Model);
        }
        // No Effort/Thinking: ADR-0046 made reasoning a per-model concern.
        fields
    }

    /// Whether selecting this template starts OAuth before the name editor.
    pub fn oauth_first(&self) -> bool {
        self.auth.is_oauth()
    }
}

/// The provider templates offered when adding a provider, in display order.
pub const PROVIDER_TEMPLATES: &[ProviderTemplate] = &[
    ProviderTemplate {
        id: "openai",
        label: "OpenAI",
        description: "OpenAI API — GPT-5.5 family",
        protocol: "openai",
        models: neenee_providers::OPENAI_BUILTIN_MODELS,
        // Official endpoint: the base URL is fixed and pre-filled from
        // `default_url`, so the editor hides the Base URL field. The model
        // collection (`OPENAI_BUILTIN_MODELS`) is seeded as channels — the
        // user only supplies a name and token, and the Models picker lists
        // the served models.
        needs_url: false,
        url_hint: "https://api.openai.com/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.openai.com/v1/chat/completions"),
        user_agent: None,
        auth: neenee_core::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "anthropic",
        label: "Anthropic",
        description: "Claude models over the Anthropic /messages API",
        protocol: "anthropic",
        models: neenee_providers::ANTHROPIC_BUILTIN_MODELS,
        needs_url: true,
        url_hint: "https://api.anthropic.com/v1/messages",
        needs_model: false,
        default_url: Some("https://api.anthropic.com/v1/messages"),
        user_agent: None,
        auth: neenee_core::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "google",
        label: "Google Gemini",
        description: "Native Gemini API — Google AI Studio or compatible relay",
        protocol: "gemini",
        models: neenee_providers::GOOGLE_BUILTIN_MODELS,
        needs_url: true,
        url_hint: "https://generativelanguage.googleapis.com/v1beta",
        needs_model: false,
        default_url: Some("https://generativelanguage.googleapis.com/v1beta"),
        user_agent: None,
        auth: neenee_core::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "deepseek",
        label: "DeepSeek",
        description: "DeepSeek V4 Flash + Pro over OpenAI chat completions",
        protocol: "openai",
        models: neenee_providers::DEEPSEEK_BUILTIN_MODELS,
        // Official endpoint: the base URL is fixed and pre-filled from
        // `default_url`, so the editor hides the Base URL field — the user
        // only supplies a name and token.
        needs_url: false,
        url_hint: "https://api.deepseek.com/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.deepseek.com/v1/chat/completions"),
        user_agent: None,
        auth: neenee_core::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "xai-oauth",
        label: "xAI OAuth",
        description: "Grok 4.x via SuperGrok subscription (browser OAuth)",
        protocol: "openai",
        models: neenee_providers::XAI_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://api.x.ai/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.x.ai/v1/chat/completions"),
        user_agent: None,
        auth: neenee_core::ChannelAuth::XaiOAuth,
    },
    ProviderTemplate {
        id: "chatgpt-oauth",
        label: "ChatGPT OAuth",
        description: "GPT-5.x via ChatGPT Pro/Plus subscription (browser OAuth)",
        protocol: "openai",
        models: neenee_providers::CHATGPT_BUILTIN_MODELS,
        // The Responses backend URL is fixed and pre-filled; the editor hides
        // the Base URL field. Auth completes via OAuth before the name editor.
        needs_url: false,
        url_hint: "https://chatgpt.com/backend-api/codex/responses",
        needs_model: false,
        default_url: Some("https://chatgpt.com/backend-api/codex/responses"),
        user_agent: None,
        auth: neenee_core::ChannelAuth::ChatGptOAuth,
    },
    ProviderTemplate {
        id: "copilot-oauth",
        label: "Copilot OAuth",
        description: "GPT-4o/5.x via GitHub Copilot subscription (device OAuth)",
        protocol: "openai",
        models: neenee_providers::COPILOT_SEED_MODELS,
        // Copilot's chat-completions backend is fixed and universally available
        // (every plan incl. Free/Student can speak it); the editor hides the
        // Base URL field. Login is the GitHub device flow.
        needs_url: false,
        url_hint: "https://api.githubcopilot.com/chat/completions",
        needs_model: false,
        default_url: Some("https://api.githubcopilot.com/chat/completions"),
        user_agent: None,
        auth: neenee_core::ChannelAuth::CopilotOAuth,
    },
    ProviderTemplate {
        id: "kimi-code",
        label: "Kimi Code",
        description: "Moonshot Kimi coding-plan endpoint",
        protocol: "openai",
        models: neenee_providers::KIMI_CODE_MODELS,
        // Official endpoint: base URL is fixed and pre-filled, no field shown.
        needs_url: false,
        url_hint: "https://api.kimi.com/coding/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.kimi.com/coding/v1/chat/completions"),
        user_agent: Some("opencode/0.1.0"),
        auth: neenee_core::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "zai-code",
        label: "ZAI Code",
        description: "Z.AI coding-plan endpoint",
        protocol: "openai",
        models: neenee_providers::ZAI_CODE_MODELS,
        // Official endpoint: base URL is fixed and pre-filled, no field shown.
        needs_url: false,
        url_hint: "https://api.z.ai/api/coding/paas/v4/chat/completions",
        needs_model: false,
        default_url: Some("https://api.z.ai/api/coding/paas/v4/chat/completions"),
        user_agent: Some("opencode/1.17.10"),
        auth: neenee_core::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "opencode-go",
        label: "OpenCode Go",
        description: "opencode.ai relay — OpenAI chat-completions coding models",
        protocol: "openai",
        models: neenee_providers::OPENCODE_GO_MODELS,
        needs_url: true,
        url_hint: "https://opencode.ai/zen/go/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://opencode.ai/zen/go/v1/chat/completions"),
        user_agent: None,
        auth: neenee_core::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "anthropic-sub2api",
        label: "Anthropic (sub2api)",
        description: "Anthropic sub2api relay",
        protocol: "anthropic",
        models: neenee_providers::ANTHROPIC_BUILTIN_MODELS,
        needs_url: true,
        url_hint: "https://relay.example.com/v1/messages",
        needs_model: false,
        default_url: None,
        user_agent: None,
        auth: neenee_core::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "openai-sub2api",
        label: "OpenAI (sub2api)",
        description: "OpenAI sub2api relay",
        protocol: "openai",
        models: neenee_providers::OPENAI_SUB2API_MODELS,
        needs_url: true,
        url_hint: "https://relay.example.com/v1/chat/completions",
        needs_model: false,
        default_url: None,
        user_agent: None,
        auth: neenee_core::ChannelAuth::ApiKey,
    },
    // Antigravity — a sub2api-style Gemini-native 中转站. The relay forwards
    // model ids verbatim to the Gemini REST surface, so the `gemini` protocol
    // reaches it unchanged. The base URL is editable and pre-filled with a
    // documentation-safe example; users can replace it with their relay host.
    // The three effort-tiered / non-preview ids are seeded as channels — they
    // resolve in the model registry, so the Models picker and add-model overlay
    // see real metadata.
    //
    // Model order is deliberate: `AddProvider` activates the FIRST seeded
    // model as the default, and `gemini-3.1-pro-high` is known to be rejected
    // by some relays for every request shape (HTTP 400 INVALID_ARGUMENT — a
    // relay-side defect, not a config issue; `-low` and `flash` often work).
    // So the generally compatible models lead and `-high` sits last, still
    // selectable from the Models picker the moment the relay accepts it.
    ProviderTemplate {
        id: "antigravity-sub2api",
        label: "Antigravity (sub2api)",
        description: "Antigravity sub2api relay",
        protocol: "gemini",
        models: neenee_providers::ANTIGRAVITY_SUB2API_MODELS,
        needs_url: true,
        url_hint: "https://relay.example.com/antigravity/v1beta",
        needs_model: false,
        default_url: Some("https://relay.example.com/antigravity/v1beta"),
        user_agent: None,
        auth: neenee_core::ChannelAuth::ApiKey,
    },
];

/// The editor header title for a create-mode provider with the given protocol
/// wire — the matching template's label (protocols are unique across templates),
/// falling back to a generic header.
pub fn provider_template_label_for(protocol: &str) -> String {
    PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.protocol == protocol)
        .map(|t| t.label.to_string())
        .unwrap_or_else(|| "＋ Add connection".to_string())
}

/// The ordered editor fields shown when **editing** an existing user provider.
/// For an API-key channel the form offers Name, Base URL, and Token (the Model
/// field is omitted — models, and their per-model reasoning, ADR-0046, are
/// managed in the Models picker). For an OAuth channel (ChatGPT/Codex or xAI)
/// only Name is editable: the Base URL and Token are fixed by the auth flow
/// (e.g. `https://api.x.ai/...`, `https://chatgpt.com/backend-api/codex/...`)
/// and must not be hand-edited, so a rename is the only safe operation.
pub fn edit_fields(protocol: &str, auth: ChannelAuth) -> Vec<CustomField> {
    let _ = protocol;
    if auth.is_oauth() {
        vec![CustomField::Name]
    } else {
        vec![CustomField::Name, CustomField::BaseUrl, CustomField::Token]
    }
}

/// Whether a protocol's model set is *closed*: the candidate list is the full,
/// fixed set and the add-model overlay must NOT offer a free-text fallback.
/// OpenAI and Anthropic relays serve an open, evolving model set, so
/// typing an unlisted id is legitimate; native Gemini is a closed family — its
/// models are enumerated by Google and forwarded verbatim by relays, so an
/// arbitrary id is almost certainly a typo or hallucination, not a real model.
#[allow(dead_code)]
pub fn protocol_model_set_closed(protocol_wire: &str) -> bool {
    matches!(protocol_wire, "gemini")
}

/// The registry model ids that match a custom protocol's wire format, used as the
/// candidate list when picking a model for a custom provider (the "list select"
/// half of "list select + custom fallback"). An unknown protocol falls back to
/// the OpenAI set, which is also the default.
pub fn protocol_model_candidates(protocol_wire: &str) -> Vec<&'static str> {
    let format = match protocol_wire {
        "anthropic" => WireFormat::AnthropicCompat,
        "gemini" => WireFormat::Google,
        _ => WireFormat::OpenAi,
    };
    let mut seen = std::collections::HashSet::new();
    baseline_models()
        .filter(|m| m.format == format)
        .map(|m| m.id)
        // Deduplicate: a model id can appear in multiple provider tables (e.g.
        // gpt-4o-mini in both `openai` and `copilot`), and inventory iteration
        // order is not guaranteed, so without dedup the candidate list — and
        // thus the first-match the picker commits — would be non-deterministic.
        .filter(|id| seen.insert(*id))
        .collect()
}

/// Human-readable model name for the hint bar / status surfaces.
///
/// Resolves the wire model id through the [`neenee_core::model`] registry so
/// the always-visible indicator shows the actual model the user is talking to
/// (e.g. `GLM-5.2`, `Kimi K2.7 Code`), not the provider preset. Falls back to
/// the raw model id for unknown models (custom / local), where the id is the
/// only label available.
pub fn model_display_name(model: &str) -> String {
    let resolved = resolve_model(model);
    if resolved.name.is_empty() {
        model.to_string()
    } else {
        resolved.name.to_string()
    }
}

/// The context window (in tokens) of a model id, resolved from the registry.
/// Returns `0` for unknown models. Replaces the former `provider_context_window`
/// now that the picker carries the active model id directly.
pub fn model_context_window(model: &str) -> usize {
    resolve_model(model).context_window
}

/// One selectable row in the **flat model picker** ([`Modal::Models`]
/// equivalent): a single (provider, model) pair drawn from anywhere in the
/// snapshot. Built by [`models_flat_filtered_from`]; the picker browses,
/// searches, and activates these directly — there is no drill-in stage.
pub struct RankedModel {
    /// Canonical id of the provider serving this model (its snapshot row id).
    pub provider_id: String,
    /// Wire model id to activate.
    pub model: String,
    /// The rendered label — the model's display name. The fuzzy match indexes
    /// directly onto these characters (the provider suffix is rendered but
    /// never matched).
    pub label: String,
    /// The provider's display name, rendered as the dim `· <provider>` suffix
    /// so identical model ids served by different instances stay
    /// distinguishable in the flat list.
    pub provider_label: String,
    /// Model-specific controls surfaced by the picker snapshot. OpenAI rows
    /// can expose effort; Anthropic rows can expose effort plus thinking.
    pub effort: Option<String>,
    pub thinking: Option<bool>,
    /// Unix epoch milliseconds of this model's last activation (`None` = never,
    /// sorts as oldest). Drives the flat list's recency sort.
    pub last_used_ms: Option<u64>,
    /// The fuzzy match against `label`, or `None` in browse mode (empty query)
    /// — and also when the row was included because its PROVIDER name matched
    /// the query but the model label did not (shown unhighlighted).
    pub m: Option<fuzzy::FuzzyMatch>,
}

/// One selectable row in the **Connections** provider list. Carries everything
/// the renderer and input handler need (copied out of the snapshot row), so
/// neither re-indexes the snapshot.
pub struct RankedProvider {
    /// Canonical provider id.
    pub id: String,
    /// Display name (the fuzzy target; mirrors `label`).
    pub name: String,
    /// Active model wire id.
    pub model: String,
    /// Every model id this provider serves.
    pub models: Vec<String>,
    /// `true` for built-in presets, `false` for user-defined providers. Drives
    /// the built-in/custom grouping and whether `e` opens the full meta editor.
    pub builtin: bool,
    /// Whether the provider is favorited (mirrors the snapshot row).
    pub favorite: bool,
    /// The rendered label — the provider's display name.
    pub label: String,
    /// The fuzzy match against `label`, or `None` in browse mode (empty query).
    pub m: Option<fuzzy::FuzzyMatch>,
}

impl RankedProvider {
    /// Whether the provider hosts more than one model. Informational for the
    /// Connections list (the flat Models picker lists each pair individually,
    /// so no drill-in remains).
    #[allow(dead_code)]
    pub fn is_multi_model(&self) -> bool {
        self.models.len() > 1
    }
}

/// Most-recently-used-first ordering of two models by their last-activation
/// timestamps. `None` (never activated) sorts as oldest. The caller applies a
/// stable tiebreaker (catalog order / label) so this never has to.
fn model_order(a: Option<u64>, b: Option<u64>) -> std::cmp::Ordering {
    // Both present → descending; one present → it wins; neither → equal.
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// The favorite → last-used-desc → name ordering of the Connections provider
/// list. Pulls each provider's live signals from its snapshot row.
fn provider_order(
    picker: &ProviderPickerSnapshot,
    a_id: &str,
    b_id: &str,
    a_name: &str,
    b_name: &str,
) -> std::cmp::Ordering {
    let signal = |id: &str| {
        picker
            .rows
            .iter()
            .find(|r| r.id == id)
            .map(|r| (r.favorite, r.last_used_ms))
            .unwrap_or((false, None))
    };
    let (a_fav, a_used) = signal(a_id);
    let (b_fav, b_used) = signal(b_id);
    b_fav
        .cmp(&a_fav)
        .then_with(|| b_used.cmp(&a_used))
        .then_with(|| a_name.cmp(b_name))
}

/// Build the **Connections** provider rows: one per snapshot row,
/// fuzzy-filtered by `query` against the provider name and sorted favorite →
/// last-used → name. An empty `query` (browse mode) keeps every provider with
/// no match positions.
pub fn providers_filtered_from(
    picker: &ProviderPickerSnapshot,
    query: &str,
) -> Vec<RankedProvider> {
    let mut rows: Vec<RankedProvider> = Vec::new();
    for prow in picker.rows.iter() {
        let label = prow.name.clone();
        let m = if query.is_empty() {
            None
        } else {
            match fuzzy::fuzzy_match(&label, query) {
                Some(m) => Some(m),
                None => continue,
            }
        };
        rows.push(RankedProvider {
            id: prow.id.clone(),
            name: prow.name.clone(),
            model: prow.model.clone(),
            models: prow.models.clone(),
            builtin: prow.builtin,
            favorite: prow.favorite,
            label,
            m,
        });
    }
    rows.sort_by(|a, b| provider_order(picker, &a.id, &b.id, &a.name, &b.name));
    rows
}

/// Build the **flat Models** rows: one [`RankedModel`] per (provider, model)
/// pair across the entire snapshot — the daily-driver switch surface, with no
/// drill-in. Sorted provider-favorite first → model last-used desc (never-used
/// oldest) → provider name → model label, so the pairs of a favorited provider
/// cluster at the top and recently used models lead within it.
///
/// Fuzzy filtering matches `query` against the model label; when the label
/// does not match but the PROVIDER name fuzzy-matches, that provider's models
/// are included unhighlighted (`m = None`) so "show me everything Anthropic
/// serves" works from the same search box. Match positions always index onto
/// the model label's characters only.
pub fn models_flat_filtered_from(
    picker: &ProviderPickerSnapshot,
    query: &str,
) -> Vec<RankedModel> {
    let mut rows: Vec<RankedModel> = Vec::new();
    for prow in &picker.rows {
        // The provider-name fallback match is computed once per provider: when
        // it hits, every model of that provider is included (unhighlighted)
        // even if its own label does not match the query.
        let provider_matches =
            !query.is_empty() && fuzzy::fuzzy_match(&prow.name, query).is_some();
        for model in &prow.models {
            let info = prow
                .model_info
                .iter()
                .find(|info| info.model == *model)
                .cloned()
                .unwrap_or_else(|| ProviderModelInfo {
                    model: model.clone(),
                    ..ProviderModelInfo::default()
                });
            let label = model_display_name(model);
            let m = if query.is_empty() {
                None
            } else {
                match fuzzy::fuzzy_match(&label, query) {
                    Some(m) => Some(m),
                    // Label missed: keep the row only via the provider-name
                    // fallback, and then without highlight positions.
                    None if provider_matches => None,
                    None => continue,
                }
            };
            rows.push(RankedModel {
                provider_id: prow.id.clone(),
                model: model.clone(),
                label,
                provider_label: prow.name.clone(),
                effort: info.effort,
                thinking: info.thinking,
                last_used_ms: info.last_used_ms,
                m,
            });
        }
    }
    // Provider-favorite first (read from the snapshot row like `provider_order`
    // does), then per-model recency, then provider name and model label as
    // stable, deterministic tiebreakers.
    let favorite = |id: &str| {
        picker
            .rows
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.favorite)
            .unwrap_or(false)
    };
    rows.sort_by(|a, b| {
        favorite(&b.provider_id)
            .cmp(&favorite(&a.provider_id))
            .then_with(|| model_order(a.last_used_ms, b.last_used_ms))
            .then_with(|| a.provider_label.cmp(&b.provider_label))
            .then_with(|| a.label.cmp(&b.label))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use neenee_core::ProviderPickerRow;

    fn row(id: &str, name: &str, models: &[&str], builtin: bool) -> ProviderPickerRow {
        ProviderPickerRow {
            id: id.to_string(),
            name: name.to_string(),
            model: models.first().copied().unwrap_or("").to_string(),
            models: models.iter().map(|m| m.to_string()).collect(),
            model_info: Vec::new(),
            builtin,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            favorite: false,
            last_used_ms: None,
            auth: Default::default(),
        }
    }

    fn sample() -> ProviderPickerSnapshot {
        ProviderPickerSnapshot {
            default_id: "openai".to_string(),
            rows: vec![
                row("kimi-code", "Kimi Code", &["kimi-k2.7-code"], true),
                row("openai", "OpenAI", &["gpt-4o", "gpt-4o-mini"], true),
                row(
                    "anthropic",
                    "Anthropic",
                    &[
                        "claude-fable-5",
                        "claude-sonnet-5",
                        "claude-opus-4-8",
                        "claude-sonnet-4-6",
                    ],
                    true,
                ),
                row("my-relay", "My Relay", &["glm-5.2", "glm-5.1"], false),
            ],
        }
    }

    #[test]
    fn display_name_resolves_from_model_registry() {
        assert_eq!(model_display_name("glm-5.2"), "GLM-5.2");
        assert_eq!(model_display_name("gpt-4o"), "GPT-4o");
    }

    #[test]
    fn display_name_falls_back_to_raw_id_for_unknown_models() {
        assert_eq!(model_display_name("acme-7b"), "acme-7b");
    }

    #[test]
    fn protocol_candidates_filter_by_wire_format() {
        let openai = protocol_model_candidates("openai");
        assert!(openai.contains(&"gpt-4o"));
        // Anthropic-format models are excluded from the OpenAI candidate list.
        assert!(!openai.contains(&"claude-opus-4-8"));
        let anthropic = protocol_model_candidates("anthropic");
        assert!(anthropic.contains(&"claude-opus-4-8"));
        assert!(!anthropic.contains(&"gpt-4o"));
    }

    #[test]
    fn gemini_candidate_set_is_the_canonical_family() {
        // The native-Gemini candidate list mirrors the ids Google plus common
        // relays/中转站 serve — so a Custom Gemini provider offers real models,
        // not hallucinated preview ids. Image/embedding/video-only models are
        // excluded (an agent only consumes the text generateContent surface).
        let gemini = protocol_model_candidates("gemini");
        for id in [
            "gemini-3.5-flash",
            "gemini-3-pro-preview",
            "gemini-3-flash-preview",
            "gemini-3.1-pro-preview",
            "gemini-2.5-flash",
            "gemini-2.5-pro",
            "gemini-2.0-flash",
        ] {
            assert!(gemini.contains(&id), "gemini candidate set missing {id}");
        }
        // Image-generation variants must NOT be in the text agent's candidate set.
        assert!(
            !gemini.contains(&"gemini-2.5-flash-image"),
            "image-only model leaked into gemini candidates"
        );
    }

    #[test]
    fn gemini_candidate_set_includes_antigravity_relay_models() {
        // The Antigravity (sub2api) relay ids are registered as native-Gemini
        // baselines, so the add-model overlay for a Gemini provider offers
        // them (the closed-set policy has real candidates to pick from).
        let gemini = protocol_model_candidates("gemini");
        for id in [
            "gemini-3.1-pro-high",
            "gemini-3.1-pro-low",
            "gemini-3-flash",
        ] {
            assert!(
                gemini.contains(&id),
                "antigravity relay model {id} missing from gemini candidates"
            );
        }
    }

    #[test]
    fn antigravity_template_is_offered_with_prefilled_url_and_seeded_models() {
        // The Antigravity (sub2api) relay ships as a curated template so a user
        // adds it from "＋ Add connection" without editing config.toml. Its host
        // is fixed, so the base URL is pre-filled (`default_url`); the three
        // effort-tiered / non-preview ids are seeded; and it speaks the gemini
        // protocol (no free-text Model field — the closed family is the seed).
        let tmpl = PROVIDER_TEMPLATES
            .iter()
            .find(|t| t.label == "Antigravity (sub2api)")
            .expect("antigravity template offered in the chooser");
        assert_eq!(tmpl.protocol, "gemini");
        assert_eq!(tmpl.models, neenee_providers::ANTIGRAVITY_SUB2API_MODELS);
        assert_eq!(
            tmpl.default_url,
            Some("https://relay.example.com/antigravity/v1beta")
        );
        assert!(tmpl.needs_url, "exposes a Base URL field (pre-filled)");
        assert!(
            !tmpl.needs_model,
            "no free-text Model field — models are seeded"
        );
        // The editor fields are Name / Base URL / Token (no Model).
        assert_eq!(
            tmpl.fields(),
            vec![CustomField::Name, CustomField::BaseUrl, CustomField::Token]
        );
    }

    #[test]
    fn openai_sub2api_template_seeds_openai_text_models() {
        let tmpl = PROVIDER_TEMPLATES
            .iter()
            .find(|t| t.label == "OpenAI (sub2api)")
            .expect("openai sub2api template offered in the chooser");
        assert_eq!(tmpl.protocol, "openai");
        assert_eq!(tmpl.models, neenee_providers::OPENAI_SUB2API_MODELS);
        assert!(tmpl.needs_url, "relay URL is user-supplied");
        assert!(
            !tmpl.needs_model,
            "model list is seeded; add-model handles custom ids"
        );
        assert_eq!(
            tmpl.fields(),
            vec![CustomField::Name, CustomField::BaseUrl, CustomField::Token]
        );
        for id in ["gpt-5.5", "gpt-5.2", "gpt-5.2-chat-latest"] {
            assert!(
                protocol_model_candidates("openai").contains(&id),
                "OpenAI candidate set missing {id}"
            );
        }
    }

    #[test]
    fn builtin_templates_prefill_official_urls_generic_relays_do_not() {
        // Provider-kind templates are now first-class instance templates and
        // pre-fill their official endpoint. Generic relay templates still leave
        // the URL empty because the user supplies the host.
        let builtin_labels = [
            "OpenAI",
            "Anthropic",
            "Google Gemini",
            "DeepSeek",
            "xAI OAuth",
            "ChatGPT OAuth",
            "Copilot OAuth",
            "Kimi Code",
            "ZAI Code",
            "OpenCode Go",
            "Antigravity (sub2api)",
        ];
        for t in PROVIDER_TEMPLATES {
            if builtin_labels.contains(&t.label) {
                assert!(t.default_url.is_some(), "{:?} should pre-fill", t.label);
            } else {
                assert!(
                    t.default_url.is_none(),
                    "{:?} generic relay must not pre-fill",
                    t.label
                );
            }
        }
    }

    #[test]
    fn gemini_model_set_is_closed_others_open() {
        // A closed set means the add-model overlay offers no free-text fallback:
        // the candidate list is the complete family, so an unmatched id is a
        // typo. OpenAI/Anthropic relays serve an open, evolving set, so typing
        // an unlisted id stays legitimate there.
        assert!(
            protocol_model_set_closed("gemini"),
            "native Gemini must be a closed model set"
        );
        assert!(
            !protocol_model_set_closed("openai"),
            "OpenAI relays keep an open model set"
        );
        assert!(
            !protocol_model_set_closed("anthropic"),
            "Anthropic relays keep an open model set"
        );
    }

    #[test]
    fn connections_lists_one_row_per_provider_including_custom() {
        let snapshot = sample();
        let rows = providers_filtered_from(&snapshot, "");
        assert_eq!(rows.len(), snapshot.rows.len());
        // The user-defined provider shows up like any built-in.
        assert!(rows.iter().any(|r| r.id == "my-relay"));
    }

    #[test]
    fn connections_fuzzy_filters_by_provider_name() {
        let snapshot = sample();
        let rows = providers_filtered_from(&snapshot, "anthro");
        assert!(rows.iter().all(|r| r.id == "anthropic"));
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn connections_sorts_favorites_first_within_group() {
        let mut snapshot = sample();
        // Favorite a built-in: it sorts to the top of the built-in group (which
        // itself precedes the custom group).
        for r in &mut snapshot.rows {
            r.favorite = r.id == "anthropic";
        }
        let rows = providers_filtered_from(&snapshot, "");
        assert_eq!(rows[0].id, "anthropic");
        assert!(rows[0].favorite);
        // Built-ins group before the custom provider regardless of favorites.
        let custom_pos = rows.iter().position(|r| r.id == "my-relay").unwrap();
        assert!(rows[..custom_pos].iter().all(|r| r.builtin));
    }

    #[test]
    fn connections_no_longer_groups_builtins_before_custom() {
        let snapshot = sample();
        let rows = providers_filtered_from(&snapshot, "");
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["anthropic", "kimi-code", "my-relay", "openai"]);
    }

    #[test]
    fn is_multi_model_tracks_model_count() {
        let snapshot = sample();
        let rows = providers_filtered_from(&snapshot, "");
        let kimi = rows.iter().find(|r| r.id == "kimi-code").unwrap();
        assert!(!kimi.is_multi_model());
        let openai = rows.iter().find(|r| r.id == "openai").unwrap();
        assert!(openai.is_multi_model());
    }

    #[test]
    fn flat_lists_every_provider_model_pair() {
        // The flat Models picker has one row per (provider, model) pair across
        // ALL snapshot rows — no drilling, no per-provider scoping.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "");
        let pair_count: usize = snapshot.rows.iter().map(|r| r.models.len()).sum();
        assert_eq!(rows.len(), pair_count);
        // Pairs from different providers coexist, each carrying its provider
        // id AND the provider display name for the `· <provider>` row suffix.
        let openai = rows
            .iter()
            .find(|r| r.provider_id == "openai" && r.model == "gpt-4o-mini")
            .expect("openai pair present");
        assert_eq!(openai.provider_label, "OpenAI");
        assert!(rows.iter().any(|r| r.provider_id == "my-relay"));
        assert!(rows
            .iter()
            .any(|r| r.provider_id == "anthropic" && r.model == "claude-opus-4-8"));
    }

    #[test]
    fn flat_sorts_favorite_provider_first_then_recency() {
        // Favorite anthropic and give two of its models recency timestamps:
        // all anthropic pairs lead (favorite provider first), the recently used
        // one before the unused one, and every non-favorite provider's pairs
        // follow, ordered provider name → model label.
        let mut snapshot = sample();
        for r in &mut snapshot.rows {
            r.favorite = r.id == "anthropic";
        }
        let anthropic = snapshot
            .rows
            .iter_mut()
            .find(|r| r.id == "anthropic")
            .unwrap();
        anthropic.model_info = vec![
            ProviderModelInfo {
                model: "claude-sonnet-5".to_string(),
                protocol: "anthropic".to_string(),
                effort: None,
                thinking: None,
                last_used_ms: Some(9_000),
            },
            ProviderModelInfo {
                model: "claude-fable-5".to_string(),
                protocol: "anthropic".to_string(),
                effort: None,
                thinking: None,
                last_used_ms: Some(100),
            },
        ];
        let rows = models_flat_filtered_from(&snapshot, "");
        let anthropic_rows: Vec<&RankedModel> = rows
            .iter()
            .filter(|r| r.provider_id == "anthropic")
            .collect();
        assert_eq!(anthropic_rows.len(), 4);
        // The favorite provider's pairs all precede every other provider's.
        let last_anthropic = rows
            .iter()
            .rposition(|r| r.provider_id == "anthropic")
            .unwrap();
        assert!(rows[..=last_anthropic]
            .iter()
            .all(|r| r.provider_id == "anthropic"));
        // Within it: most-recently-used first, never-used oldest.
        assert_eq!(anthropic_rows[0].model, "claude-sonnet-5");
        assert_eq!(anthropic_rows[1].model, "claude-fable-5");
        // Non-favorite providers order by provider name, then model label.
        let rest: Vec<(&str, &str)> = rows[last_anthropic + 1..]
            .iter()
            .map(|r| (r.provider_label.as_str(), r.label.as_str()))
            .collect();
        let mut sorted = rest.clone();
        sorted.sort();
        assert_eq!(rest, sorted, "provider name → model label tiebreak");
    }

    #[test]
    fn flat_fuzzy_filters_by_model_label() {
        // A query matching a model display name keeps that pair with highlight
        // positions indexing onto the label's characters.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "opus");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "claude-opus-4-8");
        assert!(rows[0].m.is_some(), "label match carries highlight");
    }

    #[test]
    fn flat_fuzzy_by_provider_name_includes_its_models_unhighlighted() {
        // "relay" matches no model label but DOES match the "My Relay"
        // provider name: that provider's models are included with `m = None`
        // (rendered without highlight), while other providers drop out.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "relay");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.provider_id == "my-relay"));
        assert!(
            rows.iter().all(|r| r.m.is_none()),
            "provider-name fallback rows are unhighlighted"
        );
    }

    #[test]
    fn flat_never_used_models_fall_back_to_label_order_within_provider() {
        // With no recency timestamps and no favorites, pairs order by provider
        // name then model label — a deterministic curated fallback.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "");
        let anthropic: Vec<&str> = rows
            .iter()
            .filter(|r| r.provider_id == "anthropic")
            .map(|r| r.label.as_str())
            .collect();
        let mut sorted = anthropic.clone();
        sorted.sort();
        assert_eq!(anthropic, sorted);
    }

    #[test]
    fn each_template_id_resolves_to_a_matching_spec() {
        // The template `id` is the durable join key persisted on instances as
        // `template_id`. Every UI template MUST resolve to a spec in
        // PROVIDER_TEMPLATE_SPECS with the same id, protocol, and model list —
        // otherwise the catalog's reconciliation could not re-seed an instance
        // from its template. This test catches a divergence introduced by
        // editing one table but not the other.
        for t in PROVIDER_TEMPLATES {
            let spec = neenee_providers::provider_template_spec(t.id)
                .unwrap_or_else(|| panic!("template id {} has no matching spec", t.id));
            assert_eq!(
                spec.protocol, t.protocol,
                "template {} protocol mismatch",
                t.id
            );
            assert_eq!(
                spec.models, t.models,
                "template {} model list diverged from its spec",
                t.id
            );
        }
    }

    #[test]
    fn template_ids_are_unique() {
        let mut ids: Vec<&str> = PROVIDER_TEMPLATES.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        let dups: Vec<&[&str]> = ids.windows(2).filter(|pair| pair[0] == pair[1]).collect();
        assert!(dups.is_empty(), "duplicate template ids: {dups:?}");
    }

    #[test]
    fn edit_fields_api_key_shows_name_url_token() {
        // An API-key provider exposes every editable field; editing can change
        // the endpoint and key as well as rename.
        let fields = edit_fields("openai", ChannelAuth::ApiKey);
        assert_eq!(
            fields,
            vec![CustomField::Name, CustomField::BaseUrl, CustomField::Token]
        );
    }

    #[test]
    fn edit_fields_oauth_shows_name_only() {
        // An OAuth channel's endpoint and bearer are owned by the auth flow
        // (xAI `https://api.x.ai/...`, ChatGPT
        // `https://chatgpt.com/backend-api/codex/...`). The editor must expose
        // only a rename, so the server-side guard is never the lone defense
        // against wiping them.
        let xai = edit_fields("xai", ChannelAuth::XaiOAuth);
        assert_eq!(xai, vec![CustomField::Name]);

        let chatgpt = edit_fields("chatgpt", ChannelAuth::ChatGptOAuth);
        assert_eq!(chatgpt, vec![CustomField::Name]);
    }
}
