//! Builtin seed/baseline model lists for the shipped provider presets.
//!
//! Pure static data shared by the daemon (catalog reconciliation, connection
//! re-seeding) and the frontend (add-connection chooser). Single source of
//! truth; `muta-providers` re-exports these for its preset specs.

pub const ANTHROPIC_BUILTIN_MODELS: &[&str] = &[
    "claude-fable-5",
    "claude-sonnet-5",
    "claude-opus-4-8",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
];

pub const CHATGPT_BUILTIN_MODELS: &[&str] = &["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"];

pub const DEEPSEEK_BUILTIN_MODELS: &[&str] = &[
    "deepseek-v4-flash",
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro",
    "deepseek-v4-pro-0813",
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

pub const OPENCODE_GO_MODELS: &[&str] = &["glm-5.2", "kimi-k2.7-code", "deepseek-v4-flash"];

pub const ZAI_CODE_MODELS: &[&str] = &["glm-5.3", "glm-5.3-flash", "glm-5.2"];

pub const XAI_BUILTIN_MODELS: &[&str] = &["grok-4.5", "grok-4.20", "grok-4.3", "grok-build-0.1"];
