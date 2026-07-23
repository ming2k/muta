//! OpenAI-compatible provider registry and the `Channel` → concrete `Provider`
//! factory consumed by the orchestration layer.

use neenee_core::Provider;
use neenee_core::catalog::{Channel, Transport};
use std::sync::Arc;

use crate::{
    AnthropicMessagesProvider, GoogleProvider, NEENEE_USER_AGENT, OpenAiProvider,
    ResponsesProvider, ThinkingConfig,
};

// ═════════════════════════════════════════════════════════════════════════════
// OpenAI-compatible provider wrappers for popular Chinese & global services
// ═════════════════════════════════════════════════════════════════════════════

/// Per-model `max_tokens` for the Anthropic `/messages` surface. The Messages
/// API requires `max_tokens`; capping the response at the model's registered
/// output limit (rather than a flat 8192) lets long agent turns from
/// high-output models (MiniMax M3: 131072) run untruncated. Values mirror
/// models.dev's opencode-go entries. Unknown models fall back to the default
/// inside [`AnthropicMessagesProvider`].
const ANTHROPIC_MODEL_MAX_TOKENS: &[(&str, u32)] = &[
    ("minimax-m3", 131072),
    ("minimax-m2.7", 131072),
    ("minimax-m2.5", 65536),
    ("qwen3.7-max", 65536),
    ("qwen3.7-plus", 65536),
    ("qwen3.6-plus", 65536),
    ("qwen3.5-plus", 65536),
    // Claude family served via Anthropic-compatible relays.
    // Claude 4.6+ Opus/Sonnet support a 128K synchronous output limit (1M
    // context); Haiku 4.5 supports 64K. Cap there so long agent turns are not
    // truncated by the provider's flat 8192 default.
    ("claude-opus-4-8", 128000),
    ("claude-fable-5", 128000),
    ("claude-sonnet-5", 128000),
    ("claude-sonnet-4-6", 128000),
    ("claude-haiku-4-5-20251001", 64000),
];

/// Look up the `max_tokens` for an Anthropic-format model id. `None` lets the
/// provider fall back to its built-in default.
fn anthropic_model_max_tokens(model_id: &str) -> Option<u32> {
    ANTHROPIC_MODEL_MAX_TOKENS
        .iter()
        .find(|(id, _)| *id == model_id)
        .map(|(_, tokens)| *tokens)
}

/// Specification for an OpenAI-compatible provider.
///
/// Every provider in [`OPENAI_PROVIDER_SPECS`] speaks the OpenAI
/// chat-completions wire format and differs only in endpoint, default model,
/// the environment variables consulted, and (rarely) a pinned model or a
/// required user agent. Modelling them as *data* rather than one delegating
/// newtype per vendor means adding a provider is a single table entry instead
/// of ~30 lines of boilerplate trait delegation.
pub struct OpenAiProviderSpec {
    /// Stable identifier used in config (`default_provider`) and the TUI.
    pub id: &'static str,
    /// Full chat-completions endpoint URL.
    pub base_url: &'static str,
    /// Model used when neither config nor environment specifies one.
    pub default_model: &'static str,
    /// Environment variable consulted for the API key.
    pub env_api_key: &'static str,
    /// Environment variable consulted for a model override.
    pub env_model: &'static str,
    /// When set, the endpoint pins this model and ignores any override
    /// (e.g. the Kimi coding endpoint).
    pub fixed_model: Option<&'static str>,
    /// When set, the endpoint requires this user agent unless overridden.
    pub default_user_agent: Option<&'static str>,
}

