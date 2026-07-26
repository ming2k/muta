//! General-purpose doom-loop guard: a pre-dispatch detector that intercepts
//! *any* tool call whose signature has already been issued this round, before
//! the tool runs — not just reads.
//!
//! # Why a separate guard
//!
//! [`crate::loop_guard::ReadLoopGuard`] is read-only, post-hoc, and defaults
//! to off: it observes a turn *after* the tools have executed, nudges the
//! model, and only hard-blocks on the *next* recurrence. That means the
//! repeating call's result is already in context when the nudge lands — the
//! model sees "I read it successfully" right next to "don't read it again", a
//! self-contradictory signal that strong models routinely resolve in favour of
//! re-running the call. Coverage is also limited to read-tier tools, so a
//! `bash` re-run, a `webfetch` re-fetch, or an `edit` A→B→A thrash sails
//! straight through.
//!
//! This guard is the inverse on every axis:
//! - **Pre-dispatch**: it runs *before* tools execute, so a repeated call never
//!   produces side effects or output. The model only ever sees the refusal.
//! - **All tools**: covers the common doom-loop culprits — `read`, `grep`,
//!   `glob`, `list_dir`, `bash`, `webfetch`, `websearch`, `edit_file`,
//!   `write_file` — keyed by a normalised signature, not just reads.
//! - **First repeat trips it (threshold = 2)**: by the time a call recurs in
//!   one round, it is almost never productive, and the cost of letting it run
//!   (context bloat + a contradictory nudge) outweighs the rare false positive.
//!   A progress turn (any *different* tool call) still clears the window, so
//!   legitimate interleave is unaffected.
//!
//! Detection is pure signature bookkeeping — no model call. The action is a
//! [`crate::loop_guard::GuardAction::Block`]: the signature is masked for the
//! rest of the round and an explanatory note is injected, so the model learns
//! the call is now refused and must change approach (or call `abort`).
//!
//! # Relation to `NudgeConfig`
//!
//! The doom guard is gated by [`NudgeConfig::enabled`] for consistency with the
//! read-loop guard: when nudging is off, neither guard runs. Envoy and review
//! paths disable nudging, so they stay unobstructed.

use std::collections::VecDeque;

use neenee_core::DoomGuardConfig;
use serde_json::Value;

use crate::loop_guard::GuardAction;

/// The tools this guard watches. Anything outside this set is passed through
/// untouched — MCP tools, `ask_user`, `use_skill`, `todo_*`, envoy, etc. are
/// either inherently unique or user-interactive, where a repeat is plausibly
/// legitimate and blocking would be hostile.
///
/// Kept as a sorted set so the [`covers`] lookup is O(log n).
const WATCHED_TOOLS: &[&str] = &[
    "bash",
    "edit_file",
    "glob",
    "grep",
    "list_dir",
    "read",
    "read_image",
    "read_text",
    "webfetch",
    "websearch",
    "write_file",
];

/// Whether a tool name is in the watched set. Case-sensitive — tool names are
/// canonicalised at registration, so a model that emits `Bash` does not match
/// `bash` and is left alone (a mis-emitted name would fail at dispatch anyway).
pub(crate) fn covers(name: &str) -> bool {
    WATCHED_TOOLS.binary_search(&name).is_ok()
}

/// Canonical signature of a single watched tool call, normalised so that
/// semantically-identical calls share a key but genuinely-different calls do
/// not.
///
/// The key fields are picked per argument shape:
/// - **Path-addressed calls** (`read`, `edit_file`, `write_file`, `list_dir`…):
///   `name|path` — the target file/dir. We deliberately drop `offset`/`limit`
///   so re-reading the same file at a new offset *does* count as a repeat
///   (the read-loop guard's "forward paging" carve-out does not apply here:
///   within a single turn, re-reading the same file at a different offset is
///   almost always a symptom of the model losing track of what it already has
///   in context). Mutations (`edit_file`/`write_file`) keep `path` only — two
///   edits to the same path collide even if the patch differs, because an
///   A→B→A thrash on one file is the classic doom loop.
/// - **Command-addressed calls** (`bash`): `name|command` — the literal command
///   string. Running the identical command twice in a turn is never productive.
/// - **Query-addressed calls** (`grep`, `websearch`): `name|query` — the search
///   text. A different query is a different call; the same query again is a
///   repeat.
/// - **URL-addressed calls** (`webfetch`): `name|url`.
/// - **Pattern-only calls** (`glob`): `name|pattern`.
/// - **Anything else / unparseable**: fall back to `name|<raw args>` so the
///   call is still keyed (two identical blobs still collide) but distinct
///   blobs stay distinct.
pub fn doom_signature(name: &str, args: &str) -> String {
    if !covers(name) {
        return format!("{name}|<unwatched>");
    }
    let value: Value = serde_json::from_str(args).unwrap_or(Value::Null);
    // Prefer the most specific locator present, in priority order.
    for key in ["command", "cmd"] {
        if let Some(s) = value.get(key).and_then(Value::as_str) {
            return format!("{name}|{s}");
        }
    }
    if let Some(s) = value.get("url").and_then(Value::as_str) {
        return format!("{name}|{s}");
    }
    for key in ["query", "pattern", "q"] {
        if let Some(s) = value.get(key).and_then(Value::as_str) {
            return format!("{name}|{s}");
        }
    }
    for key in ["path", "file_path", "file", "filename"] {
        if let Some(s) = value.get(key).and_then(Value::as_str) {
            return format!("{name}|{s}");
        }
    }
    // No recognised locator: key on the whole arg blob so two identical blobs
    // still collide (a true exact-repeat) but distinct blobs stay distinct.
    format!("{name}|{}", args.trim())
}

