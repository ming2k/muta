//! Builtin seed/baseline model lists for the shipped model providers.
//!
//! Pure static data shared by the daemon (catalog reconciliation) and the
//! frontend (add-connection chooser). Single source of truth;
//! `muta-providers` re-exports these for its model provider specs.

pub const ANTHROPIC_BUILTIN_MODELS: &[&str] = &[
    "claude-fable-5",
    "claude-sonnet-5",
    "claude-opus-4-8",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
];

pub const CHATGPT_BUILTIN_MODELS: &[&str] = &[];

pub const DEEPSEEK_BUILTIN_MODELS: &[&str] = &[
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "deepseek-v4-flash-vision-exp",
];

pub const COPILOT_SEED_MODELS: &[&str] = &["gpt-4o-mini"];

pub const GOOGLE_BUILTIN_MODELS: &[&str] = &[
    // Gemini 3.x
    "gemini-3.8-flash",
    "gemini-3.7-flash",
    "gemini-3.5-flash",
    "gemini-3-pro-preview",
    "gemini-3-flash-preview",
    "gemini-3.1-pro-preview",
    "gemini-3.1-pro-preview-customtools",
    // Gemini 2.5
    "gemini-2.5-flash",
    "gemini-2.5-pro",
    "gemini-2.5-flash-lite",
    // Gemini 2.0 (still widely served by relays)
    "gemini-2.0-flash",
];

pub const ANTIGRAVITY_OAUTH_MODELS: &[&str] = &[
    "gemini-3.8-flash",
    "gemini-3.8-flash-tiered",
    "gemini-3.7-flash",
    "gemini-3.7-flash-tiered",
    "gemini-pro-agent",
    "gemini-3.1-pro-low",
    "gemini-3.1-flash-lite",
    "gemini-2.5-flash",
    "gemini-2.5-pro",
];

pub const KIMI_CODE_MODELS: &[&str] = &["k3", "kimi-k2.7-code"];

pub const OPENAI_BUILTIN_MODELS: &[&str] = &[
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
];

/// Offline seed for OpenRouter. Its live `/models` catalog is authoritative;
/// this keeps the primary Nex coding model selectable before the first refresh.
pub const OPENROUTER_BUILTIN_MODELS: &[&str] = &["nex-agi/nex-n2.5-pro:free"];

pub const OPENCODE_GO_MODELS: &[&str] = &["glm-5.2", "kimi-k2.7-code", "deepseek-v4-flash"];

pub const ZAI_CODE_MODELS: &[&str] = &["glm-5.3", "glm-5.3-flash", "glm-5.2"];

pub const XAI_BUILTIN_MODELS: &[&str] = &["grok-4.5", "grok-4.20", "grok-4.3", "grok-build-0.1"];

// ═════════════════════════════════════════════════════════════════════════════
// Model provider ids — contract vocabulary (ADR-0201)
// ═════════════════════════════════════════════════════════════════════════════

/// The closed set of model provider ids.
///
/// Every connection persists one of these, so the vocabulary is contract data
/// rather than a `muta-providers` implementation detail. `muta-providers`
/// owns the behavior behind each id (endpoint, protocol, catalog source) and
/// its registry is asserted to cover this list.
///
/// A provider id names a **service surface** — (endpoint family, wire dialect,
/// model universe). It never encodes a wire protocol or an authentication mode
/// (ADR-0201 INV-1).
pub const MODEL_PROVIDER_IDS: &[&str] = &[
    "openai",
    "openai-subscription",
    "anthropic",
    "google",
    "google-antigravity",
    "github-copilot",
    "xai",
    "deepseek",
    "glm-cn",
    "kimi-code",
    "openrouter",
    "opencode-go",
    "custom",
];

/// Whether `id` names a registered model provider.
pub fn is_known_model_provider(id: &str) -> bool {
    MODEL_PROVIDER_IDS.contains(&id)
}

/// Map a provider id from any era to its canonical id, or `None` when the
/// value names no provider at all.
///
/// The pre-ADR-0201 spellings appended `-oauth` to encode an authentication
/// mode; `zai-code` named the CN endpoint with the international brand. These
/// aliases exist **only** to migrate an existing `connections.toml` on load —
/// nothing serializes them back.
pub fn canonical_provider_id(id: &str) -> Option<String> {
    let canonical = match id {
        "chatgpt-oauth" => "openai-subscription",
        "antigravity-oauth" => "google-antigravity",
        "copilot-oauth" => "github-copilot",
        "xai-oauth" => "xai",
        "zai-code" => "glm-cn",
        "custom-openai" => "custom",
        other => other,
    };
    is_known_model_provider(canonical).then(|| canonical.to_string())
}

#[cfg(test)]
mod provider_id_tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_never_encode_protocol_or_auth() {
        let mut ids = MODEL_PROVIDER_IDS.to_vec();
        ids.sort_unstable();
        let dups: Vec<&str> = ids
            .windows(2)
            .filter(|pair| pair[0] == pair[1])
            .map(|pair| pair[0])
            .collect();
        assert!(dups.is_empty(), "duplicate provider ids: {dups:?}");
        for id in MODEL_PROVIDER_IDS {
            assert!(
                !id.ends_with("-oauth") && !id.ends_with("-compatible"),
                "{id} encodes an auth mode or a protocol (ADR-0201 INV-1)"
            );
        }
    }

    #[test]
    fn legacy_ids_canonicalize_and_unknown_ids_are_rejected() {
        assert_eq!(
            canonical_provider_id("chatgpt-oauth").as_deref(),
            Some("openai-subscription")
        );
        assert_eq!(
            canonical_provider_id("deepseek").as_deref(),
            Some("deepseek")
        );
        assert_eq!(canonical_provider_id("zai-code").as_deref(), Some("glm-cn"));
        assert!(canonical_provider_id("does-not-exist").is_none());
        assert!(canonical_provider_id("").is_none());
    }
}