/// The single registry of OpenAI-compatible providers — the source of truth for
/// their endpoints, default models, and environment variables.
pub const OPENAI_PROVIDER_SPECS: &[OpenAiProviderSpec] = &[
    // Kimi Code — Moonshot AI's coding platform (api.kimi.com/coding/v1).
    // The platform pins the model id to the fixed `k3` alias (Kimi K3, 1M
    // context, always-on thinking); its live `GET /models` also lists the
    // legacy `kimi-for-coding` (K2.7) ids, kept selectable via
    // [`KIMI_CODE_MODELS`]. API key env still uses the MOONSHOT_API_KEY
    // legacy name for config compatibility. The `opencode/0.1.0` User-Agent
    // is borrowed on purpose: the endpoint was live-tested (2026-07) to
    // accept any UA — including none — under OAuth auth, but whether the
    // API-key path gates on a recognized coding-agent UA is unknown, so the
    // recognized default stays as the zero-risk choice.
    OpenAiProviderSpec {
        id: "kimi-code",
        base_url: "https://api.kimi.com/coding/v1/chat/completions",
        default_model: "k3",
        env_api_key: "MOONSHOT_API_KEY",
        env_model: "MOONSHOT_MODEL",
        fixed_model: Some("k3"),
        default_user_agent: Some("opencode/0.1.0"),
    },
    // DeepSeek V4 (Flash + Pro) is served as one multi-model `deepseek` provider
    // built in the catalog layer (both models share one DEEPSEEK_API_KEY), not as
    // two single-model registry presets.
    // ZAI Code — Z.AI (Zhipu) coding-plan platform
    // (api.z.ai/api/coding/paas/v4). A coding-agent membership endpoint that
    // serves the GLM-5 family; glm-5.2 is the current flagship. Like the Kimi
    // Code platform, it expects a recognized coding-agent User-Agent. Shares
    // the ZHIPU_API_KEY legacy name for key compatibility with the broader
    // Zhipu ecosystem, while ZAI_API_KEY is the preferred alias.
    OpenAiProviderSpec {
        id: "zai-code",
        base_url: "https://api.z.ai/api/coding/paas/v4/chat/completions",
        default_model: "glm-5.2",
        env_api_key: "ZAI_API_KEY",
        env_model: "ZAI_MODEL",
        fixed_model: None,
        default_user_agent: Some("opencode/1.17.10"),
    },
];

/// Look up an OpenAI-compatible provider spec by its identifier. Exact match
/// only; preset ids are unique and do not have alias mappings.
pub fn openai_provider_spec(id: &str) -> Option<&'static OpenAiProviderSpec> {
    OPENAI_PROVIDER_SPECS.iter().find(|spec| spec.id == id)
}

/// The Claude model ids the built-in `anthropic` provider serves, in display
/// order. The provider is a *configurable* Anthropic `/messages` relay: the
/// endpoint URL is supplied by config (defaulting to Anthropic's official API),
/// so the same preset serves the official API or any Anthropic-compatible relay.
/// Each id exists in the model registry, so its metadata (context window, output
/// limit, capabilities) resolves there.
pub const ANTHROPIC_BUILTIN_MODELS: &[&str] = &[
    "claude-fable-5",
    "claude-sonnet-5",
    "claude-opus-4-8",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
];

/// The Gemini model ids the built-in `google` provider serves (native Gemini
/// API, one key). Each id exists in the model registry. The set is the
/// canonical text-generation family that Google plus common relays/中转站
/// advertise — image/embedding/video/audio-only models are excluded since an
/// agent only consumes the `generateContent` text surface.
pub const GOOGLE_BUILTIN_MODELS: &[&str] = &[
    // ── Gemini 3.x ──
    "gemini-3.5-flash",
    "gemini-3-pro-preview",
    "gemini-3-flash-preview",
    "gemini-3.1-pro-preview",
    "gemini-3.1-pro-preview-customtools",
    // ── Gemini 2.5 ──
    "gemini-2.5-flash",
    "gemini-2.5-pro",
    "gemini-2.5-flash-lite",
    // ── Gemini 2.0 (still widely served by relays) ──
    "gemini-2.0-flash",
];

/// The model ids the built-in `deepseek` provider serves (V4 Flash + Pro over
/// the OpenAI-compatible API, one key). Each id exists in the model registry.
pub const DEEPSEEK_BUILTIN_MODELS: &[&str] = &["deepseek-v4-flash", "deepseek-v4-pro"];

/// xAI Grok models over OpenAI-compatible chat completions (SuperGrok OAuth or
/// `XAI_API_KEY`).
pub const XAI_BUILTIN_MODELS: &[&str] = &["grok-4.5", "grok-4.20", "grok-4.3", "grok-build-0.1"];

