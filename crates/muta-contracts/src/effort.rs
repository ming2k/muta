//! Reasoning **depth** — "how hard should the model think before answering?"
//!
//! muta models every provider's reasoning-depth control as a single
//! provider-independent abstraction: the [`Effort`] enum. This keeps two
//! concerns separate that are easy to conflate, and each lives on its own
//! layer:
//!
//! # Layer A — the abstraction (this module): `Effort` → public API specs
//!
//! [`Effort`] is the **only** depth concept in the codebase. The protocol layer
//! in `muta-llm-client` translates a chosen [`Effort`] onto each **public API
//! specification** a provider speaks — not onto "a brand", but onto the wire
//! shape the spec defines:
//!
//! | API specification | wire field | form | `Effort` → wire |
//! |-------------------|-----------|------|-----------------|
//! | OpenAI Responses | `reasoning.effort` | enum string | `effort.as_str()` |
//! | OpenAI chat completions | `reasoning_effort` | enum string | `effort.as_str()` |
//! | Anthropic Messages | `output_config.effort` | enum string | `effort.as_str()` |
//! | Google generateContent | `thinkingConfig.thinkingLevel` / `.thinkingBudget` | enum string / int tokens | level / a derived bucket |
//!
//! xAI (Grok), Moonshot (Kimi), DeepSeek and Z.AI (GLM) ride these specs too —
//! they implement the OpenAI Responses / chat-completions specification, so
//! they reuse the OpenAI translation verbatim. Google is the outlier that does
//! not use the word "effort" or the standard ladder, yet it is abstracted here
//! all the same: a Gemini model declares an [`Effort`] ladder like any other,
//! and the Google protocol maps each rung onto `thinkingLevel` (3.x) or a
//! `thinkingBudget` bucket (2.5). No caller outside this module ever sees a
//! provider-specific depth shape — they see [`Effort`].
//!
//! [`Effort`] controls **depth only** and is orthogonal to the reasoning on/off
//! switch ([`crate::thinking::ThinkingMode`]); see [`crate::thinking`].
//!
//! # Layer B — baseline value-sets (the `EFFORT_*` consts below)
//!
//! [`Effort`] is the *vocabulary*; a model still needs to know *which rungs it
//! accepts*. That per-model ladder is a **capability**, and — like every other
//! capability (context window, reasoning, vision) — it resolves through one
//! precedence chain (ADR-0065):
//!
//! ```text
//! live discovery (a fitting-enabled provider's GET /models)
//!        ↓  only Kimi & Copilot advertise tiers here
//! static baseline  ←  the EFFORT_* consts in this module
//!        ↓  the compiled-in fallback when upstream advertises nothing
//! &[]  (non-reasoning model, or a protocol with no depth field)
//! ```
//!
//! **The consts are baselines, never the authority when upstream advertises.**
//! Only two providers advertise effort tiers in their live `/models`
//! (`think_efforts.valid_efforts` for Kimi K3, `supports.reasoning_effort` for
//! Copilot); for them the live list wins and the const is just the seed before
//! the first fetch. Every other provider's `/models` is a bare `{id, object,
//! owned_by}` list with **no capability fields** — for them the const *is* the
//! effective ladder, sourced from that provider's prose docs. A const's doc
//! states which case applies, with a citation, so a maintainer never mistakes a
//! baseline for a live-advertised authority.
//!
//! The consts are named by the **family whose models share a value-set**
//! (`EFFORT_CLAUDE_FULL`, `EFFORT_OPENAI_GPT_5_6`, `EFFORT_GEMINI_LEVEL` …)
//! because the value-set genuinely varies per family even within one API spec
//! (GPT ≤5.5 tops out at `xhigh`, GPT-5.6 adds `max`; Gemini 3.x is enum-based,
//! Gemini 2.5 is budget-based). A rung-set shared unchanged across families
//! gets a rung-set name (`EFFORT_LOW_HIGH_MAX`, `EFFORT_COMMON`) rather than a
//! duplicated brand alias — split into per-family consts only when the sets
//! actually diverge (YAGNI).
//!
//! **The vocabulary is open, not closed.** The seven rungs are the words
//! providers use, not a ceiling: a provider may advertise a tier the vocabulary
//! does not name. [`EffortLevel`] is the open companion type — `Known(Effort)`
//! or `Other(String)` — carried on the runtime view
//! ([`crate::model::ModelCapabilities`]) so a live-advertised tier is preserved
//! and stamped through verbatim rather than dropped. [`Effort`] itself stays
//! `Copy` and closed (the static registry depends on that); openness lives only
//! where live discovery lands. See [`EffortLevel`] below.
//!
//! This module is the implementation; the prose reference for users and
//! contributors lives in `docs/reference/effort.md`.

