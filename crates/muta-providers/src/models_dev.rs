//! Bridge from the `muta-models-dev` third-party catalog into the client's
//! discovery types.
//!
//! `muta-models-dev` is deliberately schema-neutral (it must not depend on
//! `muta-providers`). This module owns the mapping from its [`DevModel`] into
//! the client's [`DiscoveredModel`], and exposes the async entry point the
//! discovery reconciler calls for `LiveCatalog::ModelsDev` presets.

use muta_contracts::ThinkingSupport;
use muta_models_dev::DevModel;

use crate::DiscoveredModel;

/// Re-exported so consumers (the discovery reconciler) can name the error.
pub use muta_models_dev::ModelsDevError;

/// Map one schema-neutral models.dev model into the client's discovered model.
///
/// Wire format is `None` here: models.dev's `npm` field describes the SDK
/// family, not an exact relay route, and the opencode-go relay routes by the
/// model's registered protocol (see `route_for_model` / wire-override table),
/// so the discovery layer must not override it. Capability hints (family,
/// context, reasoning, effort, vision) are carried as `Option`s so the
/// reconciler can trust/persist them per preset (fitting).
fn from_dev_model(m: DevModel) -> DiscoveredModel {
    let modalities_in = &m.modalities.input;
    let reasoning = m.reasoning.then_some(true);
    let thinking = if m.reasoning {
        // models.dev does not distinguish reasoning-content from summary; the
        // opencode-go relay surfaces it via the OpenAI-compatible stream, so
        // the conservative mapping is `ReasoningContent`.
        Some(ThinkingSupport::ReasoningContent)
    } else {
        None
    };
    let effort_levels = m
        .reasoning_options
        .iter()
        .filter(|opt| opt.r#type == "effort")
        .flat_map(|opt| opt.values.iter().flatten())
        .cloned()
        .collect::<Vec<_>>();
    DiscoveredModel {
        id: m.id,
        picker_enabled: None,
        protocol: None,
        family: m.family.clone(),
        context_window: m.limit.context.map(|c| c as usize),
        max_output_tokens: m.limit.output.map(|o| o as u32),
        reasoning,
        thinking,
        tool_call: m.tool_call.then_some(true),
        vision: if modalities_in.is_empty() {
            None
        } else {
            Some(modalities_in.iter().any(|m| m == "image"))
        },
        effort_levels: (!effort_levels.is_empty()).then_some(effort_levels),
    }
}

/// Resolve a `ModelsDev`-sourced provider's live model list and map it into
/// the client's discovery shape, mirroring a first-party
/// [`crate::discover_models`] call.
pub async fn models_dev_models(provider_id: &str) -> Result<Vec<DiscoveredModel>, ModelsDevError> {
    let models = muta_models_dev::provider_models(provider_id).await?;
    Ok(models.into_iter().map(from_dev_model).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_models_dev::{DevLimit, DevModalities, DevModel, DevReasoningOption};

    fn sample_model() -> DevModel {
        DevModel {
            id: "glm-5.3".to_string(),
            name: "GLM-5.3".to_string(),
            family: Some("glm".to_string()),
            reasoning: true,
            reasoning_options: vec![DevReasoningOption {
                r#type: "effort".to_string(),
                values: vec![
                    Some("low".to_string()),
                    Some("high".to_string()),
                    Some("max".to_string()),
                ],
            }],
            tool_call: true,
            limit: DevLimit {
                context: Some(1_000_000),
                output: Some(131_072),
            },
            modalities: DevModalities {
                input: vec!["text".to_string()],
                output: vec!["text".to_string()],
            },
            attachment: false,
            status: None,
        }
    }

    #[test]
    fn maps_capabilities_and_effort_ladder() {
        let dm = from_dev_model(sample_model());
        assert_eq!(dm.id, "glm-5.3");
        assert_eq!(dm.family.as_deref(), Some("glm"));
        assert_eq!(dm.context_window, Some(1_000_000));
        assert_eq!(dm.max_output_tokens, Some(131_072));
        assert_eq!(dm.thinking, Some(ThinkingSupport::ReasoningContent));
        assert_eq!(dm.reasoning, Some(true));
        assert_eq!(dm.tool_call, Some(true));
        assert_eq!(dm.vision, Some(false));
        assert_eq!(
            dm.effort_levels,
            Some(vec![
                "low".to_string(),
                "high".to_string(),
                "max".to_string()
            ])
        );
        // Wire format is intentionally not asserted from models.dev.
        assert_eq!(dm.protocol, None);
    }

    #[test]
    fn vision_is_derived_from_input_modalities() {
        let mut m = sample_model();
        m.modalities.input = vec!["text".to_string(), "image".to_string()];
        assert_eq!(from_dev_model(m).vision, Some(true));
    }
}