/// The pre-dispatch doom-loop detector.
///
/// One lives per user round in `RoundState` (see [`crate::agent::RoundState`]) and is
/// dropped when the round ends, so state never leaks across rounds. The window
/// is a sliding record of the last `config.window` watched tool-call
/// signatures; a signature that has already appeared in the window trips the guard
/// the *next* time it is about to run.
pub struct DoomLoopGuard {
    config: DoomGuardConfig,
    /// Signatures of watched tool calls already seen this round (within the
    /// window). A signature about to run that is *already present* here is a
    /// repeat → block.
    window: VecDeque<String>,
}

impl DoomLoopGuard {
    /// Construct a guard tuned by `config`. Thresholds are read live from the
    /// config, so a runtime `set_doom_guard_config` update takes effect next round.
    pub fn new(config: DoomGuardConfig) -> Self {
        Self {
            config,
            window: VecDeque::new(),
        }
    }

    /// The configured thresholds. Exposed for tests and diagnostics.
    pub fn config(&self) -> DoomGuardConfig {
        self.config
    }

    /// Pre-dispatch check: given the signatures of the calls about to run this
    /// turn, decide whether to block *before* any of them executes.
    ///
    /// Returns a [`GuardAction::Block`] if any call's signature is already in
    /// the window (i.e. it has run earlier in this round), listing every repeated
    /// signature so the dispatch layer can mask all of them. The caller is
    /// responsible for (a) not executing the blocked calls, (b) recording the
    /// signatures in its per-round mask, and (c) pushing the block's message.
    ///
    /// After this returns, the caller records *every* watched signature from
    /// turn (repeated or not) into the window, so the next turn can
    /// detect a fresh repeat. Turns with no watched tool are a no-op
    /// and do not touch the window — but a turn that mixes a watched call
    /// with anything else still feeds its watched signatures in.
    ///
    /// Note: unlike the read-loop guard, a "progress" turn does **not** clear
    /// this window. Rationale: an `A B A` pattern (do X, do Y, do X again) is
    /// still a doom loop if X is `bash run-tests`. The read guard clears on
    /// progress because reads are cheap and exploration is legitimate; here
    /// every covered tool has a real cost (command exec, fetch, mutation), so
    /// a recurrence stays flagged regardless of intervening work.
    pub fn check_ahead(&mut self, signatures: &[String]) -> GuardAction {
        if !self.config.enabled {
            return GuardAction::Continue;
        }
        let repeated: Vec<String> = signatures
            .iter()
            .filter(|sig| self.window.contains(*sig))
            .cloned()
            .collect();
        // Record every signature now, so the window reflects what has been
        // dispatched this round regardless of whether we block. A blocked call
        // never executes, but conceptually it *was* issued — and leaving it
        // out would let the model retry the identical call once unblocked.
        for sig in signatures {
            self.push(sig.clone());
        }
        if repeated.is_empty() {
            return GuardAction::Continue;
        }
        // Build one consolidated block message naming every repeat. The count
        // here is "times seen so far" (window count after this push), which is
        // 2 for the first trip — the honest framing for "you are repeating".
        let summary = repeated
            .iter()
            .map(|s| format!("- {}", humanize_sig(s)))
            .collect::<Vec<_>>()
            .join("\n");
        let message = format!(
            "You are repeating a tool call that already ran this round:\n{summary}\n\
             Re-running it cannot change the result you already have. This call is now \
             **blocked** for the rest of the turn — calling it again returns an error, \
             not a fresh result. Act on what you already have, try a *different* \
             command/file/query, or, if you genuinely cannot proceed, say so explicitly \
             or call `abort`."
        );
        GuardAction::Block {
            signatures: repeated,
            message,
        }
    }

    fn push(&mut self, signature: String) {
        self.window.push_back(signature);
        while self.window.len() > self.config.window {
            #[allow(clippy::expect_used)]
            self.window
                .pop_front()
                .expect("non-empty while over window");
        }
    }
}