/// How much reasoning effort a model should spend before answering.
///
/// A model accepts only a subset of these levels (its
/// [`crate::model::Model::effort_levels`]); callers must clamp a requested
/// level down to what the model supports rather than sending an unsupported
/// value (which the upstream rejects with 400).
///
/// Ordered ascending by depth:
/// `None < Minimal < Low < Medium < High < Xhigh < Max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    /// Disable reasoning when the provider supports an explicit off value.
    None,
    /// Minimal reasoning. Some providers expose this as a distinct tier below
    /// `low`.
    Minimal,
    /// Minimal reasoning; simple tasks may skip thinking entirely. Fastest and
    /// cheapest. Useful for sub-agents and trivial classification.
    Low,
    /// Moderate reasoning. A middle ground that may omit thinking on simple
    /// queries.
    Medium,
    /// The default depth — deep reasoning on all but the most trivial tasks.
    /// Equivalent to omitting `effort` entirely.
    High,
    /// Deeper-than-high reasoning with extended exploration. Only the Fable /
    /// Opus-4.7+ tier supports it; the best setting for most coding and
    /// agentic work on those models.
    Xhigh,
    /// Maximum reasoning with no depth cap. Correctness over cost; use when a
    /// wrong answer is expensive.
    Max,
}

impl Effort {
    /// All levels in ascending order of depth.
    pub const ORDER: [Effort; 7] = [
        Effort::None,
        Effort::Minimal,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];

    /// The wire string sent in the provider's effort field
    /// (`output_config.effort` for Anthropic).
    pub const fn as_str(self) -> &'static str {
        match self {
            Effort::None => "none",
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// The level's position in [`Effort::ORDER`], for comparison/clamping.
    fn rank(self) -> usize {
        Self::ORDER.iter().position(|e| *e == self).unwrap_or(2)
    }

    /// A short, human-facing description of the tier, shown next to the
    /// segmented effort selector in the model-settings editor so each rung of
    /// the ladder reads as a meaningful choice rather than a bare label. Keep
    /// each to one line — the picker renders it as a caption under the row.
    pub const fn description(self) -> &'static str {
        match self {
            Effort::None => "reasoning off — direct answers only",
            Effort::Minimal => "barely thinks — fastest, simplest tasks",
            Effort::Low => "light reasoning — quick, cheap, simple work",
            Effort::Medium => "balanced — moderate reasoning depth",
            Effort::High => "deep reasoning — the default for real work",
            Effort::Xhigh => "very deep — extended exploration for hard problems",
            Effort::Max => "maximum depth — no cap; correctness over cost",
        }
    }