/// GPT-5.x models served over the ChatGPT subscription backend (the Codex
/// Responses API). These are the models a ChatGPT Pro/PLUS plan unlocks; the
/// Responses transport routes them to `chatgpt.com/backend-api/codex/responses`.
/// Each id exists in the model registry.
pub const CHATGPT_BUILTIN_MODELS: &[&str] = &[
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
];

/// The minimal model seed for a fresh GitHub Copilot instance, before its
/// first live discovery completes. A Copilot instance uses `discovery: true`
/// and `fitting: true` (see [`COPILOT`](crate::COPILOT) / the `copilot-oauth`
/// template), so its real channel set is populated from
/// `GET api.githubcopilot.com/models` at runtime — this seed only needs one
/// universally available id so a brand-new instance activates without a 400.
/// `gpt-4o-mini` is unlocked on every Copilot plan (incl. Free/Student).
pub const COPILOT_SEED_MODELS: &[&str] = &["gpt-4o-mini"];

/// The model ids the built-in `openai` provider serves over the OpenAI
/// chat-completions API, one key (`OPENAI_API_KEY`). Mirrors OpenAI's current
/// frontier chat lineup — the GPT-5.6 tier-named family (`gpt-5.6-sol`, the
/// flagship, leads) plus the GPT-5.x family; `gpt-5.6-sol` is the default.
/// The legacy `gpt-4o`/`gpt-4o-mini` ids stay registered for existing
/// configs but are no longer seeded for the official provider. Each id exists
/// in the model registry.
pub const OPENAI_BUILTIN_MODELS: &[&str] = &[
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
];

/// Text/chat models commonly served by OpenAI-compatible sub2api relays.
///
/// Keep stable aliases first. Dated snapshots and image/audio/realtime models
/// are intentionally omitted; callers can still add a relay-specific model id.
pub const OPENAI_SUB2API_MODELS: &[&str] = &[
    // GPT-5.6 family (Sol/Terra/Luna) — OpenAI's tier-named flagship line.
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex-spark",
    "gpt-5.2",
    "gpt-5.2-chat-latest",
    "gpt-5.2-pro",
];

/// Models served by Moonshot's Kimi Code endpoint, in display/activation
/// order — the first entry is the initial active channel. `k3` is the
/// platform's current flagship; `kimi-k2.7-code` remains as the previous
/// pinned alias.
pub const KIMI_CODE_MODELS: &[&str] = &["k3", "kimi-k2.7-code"];

/// Models served by Z.AI's coding-plan endpoint.
pub const ZAI_CODE_MODELS: &[&str] = &["glm-5.2"];

/// Curated OpenAI-compatible models offered by the OpenCode Go template.
pub const OPENCODE_GO_MODELS: &[&str] = &["glm-5.2", "kimi-k2.7-code", "deepseek-v4-flash"];

/// The full catalogue the opencode-go relay (opencode.ai/zen/go) actually
/// serves — mirrors the opencode-go entries on models.dev, the same source
/// [`ANTHROPIC_MODEL_MAX_TOKENS`] follows. The legacy-config migration seeds
/// one channel per entry it knows (intersected with the client model
/// registry, which supplies each model's wire format and metadata).
///
/// Keeping this as an explicit allowlist — rather than deriving the seed from
/// registry families — is deliberate: a newly registered model must NOT
/// appear on the relay until the relay advertises it, otherwise users get a
/// channel that only ever answers "model not found". (Kimi `k3` and `glm-4.7`
/// are registered for other providers but unserved by go, for example.)
pub const OPENCODE_GO_SERVED_MODELS: &[&str] = &[
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "glm-5",
    "glm-5.1",
    "glm-5.2",
    "kimi-k2.5",
    "kimi-k2.6",
    "kimi-k2.7-code",
    "mimo-v2-omni",
    "mimo-v2-pro",
    "mimo-v2.5",
    "mimo-v2.5-pro",
    "minimax-m2.5",
    "minimax-m2.7",
    "minimax-m3",
    "qwen3.5-plus",
    "qwen3.6-plus",
    "qwen3.7-max",
    "qwen3.7-plus",
];

