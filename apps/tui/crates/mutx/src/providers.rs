//! Snapshot-driven provider/model picker filter & sort logic.
//!
//! The pickers render directly from [`muta_contracts::ProviderPickerSnapshot`] — one
//! [`muta_contracts::ProviderPickerRow`] per provider the harness knows how to
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
//!   surface. There is no drilling: one row per pair, Enter activates. The
//!   list is grouped into three labeled sections (Favorites → Recent → All
//!   models; see [`ModelSection`]), and [`models_body_lines`] maps the flat
//!   row indices onto the body's line geometry for the renderer.

use muta_contracts::{
    ConnectionAuth, ProviderModelInfo, ProviderPickerSnapshot, WireProtocol, baseline_models,
};

use crate::fuzzy;

/// One editable field of the provider editor. The visible set is chosen by the
/// active [`ProviderPreset`] (create) or the edited provider's protocol (edit),
/// rather than a fixed five-field form. Provider-owned model collections are
/// imported from `muta_providers`; this view layer only selects and renders
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
/// table entry per preset — mirroring `muta_providers::OPENAI_PROVIDER_SPECS`.
pub struct ProviderPreset {
    /// Stable identifier shared with the matching entry in
    /// `muta_providers::PROVIDER_PRESET_SPECS`. Persisted on the created
    /// connection as `preset_id` so the catalog can re-seed the connection
    /// from this preset's *current* model list on later startups. MUST match
    /// the spec's `id` 1:1 and never change once shipped.
    pub id: &'static str,
    /// List label, e.g. `"Custom Anthropic (Claude relay)"`.
    pub label: &'static str,
    /// One-sentence description shown wrapped under the label in the chooser's
    /// focused row. It must read as prose and cover what the user acts on:
    /// what the service is, what it serves, and how it authenticates ("sign
    /// in with an API key" vs "authorizes in the browser").
    pub description: &'static str,
    /// Wire protocol sent in `AgentRequest::AddProvider`: `"openai"` |
    /// `"anthropic"` | `"google"` (the legacy `"gemini"` label is still
    /// accepted).
    pub protocol: WireProtocol,
    /// Models seeded as channels. Empty means the user enters one via the Model
    /// field (presets can opt in when they need one).
    pub models: &'static [&'static str],
    /// Whether the editor shows a Base URL field (false for native Google).
    pub needs_url: bool,
    /// Placeholder shown in the Base URL field — the full endpoint shape.
    pub url_hint: &'static str,
    /// Whether the editor exposes a free-text Model field. Most presets seed
    /// `models`; open protocols can still add arbitrary model ids later.
    pub needs_model: bool,
    /// A concrete relay endpoint pre-filled into the Base URL field on open
    /// (create mode), so a relay-specific preset works without the user
    /// typing the host. `None` for the generic presets — their `url_hint` is
    /// a placeholder only and the field starts empty, since the user supplies
    /// their own relay host. When set, the user can still edit the value.
    pub default_url: Option<&'static str>,
    /// Preset-specific user agent. Most providers use the default agent, but
    /// the coding-plan endpoints validate this header.
    pub user_agent: Option<&'static str>,
    /// How seeded connections authenticate. `XaiOAuth` starts browser OAuth
    /// before the name editor (OAuth-first add flow).
    pub auth: muta_contracts::ConnectionAuth,
}

impl ProviderPreset {
    /// The title the preset chooser sorts and keys rows by. Every preset
    /// renders its [`Self::label`] alone as the row title — the `OAuth` /
    /// `(sub2api)` suffixes are part of the label, so this accessor exists to
    /// name that rule and give the sort a single home rather than to project
    /// a second spelling of the label.
    pub fn display_title(&self) -> &'static str {
        self.label
    }

    /// The ordered, visible editor fields for this preset (create mode).
    /// OAuth presets only ask for the connection name (auth already completed).
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

    /// Whether selecting this preset starts OAuth before the name editor.
    pub fn oauth_first(&self) -> bool {
        self.auth.is_oauth()
    }
}

