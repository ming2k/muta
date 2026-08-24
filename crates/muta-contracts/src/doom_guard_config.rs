//! Doom-guard configuration.
//!
//! [`DoomGuardConfig`] is the serializable, wire-crossing DTO that governs the
//! pre-dispatch doom-loop guard (`muta_agent::doom_guard`). It lives in
//! `muta-contracts` (the domain layer) so the harness↔TUI protocol can carry it
//! without a `muta-persistence` dependency; `muta-persistence::config` re-exports it as
//! the `[principal.doom_guard]` TOML table, and `muta-agent` applies it to each
//! round's guard at ReAct-turn boundaries.
//!
//! Default is **enabled** (`window: 16`) — flipped on in ADR-0113 §5: the
//! guard is signature bookkeeping with normalized locators, a model making
//! progress never trips it, and the cheapest token-burning loop (variant
//! `sleep N; make`) is exactly what a default-off guard never catches. Turn
//! it off explicitly with `[principal.doom_guard] enabled = false`.
//!
//! The canonical TOML key is `doom_guard` (`[principal.doom_guard]`); the
//! historical `nudge` spelling is accepted as a serde alias so existing
//! `config.toml` files keep loading; the next save writes the new key. The
//! now-removed `threshold`/`escalate_at`/`path_threshold` keys are ignored
//! silently.

use serde::{Deserialize, Serialize};

/// User-tunable doom-guard behaviour, deserialized from the
/// `[principal.doom_guard]` sub-table of `config.toml` (the historical
/// `[principal.nudge]` spelling — the historical name — deserializes
/// identically via serde alias).
/// Governs the pre-dispatch doom-loop guard (`muta_agent::doom_guard`):
/// when the model is about to re-issue a watched tool call it already ran
/// this round, the guard blocks it before it executes and injects an
/// explanatory note so the model changes approach.
///
/// **Default is enabled** (`window: 16`) — see the module docs and
/// ADR-0113 §5 for why the original opt-in default was flipped.
///
/// ```toml
/// [principal.doom_guard]
/// enabled = false  # opt out of the variant-loop defense
/// window  = 16     # sliding-window size (recent watched signatures)
/// ```
///
/// Detection is pure signature bookkeeping (no model call) and the block is
/// non-terminating — the hard backstops (`hard_stop_turns`, `abort`, `Esc`)
/// still cap. The guard trips on the *first* repeat (threshold 2): a call
/// already issued this round is blocked before it runs a second time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DoomGuardConfig {
    /// Master switch. `true` (the default): the doom guard blocks a watched
    /// tool call that repeats within the window — the cheapest defense
    /// against variant loops (`sleep 1; make` / `sleep 2; make`) burning
    /// tokens until the context overflows. Wired through
    /// `Agent::set_doom_guard_config`; forced off for envoys and the `/review`
    /// diagnostic regardless of user setting. Signatures are normalized
    /// (leading env assignments, timing no-ops, casing, path decoration), so
    /// legitimate repeats of *distinct* work are not blocked.
    pub enabled: bool,
    /// Sliding-window size: how many recent watched tool-call signatures are considered when
    /// judging whether a signature is recurring. Large enough to span a
    /// `A B A B` thrash *and* short variant cycles, small enough that an
    /// old, since-abandoned call ages out and stops counting. Default `16`.
    pub window: usize,
}

impl DoomGuardConfig {
    /// A disabled config with default window — the canonical "off" state used
    /// by envoys and the `/review` diagnostic so they run unobstructed
    /// regardless of user settings.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

impl Default for DoomGuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window: 16,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_enabled_with_wider_window() {
        let cfg = DoomGuardConfig::default();
        assert!(cfg.enabled, "on by default: the variant-loop defense");
        assert_eq!(cfg.window, 16);
    }

    #[test]
    fn disabled_helper_keeps_default_window() {
        let off = DoomGuardConfig::disabled();
        assert!(!off.enabled);
        assert_eq!(off.window, 16);
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = DoomGuardConfig {
            enabled: true,
            window: 12,
        };
        let s = toml::to_string(&cfg).unwrap();
        let parsed: DoomGuardConfig = toml::from_str(&s).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn partial_toml_keeps_defaults() {
        // Only `enabled` is set; the rest must fall back to defaults.
        let s = "enabled = true\n";
        let parsed: DoomGuardConfig = toml::from_str(s).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.window, 16);
    }

    #[test]
    fn legacy_threshold_keys_are_ignored_silently() {
        // Existing config.toml files may still carry the removed
        // threshold/escalate_at/path_threshold keys. serde ignores unknown
        // fields by default (no deny_unknown_fields), so parsing must succeed
        // and the legacy values must not affect the new shape.
        let s = "enabled = true\nthreshold = 3\nescalate_at = 6\npath_threshold = 8\n";
        let parsed: DoomGuardConfig = toml::from_str(s).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.window, 16);
    }
}