    /// Parse a lowercase effort string (`"none"`/`"minimal"`/`"low"`/
    /// `"medium"`/`"high"`/`"xhigh"`/`"max"`) into the typed [`Effort`].
    /// Returns `None` for
    /// anything else so an unrecognized config value is silently ignored
    /// rather than treated as an error — the caller keeps its default.
    pub fn parse(s: &str) -> Option<Effort> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Effort::None),
            "minimal" => Some(Effort::Minimal),
            "low" => Some(Effort::Low),
            "medium" => Some(Effort::Medium),
            "high" => Some(Effort::High),
            "xhigh" => Some(Effort::Xhigh),
            "max" => Some(Effort::Max),
            _ => None,
        }
    }

    /// Clamp `self` down to the highest allowed level ≤ `self` (so a requested
    /// `xhigh` on a model that tops out at `high` becomes `high`, never an
    /// unsupported value). When nothing allowed ranks ≤ the request, snap **up**
    /// to the ladder's shallowest tier — the ladder is authoritative, so
    /// emitting an unsupported `high` would earn a 400 (Kimi K3's
    /// `low`/`high`/`max` ladder clamps a legacy `medium` override up to
    /// `low`).
    pub fn clamp_to(self, allowed: &[Effort]) -> Effort {
        let req = self.rank();
        allowed
            .iter()
            .copied()
            .filter(|e| e.rank() <= req)
            .max_by_key(|e| e.rank())
            .unwrap_or_else(|| {
                allowed
                    .iter()
                    .copied()
                    .min_by_key(|e| e.rank())
                    .unwrap_or(Effort::High)
            })
    }

    /// Resolve this known requested effort against a channel's **open** effort
    /// ladder ([`EffortLevel`], which may carry provider-advertised tiers the
    /// vocabulary does not name). Returns the wire string to stamp.
    ///
    /// Known rungs clamp by rank exactly as [`clamp_to`](Self::clamp_to) does.
    /// [`EffortLevel::Other`] rungs cannot be ranked, so they participate only
    /// by **exact name match**: if the request's wire string equals an `Other`
    /// rung, it passes through verbatim (the provider named it, so the provider
    /// honors it); otherwise `Other` rungs are invisible to the ranking. When
    /// no ranked rung fits, the ladder's shallowest **known** rung is used
    /// (`Other` is never a fallback default — its depth is unknowable).
    ///
    /// This keeps the flexibility honest: a tier outside the vocabulary reaches
    /// the wire when the request names it, but never pretends to a rank it
    /// cannot have. A request for an `Other` tier is expressed via
    /// [`EffortLevel`] directly, not through a known [`Effort`].
    pub fn clamp_to_levels(self, allowed: &[EffortLevel]) -> EffortLevel {
        // Exact-name passthrough: a request whose wire string an `Other` rung
        // matches wins verbatim. (The request is a known Effort, so this only
        // fires when an `Other` rung happens to reuse a known name — rare, but
        // keeps the contract total.)
        let req_str = self.as_str();
        for level in allowed {
            if let EffortLevel::Other(s) = level
                && s == req_str
            {
                return EffortLevel::Other(s.clone());
            }
        }
        // Ranked clamp over the known rungs only.
        let req = self.rank();
        let known: Vec<Effort> = allowed.iter().filter_map(EffortLevel::as_known).collect();
        let clamped = known
            .iter()
            .copied()
            .filter(|e| e.rank() <= req)
            .max_by_key(|e| e.rank())
            .unwrap_or_else(|| {
                known
                    .iter()
                    .copied()
                    .min_by_key(|e| e.rank())
                    .unwrap_or(Effort::High)
            });
        EffortLevel::Known(clamped)
    }

    /// Translate this depth into a Google Gemini 2.5 `thinkingBudget` integer,
    /// the form Gemini 2.5 (Flash/Pro) accepts instead of an enum. Gemini 2.5
    /// takes a token budget in a model-specific range (`max_budget`):
    /// `gemini-2.5-flash` tops out at `24576`; `gemini-2.5-pro` at `32768`.
    ///
    /// A chosen [`Effort`] **pins** the budget — it is a deliberate request,
    /// never "let the server decide". (Gemini's own "dynamic" is `-1`, the
    /// server default when the field is *omitted*; muta reaches that by not
    /// stamping the field at all — an unset channel effort — not by mapping any
    /// rung to `-1`.) `minimal` ~10%,
    /// `low` ~25%, `medium` ~50% of `max_budget`; `high`/`xhigh`/`max` all pin
    /// to the model's full cap (`xhigh` is not a native Gemini rung — the
    /// protocol layer clamps it down to `high` first — and `max` differs from
    /// `high` only in intent: it explicitly names the cap).
    ///
    /// `None` maps to `0`, the only way to turn thinking off — but Gemini 2.5
    /// Pro rejects `0` (its floor is `128`), so callers must only honor
    /// [`Effort::None`] when the model actually supports an off budget.
    pub const fn gemini_thinking_budget(self, max_budget: u32) -> i64 {
        match self {
            // 0 = off. Only honored by models whose floor is 0 (Flash/Lite);
            // Pro rejects it (floor 128), so the protocol layer must skip
            // stamping None there.
            Effort::None => 0,
            // The floor of 1 guarantees a nonzero bucket even on a tiny max
            // budget; `core::cmp::max` is not const-stable yet, so the helper
            // spells the clamp out. `max(1)` matters only for unrealistically
            // small `max_budget` values, but keeps the contract honest.
            Effort::Minimal => nonzero(max_budget as u64 / 10),
            Effort::Low => nonzero(max_budget as u64 / 4),
            Effort::Medium => nonzero(max_budget as u64 / 2),
            // A deliberate request pins the budget to the model's full cap —
            // never dynamic (`-1`). `xhigh` is not native to Gemini; the
            // protocol layer clamps it to `high` before reaching here, and it
            // resolves to the same cap regardless.
            Effort::High | Effort::Xhigh | Effort::Max => max_budget as i64,
        }
    }
}

