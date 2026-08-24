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
    ChannelAuth, ProviderModelInfo, ProviderPickerSnapshot, WireFormat, baseline_models,
};

use crate::fuzzy;

/// One editable field of the provider editor. The visible set is chosen by the
/// active [`ProviderTemplate`] (create) or the edited provider's protocol (edit),
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
/// table entry per template — mirroring `muta_providers::OPENAI_PROVIDER_SPECS`.
pub struct ProviderTemplate {
    /// Stable identifier shared with the matching entry in
    /// `muta_providers::PROVIDER_TEMPLATE_SPECS`. Persisted on the created
    /// instance as `template_id` so the catalog can re-seed the instance from
    /// this template's *current* model list on later startups. MUST match the
    /// spec's `id` 1:1 and never change once shipped.
    pub id: &'static str,
    /// List label, e.g. `"Custom Anthropic (Claude relay)"`.
    pub label: &'static str,
    /// One-line description shown under the label in the chooser.
    pub description: &'static str,
    /// Wire protocol sent in `AgentRequest::AddProvider`: `"openai"` |
    /// `"anthropic"` | `"google"` (the legacy `"gemini"` label is still
    /// accepted).
    pub protocol: &'static str,
    /// Models seeded as channels. Empty means the user enters one via the Model
    /// field (templates can opt in when they need one).
    pub models: &'static [&'static str],
    /// Whether the editor shows a Base URL field (false for native Google).
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
    pub auth: muta_contracts::ChannelAuth,
}

impl ProviderTemplate {
    /// The title the template chooser sorts and keys rows by. Every template
    /// renders its [`Self::label`] alone as the row title — the `OAuth` /
    /// `(sub2api)` suffixes are part of the label, so this accessor exists to
    /// name that rule and give the sort a single home rather than to project
    /// a second spelling of the label.
    pub fn display_title(&self) -> &'static str {
        self.label
    }

    /// The auth-scheme badge for the chooser row: `oauth` when the template
    /// authenticates through a browser flow, `token` when it asks for an API
    /// key. This is the only per-row auth surface — the wire protocol and the
    /// seeded model count are implementation details the user does not act on.
    pub fn auth_badge(&self) -> &'static str {
        if self.auth.is_oauth() {
            "oauth"
        } else {
            "token"
        }
    }

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

