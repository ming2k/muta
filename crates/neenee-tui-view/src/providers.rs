//! Snapshot-driven provider/model picker filter & sort logic.
//!
//! The picker renders directly from [`neenee_core::ProviderPickerSnapshot`] — one
//! [`neenee_core::ProviderPickerRow`] per provider the harness knows how to
//! drive, carrying the display name, the served model ids, the active model, and
//! the live per-user signals (favorite, key-ready, last-used). Built-in and
//! user-defined providers share this single path, so a custom provider added via
//! the editor shows up like any built-in (there is no separate static table).
//!
//! The picker is **two-stage**: [`providers_filtered_from`] builds the stage-1
//! provider list; activating a multi-model provider drills into its models via
//! [`provider_models_filtered_from`] (stage 2). Single-model providers activate
//! directly.

use neenee_core::{
    KNOWN_MODELS, ProviderModelInfo, ProviderPickerSnapshot, WireFormat, resolve_model,
};

use crate::fuzzy;

/// One editable field of the provider editor. The visible set is chosen by the
/// active [`ProviderTemplate`] (create) or the edited provider's protocol (edit),
/// rather than a fixed five-field form.
///
/// Reasoning (effort/thinking) is intentionally NOT a provider-editor field —
/// ADR-0046 moved it to the per-model stage-2 `e` editor, so a provider is
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
}

