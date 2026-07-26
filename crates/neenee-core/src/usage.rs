//! Per-turn token telemetry shared across the stack.
//!
//! [`TokenUsage`] is reported by every provider turn and consumed by the core
//! token ledger, the LLM client, the CLI usage meters, and the agent loop.
//! It is not pursuit-specific (pursuit budgets book against it, but so does
//! everything else), so it lives here rather than in [`crate::pursuit`].

use serde::{Deserialize, Serialize};

/// Token usage reported by a single turn.
///
/// Per-turn telemetry. Pursuit accounting aggregates deltas from this value at
/// stop-gate boundaries (ADR-0083); the value itself remains generic.
///
/// `cache_creation_input_tokens` / `cache_read_input_tokens` carry prompt-cache
/// counts. Anthropic reports both: its `input_tokens` is ONLY the uncached
/// dynamic suffix, so the cache write/read counts must be tracked separately
/// (and added into `prompt_tokens`/`total_tokens`) or the context meter would
/// undercount every cached turn. OpenAI / Gemini / Moonshot auto-cache (or
/// session-key cache) and surface the hit as a single read count — their
/// `cache_creation_input_tokens` stays zero. The shared parser lives in
/// [`crate::cache`](crate::cache); see [`crate::CachePolicy`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    /// Tokens written to the prompt cache this turn (billed at a premium by
    /// Anthropic; absent on providers without explicit breakpoint caching).
    pub cache_creation_input_tokens: i64,
    /// Tokens served from the prompt cache this turn (billed at a steep
    /// discount by Anthropic; surfaced as `cached_tokens` /
    /// `cachedContentTokenCount` by the auto-caching providers).
    pub cache_read_input_tokens: i64,
}