/// A reasoning-depth level **as a channel knows it** — either a known rung of
/// the [`Effort`] vocabulary or an opaque wire string a provider advertises that
/// muta has no name for yet.
///
/// This is the **open** companion to the closed [`Effort`] enum. [`Effort`] is
/// the ordered vocabulary clamp/UI/config key off of; it must stay small and
/// `Copy` (the static `Model` registry is `Copy`). But a provider's live
/// `/models` may advertise a tier the vocabulary does not name (e.g. a future
/// `"turbo"`), and [ADR-0065] makes that advertisement authoritative. Dropping
/// it would silently downgrade a live capability — so the runtime, per-channel
/// view ([`crate::model::ModelCapabilities`] / [`crate::model::RemoteModelMetadata`])
/// carries [`EffortLevel`] to preserve unknown tiers verbatim and stamp them
/// through to the wire.
///
/// ### Where each type lives
///
/// | Type | Lifetime | Carries unknowns? |
/// |------|----------|-------------------|
/// | `Effort` (`&'static [Effort]`) | static registry (`Model`, `Copy`) | **no** — vetted compile-time vocabulary |
/// | `EffortLevel` (`Vec<EffortLevel>`) | runtime view (`ModelCapabilities`, `Clone`) | **yes** — live-advertised tiers preserved |
///
/// ### Ordering
///
/// [`Effort::clamp_to`] orders by the known ladder; an [`EffortLevel::Other`]
/// has **no rank**. When a request resolves to `Other`, the clamp cannot
/// compare it and passes it through verbatim (the provider named it, so the
/// provider honors it); the request path logs the unranked passthrough rather
/// than silently snapping. A `Known` level clamps against `Known` rungs as
/// before. This keeps the flexibility honest: a tier outside the vocabulary
/// reaches the wire, but never pretends to a depth it cannot be ranked at.
///
/// [ADR-0065]: ../adr/0065-runtime-fitted-model-capability-overlay.md
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum EffortLevel {
    /// A rung of the known [`Effort`] vocabulary. Serializes as its wire
    /// string (`"high"`), so existing persisted TOML round-trips unchanged.
    Known(Effort),
    /// A provider-advertised wire string the vocabulary does not name.
    /// Serialized as the raw string and stamped through to the wire verbatim.
    Other(String),
}

impl EffortLevel {
    /// The wire string to stamp onto the request (`"high"`, or the opaque
    /// provider string for [`Other`](Self::Other)).
    pub fn as_str(&self) -> &str {
        match self {
            EffortLevel::Known(e) => e.as_str(),
            EffortLevel::Other(s) => s,
        }
    }