/// The provider presets offered when adding a connection, **sorted
/// alphabetically by title**. The chooser renders rows in this order and keys
/// `↑/↓` movement to it, so the declared order here IS the display order —
/// insert new entries at their sorted position, not at the end.
pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "anthropic",
        label: "Anthropic",
        description: "Anthropic's official API for flagship Claude models with advanced reasoning; sign in with an Anthropic API key.",
        protocol: WireProtocol::AnthropicMessages,
        models: muta_providers::ANTHROPIC_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://api.anthropic.com/v1/messages",
        needs_model: false,
        default_url: Some("https://api.anthropic.com/v1/messages"),
        user_agent: None,
        auth: muta_contracts::ConnectionAuth::ApiKey,
    },
    ProviderPreset {
        id: "chatgpt-oauth",
        label: "ChatGPT subscription",
        description: "Uses your ChatGPT Plus or Pro subscription for Codex and flagship GPT models; authorizes in the browser, no API key.",
        protocol: WireProtocol::OpenAiChatCompletions,
        models: muta_providers::CHATGPT_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://chatgpt.com/backend-api/codex/responses",
        needs_model: false,
        default_url: Some("https://chatgpt.com/backend-api/codex/responses"),
        user_agent: None,
        auth: muta_contracts::ConnectionAuth::ChatGptOAuth,
    },
    ProviderPreset {
        id: "deepseek",
        label: "DeepSeek",
        description: "DeepSeek's platform API with high-performance reasoning and coding models; sign in with a DeepSeek API key.",
        protocol: WireProtocol::OpenAiResponses,
        models: muta_providers::DEEPSEEK_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://api.deepseek.com/v1/responses",
        needs_model: false,
        default_url: Some("https://api.deepseek.com/v1/responses"),
        user_agent: None,
        auth: muta_contracts::ConnectionAuth::ApiKey,
    },
    ProviderPreset {
        id: "copilot-oauth",
        label: "GitHub Copilot",
        description: "Your GitHub Copilot subscription, serving multi-vendor coding and reasoning models; authorizes on the device via GitHub.",
        protocol: WireProtocol::OpenAiChatCompletions,
        models: muta_providers::COPILOT_SEED_MODELS,
        needs_url: false,
        url_hint: "https://api.githubcopilot.com/chat/completions",
        needs_model: false,
        default_url: Some("https://api.githubcopilot.com/chat/completions"),
        user_agent: None,
        auth: muta_contracts::ConnectionAuth::CopilotOAuth,
    },
    ProviderPreset {
        id: "google",
        label: "Google AI Studio",
        description: "Google AI Studio / developer API covering the full Gemini range; sign in with a Google API key.",
        protocol: WireProtocol::GoogleGenerateContent,
        models: muta_providers::GOOGLE_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://generativelanguage.googleapis.com/v1beta",
        needs_model: false,
        default_url: Some("https://generativelanguage.googleapis.com/v1beta"),
        user_agent: None,
        auth: muta_contracts::ConnectionAuth::ApiKey,
    },
    ProviderPreset {
        id: "antigravity-oauth",
        label: "Google Antigravity",
        description: "Your Google One AI Premium subscription for flagship Gemini plus companion Claude models; authorizes in the browser.",
        protocol: WireProtocol::GoogleGenerateContent,
        models: muta_providers::ANTIGRAVITY_OAUTH_MODELS,
        needs_url: false,
        url_hint: "https://daily-cloudcode-pa.googleapis.com",
        needs_model: false,
        default_url: Some("https://daily-cloudcode-pa.googleapis.com"),
        user_agent: Some(muta_contracts::client_identity::ANTIGRAVITY_USER_AGENT),
        auth: muta_contracts::ConnectionAuth::AntigravityOAuth,
    },
    ProviderPreset {
        id: "kimi-code",
        label: "Kimi Code",
        description: "Moonshot's Kimi Coding Plan with long-context coding and reasoning models; sign in with a plan API key.",
        protocol: WireProtocol::OpenAiChatCompletions,
        models: muta_providers::KIMI_CODE_MODELS,
        needs_url: false,
        url_hint: "https://api.kimi.com/coding/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.kimi.com/coding/v1/chat/completions"),
        user_agent: Some(muta_providers::OPENCODE_USER_AGENT),
        auth: muta_contracts::ConnectionAuth::ApiKey,
    },
    ProviderPreset {
        id: "openai",
        label: "OpenAI Platform",
        description: "OpenAI's platform API for official flagship GPT and frontier reasoning models; sign in with an OpenAI API key.",
        protocol: WireProtocol::OpenAiChatCompletions,
        models: muta_providers::OPENAI_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://api.openai.com/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.openai.com/v1/chat/completions"),
        user_agent: None,
        auth: muta_contracts::ConnectionAuth::ApiKey,
    },
    ProviderPreset {
        id: "opencode-go",
        label: "OpenCode Go",
        description: "OpenCode.ai subscription relay with cloud-accelerated coding and agent models; sign in with an OpenCode API key.",
        protocol: WireProtocol::OpenAiChatCompletions,
        models: muta_providers::OPENCODE_GO_MODELS,
        needs_url: false,
        url_hint: "https://opencode.ai/zen/go/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://opencode.ai/zen/go/v1/chat/completions"),
        user_agent: None,
        auth: muta_contracts::ConnectionAuth::ApiKey,
    },
    ProviderPreset {
        id: "zai-code",
        label: "ZAI Code (CN)",
        description: "Zhipu's Z.AI Coding Plan with flagship GLM and code-enhanced models; sign in with a plan API key.",
        protocol: WireProtocol::OpenAiChatCompletions,
        models: muta_providers::ZAI_CODE_MODELS,
        needs_url: false,
        url_hint: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
        needs_model: false,
        default_url: Some("https://open.bigmodel.cn/api/coding/paas/v4/chat/completions"),
        user_agent: Some(muta_providers::ZCODE_USER_AGENT),
        auth: muta_contracts::ConnectionAuth::ApiKey,
    },
    ProviderPreset {
        id: "xai-oauth",
        label: "xAI",
        description: "Your SuperGrok or X Premium subscription for flagship Grok reasoning models; authorizes in the browser.",
        protocol: WireProtocol::OpenAiChatCompletions,
        models: muta_providers::XAI_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://api.x.ai/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.x.ai/v1/chat/completions"),
        user_agent: None,
        auth: muta_contracts::ConnectionAuth::XaiOAuth,
    },
];

/// The generic OpenAI-compatible connection definition. It is intentionally
/// separate from [`PROVIDER_PRESETS`]: the Connections surface exposes
/// "Add preset connection" and "Add custom connection" as sibling actions,
/// so a custom endpoint is never presented as though it were a preset.
///
/// The stable `custom-openai` id is retained to recognize existing
/// connections. New custom connections persist without a preset id.
pub const CUSTOM_CONNECTION: ProviderPreset = ProviderPreset {
    id: "custom-openai",
    label: "Custom connection",
    description: "Any OpenAI-compatible endpoint you bring — a custom gateway, local runtime, or relay; you set the base URL and key.",
    protocol: WireProtocol::OpenAiChatCompletions,
    models: &[],
    needs_url: true,
    url_hint: "https://relay.example.com/v1/chat/completions",
    needs_model: true,
    default_url: None,
    user_agent: None,
    auth: muta_contracts::ConnectionAuth::ApiKey,
};

/// Resolve either a curated preset or the standalone custom-connection
/// definition by its stable persisted id.
pub fn connection_definition(id: &str) -> Option<&'static ProviderPreset> {
    if id == CUSTOM_CONNECTION.id {
        return Some(&CUSTOM_CONNECTION);
    }
    PROVIDER_PRESETS.iter().find(|preset| preset.id == id)
}

/// The editor header title for a create-mode connection — the label of the
/// preset the flow was seeded from, falling back to a generic header. The
/// lookup is by **preset id**, not wire protocol: several presets share the
/// `openai` protocol, and a first-match-by-protocol lookup would mislabel
/// the editor (e.g. "ChatGPT subscription" for the ChatGPT OAuth preset).
pub fn preset_label_for(preset_id: Option<&str>) -> String {
    preset_id
        .and_then(connection_definition)
        .map(|t| t.label.to_string())
        .unwrap_or_else(|| "＋ Add connection".to_string())
}