impl ProviderTemplate {
    /// The ordered, visible editor fields for this template (create mode).
    pub fn fields(&self) -> Vec<CustomField> {
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
}

/// Text/chat models commonly served by OpenAI sub2api relays.
///
/// Keep stable aliases first. Dated snapshots and image/audio/realtime models
/// are intentionally omitted from the create template; users can still add a
/// relay-specific id from the provider's model list.
pub const OPENAI_SUB2API_MODELS: &[&str] = &[
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex-spark",
    "gpt-5.2",
    "gpt-5.2-chat-latest",
    "gpt-5.2-pro",
];

/// The provider templates offered when adding a provider, in display order.
pub const PROVIDER_TEMPLATES: &[ProviderTemplate] = &[
    ProviderTemplate {
        label: "OpenAI",
        description: "OpenAI API — GPT-5.5 family",
        protocol: "openai",
        models: neenee_providers::OPENAI_BUILTIN_MODELS,
        // Official endpoint: the base URL is fixed and pre-filled from
        // `default_url`, so the editor hides the Base URL field. The model
        // collection (`OPENAI_BUILTIN_MODELS`) is seeded as channels — the
        // user only supplies a name and token, and the stage-2 picker lists
        // the served models.
        needs_url: false,
        url_hint: "https://api.openai.com/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.openai.com/v1/chat/completions"),
        user_agent: None,
    },
    ProviderTemplate {
        label: "Anthropic",
        description: "Claude models over the Anthropic /messages API",
        protocol: "anthropic",
        models: neenee_providers::ANTHROPIC_BUILTIN_MODELS,
        needs_url: true,
        url_hint: "https://api.anthropic.com/v1/messages",
        needs_model: false,
        default_url: Some("https://api.anthropic.com/v1/messages"),
        user_agent: None,
    },
    ProviderTemplate {
        label: "Google Gemini",
        description: "Native Gemini API — Google AI Studio or compatible relay",
        protocol: "gemini",
        models: neenee_providers::GOOGLE_BUILTIN_MODELS,
        needs_url: true,
        url_hint: "https://generativelanguage.googleapis.com/v1beta",
        needs_model: false,
        default_url: Some("https://generativelanguage.googleapis.com/v1beta"),
        user_agent: None,
    },
    ProviderTemplate {
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
    },
    ProviderTemplate {
        label: "Kimi Code",
        description: "Moonshot Kimi coding-plan endpoint",
        protocol: "openai",
        models: &["kimi-k2.7-code"],
        // Official endpoint: base URL is fixed and pre-filled, no field shown.
        needs_url: false,
        url_hint: "https://api.kimi.com/coding/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://api.kimi.com/coding/v1/chat/completions"),
        user_agent: Some("opencode/0.1.0"),
    },
    ProviderTemplate {
        label: "ZAI Code",
        description: "Z.AI coding-plan endpoint",
        protocol: "openai",
        models: &["glm-5.2"],
        // Official endpoint: base URL is fixed and pre-filled, no field shown.
        needs_url: false,
        url_hint: "https://api.z.ai/api/coding/paas/v4/chat/completions",
        needs_model: false,
        default_url: Some("https://api.z.ai/api/coding/paas/v4/chat/completions"),
        user_agent: Some("opencode/1.17.10"),
    },
    ProviderTemplate {
        label: "OpenCode Go",
        description: "opencode.ai relay — OpenAI chat-completions coding models",
        protocol: "openai",
        models: &["glm-5.2", "kimi-k2.7-code", "deepseek-v4-flash"],
        needs_url: true,
        url_hint: "https://opencode.ai/zen/go/v1/chat/completions",
        needs_model: false,
        default_url: Some("https://opencode.ai/zen/go/v1/chat/completions"),
        user_agent: None,
    },
    ProviderTemplate {
        label: "Anthropic (sub2api)",
        description: "Anthropic sub2api relay",
        protocol: "anthropic",
        models: neenee_providers::ANTHROPIC_BUILTIN_MODELS,
        needs_url: true,
        url_hint: "https://relay.example.com/v1/messages",
        needs_model: false,
        default_url: None,
        user_agent: None,
    },
    ProviderTemplate {
        label: "OpenAI (sub2api)",
        description: "OpenAI sub2api relay",
        protocol: "openai",
        models: OPENAI_SUB2API_MODELS,
        needs_url: true,
        url_hint: "https://relay.example.com/v1/chat/completions",
        needs_model: false,
        default_url: None,
        user_agent: None,
    },
    // Antigravity — a sub2api-style Gemini-native 中转站. The relay forwards
    // model ids verbatim to the Gemini REST surface, so the `gemini` protocol
    // reaches it unchanged. The base URL is editable and pre-filled with a
    // documentation-safe example; users can replace it with their relay host.
    // The three effort-tiered / non-preview ids are seeded as channels — they
    // resolve in the model registry, so the stage-2 list and add-model overlay
    // see real metadata.
    //
    // Model order is deliberate: `AddProvider` activates the FIRST seeded
    // model as the default, and `gemini-3.1-pro-high` is known to be rejected
    // by some relays for every request shape (HTTP 400 INVALID_ARGUMENT — a
    // relay-side defect, not a config issue; `-low` and `flash` often work).
    // So the generally compatible models lead and `-high` sits last, still
    // selectable from stage 2 the moment the relay accepts it.
    ProviderTemplate {
        label: "Antigravity (sub2api)",
        description: "Antigravity sub2api relay",
        protocol: "gemini",
        models: &[
            "gemini-3-flash",
            "gemini-3.1-pro-low",
            "gemini-3.1-pro-high",
        ],
        needs_url: true,
        url_hint: "https://relay.example.com/antigravity/v1beta",
        needs_model: false,
        default_url: Some("https://relay.example.com/antigravity/v1beta"),
        user_agent: None,
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
        .unwrap_or_else(|| "＋ Add provider".to_string())
}

/// The ordered editor fields shown when **editing** an existing user provider:
/// Name, Base URL, and Token. The Model field is omitted — models (and their
/// per-model reasoning, ADR-0046) are managed in the stage-2 list. The Base URL
/// is editable for every protocol, including native Gemini (a 中转站/relay
/// supplies its versioned host, e.g. `https://relay.example.com/v1beta`).
pub fn edit_fields(protocol: &str) -> Vec<CustomField> {
    let _ = protocol;
    vec![CustomField::Name, CustomField::BaseUrl, CustomField::Token]
}

/// Whether a protocol's model set is *closed*: the candidate list is the full,
/// fixed set and the add-model overlay must NOT offer a free-text fallback.
/// OpenAI and Anthropic relays serve an open, evolving model set, so
/// typing an unlisted id is legitimate; native Gemini is a closed family — its
/// models are enumerated by Google and forwarded verbatim by relays, so an
/// arbitrary id is almost certainly a typo or hallucination, not a real model.
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
        "gemini" => WireFormat::Gemini,
        _ => WireFormat::OpenAiCompat,
    };
    KNOWN_MODELS
        .iter()
        .filter(|m| m.format == format)
        .map(|m| m.id)
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