/// Reduce a machine signature (`name|locator`) to a short human phrase for the
/// block message, e.g. `bash ls -la`, `read_text src/main.rs`.
fn humanize_sig(signature: &str) -> String {
    let mut parts = signature.splitn(2, '|');
    let name = parts.next().unwrap_or("").trim();
    let rest = parts.next().unwrap_or("");
    if rest.is_empty() || rest == "<unwatched>" {
        name.to_string()
    } else {
        format!("{name} {rest}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> DoomGuardConfig {
        DoomGuardConfig {
            enabled: true,
            ..DoomGuardConfig::default()
        }
    }

    #[test]
    fn covers_the_watched_set() {
        assert!(covers("bash"));
        assert!(covers("read_text"));
        assert!(covers("write_file"));
        assert!(!covers("use_skill"));
        assert!(!covers("ask_user"));
        assert!(!covers("mcp_tool"));
    }

    #[test]
    fn first_occurrence_is_allowed() {
        let mut g = DoomLoopGuard::new(enabled());
        let action = g.check_ahead(&[doom_signature("bash", r#"{"command":"ls"}"#)]);
        assert_eq!(action, GuardAction::Continue);
    }

    #[test]
    fn second_occurrence_blocks_and_masks_signature() {
        let mut g = DoomLoopGuard::new(enabled());
        let s = doom_signature("bash", r#"{"command":"make test"}"#);
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&s)),
            GuardAction::Continue
        );
        let action = g.check_ahead(std::slice::from_ref(&s));
        match action {
            GuardAction::Block {
                signatures,
                message,
            } => {
                assert_eq!(signatures, vec![s]);
                assert!(message.contains("blocked"));
                assert!(message.contains("make test"));
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn disabled_guard_never_blocks() {
        let mut g = DoomLoopGuard::new(DoomGuardConfig::disabled());
        let s = doom_signature("bash", r#"{"command":"ls"}"#);
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&s)),
            GuardAction::Continue
        );
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&s)),
            GuardAction::Continue
        );
    }

    #[test]
    fn distinct_commands_do_not_collide() {
        let mut g = DoomLoopGuard::new(enabled());
        let a = doom_signature("bash", r#"{"command":"ls"}"#);
        let b = doom_signature("bash", r#"{"command":"pwd"}"#);
        assert_eq!(g.check_ahead(&[a]), GuardAction::Continue);
        assert_eq!(g.check_ahead(&[b]), GuardAction::Continue);
    }

    #[test]
    fn path_keyed_read_ignores_offset() {
        // read_text on the same file at a different offset is still a repeat
        // under the doom guard — re-reading a file mid-turn is the smell.
        let a = doom_signature("read_text", r#"{"path":"a.rs","offset":1}"#);
        let b = doom_signature("read_text", r#"{"path":"a.rs","offset":50}"#);
        assert_eq!(a, b, "same path collapses to one signature");
    }

    #[test]
    fn edit_thrash_on_same_path_collides() {
        let a = doom_signature("edit_file", r#"{"path":"a.rs","old":"x","new":"y"}"#);
        let b = doom_signature("edit_file", r#"{"path":"a.rs","old":"y","new":"x"}"#);
        assert_eq!(a, b, "edits to the same path collide (A->B->A thrash)");
    }

    #[test]
    fn window_evicts_old_signatures() {
        let mut g = DoomLoopGuard::new(DoomGuardConfig {
            enabled: true,
            window: 2,
        });
        let s = doom_signature("bash", r#"{"command":"ls"}"#);
        let other1 = doom_signature("bash", r#"{"command":"pwd"}"#);
        let other2 = doom_signature("bash", r#"{"command":"whoami"}"#);
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&s)),
            GuardAction::Continue
        );
        assert_eq!(g.check_ahead(&[other1]), GuardAction::Continue);
        // s has aged out (window=2), so it is fresh again.
        assert_eq!(g.check_ahead(&[other2]), GuardAction::Continue);
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&s)),
            GuardAction::Continue
        );
    }

    #[test]
    fn unwatched_tool_signatures_do_not_block() {
        let mut g = DoomLoopGuard::new(enabled());
        // An unwatched tool produces a sentinel signature; two of them must
        // not trip the guard (it never looks at unwatched tools).
        assert_eq!(
            g.check_ahead(&[doom_signature("use_skill", r#"{"name":"x"}"#)]),
            GuardAction::Continue
        );
    }

    #[test]
    fn humanize_sig_formats_locators() {
        assert_eq!(humanize_sig("bash|ls -la"), "bash ls -la");
        assert_eq!(
            humanize_sig("read_text|src/main.rs"),
            "read_text src/main.rs"
        );
        assert_eq!(humanize_sig("grep"), "grep");
        assert_eq!(humanize_sig("use_skill|<unwatched>"), "use_skill");
    }
}