/// Resolve the provider **type** label for a Connections row from its preset
/// id — e.g. `preset_id = "openai"` → `"OpenAI Platform"`. This is the
/// provider *kind* shown beside the user-given connection name (distinct from
/// the connection name itself). Returns `None` for legacy connections with no
/// recorded preset, in which case the row renders the connection name alone.
pub fn provider_type_label(preset_id: &str) -> Option<&'static str> {
    if preset_id.is_empty() {
        return None;
    }
    connection_definition(preset_id).map(|t| t.label)
}

/// The ordered editor fields shown when **editing** an existing user provider.
/// For an API-key custom provider the form offers Name, Base URL, and Token (the Model
/// field is omitted — models, and their per-model reasoning, ADR-0046, are
/// managed in the Models picker). For a preset provider (where endpoint and models
/// are derived from the hardcoded preset spec), Base URL is fixed by the preset,
/// so only Name and Token are offered. For an OAuth connection (ChatGPT/Codex, xAI,
/// Copilot, Antigravity) only Name is editable: the Base URL and Token are fixed by
/// the auth flow and must not be hand-edited, so a rename is the only safe operation.
pub fn edit_fields(is_preset: bool, auth: ConnectionAuth) -> Vec<CustomField> {
    if auth.is_oauth() {
        vec![CustomField::Name]
    } else if is_preset {
        vec![CustomField::Name, CustomField::Token]
    } else {
        vec![CustomField::Name, CustomField::BaseUrl, CustomField::Token]
    }
}

/// Whether a protocol's model set is *closed*: the candidate list is the full,
/// fixed set and the add-model overlay must NOT offer a free-text fallback.
/// OpenAI and Anthropic relays serve an open, evolving model set, so
/// typing an unlisted id is legitimate; native Google is a closed family — its
/// models are enumerated by Google and forwarded verbatim by relays, so an
/// arbitrary id is almost certainly a typo or hallucination, not a real model.
#[cfg(test)]
pub fn protocol_model_set_closed(protocol_wire: &str) -> bool {
    protocol_wire == WireProtocol::GoogleGenerateContent.as_str()
}

/// The registry model ids that match a custom protocol's wire format, used as the
/// candidate list when picking a model for a custom provider (the "list select"
/// half of "list select + custom fallback"). An unknown protocol falls back to
/// the OpenAI set, which is also the default.
pub fn protocol_model_candidates(protocol_wire: &str) -> Vec<&'static str> {
    let Ok(protocol) = protocol_wire.parse::<WireProtocol>() else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    baseline_models()
        .filter(|m| m.protocol == protocol)
        .map(|m| m.id)
        // Deduplicate: a model id can appear in multiple provider tables (e.g.
        // gpt-4o-mini in both `openai` and `copilot`), and inventory iteration
        // order is not guaranteed, so without dedup the candidate list — and
        // thus the first-match the picker commits — would be non-deterministic.
        .filter(|id| seen.insert(*id))
        .collect()
}

/// The context window (in tokens) of a model id, resolved from the registry.
/// Returns `0` for unknown models. Replaces the former `provider_context_window`
/// now that the picker carries the active model id directly.
pub fn model_context_window(model: &str) -> usize {
    muta_contracts::model::resolve(model).context_window
}

/// One selectable row in the **flat model picker** ([`crate::modal::Modal::Models`]
/// equivalent): a single (provider, model) pair drawn from anywhere in the
/// snapshot. Built by [`models_flat_filtered_from`]; the picker browses,
/// searches, and activates these directly — there is no drill-in stage.
#[derive(Clone, Debug)]
pub struct RankedModel {
    /// The section this row belongs to (Favorites, Recent, or All).
    pub section: ModelSection,
    /// Canonical id of the provider serving this model (its snapshot row id).
    pub provider_id: String,
    /// Wire model id to activate. This is also the rendered label and the
    /// fuzzy-match target: the picker is id-first by policy — upstream
    /// discovery only guarantees the wire id, so every row shows the same
    /// kind of label (never a mix of curated names and raw ids).
    pub model: String,
    /// The provider's display name, rendered as the dim `· <provider>` suffix
    /// so identical model ids served by different instances stay
    /// distinguishable in the flat list.
    pub provider_label: String,
    /// Model-specific controls surfaced by the picker snapshot. OpenAI rows
    /// can expose effort; Anthropic rows can expose effort plus thinking.
    pub effort: Option<String>,
    pub thinking: Option<bool>,
    /// Whether this model is favorited (mirrors the snapshot's per-model
    /// `favorite` flag; ADR-0046). A starred daily-driver model sorts into
    /// the leading **Favorites** section of the flat list wherever it is
    /// served and shows a `★` glyph.
    pub favorite: bool,
    /// Unix epoch ms of this model's last activation (`None` = never used).
    /// A model with usage history sorts into the **Recent** section
    /// (most-recently-used first).
    pub last_used_ms: Option<u64>,
    /// The fuzzy match against the model id, or `None` in browse mode (empty
    /// query) — and also when the row was included because its PROVIDER name
    /// matched the query but the model id did not (shown unhighlighted).
    pub m: Option<fuzzy::FuzzyMatch>,
}

impl RankedModel {
    /// The row's list section.
    pub fn section(&self) -> ModelSection {
        self.section
    }
}

/// The three sections of the flat Models picker list, in display order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum ModelSection {
    /// ★-favorited models (ADR-0046) — pinned user intent leads the list.
    Favorites,
    /// Models with usage history, most recently used first.
    Recent,
    /// Every remaining (provider, model) pair, ASCII by model id.
    All,
}

impl ModelSection {
    /// The section's label row: `FAVORITES`, `RECENT`, `ALL MODELS`. Rendered
    /// as a dim uppercase tag (the same section-tag voice as the chrome
    /// labels, e.g. the todo bar's `TODOS`).
    pub fn label(self) -> &'static str {
        match self {
            ModelSection::Favorites => "FAVORITES",
            ModelSection::Recent => "RECENT",
            ModelSection::All => "ALL MODELS",
        }
    }
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
    /// The add-connection preset that birthed this instance (`"openai"`, …),
    /// when known. Surfaced so the Connections list can show the provider
    /// *type* beside the instance name.
    pub preset_id: String,
    /// Client identity configured for this connection.
    pub client_identity: muta_contracts::ClientIdentity,
    /// The rendered label — the provider's display name (the instance name).
    pub label: String,
    /// The fuzzy match against `label`, or `None` in browse mode (empty query).
    pub m: Option<fuzzy::FuzzyMatch>,
}

