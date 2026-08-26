//! Doom-guard configuration.
//!
//! [`DoomGuardConfig`] is the serializable, wire-crossing DTO that governs the
//! pre-dispatch doom-loop guard (`muta_agent::doom_guard`). It lives in
//! `muta-contracts` (the domain layer) so the harness↔TUI protocol can carry it
//! without a `muta-persistence` dependency; `muta-persistence::config` re-exports it as
//! the `[master.doom_guard]` TOML table, and `muta-agent` applies it to each
//! round's guard at ReAct-turn boundaries.
//!
//! Default is **enabled** (`window: 16`, `threshold: 3`) — the guard flipped
//! on in ADR-0113 §5 and its strictness relaxed in ADR-0148: the guard is
//! signature bookkeeping with normalized locators, a model making progress
//! never trips it, and the cheapest token-burning loop (variant
//! `sleep N; make`) is still capped at its third occurrence. Turn it off
//! explicitly with `[master.doom_guard] enabled = false`, or restore the
//! ADR-0113 first-repeat block with `threshold = 2`.
//!
//! The canonical TOML key is `doom_guard` (`[master.doom_guard]`); the
//! historical `nudge` spelling is accepted as a serde alias so existing
//! `config.toml` files keep loading; the next save writes the new key. The
//! now-removed `escalate_at`/`path_threshold` keys are ignored silently;
//! `threshold` is live again (ADR-0148).

use serde::{Deserialize, Serialize};

/// User-tunable doom-guard behaviour, deserialized from the
/// `[master.doom_guard]` sub-table of `config.toml` (the historical
/// `[master.nudge]` spelling — the historical name — deserializes
/// identically via serde alias).
/// Governs the pre-dispatch doom-loop guard (`muta_agent::doom_guard`):
/// when the model is about to re-issue a watched tool call it already ran
/// this round, the guard blocks it before it executes and injects an
/// explanatory note so the model changes approach.
///
/// **Default is enabled** (`window: 16`, `threshold: 3`) — see the module
/// docs, ADR-0113 §5 (default flip) and ADR-0148 (threshold relaxation).
///
/// ```toml
/// [master.doom_guard]
/// enabled = false  # opt out of the variant-loop defense
/// window  = 16     # sliding-window size (recent watched signatures)
/// threshold = 3    # occurrences in-window before a block (>= 2)
/// ```
///
/// Detection is pure signature bookkeeping (no model call) and the block is
/// non-terminating — the hard backstops (`hard_stop_turns`, `abort`, `Esc`)
/// still cap. The guard trips when a call reaches `threshold` occurrences
/// in-window (default 3: one same-signature re-run is tolerated, the second
/// repeat is blocked); `threshold = 2` restores the ADR-0113 first-repeat
/// block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DoomGuardConfig {
    /// Master switch. `true` (the default): the doom guard blocks a watched
    /// tool call once it recurs within the window enough times to reach
    /// `threshold` — the cheapest defense against variant loops
    /// (`sleep 1; make` / `sleep 2; make`) burning tokens until the context
    /// overflows. Wired through `Agent::set_doom_guard_config`; forced off
    /// for runners and the `/review`

    /// diagnostic regardless of user setting. Signatures are normalized
    /// (leading env assignments, timing no-ops, casing, path decoration), so
    /// legitimate repeats of *distinct* work are not blocked.
    pub enabled: bool,
    /// Sliding-window size: how many recent watched tool-call signatures are considered when
    /// judging whether a signature is recurring. Large enough to span a
    /// `A B A B` thrash *and* short variant cycles, small enough that an
    /// old, since-abandoned call ages out and stops counting. Default `16`.
    pub window: usize,
    /// Occurrences within the window before a repeat is blocked: `2` blocks
    /// on the first repeat (the strict ADR-0113 behavior), `3` (the
    /// default, ADR-0148) tolerates one same-signature re-run — a transient
    /// retry, a re-run of the same test command after an edit — and blocks
    /// the second. Clamped to `>= 2` at use sites (below that the guard
    /// would fire on first occurrence and block all progress).
    pub threshold: usize,
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
            threshold: 3,
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
        assert_eq!(
            cfg.threshold, 3,
            "one same-signature re-run tolerated (ADR-0148)"
        );
    }

    #[test]
    fn disabled_helper_keeps_default_window() {
        let off = DoomGuardConfig::disabled();
        assert!(!off.enabled);
        assert_eq!(off.window, 16);
        assert_eq!(off.threshold, 3);
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = DoomGuardConfig {
            enabled: true,
            window: 12,
            threshold: 4,
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
        assert_eq!(parsed.threshold, 3);
    }

    #[test]
    fn legacy_escalation_keys_are_ignored_and_threshold_is_live() {
        // Existing config.toml files may still carry the removed
        // escalate_at/path_threshold keys. serde ignores unknown fields by
        // default (no deny_unknown_fields), so parsing must succeed and the
        // legacy values must not affect the shape. `threshold` is a live
        // key again (ADR-0148), so it parses as a real field.
        let s = "enabled = true\nthreshold = 2\nescalate_at = 6\npath_threshold = 8\n";
        let parsed: DoomGuardConfig = toml::from_str(s).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.window, 16);
        assert_eq!(
            parsed.threshold, 2,
            "threshold is a live key again (ADR-0148)"
        );
    }
}
