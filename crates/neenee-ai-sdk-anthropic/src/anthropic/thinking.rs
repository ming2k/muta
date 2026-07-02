//! Anthropic Messages — resolved thinking/effort configuration.
//!
//! [`ThinkingConfig`] carries the two orthogonal reasoning knobs —
//! [`ThinkingMode`] (on/off) and [`Effort`] (depth) — that the request layer
//! stamps onto every `/messages` body. This is an Anthropic-transport concern
//! (it configures *how* the request encodes reasoning), so it lives here
//! rather than in `neenee-core` (which holds only the model *capabilities*:
//! `ThinkingMode`, `Effort`, `ThinkingSupport`).
//!
//! **Reasoning is opt-in.** The default for every model is thinking **off**
//! with no explicit effort — extended thinking is a per-model decision the user
//! makes in the model editor, not something a model enables on its own
//! (ADR-0046). A request only carries a `thinking` object when the user has
//! turned it on for that model.

use neenee_core::{Effort, thinking::ThinkingSupport};

/// Re-export the on/off enum so callers reaching it through this module keep a
/// stable path.
pub use neenee_core::ThinkingMode;

/// Resolved thinking/effort configuration for an Anthropic Messages provider.
///
/// The two knobs are **orthogonal** ([`ThinkingMode`] = on/off switch,
/// [`Effort`] = depth throttle) and are surfaced as such — never coupled.
///
/// `effort` is `Option<Effort>`:
/// - `None` — no explicit choice: use the model default (`high`) and **omit**
///   `output_config` from the wire.
/// - `Some(e)` — an explicit user override: always emit
///   `output_config: {effort: e}`, **even when `e == High`**. An explicit
///   choice is honored verbatim — "what you set is what you send".
///
/// The chosen effort is clamped to the model's `effort_levels` at request-build
/// time (see [`Self::resolve_for`]).
#[derive(Debug, Clone, Copy)]
pub struct ThinkingConfig {
    pub mode: ThinkingMode,
    pub effort: Option<Effort>,
}

impl ThinkingConfig {
    /// The default configuration for a model: **thinking off, no explicit
    /// effort**. Extended thinking is opt-in (ADR-0046).
    pub fn for_model(_model: &neenee_core::Model) -> Self {
        Self::default()
    }

    /// Default: thinking off, no explicit effort.
    pub const fn default() -> Self {
        Self {
            mode: ThinkingMode::Off,
            effort: None,
        }
    }

    /// Set the thinking mode. Returns `self` for chaining.
    pub fn with_mode(mut self, mode: ThinkingMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set an explicit effort override. The value is **not** clamped here;
    /// clamping against the model's supported levels happens at request-build
    /// time. Once set (even to [`Effort::High`]) the request will emit
    /// `output_config`. Returns `self` for chaining.
    pub fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = Some(effort);
        self
    }

    /// Resolve this config against a concrete model's `effort_levels`,
    /// returning a new config whose explicit effort (if any) is clamped to the
    /// model's supported levels. The mode and the effort's explicit/implicit
    /// distinction are honored unchanged. An empty `effort_levels` disables
    /// clamping. This is what `request::stamp_thinking` calls.
    pub(super) fn resolve_for(self, effort_levels: &[Effort]) -> Self {
        if effort_levels.is_empty() {
            return self;
        }
        Self {
            mode: self.mode,
            effort: self.effort.map(|e| e.clamp_to(effort_levels)),
        }
    }

    /// Whether the resolved model requires the manual-thinking beta header.
    /// Used by `request::beta_header`.
    pub(super) fn needs_manual_beta(self, support: ThinkingSupport) -> bool {
        matches!(support, ThinkingSupport::AnthropicManual) && self.mode.is_on()
    }
}

impl Default for ThinkingConfig {
    fn default() -> Self {
        Self {
            mode: ThinkingMode::Off,
            effort: None,
        }
    }
}