impl RankedProvider {
    /// Whether the provider hosts more than one model. Informational for the
    /// Connections list (the flat Models picker lists each pair individually,
    /// so no drill-in remains).
    #[cfg(test)]
    pub fn is_multi_model(&self) -> bool {
        self.models.len() > 1
    }
}

/// The last-used-desc → name ordering of the Connections provider list. Pulls
/// each provider's recency signal from its snapshot row. (Favorite is
/// model-level now — ADR-0046 — so the Connections list no longer sorts by it.)
fn provider_order(
    picker: &ProviderPickerSnapshot,
    a_id: &str,
    b_id: &str,
    a_name: &str,
    b_name: &str,
) -> std::cmp::Ordering {
    let used = |id: &str| {
        picker
            .rows
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.last_used_ms)
    };
    let a_used = used(a_id);
    let b_used = used(b_id);
    b_used.cmp(&a_used).then_with(|| a_name.cmp(b_name))
}

/// Build the **Connections** provider rows: one per snapshot row,
/// fuzzy-filtered by `query` against the provider (instance) name and sorted
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
            preset_id: prow.preset_id.clone(),
            client_identity: prow.client_identity.clone(),
            label,
            m,
        });
    }
    rows.sort_by(|a, b| provider_order(picker, &a.id, &b.id, &a.name, &b.name));
    rows
}

/// Build the **flat Models** rows: [`RankedModel`] rows sectioned into three labeled groups:
///
/// 1. **Favorites** — ★-marked models (ADR-0046), ASCII by model id;
/// 2. **Recent** — models with usage history, most recently used first
///    (recency-desc, ASCII id as the tiebreaker);
/// 3. **All models** — ALL available models across ready providers (including favorites
///    and recent models), ASCII by model id (provider label as the stable tiebreaker).
///
/// Fuzzy filtering matches `query` against the model **id** (the rendered
/// label — the picker is id-first: upstream discovery only guarantees the
/// wire id, so every row shows the same kind of label). When the id does not
/// match but the PROVIDER name fuzzy-matches, that provider's models are
/// included unhighlighted (`m = None`) so "show me everything Anthropic
/// serves" works from the same search box. Match positions always index onto
/// the model id's characters only. The sectioned ordering is applied in
/// search mode too, so filtered results keep the same visual grouping.
pub fn models_flat_filtered_from(
    picker: &ProviderPickerSnapshot,
    current_provider: &str,
    current_model: &str,
    query: &str,
) -> Vec<RankedModel> {
    let _ = (current_provider, current_model);
    let mut candidates: Vec<RankedModel> = Vec::new();
    for prow in &picker.rows {
        // Daily-driver model picker only shows models from ready/authenticated connections.
        if !prow.key_ready {
            continue;
        }
        // The provider-name fallback match is computed once per provider: when
        // it hits, every model of that provider is included (unhighlighted)
        // even if its own id does not match the query.
        let provider_matches = !query.is_empty() && fuzzy::fuzzy_match(&prow.name, query).is_some();
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
            let m = if query.is_empty() {
                None
            } else {
                match fuzzy::fuzzy_match(model, query) {
                    Some(m) => Some(m),
                    // Id missed: keep the row only via the provider-name
                    // fallback, and then without highlight positions.
                    None if provider_matches => None,
                    None => continue,
                }
            };
            candidates.push(RankedModel {
                section: ModelSection::All,
                provider_id: prow.id.clone(),
                model: model.clone(),
                provider_label: prow.name.clone(),
                effort: info.effort,
                thinking: info.thinking,
                favorite: info.favorite,
                last_used_ms: info.last_used_ms,
                m,
            });
        }
    }

    // 1. Favorites section: starred models, sorted by ASCII model id then provider label.
    let mut favorites: Vec<RankedModel> = candidates
        .iter()
        .filter(|r| r.favorite)
        .cloned()
        .map(|mut r| {
            r.section = ModelSection::Favorites;
            r
        })
        .collect();
    favorites.sort_by(|a, b| {
        a.model
            .cmp(&b.model)
            .then_with(|| a.provider_label.cmp(&b.provider_label))
    });

    // 2. Recent section: models with usage history, sorted by recency desc, then ASCII model id, then provider label.
    let mut recent: Vec<RankedModel> = candidates
        .iter()
        .filter(|r| r.last_used_ms.is_some())
        .cloned()
        .map(|mut r| {
            r.section = ModelSection::Recent;
            r
        })
        .collect();
    recent.sort_by(|a, b| {
        let a_used = a.last_used_ms.unwrap_or(0);
        let b_used = b.last_used_ms.unwrap_or(0);
        b_used
            .cmp(&a_used)
            .then_with(|| a.model.cmp(&b.model))
            .then_with(|| a.provider_label.cmp(&b.provider_label))
    });

    // 3. All models section: contains EVERY candidate model across ready providers.
    let mut all: Vec<RankedModel> = candidates
        .into_iter()
        .map(|mut r| {
            r.section = ModelSection::All;
            r
        })
        .collect();
    all.sort_by(|a, b| {
        a.model
            .cmp(&b.model)
            .then_with(|| a.provider_label.cmp(&b.provider_label))
    });

    let mut rows = Vec::with_capacity(favorites.len() + recent.len() + all.len());
    rows.extend(favorites);
    rows.extend(recent);
    rows.extend(all);
    rows
}

/// One **body line** of the flat Models list: either a selectable row or a
/// dim section label. The body the renderer paints is
/// [`models_body_lines`] — `row_index` addresses into it skip the label rows
/// (the selection cursor is a *row* cursor, not a *line* cursor, so ↑/↓ can
/// never land on a label).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModelBodyLine {
    /// A selectable row — the payload is the row's index into the flat
    /// [`RankedModel`] slice (the same slice `modal_index` addresses).
    Row(usize),
    /// A dim section label — the payload is the section being announced.
    /// Never selectable.
    Section(ModelSection),
}