/// One selectable row in the **stage-2 model sub-list**: a single
/// (provider, model) pair within one drilled-into provider. Built by
/// [`provider_models_filtered_from`]; the picker browses, searches, and
/// activates these once a multi-model provider is opened.
pub struct RankedModel {
    /// Canonical id of the provider serving this model (its snapshot row id).
    pub provider_id: String,
    /// Wire model id to activate.
    pub model: String,
    /// The rendered label — the model's display name (stage 2 is already scoped
    /// to one provider, so no provider suffix). The fuzzy match indexes directly
    /// onto these characters.
    pub label: String,
    /// Channel protocol and model-specific controls surfaced by the picker
    /// snapshot. OpenAI rows can expose effort; Anthropic rows can expose
    /// effort plus thinking.
    pub protocol: String,
    pub effort: Option<String>,
    pub thinking: Option<bool>,
    /// Unix epoch milliseconds of this model's last activation (`None` = never,
    /// sorts as oldest). Drives the stage-2 recency sort.
    pub last_used_ms: Option<u64>,
    /// The fuzzy match against `label`, or `None` in browse mode (empty query),
    /// where every row is shown unhighlighted.
    pub m: Option<fuzzy::FuzzyMatch>,
}

/// One selectable row in the **stage-1 provider list**. Carries everything the
/// renderer and input handler need (copied out of the snapshot row), so neither
/// re-indexes the snapshot. The two-stage picker shows providers first (this),
/// then drills into a single provider's models ([`RankedModel`]) on activation.
pub struct RankedProvider {
    /// Index into [`ProviderPickerSnapshot::rows`] of this provider (stable
    /// across re-filtering, so it identifies the drilled-into provider).
    pub row_idx: usize,
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
    /// Whether the provider hosts more than one model (its activation opens the
    /// stage-2 model picker). Single-model providers activate directly.
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

/// The favorite → last-used-desc → name ordering shared by both picker stages.
/// Pulls each provider's live signals from its snapshot row.
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

/// Build the **stage-1** provider rows: one per snapshot row, fuzzy-filtered by
/// `query` against the provider name and sorted favorite → last-used → name. An
/// empty `query` (browse mode) keeps every provider with no match positions.
pub fn providers_filtered_from(
    picker: &ProviderPickerSnapshot,
    query: &str,
) -> Vec<RankedProvider> {
    let mut rows: Vec<RankedProvider> = Vec::new();
    for (row_idx, prow) in picker.rows.iter().enumerate() {
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
            row_idx,
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

/// Build the **stage-2** model rows for a single provider: one [`RankedModel`]
/// per model the provider serves, fuzzy-filtered by `query` against the model
/// display name, sorted most-recently-used first (catalog order as the stable
/// fallback). `row_idx` indexes into `picker.rows`; an out-of-range index
/// yields no rows.
pub fn provider_models_filtered_from(
    picker: &ProviderPickerSnapshot,
    row_idx: usize,
    query: &str,
) -> Vec<RankedModel> {
    let Some(prow) = picker.rows.get(row_idx) else {
        return Vec::new();
    };
    let mut rows: Vec<RankedModel> = Vec::new();
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
        // Stage 2 is already scoped to one provider, so the label is just the
        // model name — no provider suffix to disambiguate.
        let label = model_display_name(model);
        let m = if query.is_empty() {
            None
        } else {
            match fuzzy::fuzzy_match(&label, query) {
                Some(m) => Some(m),
                None => continue,
            }
        };
        rows.push(RankedModel {
            provider_id: prow.id.clone(),
            model: model.clone(),
            label,
            protocol: info.protocol,
            effort: info.effort,
            thinking: info.thinking,
            last_used_ms: info.last_used_ms,
            m,
        });
    }
    // Most-recently-used first; `None` (never activated) sorts as oldest, and
    // catalog order (the iteration order above) is the stable tiebreaker so a
    // never-used provider keeps its curated model sequence.
    rows.sort_by(|a, b| {
        model_order(a.last_used_ms, b.last_used_ms).then_with(|| a.label.cmp(&b.label))
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
        // The Antigravity (sub2api) relay ids are registered in KNOWN_MODELS as
        // native Gemini, so the add-model overlay for a Gemini provider offers
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
        // adds it from "＋ Add provider" without editing config.toml. Its host
        // is fixed, so the base URL is pre-filled (`default_url`); the three
        // effort-tiered / non-preview ids are seeded; and it speaks the gemini
        // protocol (no free-text Model field — the closed family is the seed).
        let tmpl = PROVIDER_TEMPLATES
            .iter()
            .find(|t| t.label == "Antigravity (sub2api)")
            .expect("antigravity template offered in the chooser");
        assert_eq!(tmpl.protocol, "gemini");
        assert_eq!(
            tmpl.models,
            &[
                "gemini-3-flash",
                "gemini-3.1-pro-low",
                "gemini-3.1-pro-high"
            ]
        );
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
        assert_eq!(tmpl.models, OPENAI_SUB2API_MODELS);
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
    fn stage1_lists_one_row_per_provider_including_custom() {
        let snapshot = sample();
        let rows = providers_filtered_from(&snapshot, "");
        assert_eq!(rows.len(), snapshot.rows.len());
        // The user-defined provider shows up like any built-in.
        assert!(rows.iter().any(|r| r.id == "my-relay"));
    }

    #[test]
    fn stage1_fuzzy_filters_by_provider_name() {
        let snapshot = sample();
        let rows = providers_filtered_from(&snapshot, "anthro");
        assert!(rows.iter().all(|r| r.id == "anthropic"));
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn stage1_sorts_favorites_first_within_group() {
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
    fn stage1_no_longer_groups_builtins_before_custom() {
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
    fn stage2_lists_a_single_providers_models() {
        let snapshot = sample();
        let idx = snapshot.rows.iter().position(|r| r.id == "openai").unwrap();
        let rows = provider_models_filtered_from(&snapshot, idx, "");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.provider_id == "openai"));
        assert!(rows.iter().any(|r| r.model == "gpt-4o"));
    }

    #[test]
    fn stage2_single_model_provider_yields_one_row() {
        let snapshot = sample();
        let idx = snapshot
            .rows
            .iter()
            .position(|r| r.id == "kimi-code")
            .unwrap();
        let rows = provider_models_filtered_from(&snapshot, idx, "");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "kimi-k2.7-code");
    }

    #[test]
    fn stage2_sorts_most_recently_used_model_first() {
        // Build an OpenAI row whose two models carry per-model last-used
        // timestamps: gpt-4o-mini was used more recently than gpt-4o, so it
        // must sort above gpt-4o even though gpt-4o is listed first (catalog
        // order).
        let mut snapshot = sample();
        let openai = snapshot.rows.iter_mut().find(|r| r.id == "openai").unwrap();
        openai.model_info = vec![
            ProviderModelInfo {
                model: "gpt-4o".to_string(),
                protocol: "openai".to_string(),
                effort: None,
                thinking: None,
                last_used_ms: Some(100),
            },
            ProviderModelInfo {
                model: "gpt-4o-mini".to_string(),
                protocol: "openai".to_string(),
                effort: None,
                thinking: None,
                last_used_ms: Some(5_000),
            },
        ];
        let idx = snapshot.rows.iter().position(|r| r.id == "openai").unwrap();
        let rows = provider_models_filtered_from(&snapshot, idx, "");
        assert_eq!(rows[0].model, "gpt-4o-mini", "most-recently-used first");
        assert_eq!(rows[1].model, "gpt-4o");
    }

    #[test]
    fn stage2_never_used_models_keep_catalog_order() {
        // When no model has a recency timestamp, the stage-2 list falls back
        // to the curated catalog order (here gpt-4o before gpt-4o-mini) plus a
        // stable label tiebreaker.
        let snapshot = sample();
        let idx = snapshot.rows.iter().position(|r| r.id == "openai").unwrap();
        let rows = provider_models_filtered_from(&snapshot, idx, "");
        let ids: Vec<&str> = rows.iter().map(|r| r.model.as_str()).collect();
        assert_eq!(ids, vec!["gpt-4o", "gpt-4o-mini"]);
    }
}