/// Gemini-native models advertised by Antigravity sub2api relays.
///
/// The order is deliberate: callers use the first model as the initial active
/// channel, while some relays reject the `-high` variant.
pub const ANTIGRAVITY_SUB2API_MODELS: &[&str] = &[
    "gemini-3-flash",
    "gemini-3.1-pro-low",
    "gemini-3.1-pro-high",
];

// ═════════════════════════════════════════════════════════════════════════════
// Provider templates — the seed spec for a user-added provider instance
// ═════════════════════════════════════════════════════════════════════════════

/// The reconciliation-relevant subset of an add-provider template.
///
/// A provider instance created from a template records the template's stable
/// [`id`](ProviderTemplateSpec::id) on its `UserProviderConfig`. At startup the
/// catalog uses the template protocol and seed `models` to reconcile the
/// instance. Fixed instances mirror the seed list; API-discovered instances
/// retain only provider-advertised ids known to the client for that protocol.
/// This struct is the source of truth for that mapping; it intentionally
/// lives in `neenee-providers` (where the model constants live) so the
/// reconciliation layer in `neenee-agent` and the UI templates in
/// `neenee-tui-view` both read one table. The UI-only fields (label /
/// description / placeholders) are **not** duplicated here.
pub struct ProviderTemplateSpec {
    /// Stable identifier persisted on the instance (`template_id`). Never
    /// reused and never renamed once shipped — it is the durable join key
    /// between an instance and its template.
    pub id: &'static str,
    /// Wire protocol the template's channels speak: `"openai"` | `"anthropic"`
    /// | `"gemini"`.
    pub protocol: &'static str,
    /// The model ids the template initially seeds, in display/activation order.
    /// Fixed instances continue to mirror this list.
    pub models: &'static [&'static str],
    /// Whether this template supports **live model-list discovery** — fetching
    /// the provider's actual `GET /models` list at startup instead of mirroring
    /// the compiled-in [`models`](Self::models) snapshot.
    ///
    /// When `true`, an instance created from this template defaults to
    /// `ModelSource::Api` (live availability intersected with the client model
    /// registry, retaining the last valid subset on error). When `false`, the
    /// instance always uses the snapshot. A template is marked `false` when its
    /// endpoint is a fixed single-model membership platform (Z.AI Code) or its
    /// model list is derived at runtime (opencode-go), since those would
    /// regress under a live overwrite.
    pub discovery: bool,
    /// Whether live discovery may **fit capability metadata** for model ids the
    /// client registry does not know — materializing them as channels with
    /// their advertised context window, reasoning, vision, and effort tiers
    /// (persisted per instance, then overlaid onto model resolution; see
    /// `neenee_core::model::register_fitted_models`).
    ///
    /// This is a trust decision, not a technical one: it is enabled only for
    /// official first-party endpoints whose `/models` advertises real
    /// capability fields (the Kimi Code platform). Arbitrary relays must never
    /// get it — a malicious or sloppy relay could otherwise inflate a model's
    /// context window or claim vision support the model lacks. When `false`,
    /// discovery keeps only registry-known ids (the historical behavior).
    pub fitting: bool,
}

