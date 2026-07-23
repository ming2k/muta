//! Doom-guard configuration.
//!
//! [`DoomGuardConfig`] is the serializable, wire-crossing DTO that governs the
//! pre-dispatch doom-loop guard (`neenee_agent::doom_guard`). It lives in
//! `neenee-core` (the domain layer) so the harness↔TUI protocol can carry it
//! without a `neenee-persistence` dependency; `neenee-persistence::config` re-exports it as
//! the `[principal.nudge]` TOML table, and `neenee-agent` reads it before each
//! tool round to decide whether to intercept a repeating call.
//!
//! Default is **disabled** — opt in through the advanced
//! `[principal.nudge]` sub-table in `config.toml`.
//!
//! The TOML key is kept as `nudge` for backward compatibility (existing
//! `config.toml` files keep working; serde ignores the now-removed
//! `threshold`/`escalate_at`/`path_threshold` keys silently).

use serde::{Deserialize, Serialize};

/// User-tunable doom-guard behaviour, deserialized from the `[principal.nudge]`
/// sub-table of `config.toml`. Governs the pre-dispatch doom-loop guard
/// (`neenee_agent::doom_guard`): when the model is about to re-issue a watched
/// tool call it already ran this turn, the guard blocks it before it executes
/// and injects an explanatory note so the model changes approach.
///
/// **Default is disabled.** The guard is an opt-in safety net, not a
/// default-on interruption: a model making progress should never see a block,
/// and a stuck model has the `abort` tool (the user has `Esc`). Turn it on when
/// you want the harness to break doom loops automatically.
///
/// ```toml
/// [principal.nudge]
/// enabled = true   # master switch (default false)
/// window  = 8      # sliding-window size (recent watched rounds)
/// ```
///
/// Detection is pure signature bookkeeping (no model call) and the block is
/// non-terminating — the hard backstops (`hard_stop_turns`, `abort`, `Esc`)
/// still cap. The guard trips on the *first* repeat (threshold 2): a call
/// already issued this turn is blocked before it runs a second time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DoomGuardConfig {
    /// Master switch. `false` (the default) disables the doom guard entirely
    /// — repeating calls are not blocked. Wired through
    /// `Agent::set_doom_guard_config`; flipped off for envoys and the `/review`
    /// diagnostic regardless of user setting.
    pub enabled: bool,
    /// Sliding-window size: how many recent watched rounds are considered when
    /// judging whether a signature is recurring. Large enough to span a
    /// `A B A B` thrash, small enough that an old, since-abandoned call ages
    /// out and stops counting. Default `8`.
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
            enabled: false,
            window: 8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        let cfg = DoomGuardConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.window, 8);
    }

    #[test]
    fn disabled_helper_keeps_default_window() {
        let off = DoomGuardConfig::disabled();
        assert!(!off.enabled);
        assert_eq!(off.window, 8);
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
        assert_eq!(parsed.window, 8);
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
        assert_eq!(parsed.window, 8);
    }
}