    /// The known rung, when this is one. `None` for [`Other`](Self::Other).
    pub fn as_known(&self) -> Option<Effort> {
        match self {
            EffortLevel::Known(e) => Some(*e),
            EffortLevel::Other(_) => None,
        }
    }

    /// `true` when this is a known rung of the vocabulary.
    pub const fn is_known(&self) -> bool {
        matches!(self, EffortLevel::Known(_))
    }

    /// Parse a wire string into a level: a known rung when the vocabulary
    /// names it, else [`Other`](Self::Other) carrying the raw string. This is
    /// the **non-dropping** parse — unlike [`Effort::parse`], it never returns
    /// `None`, so a provider-advertised tier is always preserved.
    pub fn parse(s: &str) -> EffortLevel {
        match Effort::parse(s) {
            Some(e) => EffortLevel::Known(e),
            None => EffortLevel::Other(s.trim().to_string()),
        }
    }
}

impl From<Effort> for EffortLevel {
    fn from(e: Effort) -> EffortLevel {
        EffortLevel::Known(e)
    }
}

/// `≥ 1` floor for a Gemini budget bucket. `core::cmp::max` is not yet
/// const-stable, so [`Effort::gemini_thinking_budget`] spells the clamp through
/// this helper. Kept private: only the budget translation needs it.
const fn nonzero(tokens: u64) -> i64 {
    if tokens < 1 { 1 } else { tokens as i64 }
}

// ─────────────────────────────────────────────────────────────────────────
// Baseline value-sets (Layer B).
//
// Each const is the **seed ladder** for the model family named in its doc.
// The precedence is live discovery → these baselines → `&[]` (see the module
// doc). The first doc line of each const states whether upstream advertises
// tiers (so the baseline is just a pre-fetch seed) or advertises nothing (so
// the baseline *is* the effective ladder, sourced from prose docs).
// ─────────────────────────────────────────────────────────────────────────

/// **Upstream advertises nothing** — effective ladder, sourced from prose.
/// `low`/`medium`/`high`: the conservative set for any model whose higher
/// tiers (`xhigh`/`max`) are unknown — third-party Anthropic-compatible relays
/// serving non-Claude models. Sending an unadvertised tier to such an upstream
/// risks a 400, so this safe subset is the default.
pub const EFFORT_COMMON: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High];