/// The single registry of provider templates offered when adding a provider.
///
/// Each entry's `id` MUST be unique. The set is the source of truth shared by
/// the add-provider UI and the catalog's model reconciliation — a template id
/// recorded on a user instance resolves back to its entry here.
pub const PROVIDER_TEMPLATE_SPECS: &[ProviderTemplateSpec] = &[
    ProviderTemplateSpec {
        id: "openai",
        protocol: "openai",
        models: OPENAI_BUILTIN_MODELS,
        discovery: true,
        fitting: false,
    },
    ProviderTemplateSpec {
        id: "anthropic",
        protocol: "anthropic",
        models: ANTHROPIC_BUILTIN_MODELS,
        discovery: true,
        fitting: false,
    },
    ProviderTemplateSpec {
        id: "google",
        protocol: "gemini",
        models: GOOGLE_BUILTIN_MODELS,
        discovery: true,
        fitting: false,
    },
    ProviderTemplateSpec {
        id: "deepseek",
        protocol: "openai",
        models: DEEPSEEK_BUILTIN_MODELS,
        discovery: true,
        fitting: false,
    },
    ProviderTemplateSpec {
        id: "xai-oauth",
        protocol: "openai",
        models: XAI_BUILTIN_MODELS,
        discovery: true,
        fitting: false,
    },
    ProviderTemplateSpec {
        id: "chatgpt-oauth",
        // The Responses transport is the OpenAI wire family; discovery is
        // disabled because the ChatGPT subscription backend does not expose a
        // standard `GET /models` list, and the plan-unlocked set is fixed.
        protocol: "openai",
        models: CHATGPT_BUILTIN_MODELS,
        discovery: false,
        fitting: false,
    },
    ProviderTemplateSpec {
        id: "copilot-oauth",
        // Copilot speaks the OpenAI chat-completions wire family against
        // api.githubcopilot.com. Discovery + fitting are enabled so the
        // instance tracks the user's actual plan-unlocked model set (which
        // varies by plan: Free/Student get only the GPT-4o chat family, Pro+
        // unlocks GPT-5) without a hardcoded model list — every advertised id
        // the client registry does not know is fitted with its advertised
        // capability metadata, mirroring the kimi-code flow.
        protocol: "openai",
        discovery: true,
        fitting: true,
        // Minimal seed: the id a fresh Copilot instance activates before the
        // first live discovery completes. `gpt-4o-mini` is universally
        // available across every Copilot plan, so the seed never 400s.
        models: COPILOT_SEED_MODELS,
    },
    ProviderTemplateSpec {
        id: "kimi-code",
        protocol: "openai",
        // The Kimi Code platform exposes a live /models endpoint, so instances
        // created from this template track the platform's actual model list.
        discovery: true,
        // It is also a trusted first-party endpoint whose /models advertises
        // real capability fields: platform-native ids the static registry does
        // not know (e.g. `kimi-for-coding`, and every future model) are fitted
        // with their advertised metadata instead of being intersected away —
        // new platform models become usable with zero client changes.
        fitting: true,
        models: KIMI_CODE_MODELS,
    },
    ProviderTemplateSpec {
        id: "zai-code",
        protocol: "openai",
        // Same rationale as kimi-code: a single pinned membership-platform model.
        discovery: false,
        fitting: false,
        models: ZAI_CODE_MODELS,
    },
    ProviderTemplateSpec {
        id: "opencode-go",
        protocol: "openai",
        // opencode-go's model list is derived at runtime from KNOWN_MODELS and
        // spans multiple transports; a live overwrite would regress it.
        discovery: false,
        fitting: false,
        models: OPENCODE_GO_MODELS,
    },
    ProviderTemplateSpec {
        id: "anthropic-sub2api",
        protocol: "anthropic",
        // A sub2api relay advertises whatever Claude models it forwards; live
        // discovery surfaces the relay's actual set.
        discovery: true,
        fitting: false,
        models: ANTHROPIC_BUILTIN_MODELS,
    },
    ProviderTemplateSpec {
        id: "openai-sub2api",
        protocol: "openai",
        discovery: true,
        fitting: false,
        models: OPENAI_SUB2API_MODELS,
    },
    ProviderTemplateSpec {
        id: "antigravity-sub2api",
        protocol: "gemini",
        discovery: true,
        fitting: false,
        models: ANTIGRAVITY_SUB2API_MODELS,
    },
];

/// Look up a template spec by its stable id. Exact match only.
pub fn provider_template_spec(id: &str) -> Option<&'static ProviderTemplateSpec> {
    PROVIDER_TEMPLATE_SPECS.iter().find(|spec| spec.id == id)
}

impl OpenAiProviderSpec {
    /// Resolve the model to use: a pinned `fixed_model` always wins, otherwise
    /// the caller's override, otherwise the provider default.
    pub fn resolve_model(&self, override_model: Option<String>) -> String {
        if let Some(fixed) = self.fixed_model {
            return fixed.to_string();
        }
        override_model.unwrap_or_else(|| self.default_model.to_string())
    }

