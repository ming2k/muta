//! The `custom-openai` provider template: any OpenAI-compatible endpoint the
//! user supplies — third-party relays, self-hosted gateways, subscription
//! bundles that expose a `/v1/chat/completions` surface.
//!
//! This template is the generic escape hatch: unlike the curated templates it
//! seeds **no** model list. The editor shows a free-text Model field (with the
//! registry-known OpenAI ids as fuzzy suggestions plus the raw typed id as a
//! custom value), so a single model id of the user's choosing becomes the one
//! seeded channel. More models are added afterwards from the Models picker.
//!
//! Baselines for the third-party ids this template is known to serve are
//! registered alongside (see [`MODELS`]); ids the registry does not know
//! resolve through the conservative fallback, which is fine for a relay whose
//! serving contract is user-asserted.

use neenee_contracts::thinking::ThinkingSupport;
use neenee_contracts::{Model, WireFormat};

use super::ProviderTemplateSpec;

/// Baseline capability metadata for third-party model ids surfaced by
/// OpenAI-compatible relays but not registered by a curated template.
///
/// These are ids whose serving contract differs from the same-named registry
/// entry (e.g. a relay that expects the cased spelling `GLM-5.2` where the
/// registry knows the lowercase `glm-5.2`). Casing matters: model lookup is
/// exact-match, so the cased spelling needs its own baseline to carry a
/// context window at all.
pub const MODELS: &[Model] = &[
    // ── WeChat OpenAI-compatible endpoint (chatapi.weixin.qq.com) ─────────
    // Served ids are case-sensitive there ("GLM-5.2", "Deepseek-v4-flash");
    // the lowercase spellings 400 with `invalid model`. Windows follow the
    // endpoint's advertised spec: 200K input / 48K output.
    Model {
        id: "GLM-5.2",
        family: "glm",
        context_window: 200_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_contracts::effort::EFFORT_GLM_5,
    },
    Model {
        id: "Deepseek-v4-flash",
        family: "deepseek",
        context_window: 200_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_contracts::effort::EFFORT_LOW_HIGH_MAX,
    },
];

inventory::submit!(neenee_contracts::model::BaselineModels(MODELS));

pub(crate) const TEMPLATE_SPEC: ProviderTemplateSpec = ProviderTemplateSpec {
    id: "custom-openai",
    baselines: MODELS,
    base_url: "",
    user_agent: None,
    protocol: "openai",
    // No live discovery: arbitrary relays' `GET /models` is an availability
    // signal at best, and a user-supplied single-model endpoint must keep the
    // id the user typed rather than being re-seeded from a template snapshot.
    discovery: false,
    fitting: false,
    models: &[],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cased_third_party_ids_are_case_insensitively_unique() {
        // The registry rejects duplicate baseline ids, and lookup is
        // exact-match — so a cased id and its lowercase cousin are two
        // distinct entries. Guard the pairing explicitly so neither a rename
        // nor a lowercase normalization can silently merge them.
        let glm = neenee_contracts::model::resolve("GLM-5.2");
        assert_eq!(glm.context_window, 200_000);
        let lowercase = neenee_contracts::model::resolve("glm-5.2");
        assert_eq!(lowercase.context_window, 1_000_000);
        assert_ne!(glm.context_window, lowercase.context_window);
    }

    #[test]
    fn template_seeds_no_models() {
        // The generic template's contract: no seeded models — the Model field
        // supplies the one id, and the catalog must never re-seed the
        // instance from an empty snapshot (reseed's empty guard).
        assert!(TEMPLATE_SPEC.models.is_empty());
    }
}
