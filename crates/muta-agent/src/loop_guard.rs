//! The guard-action type and the per-round guard state that backs the pre-dispatch
//! doom-loop detector ([`crate::doom_guard`]).
//!
//! The old read-only, post-hoc `ReadLoopGuard` (nudge-then-block, read-only) was
//! replaced by [`crate::doom_guard::DoomLoopGuard`], which intercepts *any*
//! watched tool's repeat *before* it executes. See that module for the rationale.
//! What remains here is the shared vocabulary — the [`GuardAction`] a guard
//! returns, and [`RoundGuardState`] — the per-round carrier that holds the doom
//! guard and the per-round block mask the dispatch layer consults.

use std::collections::HashSet;

/// The outcome a guard returns for one ReAct turn's tool calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum GuardAction {
    /// No action — the turn is fine, keep going.
    #[default]
    Continue,
    /// Inject `message` as a hidden user message before the next model request.
    /// Non-terminating: the round keeps running.
    Inject(String),
    /// Hard-block one or more tool-call `signatures` for the remainder of this
    /// round. The agent records them in a per-round mask ([`RoundGuardState`]) and
    /// short-circuits any subsequent call whose canonical signature matches,
    /// returning an explanatory `ToolOutput` instead of executing it — so the
    /// model *cannot* re-issue the looped call, only issue something *different*.
    /// Non-terminating and surgical: it leaves all other calls untouched.
    ///
    /// `message` is a steering note injected alongside the block (same vehicle
    /// as [`Inject`](Self::Inject)) so the model learns *why* the call is now
    /// refused and what to do instead.
    Block {
        signatures: Vec<String>,
        message: String,
    },
    /// Abort the round with `reason` as a terminal error. Hard-terminating.
    Abort(String),
}

impl GuardAction {
    /// Severity rank for merging: Abort > Block > Inject > Continue.
    fn severity(&self) -> u8 {
        match self {
            GuardAction::Continue => 0,
            GuardAction::Inject(_) => 1,
            GuardAction::Block { .. } => 2,
            GuardAction::Abort(_) => 3,
        }
    }

    /// Merge two actions, keeping the more severe. Two `Inject`s of equal
    /// severity concatenate so both messages reach the model; two `Block`s merge
    /// their signature sets; a `Block` absorbs a weaker `Inject` by folding the
    /// inject's message into the block's. A more severe action always wins
    /// outright.
    pub fn merge(self, other: GuardAction) -> GuardAction {
        match (self.severity(), other.severity()) {
            // Equal severity: combine payloads.
            (1, 1) => {
                let GuardAction::Inject(mut s) = self else {
                    unreachable!("severity 1 is Inject")
                };
                let GuardAction::Inject(t) = other else {
                    unreachable!("severity 1 is Inject")
                };
                s.push_str("\n\n");
                s.push_str(&t);
                GuardAction::Inject(s)
            }
            (2, 2) => {
                let GuardAction::Block {
                    mut signatures,
                    mut message,
                } = self
                else {
                    unreachable!("severity 2 is Block")
                };
                let GuardAction::Block {
                    signatures: other_sigs,
                    message: other_msg,
                } = other
                else {
                    unreachable!("severity 2 is Block")
                };
                for sig in other_sigs {
                    if !signatures.contains(&sig) {
                        signatures.push(sig);
                    }
                }
                if !other_msg.is_empty() {
                    message.push_str("\n\n");
                    message.push_str(&other_msg);
                }
                GuardAction::Block {
                    signatures,
                    message,
                }
            }
            // Block absorbs a weaker Inject's message.
            (2, 1) => {
                let GuardAction::Block {
                    signatures,
                    mut message,
                } = self
                else {
                    unreachable!("severity 2 is Block")
                };
                if let GuardAction::Inject(t) = other
                    && !t.is_empty()
                {
                    message.push_str("\n\n");
                    message.push_str(&t);
                }
                GuardAction::Block {
                    signatures,
                    message,
                }
            }
            // Winner is whichever side is more severe.
            (a, b) if a >= b => self,
            _ => other,
        }
    }
}

/// Per-round state carrying the pre-dispatch doom-loop guard and the per-round
/// block mask the dispatch layer consults. Lives in `RoundState`; one per round,
/// dropped when the round ends, so block state never leaks across rounds.
#[derive(Default)]
pub struct RoundGuardState {
    /// The pre-dispatch doom-loop guard. Consulted by [`Self::check_doom_ahead`]
    /// before any tool runs. `None` when nudging is disabled (the agent does not
    /// attach one).
    doom: Option<crate::doom_guard::DoomLoopGuard>,
    /// Tool-call signatures hard-blocked by the doom guard this round
    /// ([`GuardAction::Block`]). The dispatch layer consults
    /// [`Self::is_blocked`] before executing a call and short-circuits any
    /// match. Per-round: cleared when the round ends (the `RoundState` owning it is
    /// dropped), so a block never leaks across rounds. A progress turn does
    /// *not* clear it — blocking a proven-looping call for the remainder of the
    /// round is the point, even if the model makes other progress in between.
    blocked_signatures: HashSet<String>,
}