    /// Build a concrete [`OpenAiProvider`] for this spec. `user_agent` overrides
    /// the spec default (used by the Kimi coding endpoint).
    pub fn build(
        &self,
        api_key: String,
        override_model: Option<String>,
        user_agent: Option<String>,
    ) -> OpenAiProvider {
        let model = self.resolve_model(override_model);
        let agent = user_agent
            .or_else(|| self.default_user_agent.map(str::to_string))
            .unwrap_or_else(|| NEENEE_USER_AGENT.to_string());
        OpenAiProvider::with_base_url_and_user_agent(api_key, model, self.base_url, &agent)
            .with_id(self.id.to_string())
    }
}

/// Construct the concrete `Provider` for a [`neenee_core::catalog::Channel`].
///
/// This is the construction layer that knows about every concrete `Provider`
/// implementation; it lives in `neenee-providers` (not `neenee-core`) so the
/// domain crate stays free of HTTP I/O. `entry_id` becomes the provider's
/// attribution id (`Provider::provider_id`) so assistant responses are
/// attributed to the logical model even after a mid-session switch.
///
/// `session_id` participates in prompt-cache control (ADR-0067): when the
/// resolved [`neenee_core::CachePolicy`] for the model's family is
/// [`SessionKey`](neenee_core::CachePolicy::SessionKey) (Moonshot / Kimi), the
/// session id is stamped as the provider's `prompt_cache_key` so the server-side
/// cache namespaces per conversation and repeated prefixes hit at a discount.
/// Pass `None` when no session is known yet (shared bootstrap); the key is then
/// left unset and the provider caches at the server's default granularity.
pub fn build_provider_for_channel(
    channel: &Channel,
    entry_id: &str,
    session_id: Option<&str>,
) -> Arc<dyn Provider> {
    match &channel.transport {
        Transport::Google {
            base_url,
            user_agent,
        } => {
            let capabilities = channel.capabilities();
            Arc::new(
                GoogleProvider::with_base_url_and_user_agent(
                    channel.api_key.expose_secret().to_string(),
                    channel.model.clone(),
                    base_url,
                    user_agent,
                )
                .with_model_capabilities(capabilities)
                .with_id(entry_id.to_string()),
            )
        }
        Transport::Anthropic {
            base_url,
            user_agent,
            effort,
            thinking,
            copilot,
        } => {
            let mut provider = AnthropicMessagesProvider::with_base_url_and_user_agent(
                channel.api_key.expose_secret().to_string(),
                channel.model.clone(),
                base_url,
                user_agent,
            )
            .with_id(entry_id.to_string());
            // Cap the response length at the model's registered output limit so
            // high-output models (MiniMax M3) are not truncated by the default.
            let capabilities = channel.capabilities();
            if let Some(max_tokens) = capabilities
                .max_output_tokens
                .or_else(|| anthropic_model_max_tokens(&channel.model))
            {
                provider = provider.with_max_tokens(max_tokens);
            }
            // Apply the two reasoning knobs INDEPENDENTLY. effort (depth) and
            // thinking (on/off) are orthogonal on the wire, so we never couple
            // them: setting effort must not implicitly turn thinking on, and an
            // explicit thinking override must not change effort. Each is an
            // optional override layered onto the model-derived default
            // (`for_model`: thinking **off** unless the user opts in — ADR-0046);
            // anything unset keeps that default. Effort is clamped to the
            // model's registered levels at request-build time.
            let mut cfg = ThinkingConfig::for_model(&neenee_core::model::resolve(&channel.model));
            if let Some(mode) = thinking {
                cfg = cfg.with_mode(*mode);
            }
            if let Some(effort) = effort {
                cfg = cfg.with_effort(*effort);
            }
            provider = provider
                .with_thinking(cfg)
                .with_model_capabilities(capabilities)
                .with_copilot(*copilot);
            Arc::new(provider)
        }
        Transport::OpenAi {
            base_url,
            user_agent,
            effort,
            copilot,
        } => {
            let capabilities = channel.capabilities();
            let policy = neenee_core::CachePolicy::for_family(&capabilities.family);
            let cache_key = if policy.injects_session_key() {
                session_id.map(str::to_string)
            } else {
                None
            };
            let provider = OpenAiProvider::with_base_url_and_user_agent(
                channel.api_key.expose_secret().to_string(),
                channel.model.clone(),
                base_url,
                user_agent,
            )
            .with_reasoning_effort(*effort)
            .with_prompt_cache_key(cache_key)
            .with_model_capabilities(capabilities)
            .with_copilot(*copilot)
            .with_id(entry_id.to_string());
            Arc::new(provider)
        }
        Transport::OpenAiResponses {
            base_url,
            user_agent,
            effort,
            account_id,
            copilot,
        } => {
            let capabilities = channel.capabilities();
            let provider = ResponsesProvider::new(
                channel.api_key.expose_secret().to_string(),
                channel.model.clone(),
                base_url,
                account_id.clone(),
            )
            .with_user_agent(user_agent)
            .with_reasoning_effort(*effort)
            .with_model_capabilities(capabilities)
            .with_copilot(*copilot)
            .with_id(entry_id.to_string());
            Arc::new(provider)
        }
    }
}