/// The provider templates offered when adding a provider, **sorted
/// alphabetically by title**. The chooser renders rows in this order and keys
/// `↑/↓` movement to it, so the declared order here IS the display order —
/// insert new entries at their sorted position, not at the end.
pub const PROVIDER_TEMPLATES: &[ProviderTemplate] = &[
    ProviderTemplate {
        id: "anthropic",
        label: "Anthropic",
        description: "Claude models over the Anthropic /messages API",
        protocol: "anthropic",
        models: muta_providers::ANTHROPIC_BUILTIN_MODELS,
        needs_url: true,
        url_hint: "https://api.anthropic.com/v1/messages",
        needs_model: false,
        default_url: Some("https://api.anthropic.com/v1/messages"),
        user_agent: None,
        auth: muta_contracts::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "chatgpt-oauth",
        label: "ChatGPT",
        description: "GPT-5.x via ChatGPT Pro/Plus subscription (browser OAuth)",
        protocol: "openai",
        models: muta_providers::CHATGPT_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://chatgpt.com/backend-api/codex/responses",
        needs_model: false,
        default_url: Some("https://chatgpt.com/backend-api/codex/responses"),
        user_agent: None,
        auth: muta_contracts::ChannelAuth::ChatGptOAuth,
    },
    ProviderTemplate {
        id: "custom-openai",
        label: "Custom Provider",
        description: "Any OpenAI-compatible endpoint — custom relay or self-hosted gateway",
        protocol: "openai",
        models: &[],
        needs_url: true,
        url_hint: "https://relay.example.com/v1/chat/completions",
        needs_model: true,
        default_url: None,
        user_agent: None,
        auth: muta_contracts::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "deepseek",
        label: "DeepSeek",
        description: "DeepSeek V4 Flash (0731) + Pro (0813) over the OpenAI Responses API",
        protocol: "openai-responses",
        models: muta_providers::DEEPSEEK_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://api.deepseek.com/v1/responses",
        needs_model: false,
        default_url: Some("https://api.deepseek.com/v1/responses"),
        user_agent: None,
        auth: muta_contracts::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "copilot-oauth",
        label: "GitHub Copilot",
        description: "GPT-4o/5.x via GitHub Copilot subscription (device OAuth)",
        protocol: "openai",
        models: muta_providers::COPILOT_SEED_MODELS,
        needs_url: false,
        url_hint: "https://api.githubcopilot.com/chat/completions",
        needs_model: false,
        default_url: Some("https://api.githubcopilot.com/chat/completions"),
        user_agent: None,
        auth: muta_contracts::ChannelAuth::CopilotOAuth,
    },
    ProviderTemplate {
        id: "google",
        label: "Google AI Studio",
        description: "Native Google API — Google AI Studio or compatible relay",
        protocol: "google",
        models: muta_providers::GOOGLE_BUILTIN_MODELS,
        needs_url: true,
        url_hint: "https://generativelanguage.googleapis.com/v1beta",
        needs_model: false,
        default_url: Some("https://generativelanguage.googleapis.com/v1beta"),
        user_agent: None,
        auth: muta_contracts::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "antigravity-oauth",
        label: "Google Antigravity",
        description: "Gemini 3.x / Claude / GPT models via Google One AI Premium (Antigravity)",
        protocol: "google",
        models: muta_providers::ANTIGRAVITY_OAUTH_MODELS,
        needs_url: false,
        url_hint: "https://daily-cloudcode-pa.googleapis.com",
        needs_model: false,
        default_url: Some("https://daily-cloudcode-pa.googleapis.com"),
        user_agent: Some(muta_contracts::client_identity::ANTIGRAVITY_USER_AGENT),
        auth: muta_contracts::ChannelAuth::AntigravityOAuth,
    },
    ProviderTemplate {
        id: "kimi-code",
        label: "Kimi Code",
        description: "Moonshot Kimi coding-plan endpoint",
        protocol: "openai",
        models: muta_providers::KIMI_CODE_MODELS,
        needs_url: false,
        url_hint: "https://api.kimi.com/coding/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.kimi.com/coding/v1/chat/completions"),
        user_agent: Some(muta_providers::OPENCODE_USER_AGENT),
        auth: muta_contracts::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "openai",
        label: "OpenAI",
        description: "OpenAI API — GPT-5.5 family",
        protocol: "openai",
        models: muta_providers::OPENAI_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://api.openai.com/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.openai.com/v1/chat/completions"),
        user_agent: None,
        auth: muta_contracts::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "opencode-go",
        label: "OpenCode Go",
        description: "opencode.ai relay — OpenAI chat-completions coding models",
        protocol: "openai",
        models: muta_providers::OPENCODE_GO_MODELS,
        needs_url: true,
        url_hint: "https://opencode.ai/zen/go/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://opencode.ai/zen/go/v1/chat/completions"),
        user_agent: None,
        auth: muta_contracts::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "zai-code",
        label: "ZAI Code (CN)",
        description: "Zhipu BigModel / Z.AI coding-plan endpoint (CN)",
        protocol: "openai",
        models: muta_providers::ZAI_CODE_MODELS,
        needs_url: false,
        url_hint: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
        needs_model: false,
        default_url: Some("https://open.bigmodel.cn/api/coding/paas/v4/chat/completions"),
        user_agent: Some(muta_providers::ZCODE_USER_AGENT),
        auth: muta_contracts::ChannelAuth::ApiKey,
    },
    ProviderTemplate {
        id: "xai-oauth",
        label: "xAI",
        description: "Grok 4.x via SuperGrok subscription (browser OAuth)",
        protocol: "openai",
        models: muta_providers::XAI_BUILTIN_MODELS,
        needs_url: false,
        url_hint: "https://api.x.ai/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.x.ai/v1/chat/completions"),
        user_agent: None,
        auth: muta_contracts::ChannelAuth::XaiOAuth,
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

/// Resolve the provider **type** label for a Connections row from its template
/// id — e.g. `template_id = "openai-sub2api"` → `"OpenAI (sub2api)"`. This is
/// the provider *kind* shown beside the user-given instance name (distinct from
/// the instance name itself). Returns `None` for legacy instances with no
/// recorded template, in which case the row renders the instance name alone.
pub fn provider_type_label(template_id: &str) -> Option<&'static str> {
    if template_id.is_empty() {
        return None;
    }
    PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.id == template_id)
        .map(|t| t.label)
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
/// typing an unlisted id is legitimate; native Google is a closed family — its
/// models are enumerated by Google and forwarded verbatim by relays, so an
/// arbitrary id is almost certainly a typo or hallucination, not a real model.
#[allow(dead_code)]
pub fn protocol_model_set_closed(protocol_wire: &str) -> bool {
    matches!(protocol_wire, "google" | "gemini")
}