impl RoundGuardState {
    /// Build empty guard state. Attach the doom guard via [`Self::with_doom`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the pre-dispatch doom-loop guard. Consumed by
    /// [`Self::check_doom_ahead`]. Builder-style companion to [`Self::new`],
    /// used by `Agent::guards_default`; tests that do not exercise the doom
    /// guard can omit it.
    pub fn with_doom(mut self, doom: crate::doom_guard::DoomLoopGuard) -> Self {
        self.doom = Some(doom);
        self
    }

    /// Pre-dispatch check: given the `(name, args)` pairs of the calls about
    /// to execute this turn, ask the doom guard whether any is a repeat of a
    /// call already issued this round. Returns the guard's [`GuardAction`]
    /// (`Block` to intercept, `Continue` to proceed) and, on `Block`, records
    /// the repeated signatures in the per-round mask so [`Self::is_blocked`]
    /// short-circuits them at dispatch time too.
    ///
    /// This runs *before* the tools execute — the decisive difference from the
    /// removed post-hoc read-loop guard, which fired after the repeat had
    /// already run. `calls` is the full turn (all tools the model asked for);
    /// the doom guard keys only on its watched set, so unwatched tools are
    /// transparent and never enter the window or trip a block.
    pub fn check_doom_ahead(&mut self, calls: &[(&str, &str)]) -> GuardAction {
        let Some(doom) = self.doom.as_mut() else {
            return GuardAction::Continue;
        };
        // Only watched tools have meaningful signatures; unwatched tools are
        // transparent to the doom guard.
        let signatures: Vec<String> = calls
            .iter()
            .filter(|(name, _)| crate::doom_guard::covers(name))
            .map(|(name, args)| crate::doom_guard::doom_signature(name, args))
            .collect();
        if signatures.is_empty() {
            return GuardAction::Continue;
        }
        let action = doom.check_ahead(&signatures);
        if let GuardAction::Block { ref signatures, .. } = action {
            for sig in signatures {
                self.blocked_signatures.insert(sig.clone());
            }
        }
        action
    }

    /// Whether a single call (name + raw args) is hard-blocked this round by the
    /// doom guard's signature mask. Unwatched tools never have a doom signature,
    /// so they are always admitted.
    pub fn is_blocked(&self, name: &str, args: &str) -> bool {
        if self.blocked_signatures.is_empty() || !crate::doom_guard::covers(name) {
            return false;
        }
        let doom = crate::doom_guard::doom_signature(name, args);
        self.blocked_signatures.contains(&doom)
    }

    /// A compact, log-friendly summary of what is currently blocked. Returns
    /// `None` when nothing is masked so callers can cheaply skip logging.
    pub fn blocked_summary(&self) -> Option<Vec<String>> {
        if self.blocked_signatures.is_empty() {
            None
        } else {
            let mut v: Vec<String> = self.blocked_signatures.iter().cloned().collect();
            v.sort();
            Some(v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_picks_the_more_severe() {
        let inject = GuardAction::Inject("a".to_string());
        let block = GuardAction::Block {
            signatures: vec!["x".to_string()],
            message: "m".to_string(),
        };
        // More severe wins; when severities differ the winner is returned as-is.
        assert_eq!(GuardAction::Continue.merge(block.clone()), block);
        assert_eq!(block.clone().merge(GuardAction::Continue), block);
        assert_eq!(GuardAction::Continue.merge(inject.clone()), inject);
    }

    #[test]
    fn merge_two_injects_concatenates() {
        let a = GuardAction::Inject("a".to_string());
        let b = GuardAction::Inject("b".to_string());
        match a.merge(b) {
            GuardAction::Inject(s) => assert_eq!(s, "a\n\nb"),
            other => panic!("expected Inject, got {other:?}"),
        }
    }

    #[test]
    fn merge_two_blocks_unions_signatures() {
        let a = GuardAction::Block {
            signatures: vec!["x".to_string()],
            message: "m1".to_string(),
        };
        let b = GuardAction::Block {
            signatures: vec!["y".to_string()],
            message: "m2".to_string(),
        };
        match a.merge(b) {
            GuardAction::Block {
                signatures,
                message,
            } => {
                assert_eq!(signatures, vec!["x".to_string(), "y".to_string()]);
                assert_eq!(message, "m1\n\nm2");
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn is_blocked_empty_mask_admits_everything() {
        let state = RoundGuardState::new();
        assert!(!state.is_blocked("bash", r#"{"command":"ls"}"#));
    }

    #[test]
    fn is_blocked_unwatched_tool_admitted_even_if_present_in_args() {
        let state = RoundGuardState::new();
        // use_skill is not watched; even with a mask it would be admitted, but
        // here the mask is empty so this just confirms the fast path.
        assert!(!state.is_blocked("use_skill", r#"{"name":"x"}"#));
    }
}