/// Map every selectable row index to its body-line index, inserting a
/// [`ModelBodyLine::Section`] label before each non-empty section in display
/// order. Rows whose section is empty produce no lines at all, so an empty
/// Favorites section, for instance, renders no `FAVORITES` header. The
/// returned vector's length is the body's total line count (what the modal's
/// scroll math must use); `row_line[i]` is where flat row `i` paints.
///
/// Blank row between sections is *not* included here — the renderer adds one
/// spacer line before every section label after the first (see
/// `model_list_body`), keeping this mapping pure row/label geometry.
pub fn models_body_lines(models: &[RankedModel]) -> (Vec<ModelBodyLine>, Vec<usize>) {
    let mut lines: Vec<ModelBodyLine> = Vec::with_capacity(models.len() + 3);
    let mut row_line: Vec<usize> = Vec::with_capacity(models.len());
    // Sections arrive in display order because `models_flat_filtered_from`
    // sorts by `ModelSection` first — walk the boundary transitions.
    let mut current: Option<ModelSection> = None;
    for (i, rm) in models.iter().enumerate() {
        let section = rm.section();
        if current != Some(section) {
            lines.push(ModelBodyLine::Section(section));
            current = Some(section);
        }
        row_line.push(lines.len());
        lines.push(ModelBodyLine::Row(i));
    }
    (lines, row_line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::ProviderPickerRow;

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
            preset_id: String::new(),
            client_identity: Default::default(),
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

    /// Build a `ProviderModelInfo` with everything neutral — no favorite, no
    /// recency, no reasoning knobs.
    fn info(model: &str) -> ProviderModelInfo {
        ProviderModelInfo {
            model: model.to_string(),
            protocol: String::new(),
            effort: None,
            thinking: None,
            favorite: false,
            last_used_ms: None,
        }
    }

    /// The sample with one favorited model (`claude-sonnet-5`) and two with
    /// usage history — `glm-5.1` used most recently (t=2000), `gpt-4o` older
    /// (t=1000) — so all three sections are populated and the RECENT section
    /// has a meaningful internal order.
    fn sectioned() -> ProviderPickerSnapshot {
        let mut snapshot = sample();
        for prow in &mut snapshot.rows {
            let (id, info) = match prow.id.as_str() {
                "anthropic" => ("anthropic", info("claude-sonnet-5")),
                "openai" => {
                    let mut i = info("gpt-4o");
                    i.last_used_ms = Some(1_000);
                    ("openai", i)
                }
                "my-relay" => {
                    let mut i = info("glm-5.1");
                    i.last_used_ms = Some(2_000);
                    ("my-relay", i)
                }
                _ => continue,
            };
            assert_eq!(id, prow.id);
            prow.model_info = vec![info];
        }
        // The favorite flag lands after the match above (the borrow ends).
        for prow in &mut snapshot.rows {
            if prow.id == "anthropic" {
                prow.model_info[0].favorite = true;
            }
        }
        snapshot
    }

    #[test]
    fn flat_rows_show_the_raw_wire_id() {
        // Id-first policy: the picker never renders curated display names.
        // Known and unknown ids alike surface as their raw wire id — the row
        // label IS `model`, so there is no label mapping left to drift.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        let glm = rows
            .iter()
            .find(|r| r.model == "glm-5.2")
            .expect("relay pair present");
        assert_eq!(glm.provider_label, "My Relay");
        // The rendered label is the id itself (verified via the fuzzy match
        // target): matching "glm-5.2" hits positions inside the id.
        assert!(
            fuzzy::fuzzy_match(&glm.model, "glm52").is_some(),
            "the match target is the id"
        );
    }

    #[test]
    fn flat_sections_favorites_then_recent_then_all() {
        // Three-section ordering: Favorites lead, Recent (usage history,
        // most-recent-first) follow, All models contains all candidate models in ASCII order.
        // The current pair no longer pins to the top — it keeps its natural
        // section position in ALL MODELS.
        let snapshot = sectioned();
        let rows = models_flat_filtered_from(&snapshot, "my-relay", "glm-5.2", "");

        // Section boundaries: collect the section of each row and assert the
        // sequence is the display order with no interleaving.
        let sections: Vec<ModelSection> = rows.iter().map(|r| r.section()).collect();
        let mut first_all = sections.len();
        for (i, s) in sections.iter().enumerate() {
            match s {
                ModelSection::Favorites if i < 1 => {}
                ModelSection::Recent if (1..3).contains(&i) => {}
                ModelSection::All => {
                    first_all = first_all.min(i);
                }
                _ => panic!("unexpected section {s:?} at index {i}: {sections:?}"),
            }
        }
        assert_eq!(first_all, 3, "ALL MODELS starts after both lead sections");

        // Favorites section: exactly the starred model, first.
        assert!(rows[0].favorite);
        assert_eq!(rows[0].model, "claude-sonnet-5");
        assert_eq!(rows[0].section(), ModelSection::Favorites);

        // Recent section: glm-5.1 (t=2000) before gpt-4o (t=1000).
        assert_eq!(rows[1].model, "glm-5.1", "most recent first");
        assert_eq!(rows[2].model, "gpt-4o");
        assert_eq!(rows[1].last_used_ms, Some(2_000));
        assert_eq!(rows[2].last_used_ms, Some(1_000));
        assert_eq!(rows[1].section(), ModelSection::Recent);
        assert_eq!(rows[2].section(), ModelSection::Recent);

        // All models: plain ASCII, containing ALL 9 models (including favorites, recent, and current pair).
        assert_eq!(rows[first_all..].len(), 9);
        assert!(
            rows[first_all..]
                .iter()
                .all(|r| r.section() == ModelSection::All)
        );
        let rest: Vec<&str> = rows[first_all..].iter().map(|r| r.model.as_str()).collect();
        let mut sorted = rest.clone();
        sorted.sort();
        assert_eq!(rest, sorted);
        assert!(rest.contains(&"claude-sonnet-5"));
        assert!(rest.contains(&"glm-5.1"));
        assert!(rest.contains(&"gpt-4o"));
    }

    #[test]
    fn flat_recent_orders_by_recency_desc_ascii_tiebreak() {
        // Two models with the SAME recency fall back to ASCII id order, so
        // the section stays deterministic when timestamps collide.
        let mut snapshot = sample();
        for prow in &mut snapshot.rows {
            let ids: Vec<String> = prow.models.clone();
            prow.model_info = ids
                .iter()
                .map(|m| {
                    let mut i = info(m);
                    i.last_used_ms = Some(5_000);
                    i
                })
                .collect();
        }
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        // Every model is recent → RECENT section contains all models, and ALL MODELS section contains all models.
        let recent_rows: Vec<&RankedModel> = rows
            .iter()
            .filter(|r| r.section() == ModelSection::Recent)
            .collect();
        let all_rows: Vec<&RankedModel> = rows
            .iter()
            .filter(|r| r.section() == ModelSection::All)
            .collect();
        assert_eq!(recent_rows.len(), 9);
        assert_eq!(all_rows.len(), 9);
        let ids: Vec<&str> = recent_rows.iter().map(|r| r.model.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn flat_favorite_outranks_recency() {
        // Precedence: a favorite always wins over the recency signal —
        // favorites are pinned user intent, recency is emergent. A starred
        // model with NO usage history still leads a used-but-unstarred one.
        let snapshot = sectioned();
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        assert_eq!(
            rows[0].section(),
            ModelSection::Favorites,
            "the unstarred-but-recent glm-5.1 must not lead"
        );
        assert_eq!(rows[0].model, "claude-sonnet-5");
        assert_eq!(rows[1].section(), ModelSection::Recent);
    }

    #[test]
    fn flat_current_pair_keeps_its_section_not_the_top() {
        // The live (provider, model) pair is identified by its ● glyph, not
        // by list position any more: make the never-used glm-5.2 the current
        // pair — it stays in ALL MODELS at its ASCII position while the
        // favorite keeps the lead.
        let snapshot = sectioned();
        let rows = models_flat_filtered_from(&snapshot, "my-relay", "glm-5.2", "");
        assert_eq!(rows[0].model, "claude-sonnet-5", "favorite still leads");
        let current = rows
            .iter()
            .find(|r| r.provider_id == "my-relay" && r.model == "glm-5.2")
            .expect("current pair present");
        assert_eq!(
            current.section(),
            ModelSection::All,
            "current pair is not pinned to the top"
        );
    }

    #[test]
    fn flat_sections_survive_a_fuzzy_query() {
        // Search mode keeps the same grouping: filtered rows stay ordered
        // Favorites → Recent → All.
        let snapshot = sectioned();
        let rows = models_flat_filtered_from(&snapshot, "", "", "g");
        // Matches gpt-4o (recent) and glm-5.1/glm-5.2 (recent/plain).
        let sections: Vec<ModelSection> = rows.iter().map(|r| r.section()).collect();
        let mut ordered = sections.clone();
        ordered.sort();
        assert_eq!(sections, ordered, "sections never regress under a query");
    }

    #[test]
    fn body_lines_interleave_labels_and_rows() {
        // The body geometry: a section label precedes each non-empty section,
        // rows keep their flat index, and empty sections emit nothing.
        let snapshot = sectioned();
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        let (lines, row_line) = models_body_lines(&rows);

        // One label per non-empty section (all three are populated here).
        let labels: Vec<&str> = lines
            .iter()
            .filter_map(|l| match l {
                ModelBodyLine::Section(s) => Some(s.label()),
                ModelBodyLine::Row(_) => None,
            })
            .collect();
        assert_eq!(labels, vec!["FAVORITES", "RECENT", "ALL MODELS"]);

        // Labels come from the display-ordered section enum.
        assert!(lines[0] == ModelBodyLine::Section(ModelSection::Favorites));

        // Row 0 (the favorite) paints one line below its label; the row map
        // is strictly increasing and within the body.
        assert_eq!(row_line[0], 1);
        assert!(row_line.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(
            row_line.last().copied(),
            Some(lines.len() - 1),
            "the last row paints the last line"
        );
        // Every Row(i) entry's mapped line actually holds that row.
        for (i, line) in row_line.iter().enumerate() {
            assert_eq!(lines[*line], ModelBodyLine::Row(i));
        }
    }

    #[test]
    fn body_lines_skip_empty_sections() {
        // A snapshot with neither favorites nor usage renders ONE label
        // (ALL MODELS) — no empty FAVORITES/RECENT headers.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        let (lines, _) = models_body_lines(&rows);
        let labels: Vec<&str> = lines
            .iter()
            .filter_map(|l| match l {
                ModelBodyLine::Section(s) => Some(s.label()),
                ModelBodyLine::Row(_) => None,
            })
            .collect();
        assert_eq!(labels, vec!["ALL MODELS"]);
    }

    #[test]
    fn flat_sorts_ascii_with_provider_label_tiebreak() {
        // Full-list invariant inside each section: rows never increase across
        // ASCII model id, then provider label. Run against the plain sample's
        // every adjacent pair.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        let keys: Vec<(ModelSection, String, String)> = rows
            .iter()
            .map(|r| (r.section(), r.model.clone(), r.provider_label.clone()))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn flat_fuzzy_filters_by_model_id() {
        // A query matching a model id keeps that pair with highlight
        // positions indexing onto the id's characters.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "", "", "opus");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "claude-opus-4-8");
        assert!(rows[0].m.is_some(), "id match carries highlight");
    }

    #[test]
    fn protocol_candidates_filter_by_wire_format() {
        let openai = protocol_model_candidates(WireProtocol::OpenAiChatCompletions.as_str());
        assert!(openai.contains(&"gpt-4o"));
        // Anthropic-format models are excluded from the OpenAI candidate list.
        assert!(!openai.contains(&"claude-opus-4-8"));
        let anthropic = protocol_model_candidates(WireProtocol::AnthropicMessages.as_str());
        assert!(anthropic.contains(&"claude-opus-4-8"));
        assert!(!anthropic.contains(&"gpt-4o"));
    }

    #[test]
    fn google_candidate_set_is_the_canonical_family() {
        // The native-Google candidate list mirrors the ids Google plus common
        // relays/中转站 serve — so a Custom Google provider offers real models,
        // not hallucinated preview ids. Image/embedding/video-only models are
        // excluded (an agent only consumes the text generateContent surface).
        let google = protocol_model_candidates(WireProtocol::GoogleGenerateContent.as_str());
        for id in [
            "gemini-3.7-flash",
            "gemini-3.5-flash",
            "gemini-3-pro-preview",
            "gemini-3-flash-preview",
            "gemini-3.1-pro-preview",
            "gemini-2.5-flash",
            "gemini-2.5-pro",
            "gemini-2.0-flash",
        ] {
            assert!(google.contains(&id), "google candidate set missing {id}");
        }
        // Image-generation variants must NOT be in the text agent's candidate set.
        assert!(
            !google.contains(&"gemini-2.5-flash-image"),
            "image-only model leaked into google candidates"
        );
    }

    #[test]
    fn google_candidate_set_includes_antigravity_relay_models() {
        // The Antigravity (sub2api) relay ids are registered as native-Google
        // baselines, so the add-model overlay for a Google provider offers
        // them (the closed-set policy has real candidates to pick from).
        let google = protocol_model_candidates(WireProtocol::GoogleGenerateContent.as_str());
        for id in [
            "gemini-3.1-pro-high",
            "gemini-3.1-pro-low",
            "gemini-3-flash",
        ] {
            assert!(
                google.contains(&id),
                "antigravity relay model {id} missing from google candidates"
            );
        }
    }

    #[test]
    fn antigravity_preset_is_offered_with_prefilled_url_and_seeded_models() {
        let tmpl = PROVIDER_PRESETS
            .iter()
            .find(|t| t.id == "antigravity-oauth")
            .expect("antigravity preset offered in the chooser");
        assert_eq!(tmpl.label, "Google Antigravity");
        assert_eq!(tmpl.protocol, WireProtocol::GoogleGenerateContent);
        assert_eq!(tmpl.models, muta_providers::ANTIGRAVITY_OAUTH_MODELS);
        assert_eq!(
            tmpl.default_url,
            Some("https://daily-cloudcode-pa.googleapis.com")
        );
        assert!(!tmpl.needs_url, "OAuth preset hides Base URL field");
        assert!(
            !tmpl.needs_model,
            "no free-text Model field — models are seeded"
        );
        assert_eq!(tmpl.fields(), vec![CustomField::Name]);
    }

    #[test]
    fn openai_preset_seeds_openai_text_models() {
        let tmpl = PROVIDER_PRESETS
            .iter()
            .find(|t| t.id == "openai")
            .expect("openai preset offered in the chooser");
        assert_eq!(tmpl.protocol, WireProtocol::OpenAiChatCompletions);
        assert_eq!(tmpl.models, muta_providers::OPENAI_BUILTIN_MODELS);
        assert!(
            !tmpl.needs_url,
            "official endpoint URL is prefilled and hidden"
        );
        assert!(
            !tmpl.needs_model,
            "model list is seeded; add-model handles custom ids"
        );
        assert_eq!(tmpl.fields(), vec![CustomField::Name, CustomField::Token]);
        for id in ["gpt-5.5", "gpt-5.4", "gpt-5.6-sol"] {
            assert!(
                protocol_model_candidates(WireProtocol::OpenAiChatCompletions.as_str())
                    .contains(&id),
                "OpenAI candidate set missing {id}"
            );
        }
    }

    #[test]
    fn builtin_presets_prefill_official_urls_generic_relays_do_not() {
        let builtin_labels = [
            "OpenAI Platform",
            "Anthropic",
            "Google AI Studio",
            "DeepSeek",
            "xAI",
            "ChatGPT subscription",
            "GitHub Copilot",
            "Google Antigravity",
            "Kimi Code",
            "ZAI Code (CN)",
            "OpenCode Go",
        ];
        for t in PROVIDER_PRESETS {
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
    fn google_model_set_is_closed_others_open() {
        // A closed set means the add-model overlay offers no free-text fallback:
        // the candidate list is the complete family, so an unmatched id is a
        // typo. OpenAI/Anthropic relays serve an open, evolving set, so typing
        // an unlisted id stays legitimate there.
        assert!(
            protocol_model_set_closed(WireProtocol::GoogleGenerateContent.as_str()),
            "native Google must be a closed model set"
        );
        assert!(
            !protocol_model_set_closed(WireProtocol::OpenAiChatCompletions.as_str()),
            "OpenAI relays keep an open model set"
        );
        assert!(
            !protocol_model_set_closed(WireProtocol::AnthropicMessages.as_str()),
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
    fn connections_orders_by_last_used_then_name_no_favorite() {
        // Favorite is model-level now (ADR-0046), so the Connections list no
        // longer sorts by it — only last-used desc, then instance name.
        let mut snapshot = sample();
        // Give kimi-code a recent activation so it leads.
        for r in &mut snapshot.rows {
            r.last_used_ms = (r.id == "kimi-code").then_some(1_000);
        }
        let rows = providers_filtered_from(&snapshot, "");
        assert_eq!(rows[0].id, "kimi-code");
        // The rest fall back to name order.
        let rest: Vec<&str> = rows[1..].iter().map(|r| r.id.as_str()).collect();
        assert_eq!(rest, vec!["anthropic", "my-relay", "openai"]);
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
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
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
        assert!(
            rows.iter()
                .any(|r| r.provider_id == "anthropic" && r.model == "claude-opus-4-8")
        );
    }

    #[test]
    fn flat_sorts_favorite_model_first_then_ascii() {
        // Favorite is model-level (ADR-0046): a starred model sorts into the
        // leading section of the flat list wherever it is served. Give one
        // anthropic model recency and favorite another: the favorited model
        // leads in FAVORITES, the used model is in RECENT, and ALL MODELS contains all 8 models.
        let mut snapshot = sample();
        let anthropic = snapshot
            .rows
            .iter_mut()
            .find(|r| r.id == "anthropic")
            .unwrap();
        let mut starred = info("claude-sonnet-5");
        starred.favorite = true;
        let mut used = info("claude-fable-5");
        used.last_used_ms = Some(100);
        anthropic.model_info = vec![starred, used];
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        // The favorited model leads the FAVORITES section.
        assert!(rows[0].favorite);
        assert_eq!(rows[0].model, "claude-sonnet-5");
        assert_eq!(rows[0].section(), ModelSection::Favorites);
        // The used model is in the RECENT section.
        assert_eq!(rows[1].model, "claude-fable-5");
        assert_eq!(rows[1].section(), ModelSection::Recent);
        // Everything from the third row on is the ALL MODELS section (all 9 models).
        assert_eq!(rows[2..].len(), 9);
        assert!(rows[2..].iter().all(|r| r.section() == ModelSection::All));
        let rest: Vec<&str> = rows[2..].iter().map(|r| r.model.as_str()).collect();
        let mut sorted = rest.clone();
        sorted.sort();
        assert_eq!(rest, sorted);
    }

    #[test]
    fn flat_fuzzy_by_provider_name_includes_its_models_unhighlighted() {
        // "relay" matches no model id but DOES match the "My Relay"
        // provider name: that provider's models are included with `m = None`
        // (rendered without highlight), while other providers drop out.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "", "", "relay");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.provider_id == "my-relay"));
        assert!(
            rows.iter().all(|r| r.m.is_none()),
            "provider-name fallback rows are unhighlighted"
        );
    }

    #[test]
    fn flat_rows_order_ascii_without_current_or_favorite() {
        // With no favorites and no usage history, the whole list is the ALL
        // MODELS section in pure ASCII order (provider label as the
        // tiebreak) — deterministic regardless of provider order.
        let snapshot = sample();
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        let ids: Vec<&str> = rows.iter().map(|r| r.model.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn each_preset_id_resolves_to_a_matching_spec() {
        // The preset `id` is the durable join key persisted on connections as
        // `preset_id`. Every UI preset MUST resolve to a spec in
        // PROVIDER_PRESET_SPECS with the same id, protocol, and model list —
        // otherwise the catalog's reconciliation could not re-seed a connection
        // from its preset. This test catches a divergence introduced by
        // editing one table but not the other.
        for t in PROVIDER_PRESETS {
            let spec = muta_providers::provider_preset_spec(t.id)
                .unwrap_or_else(|| panic!("preset id {} has no matching spec", t.id));
            assert_eq!(
                spec.protocol, t.protocol,
                "preset {} protocol mismatch",
                t.id
            );
            assert_eq!(
                spec.models, t.models,
                "preset {} model list diverged from its spec",
                t.id
            );
        }
    }

    #[test]
    fn preset_ids_are_unique() {
        let mut ids: Vec<&str> = PROVIDER_PRESETS
            .iter()
            .chain(std::iter::once(&CUSTOM_CONNECTION))
            .map(|t| t.id)
            .collect();
        ids.sort_unstable();
        let dups: Vec<&[&str]> = ids.windows(2).filter(|pair| pair[0] == pair[1]).collect();
        assert!(dups.is_empty(), "duplicate preset ids: {dups:?}");
    }

    #[test]
    fn openai_platform_preset_is_labeled_to_distinguish_chatgpt() {
        // The `openai` preset is the platform/API-key billing plan, distinct
        // from the ChatGPT subscription preset that shares its wire protocol.
        // The label must say so — a bare "OpenAI" reads as the company and
        // matches the subscription plan users actually have.
        let openai = PROVIDER_PRESETS.iter().find(|t| t.id == "openai").unwrap();
        assert_eq!(openai.label, "OpenAI Platform");
        let chatgpt = PROVIDER_PRESETS
            .iter()
            .find(|t| t.id == "chatgpt-oauth")
            .unwrap();
        assert_eq!(chatgpt.label, "ChatGPT subscription");
        assert_eq!(openai.protocol, chatgpt.protocol);
        assert!(
            chatgpt.models.contains(&"gpt-5.3-codex-spark"),
            "the subscription preset must expose the Codex model allowed by OpenCode"
        );
    }

    #[test]
    fn editor_title_resolves_by_preset_id_not_protocol() {
        // Several presets share the `openai` wire protocol (chatgpt-oauth is
        // declared first). A create-mode editor title must resolve from the
        // seeded preset id, otherwise every openai-protocol flow would be
        // headed "ChatGPT subscription".
        assert_eq!(preset_label_for(Some("openai")), "OpenAI Platform");
        assert_eq!(
            preset_label_for(Some("chatgpt-oauth")),
            "ChatGPT subscription"
        );
        assert_eq!(preset_label_for(Some("custom-openai")), "Custom connection");
        assert_eq!(preset_label_for(Some("deepseek")), "DeepSeek");
        // Unknown / unseeded ids fall back to the generic header.
        assert_eq!(preset_label_for(None), "＋ Add connection");
        assert_eq!(
            preset_label_for(Some("no-such-preset")),
            "＋ Add connection"
        );
    }

    #[test]
    fn custom_connection_is_not_a_preset_chooser_row() {
        assert!(
            PROVIDER_PRESETS
                .iter()
                .all(|preset| preset.id != "custom-openai"),
            "custom connections have their own Connections-level branch"
        );
        assert_eq!(
            connection_definition("custom-openai").map(|definition| definition.id),
            Some("custom-openai")
        );
    }

    #[test]
    fn edit_fields_api_key_shows_name_url_token_for_custom_only() {
        // A pure-custom API-key provider exposes Name, Base URL, and Token.
        let custom_fields = edit_fields(false, ConnectionAuth::ApiKey);
        assert_eq!(
            custom_fields,
            vec![CustomField::Name, CustomField::BaseUrl, CustomField::Token]
        );

        // A preset API-key provider derives its Base URL from the preset spec,
        // so it only exposes Name and Token.
        let preset_fields = edit_fields(true, ConnectionAuth::ApiKey);
        assert_eq!(preset_fields, vec![CustomField::Name, CustomField::Token]);
    }

    #[test]
    fn edit_fields_oauth_shows_name_only() {
        // An OAuth connection's endpoint and bearer are owned by the auth flow
        // (xAI `https://api.x.ai/...`, ChatGPT
        // `https://chatgpt.com/backend-api/codex/...`). The editor must expose
        // only a rename, so the server-side guard is never the lone defense
        // against wiping them.
        let xai = edit_fields(true, ConnectionAuth::XaiOAuth);
        assert_eq!(xai, vec![CustomField::Name]);

        let chatgpt = edit_fields(true, ConnectionAuth::ChatGptOAuth);
        assert_eq!(chatgpt, vec![CustomField::Name]);
    }

    #[test]
    fn flat_rows_exclude_unready_providers() {
        let mut snapshot = sample();
        // Mark openai as not key_ready
        snapshot.rows[1].key_ready = false;
        let rows = models_flat_filtered_from(&snapshot, "", "", "");
        assert!(!rows.iter().any(|r| r.provider_id == "openai"));
        assert!(rows.iter().any(|r| r.provider_id == "kimi-code"));
    }
}