/// The registry model ids that match a custom protocol's wire format, used as the
/// candidate list when picking a model for a custom provider (the "list select"
/// half of "list select + custom fallback"). An unknown protocol falls back to
/// the OpenAI set, which is also the default.
pub fn protocol_model_candidates(protocol_wire: &str) -> Vec<&'static str> {
    let format = match protocol_wire {
        "anthropic" => WireFormat::AnthropicCompat,
        "google" | "gemini" => WireFormat::Google,
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
pub struct RankedModel {
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
    /// (most-recently-used first). `Some(_)` outranks recency `None`, but a
    /// favorite always wins over recency — favorites are a pinned intent,
    /// recency is an emergent signal.
    pub last_used_ms: Option<u64>,
    /// The fuzzy match against the model id, or `None` in browse mode (empty
    /// query) — and also when the row was included because its PROVIDER name
    /// matched the query but the model id did not (shown unhighlighted).
    pub m: Option<fuzzy::FuzzyMatch>,
}

impl RankedModel {
    /// The row's list section. The flat Models picker renders three sections
    /// in a fixed order — Favorites, Recent, All models — and this key is the
    /// single source of truth for which one a row belongs to (see
    /// [`models_flat_filtered_from`] for the precedence rules).
    pub fn section(&self) -> ModelSection {
        if self.favorite {
            ModelSection::Favorites
        } else if self.last_used_ms.is_some() {
            ModelSection::Recent
        } else {
            ModelSection::All
        }
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
    #[allow(dead_code)]
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

/// Build the **flat Models** rows: one [`RankedModel`] per (provider, model)
/// pair across the entire snapshot — the daily-driver switch surface, with no
/// drill-in.
///
/// **Ordering is three-sectioned** (the picker renders one dim section-label
/// row between the groups; see [`ModelSection`]):
///
/// 1. **Favorites** — ★-marked models (ADR-0046), ASCII by model id;
/// 2. **Recent** — models with usage history, most recently used first
///    (recency-desc, ASCII id as the tiebreaker);
/// 3. **All models** — everything else, ASCII by model id (provider label as
///    the stable tiebreaker for the same id served by multiple instances).
///
/// Precedence is favorite > recent > rest: a favorite is a pinned user intent
/// and always wins over the emergent recency signal. The currently-active
/// (provider, model) pair is *not* pinned to the top of the list any more —
/// it keeps its natural section position and is identified by its `●` glyph
/// (and the modal's open-on-current cursor placement) instead.
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
    let mut rows: Vec<RankedModel> = Vec::new();
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
            rows.push(RankedModel {
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
    // Three-section ordering:
    //   section — favorites > recent > rest;
    //   inside Favorites/All — ASCII model id, then provider label as the
    //     stable tiebreaker for the same id served by multiple instances;
    //   inside Recent — recency desc, then the same ASCII tiebreak.
    rows.sort_by(|a, b| {
        a.section()
            .cmp(&b.section())
            .then_with(|| {
                // Only the Recent section keys on recency; the other two have
                // `None` recency or ignore it, where `None > Some` would be
                // the wrong direction, so equalize to keep ASCII order.
                let (a_used, b_used) = match (a.section(), b.section()) {
                    (ModelSection::Recent, ModelSection::Recent) => {
                        (a.last_used_ms.unwrap_or(0), b.last_used_ms.unwrap_or(0))
                    }
                    _ => (0, 0),
                };
                b_used.cmp(&a_used)
            })
            .then_with(|| a.model.cmp(&b.model))
            .then_with(|| a.provider_label.cmp(&b.provider_label))
    });
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
        // most-recent-first) follow, everything else trails in ASCII order.
        // The current pair no longer pins to the top — it keeps its natural
        // section position.
        let snapshot = sectioned();
        let rows = models_flat_filtered_from(&snapshot, "my-relay", "glm-5.2", "");

        // Section boundaries: collect the section of each row and assert the
        // sequence is the display order with no interleaving.
        let sections: Vec<ModelSection> = rows.iter().map(|r| r.section()).collect();
        let mut first_all = sections.len();
        for (i, s) in sections.iter().enumerate() {
            match s {
                ModelSection::Favorites if i < 2 => {}
                ModelSection::Recent if (1..3).contains(&i) => {}
                ModelSection::All => {
                    first_all = first_all.min(i);
                }
                _ => panic!("unexpected section {s:?} at index {i}: {sections:?}"),
            }
        }
        assert!(first_all >= 3, "ALL MODELS starts after both lead sections");

        // Favorites section: exactly the starred model, first.
        assert!(rows[0].favorite);
        assert_eq!(rows[0].model, "claude-sonnet-5");

        // Recent section: glm-5.1 (t=2000) before gpt-4o (t=1000).
        assert_eq!(rows[1].model, "glm-5.1", "most recent first");
        assert_eq!(rows[2].model, "gpt-4o");
        assert_eq!(rows[1].last_used_ms, Some(2_000));
        assert_eq!(rows[2].last_used_ms, Some(1_000));

        // All models: plain ASCII, current pair included at its natural spot.
        let rest: Vec<&str> = rows[first_all..].iter().map(|r| r.model.as_str()).collect();
        let mut sorted = rest.clone();
        sorted.sort();
        assert_eq!(rest, sorted);
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
        // Every model is recent → the whole list is the RECENT section, and
        // with one shared timestamp the order is pure ASCII.
        assert!(rows.iter().all(|r| r.section() == ModelSection::Recent));
        let ids: Vec<&str> = rows.iter().map(|r| r.model.as_str()).collect();
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
        let openai = protocol_model_candidates("openai");
        assert!(openai.contains(&"gpt-4o"));
        // Anthropic-format models are excluded from the OpenAI candidate list.
        assert!(!openai.contains(&"claude-opus-4-8"));
        let anthropic = protocol_model_candidates("anthropic");
        assert!(anthropic.contains(&"claude-opus-4-8"));
        assert!(!anthropic.contains(&"gpt-4o"));
    }

    #[test]
    fn google_candidate_set_is_the_canonical_family() {
        // The native-Google candidate list mirrors the ids Google plus common
        // relays/中转站 serve — so a Custom Google provider offers real models,
        // not hallucinated preview ids. Image/embedding/video-only models are
        // excluded (an agent only consumes the text generateContent surface).
        let google = protocol_model_candidates("google");
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
        let google = protocol_model_candidates("google");
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
    fn antigravity_template_is_offered_with_prefilled_url_and_seeded_models() {
        let tmpl = PROVIDER_TEMPLATES
            .iter()
            .find(|t| t.id == "antigravity-oauth")
            .expect("antigravity template offered in the chooser");
        assert_eq!(tmpl.label, "Google Antigravity");
        assert_eq!(tmpl.protocol, "google");
        assert_eq!(tmpl.models, muta_providers::ANTIGRAVITY_OAUTH_MODELS);
        assert_eq!(
            tmpl.default_url,
            Some("https://daily-cloudcode-pa.googleapis.com")
        );
        assert!(!tmpl.needs_url, "OAuth template hides Base URL field");
        assert!(
            !tmpl.needs_model,
            "no free-text Model field — models are seeded"
        );
        assert_eq!(tmpl.fields(), vec![CustomField::Name]);
    }

    #[test]
    fn openai_template_seeds_openai_text_models() {
        let tmpl = PROVIDER_TEMPLATES
            .iter()
            .find(|t| t.id == "openai")
            .expect("openai template offered in the chooser");
        assert_eq!(tmpl.protocol, "openai");
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
                protocol_model_candidates("openai").contains(&id),
                "OpenAI candidate set missing {id}"
            );
        }
    }

    #[test]
    fn builtin_templates_prefill_official_urls_generic_relays_do_not() {
        let builtin_labels = [
            "OpenAI",
            "Anthropic",
            "Google AI Studio",
            "DeepSeek",
            "xAI",
            "ChatGPT",
            "GitHub Copilot",
            "Google Antigravity",
            "Kimi Code",
            "ZAI Code (CN)",
            "OpenCode Go",
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
    fn google_model_set_is_closed_others_open() {
        // A closed set means the add-model overlay offers no free-text fallback:
        // the candidate list is the complete family, so an unmatched id is a
        // typo. OpenAI/Anthropic relays serve an open, evolving set, so typing
        // an unlisted id stays legitimate there.
        assert!(
            protocol_model_set_closed("google"),
            "native Google must be a closed model set"
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
        // leads the whole list regardless of the recency signal.
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
        // The favorited model leads the whole flat list.
        assert!(rows[0].favorite);
        assert_eq!(rows[0].model, "claude-sonnet-5");
        // The used-but-unstarred model leads the RECENT section.
        assert_eq!(rows[1].model, "claude-fable-5");
        assert_eq!(rows[1].section(), ModelSection::Recent);
        // Everything from the third row on is the plain ALL MODELS section.
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
    fn each_template_id_resolves_to_a_matching_spec() {
        // The template `id` is the durable join key persisted on instances as
        // `template_id`. Every UI template MUST resolve to a spec in
        // PROVIDER_TEMPLATE_SPECS with the same id, protocol, and model list —
        // otherwise the catalog's reconciliation could not re-seed an instance
        // from its template. This test catches a divergence introduced by
        // editing one table but not the other.
        for t in PROVIDER_TEMPLATES {
            let spec = muta_providers::provider_template_spec(t.id)
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