#[cfg(test)]
mod spec_tests {
    use super::*;

    #[test]
    fn kimi_code_uses_kimi_code_platform() {
        let spec = openai_provider_spec("kimi-code").expect("kimi-code spec");
        // The Kimi Code platform pins the model id — overrides are ignored.
        assert_eq!(spec.resolve_model(None), "k3");
        assert_eq!(spec.resolve_model(Some("kimi-k2.7-code".to_string())), "k3");

        let provider = spec.build("test-key".to_string(), None, None);
        assert_eq!(
            provider.endpoint.base_url(),
            "https://api.kimi.com/coding/v1/chat/completions"
        );
        assert_eq!(provider.endpoint.model_id(), "k3");
        // The Kimi Code platform requires a recognized coding-agent UA.
        assert_eq!(provider.endpoint.user_agent(), "opencode/0.1.0");
        // The registry stamps the preset id onto the concrete provider so
        // assistant responses can be attributed to "kimi-code".
        assert_eq!(provider.endpoint.id(), "kimi-code");
        assert_eq!(provider.provider_id(), "kimi-code");
        assert_eq!(provider.model(), "k3");
    }

    #[test]
    fn openai_compat_spec_resolves_model_override_and_default() {
        let spec = openai_provider_spec("zai-code").expect("zai-code spec");
        assert_eq!(spec.resolve_model(None), "glm-5.2");
        assert_eq!(spec.resolve_model(Some("glm-5.1".to_string())), "glm-5.1");
    }

    #[test]
    fn deepseek_is_not_a_registry_preset() {
        // DeepSeek is now a multi-model catalog entry, not a single-model registry
        // preset: neither the merged id nor the old split ids resolve here.
        assert!(openai_provider_spec("deepseek").is_none());
        assert!(openai_provider_spec("deepseek-v4-flash").is_none());
        assert!(openai_provider_spec("deepseek-v4-pro").is_none());
        // Qwen was removed from the registry and must not resolve.
        assert!(openai_provider_spec("qwen").is_none());
    }

    #[test]
    fn provider_template_specs_have_unique_nonempty_ids() {
        // Template ids are the durable join key between an instance and its
        // template, so they must be unique and non-empty.
        let mut ids: Vec<&str> = PROVIDER_TEMPLATE_SPECS.iter().map(|spec| spec.id).collect();
        ids.sort_unstable();
        assert!(
            ids.iter().all(|id| !id.is_empty()),
            "template ids must be non-empty"
        );
        let dups: Vec<&[&str]> = ids.windows(2).filter(|pair| pair[0] == pair[1]).collect();
        assert!(dups.is_empty(), "duplicate template ids: {dups:?}");
    }

    #[test]
    fn provider_template_spec_resolves_each_known_id() {
        // The reconciliation layer resolves an instance's template_id back to a
        // spec here; every id in the table must round-trip.
        for spec in PROVIDER_TEMPLATE_SPECS {
            let resolved = provider_template_spec(spec.id).expect("id resolves");
            assert_eq!(resolved.id, spec.id);
            assert!(!resolved.models.is_empty(), "{} has no models", spec.id);
        }
        // Unknown ids resolve to None (graceful: an unknown template_id leaves
        // the instance untouched).
        assert!(provider_template_spec("does-not-exist").is_none());
    }
}