/// **Upstream advertises nothing** — effective ladder, sourced from Anthropic's
/// `output_config.effort` docs (platform.claude.com/docs/effort). The full
/// `low..=max` range including `xhigh`, honored by the models that accept every
/// tier: Claude Opus 4.8 / 4.7 (and Fable 5 / Mythos 5). `xhigh` is *not*
/// universal — Opus/Sonnet 4.6 reject it (use [`EFFORT_CLAUDE_NO_XHIGH`]).
pub const EFFORT_CLAUDE_FULL: &[Effort] = &[
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// **Upstream advertises nothing** — effective ladder, sourced from Anthropic's
/// effort docs. `low`/`medium`/`high`/`max`: Claude Sonnet 4.6 and Opus 4.6,
/// which honor `max` but **not** `xhigh` (that is limited to Opus 4.8 / 4.7 and
/// the Fable/Mythos line). Requesting `xhigh` here clamps down to `high`.
pub const EFFORT_CLAUDE_NO_XHIGH: &[Effort] =
    &[Effort::Low, Effort::Medium, Effort::High, Effort::Max];

/// **Upstream advertises nothing** — effective ladder, sourced from OpenAI's
/// reasoning guide (developers.openai.com/api/docs/guides/reasoning). GPT ≤5.5:
/// `none`/`minimal`/`low`/`medium`/`high`/`xhigh`. `max` is **not** a value this
/// tier accepts — GPT-5.6 ([`EFFORT_OPENAI_GPT_5_6`]) is the first to add it.
pub const EFFORT_OPENAI_GPT: &[Effort] = &[
    Effort::None,
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
];

/// **Upstream advertises nothing** — effective ladder, sourced from OpenAI's
/// reasoning guide. GPT-5.6 (Sol/Terra/Luna): the first OpenAI family to expose
/// `max`. Earlier GPT-5.x ([`EFFORT_OPENAI_GPT`]) top out at `xhigh`.
pub const EFFORT_OPENAI_GPT_5_6: &[Effort] = &[
    Effort::None,
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// **Upstream advertises nothing** — effective ladder, sourced from xAI's
/// reasoning docs (docs.x.ai/developers/model-capabilities/text/reasoning).
/// Grok 4.x: `none`/`low`/`medium`/`high` (4.3 honors `none`; 4.5+ cannot
/// disable reasoning).
pub const EFFORT_XAI_GROK: &[Effort] = &[Effort::None, Effort::Low, Effort::Medium, Effort::High];

/// `low`/`high`/`max` — a rung set shared unchanged across two families, so it
/// gets a rung-set name rather than a duplicated brand alias (split into
/// per-family consts only if the sets ever diverge). The two families differ in
/// **how their ladders are resolved**:
///
/// - **Moonshot Kimi K3** (`k3`) — **upstream advertises** tiers via
///   `think_efforts.valid_efforts` on its live `/models`
///   (platform.kimi.ai/docs/api/models-overview). This const is the pre-fetch
///   **seed**; `register_fitted_models` (ADR-0065) refreshes it from the live
///   list at startup, and an empty live list never wipes the seed. K3 always
///   reasons; depth is tunable (default `max`).
/// - **DeepSeek** (`deepseek-v4-pro` / `-flash`) — **upstream advertises
///   nothing**: its `/models` is a bare `{id, object, owned_by}` list
///   (api-docs.deepseek.com/api/list-models), so this const *is* the effective
///   ladder, sourced from the chat-completions request-schema enum
///   (api-docs.deepseek.com/api/create-chat-completion: `low`/`high`/`max`,
///   default `high`; `medium`/`xhigh` are compat aliases that remap to `high`).
pub const EFFORT_LOW_HIGH_MAX: &[Effort] = &[Effort::Low, Effort::High, Effort::Max];

/// **Upstream advertises nothing** — effective ladder, sourced from Z.AI's
/// chat-completion reference (docs.z.ai/api-reference/llm/chat-completion).
/// GLM-5.2 / GLM-5.3 — the GLM models that honor `reasoning_effort` (default `max`);
/// GLM-4.x uses a `thinking` on/off object with no depth field, so they keep an
/// empty ladder. Z.AI maps compat rungs (`low`/`medium`→`high`, `xhigh`→`max`,
/// `none`/`minimal`→skip thinking), making `low`/`high`/`xhigh`/`max` the
/// effective depth set.
pub const EFFORT_GLM_5: &[Effort] = &[Effort::Low, Effort::High, Effort::Xhigh, Effort::Max];

/// **Upstream advertises nothing** — effective ladder, sourced from Google's
/// thinking docs (ai.google.dev/gemini-api/docs/generate-content/thinking).
/// Gemini **3.x** maps onto `thinkingConfig.thinkingLevel`: exactly
/// `minimal`/`low`/`medium`/`high` — no `none`/`xhigh`/`max`, and it cannot
/// fully disable thinking (`minimal` is the floor; Gemini 3.1 Pro does not even
/// support `minimal`, clamping up to `low`). A `max`/`xhigh` request clamps
/// down to `high`.
pub const EFFORT_GEMINI_LEVEL: &[Effort] =
    &[Effort::Minimal, Effort::Low, Effort::Medium, Effort::High];

/// **Upstream advertises nothing** — effective ladder, sourced from Google's
/// thinking docs. Gemini **2.5** maps onto a `thinkingConfig.thinkingBudget`
/// integer bucket (Flash: `0`–`24576`, Pro: `128`–`32768`, `-1` = dynamic),
/// translated from each [`Effort`] rung by [`Effort::gemini_thinking_budget`].
/// `None` is deliberately excluded: `0` (off) is honored by Flash but rejected
/// by Pro (floor `128`), so off is model-specific and handled in the protocol
/// layer rather than advertised here.
pub const EFFORT_GEMINI_BUDGET: &[Effort] = &[
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Max,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips() {
        for e in Effort::ORDER {
            assert_eq!(Effort::parse(e.as_str()), Some(e));
        }
        assert_eq!(Effort::parse("nonsense"), None);
        assert_eq!(Effort::parse("  HIGH "), Some(Effort::High));
    }

    #[test]
    fn every_tier_has_a_nonempty_description() {
        for e in Effort::ORDER {
            assert!(
                !e.description().is_empty(),
                "{:?} is missing a picker caption",
                e
            );
        }
        // None must read as the explicit off state, distinct from the tiers.
        assert!(Effort::None.description().contains("off"));
    }

    #[test]
    fn clamp_downgrades_unsupported_tier() {
        // xhigh on a model that tops out at high → high.
        assert_eq!(Effort::Xhigh.clamp_to(EFFORT_COMMON), Effort::High);
        // max on a full-tier model stays max.
        assert_eq!(Effort::Max.clamp_to(EFFORT_CLAUDE_FULL), Effort::Max);
        // low is honored everywhere.
        assert_eq!(Effort::Low.clamp_to(EFFORT_COMMON), Effort::Low);
    }

    #[test]
    fn clamp_snaps_up_to_shallowest_supported_tier() {
        // Kimi K3's ladder skips `medium`: a legacy `medium` override snaps
        // up to `low` rather than emitting an unsupported wire value.
        assert_eq!(Effort::Medium.clamp_to(EFFORT_LOW_HIGH_MAX), Effort::Low);
        // high is on K3's ladder and stays; max is honored too.
        assert_eq!(Effort::High.clamp_to(EFFORT_LOW_HIGH_MAX), Effort::High);
        assert_eq!(Effort::Max.clamp_to(EFFORT_LOW_HIGH_MAX), Effort::Max);
        // An empty ladder keeps the historical wire-default fallback.
        assert_eq!(Effort::Low.clamp_to(&[]), Effort::High);
    }

    #[test]
    fn gemini_level_ladder_clamps_deep_rungs_down() {
        // Gemini 3.x tops out at `high`; xhigh/max clamp down, never escape.
        assert_eq!(Effort::Max.clamp_to(EFFORT_GEMINI_LEVEL), Effort::High);
        assert_eq!(Effort::Xhigh.clamp_to(EFFORT_GEMINI_LEVEL), Effort::High);
        // minimal is the floor on most 3.x models and is honored.
        assert_eq!(
            Effort::Minimal.clamp_to(EFFORT_GEMINI_LEVEL),
            Effort::Minimal
        );
    }

    #[test]
    fn deepseek_and_glm_ladders_clamp() {
        // DeepSeek maps medium→high (its ladder skips medium), xhigh→high.
        assert_eq!(Effort::Medium.clamp_to(EFFORT_LOW_HIGH_MAX), Effort::Low);
        assert_eq!(Effort::Xhigh.clamp_to(EFFORT_LOW_HIGH_MAX), Effort::High);
        // GLM-5.2 honors xhigh (Z.AI maps xhigh→max, but xhigh is on-ladder).
        assert_eq!(Effort::Xhigh.clamp_to(EFFORT_GLM_5), Effort::Xhigh);
        assert_eq!(Effort::Medium.clamp_to(EFFORT_GLM_5), Effort::Low);
    }

    #[test]
    fn gemini_thinking_budget_buckets_within_range() {
        // Gemini 2.5 Flash: 0–24576. Each rung is a fraction of the cap.
        assert_eq!(Effort::None.gemini_thinking_budget(24576), 0);
        assert_eq!(Effort::Minimal.gemini_thinking_budget(24576), 2457);
        assert_eq!(Effort::Low.gemini_thinking_budget(24576), 6144);
        assert_eq!(Effort::Medium.gemini_thinking_budget(24576), 12288);
        assert_eq!(Effort::High.gemini_thinking_budget(24576), 24576);
        assert_eq!(Effort::Max.gemini_thinking_budget(24576), 24576);
        // Gemini 2.5 Pro: 128–32768. Buckets scale by the larger cap.
        assert_eq!(Effort::Medium.gemini_thinking_budget(32768), 16384);
        assert_eq!(Effort::High.gemini_thinking_budget(32768), 32768);
        // A floor of 1 is guaranteed even on a tiny max budget.
        assert_eq!(Effort::Minimal.gemini_thinking_budget(5), 1);
    }

    #[test]
    fn effort_level_parse_never_drops() {
        // Known rungs parse to Known.
        assert_eq!(EffortLevel::parse("high"), EffortLevel::Known(Effort::High));
        assert_eq!(
            EffortLevel::parse("  MAX "),
            EffortLevel::Known(Effort::Max)
        );
        // An unknown provider tier is preserved verbatim as Other — the whole
        // point: a live-advertised tier outside the vocabulary is not lost.
        assert_eq!(
            EffortLevel::parse("turbo"),
            EffortLevel::Other("turbo".to_string())
        );
        assert_eq!(
            EffortLevel::parse("draft"),
            EffortLevel::Other("draft".to_string())
        );
    }

    #[test]
    fn effort_level_round_trips_through_serde() {
        // Known serializes as its wire string (back-compat with persisted TOML).
        let high = EffortLevel::Known(Effort::High);
        assert_eq!(serde_json::to_string(&high).unwrap(), "\"high\"");
        assert_eq!(
            serde_json::from_str::<EffortLevel>("\"high\"").unwrap(),
            EffortLevel::Known(Effort::High)
        );
        // Other serializes as the raw string and round-trips.
        let turbo = EffortLevel::Other("turbo".to_string());
        assert_eq!(serde_json::to_string(&turbo).unwrap(), "\"turbo\"");
        assert_eq!(
            serde_json::from_str::<EffortLevel>("\"turbo\"").unwrap(),
            EffortLevel::Other("turbo".to_string())
        );
    }

    #[test]
    fn clamp_to_levels_ranks_known_and_passes_through_other() {
        // Known request against a ladder with an Other rung: Other is invisible
        // to ranking; the request clamps among known rungs.
        let ladder = vec![
            EffortLevel::Known(Effort::Low),
            EffortLevel::Other("turbo".to_string()),
            EffortLevel::Known(Effort::High),
        ];
        // xhigh (above the known max of high) clamps down to high; turbo is not
        // a ranking target.
        assert_eq!(
            Effort::Xhigh.clamp_to_levels(&ladder),
            EffortLevel::Known(Effort::High)
        );
        // A request shallower than the floor snaps up to the shallowest known.
        assert_eq!(
            Effort::Minimal.clamp_to_levels(&ladder),
            EffortLevel::Known(Effort::Low)
        );
    }

    #[test]
    fn clamp_to_levels_exact_name_match_passes_through() {
        // If an Other rung reuses a known request's wire string, it passes
        // through verbatim (provider named it, provider honors it).
        let ladder = vec![EffortLevel::Other("high".to_string())];
        assert_eq!(
            Effort::High.clamp_to_levels(&ladder),
            EffortLevel::Other("high".to_string())
        );
    }

    #[test]
    fn clamp_to_levels_other_is_never_a_default() {
        // When no ranked rung fits, the fallback is the shallowest KNOWN rung —
        // never an Other tier, whose depth is unknowable.
        let ladder = vec![
            EffortLevel::Other("turbo".to_string()),
            EffortLevel::Known(Effort::High),
        ];
        assert_eq!(
            Effort::Minimal.clamp_to_levels(&ladder),
            EffortLevel::Known(Effort::High)
        );
    }
}