#[cfg(test)]
mod build_tests {
    use super::*;

    #[test]
    fn build_provider_stamps_entry_id_on_openai_compat() {
        let channel = Channel {
            id: "default".to_string(),
            label: "OpenAI".to_string(),
            transport: Transport::OpenAi {
                base_url: "https://api.openai.com/v1/chat/completions".to_string(),
                user_agent: "agent".to_string(),
                effort: None,
                copilot: false,
            },
            api_key: "k".into(),
            model: "gpt-4o".to_string(),
            remote: None,
        };
        let provider = build_provider_for_channel(&channel, "openai", None);
        assert_eq!(provider.provider_id(), "openai");
        assert_eq!(provider.model(), "gpt-4o");
    }

    #[test]
    fn build_provider_dispatches_anthropic_transport() {
        // opencode-go's MiniMax/Qwen models reach an Anthropic /messages
        // endpoint; the catalog builds an Anthropic transport for them, and
        // build_provider_for_channel must dispatch it to the messages provider.
        let channel = Channel {
            id: "minimax-m3".to_string(),
            label: "MiniMax M3".to_string(),
            transport: Transport::Anthropic {
                base_url: "https://opencode.ai/zen/go/v1/messages".to_string(),
                user_agent: "agent".to_string(),
                effort: None,
                thinking: None,
                copilot: false,
            },
            api_key: "go-key".into(),
            model: "minimax-m3".to_string(),
            remote: None,
        };
        let provider = build_provider_for_channel(&channel, "opencode-go", None);
        assert_eq!(provider.provider_id(), "opencode-go");
        assert_eq!(provider.model(), "minimax-m3");
    }

    #[test]
    fn anthropic_max_tokens_derives_from_model_output_limit() {
        // minimax-m3's registered output limit (131072) must cap the request's
        // max_tokens, not the provider's flat 8192 default. Construct directly
        // so the typed field is readable (the trait object returned by
        // build_provider_for_channel is not downcastable).
        let provider = AnthropicMessagesProvider::with_base_url_and_user_agent(
            "k".to_string(),
            "minimax-m3".to_string(),
            "https://opencode.ai/zen/go/v1/messages",
            "agent",
        )
        .with_max_tokens(anthropic_model_max_tokens("minimax-m3").unwrap());
        assert_eq!(provider.max_tokens, 131072);
        // An unknown model id falls back to None (the provider keeps its
        // default), proving the lookup does not invent a limit.
        assert!(anthropic_model_max_tokens("not-a-model").is_none());
    }

    #[test]
    fn claude_models_cap_max_tokens_above_the_flat_default() {
        // Claude's registered output limit must lift the request cap above the
        // provider's flat 8192 default so long agent turns are not truncated.
        let opus = AnthropicMessagesProvider::with_base_url_and_user_agent(
            "k".to_string(),
            "claude-opus-4-8".to_string(),
            "https://relay.example.com/v1/messages",
            "agent",
        )
        .with_max_tokens(anthropic_model_max_tokens("claude-opus-4-8").unwrap());
        assert_eq!(opus.max_tokens, 128000);
    }

    #[test]
    fn builtin_provider_models_resolve_with_expected_wire_formats() {
        use neenee_core::WireFormat;
        // Every model a multi-model built-in serves must exist in the model
        // registry (so metadata resolves) and carry the wire format its provider
        // speaks.
        for (&id, expected) in crate::ANTHROPIC_BUILTIN_MODELS
            .iter()
            .map(|id| (id, WireFormat::AnthropicCompat))
            .chain(
                crate::GOOGLE_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::Google)),
            )
            .chain(
                crate::DEEPSEEK_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::OPENAI_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::XAI_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::CHATGPT_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::COPILOT_SEED_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::OPENAI_SUB2API_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::KIMI_CODE_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::ZAI_CODE_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::OPENCODE_GO_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::ANTIGRAVITY_SUB2API_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::Google)),
            )
        {
            let model = neenee_core::model::resolve(id);
            assert_eq!(model.id, id, "model {id} must be registered");
            assert_eq!(model.format, expected, "{id} wire format");
        }
    }
}
