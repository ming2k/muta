//! General-purpose doom-loop guard: a pre-dispatch detector that intercepts
//! *any* tool call whose signature has already been issued this round, before
//! the tool runs — not just reads.
//!
//! # Why a separate guard
//!
//! `crate::loop_guard::ReadLoopGuard` is read-only, post-hoc, and defaults
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
//! - **All tools**: covers the common doom-loop culprits — `read`,
//!   `find_files`, `list_dir`, `search_text`, `bash`, `webfetch`, `websearch`, `edit_file`,
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
//! The doom guard is gated by `NudgeConfig::enabled` for consistency with the
//! read-loop guard: when nudging is off, neither guard runs. Runner and review
//! paths disable nudging, so they stay unobstructed.

use std::collections::VecDeque;

use muta_contracts::DoomGuardConfig;
use serde_json::Value;

use crate::loop_guard::GuardAction;

/// The tools this guard watches. Anything outside this set is passed through
/// untouched — MCP tools, `ask_user`, `use_skill`, `todo_*`, runner, etc. are
/// either inherently unique or user-interactive, where a repeat is plausibly
/// legitimate and blocking would be hostile.
///
/// Kept as a sorted set so the [`covers`] lookup is O(log n).
const WATCHED_TOOLS: &[&str] = &[
    "bash",
    "edit_file",
    "find_files",
    "list_dir",
    "read",
    "read_image",
    "read_text",
    "search_text",
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
/// - **Range-addressed file reads** (`read_text`, `read`):
///   `name|path|offset={offset}|limit={limit}` — normalized line range (ADR-0034).
///   A read to a different offset/limit represents legitimate forward paging
///   or section inspection and produces a distinct signature. Re-reading the
///   identical range on the same path collides and is blocked.
/// - **Path-addressed mutations / directory reads** (`edit_file`, `write_file`,
///   `list_dir`, `read_image`): `name|path` — the target file/dir. Mutations
///   keep `path` only because an A→B→A edit thrash on one file is the classic
///   doom loop.
/// - **Command-addressed calls** (`bash`): `name|command` — the literal command
///   string. Running the identical command twice in a turn is never productive.
/// - **Query-addressed calls** (`search_text`, `websearch`): `name|query` — the search
///   text. A different query is a different call; the same query again is a
///   repeat.
/// - **URL-addressed calls** (`webfetch`): `name|url`.
/// - **Pattern-list calls** (`find_files`): the whole normalized argument set.
/// - **Anything else / unparseable**: fall back to `name|<raw args>` so the
///   call is still keyed (two identical blobs still collide) but distinct
///   blobs stay distinct.
pub fn doom_signature(name: &str, args: &str) -> String {
    if !covers(name) {
        return format!("{name}|<unwatched>");
    }
    let value: Value = serde_json::from_str(args).unwrap_or(Value::Null);
    if name == "find_files" {
        let path = value
            .get("path")
            .and_then(Value::as_str)
            .map(normalize_path_locator)
            .unwrap_or_else(|| ".".to_string());
        return format!(
            "{name}|{path}|include={}|exclude={}",
            normalized_string_array(&value, "patterns", false),
            normalized_string_array(&value, "exclude", false)
        );
    }
    if name == "search_text" {
        let query = value
            .get("query")
            .and_then(Value::as_str)
            .map(normalize_query_locator)
            .unwrap_or_default();
        let path = value
            .get("path")
            .and_then(Value::as_str)
            .map(normalize_path_locator)
            .unwrap_or_else(|| ".".to_string());
        return format!(
            "{name}|{query}|{path}|include={}|exclude={}|literal={}",
            normalized_string_array(&value, "include", false),
            normalized_string_array(&value, "exclude", false),
            value
                .get("literal")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        );
    }
    if name == "read_text" || name == "read" {
        let path = value
            .get("path")
            .or_else(|| value.get("file_path"))
            .or_else(|| value.get("file"))
            .or_else(|| value.get("filename"))
            .and_then(Value::as_str)
            .map(normalize_path_locator)
            .unwrap_or_default();
        let offset = value
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1);
        let limit = value.get("limit").and_then(Value::as_u64).unwrap_or(0);
        return format!("{name}|{path}|offset={offset}|limit={limit}");
    }
    // Prefer the most specific locator present, in priority order.
    for key in ["command", "cmd"] {
        if let Some(s) = value.get(key).and_then(Value::as_str) {
            return format!("{name}|{}", normalize_command_locator(s));
        }
    }
    if let Some(s) = value.get("url").and_then(Value::as_str) {
        return format!("{name}|{}", normalize_query_locator(s));
    }
    for key in ["query", "pattern", "q"] {
        if let Some(s) = value.get(key).and_then(Value::as_str) {
            return format!("{name}|{}", normalize_query_locator(s));
        }
    }
    for key in ["path", "file_path", "file", "filename"] {
        if let Some(s) = value.get(key).and_then(Value::as_str) {
            return format!("{name}|{}", normalize_path_locator(s));
        }
    }
    // No recognised locator: key on the whole arg blob so two identical blobs
    // still collide (a true exact-repeat) but distinct blobs stay distinct.
    format!("{name}|{}", args.trim())
}

/// Normalize a shell-command locator so cosmetic variation does not defeat
/// signature equality: drop leading `VAR=value` assignments, drop throwaway
/// segments whose first token is a timing no-op (`sleep 2; make test` ≡
/// `make test`), collapse whitespace, and lowercase the leading token
/// (program name). Genuinely different commands still differ.
fn normalize_command_locator(raw: &str) -> String {
    let mut meaningful: Vec<String> = Vec::new();
    for segment in raw.split([';', '\n']) {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let cleaned = strip_env_assignments(segment);
        if cleaned.is_empty() {
            continue;
        }
        let first = cleaned.split_whitespace().next().unwrap_or("");
        if is_noise_first_token(first) {
            continue;
        }
        let mut tokens: Vec<String> = cleaned.split_whitespace().map(str::to_string).collect();
        if let Some(first) = tokens.first_mut() {
            *first = first.to_lowercase();
        }
        meaningful.push(tokens.join(" "));
    }
    if meaningful.is_empty() {
        // Everything was noise (e.g. a bare `sleep 5`): key on the no-op's
        // name alone — `sleep 5` vs `sleep 9` is the classic variant-loop
        // noise, and there is no other intent to preserve.
        let first = strip_env_assignments(raw.trim())
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_lowercase();
        return first;
    }
    meaningful.join("; ")
}

/// Strip leading `VAR=value` assignments from a command segment (they are
/// environment plumbing, not intent).
fn strip_env_assignments(segment: &str) -> String {
    let mut tokens: Vec<&str> = Vec::new();
    for tok in segment.split_whitespace() {
        if tokens.is_empty()
            && tok.contains('=')
            && tok.split_once('=').is_some_and(|(k, v)| {
                !k.is_empty()
                    && !v.is_empty()
                    && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        {
            continue; // leading assignment: drop
        }
        tokens.push(tok);
    }
    tokens.join(" ")
}

/// First tokens whose segments carry no distinguishing intent for loop
/// detection: timing/no-ops the model uses to vary a signature.
fn is_noise_first_token(token: &str) -> bool {
    matches!(token.to_lowercase().as_str(), "sleep" | "true" | ":")
}

/// Normalize query/pattern/url locators: trim + collapse whitespace and
/// lowercase (search text casing is not intent). Kept conservative — no
/// stemming or stopword removal, which could over-merge distinct queries.
fn normalize_query_locator(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Normalize path locators: trim separators and trailing slashes so
/// `src/`, `./src` key identically.
fn normalize_path_locator(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    trimmed.strip_prefix("./").unwrap_or(trimmed).to_string()
}

fn normalized_string_array(value: &Value, key: &str, lowercase: bool) -> String {
    let mut strings = value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|item| {
            let item = item.trim();
            if lowercase {
                item.to_lowercase()
            } else {
                item.to_string()
            }
        })
        .collect::<Vec<_>>();
    strings.sort();
    strings.dedup();
    strings.join(",")
}

/// The pre-dispatch doom-loop detector.
///
/// One lives per user round in `RoundState` (see `crate::agent::RoundState`) and is
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
    let parts: Vec<&str> = signature.split('|').collect();
    if (parts.first() == Some(&"read_text") || parts.first() == Some(&"read")) && parts.len() == 4 {
        let name = parts[0];
        let path = parts[1];
        let offset = parts[2].strip_prefix("offset=").unwrap_or("1");
        let limit = parts[3].strip_prefix("limit=").unwrap_or("0");
        if offset == "1" && limit == "0" {
            format!("{name} {path}")
        } else if limit == "0" {
            format!("{name} {path} :{offset},$")
        } else {
            format!("{name} {path} :{offset},limit={limit}")
        }
    } else {
        let mut parts = signature.splitn(2, '|');
        let name = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("");
        if rest.is_empty() || rest == "<unwatched>" {
            name.to_string()
        } else {
            format!("{name} {rest}")
        }
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
    fn read_distinct_ranges_do_not_collide() {
        let a = doom_signature("read_text", r#"{"path":"a.rs","offset":1,"limit":100}"#);
        let b = doom_signature("read_text", r#"{"path":"a.rs","offset":101,"limit":100}"#);
        assert_ne!(
            a, b,
            "different offsets must not collide so forward paging works"
        );
    }

    #[test]
    fn read_same_range_collides_and_normalizes_defaults() {
        let a = doom_signature("read_text", r#"{"path":"a.rs"}"#);
        let b = doom_signature("read_text", r#"{"path":"a.rs","offset":1,"limit":0}"#);
        assert_eq!(
            a, b,
            "implicit defaults must match explicit offset 1 limit 0"
        );
        let c = doom_signature("read_text", r#"{"path":"a.rs","offset":1}"#);
        assert_eq!(a, c);
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
            humanize_sig(&doom_signature("read_text", r#"{"path":"src/main.rs"}"#)),
            "read_text src/main.rs"
        );
        assert_eq!(
            humanize_sig(&doom_signature(
                "read_text",
                r#"{"path":"src/main.rs","offset":110}"#
            )),
            "read_text src/main.rs :110,$"
        );
        assert_eq!(
            humanize_sig(&doom_signature(
                "read_text",
                r#"{"path":"src/main.rs","offset":110,"limit":50}"#
            )),
            "read_text src/main.rs :110,limit=50"
        );
        assert_eq!(humanize_sig("search_text"), "search_text");
        assert_eq!(humanize_sig("use_skill|<unwatched>"), "use_skill");
    }

    // ── Signature normalization (v2) ─────────────────────────────────────
    // The guard is only as good as signature equality: before normalization,
    // `sleep 1; make test` vs `sleep 2; make test` were distinct signatures
    // and the guard never fired on variant loops.

    #[test]
    fn sleep_noise_variants_collide() {
        let a = doom_signature("bash", r#"{"command":"sleep 1; make test"}"#);
        let b = doom_signature("bash", r#"{"command":"sleep 2; make test"}"#);
        assert_eq!(a, b, "timing no-op must not distinguish signatures");
    }

    #[test]
    fn bare_sleep_variants_collide() {
        let a = doom_signature("bash", r#"{"command":"sleep 5"}"#);
        let b = doom_signature("bash", r#"{"command":"sleep 9"}"#);
        assert_eq!(a, b, "a pure no-op keys on its stripped form");
    }

    #[test]
    fn env_assignment_prefixes_collide() {
        let a = doom_signature("bash", r#"{"command":"FOO=1 make test"}"#);
        let b = doom_signature("bash", r#"{"command":"make test"}"#);
        assert_eq!(a, b, "leading VAR=value is plumbing, not intent");
    }

    #[test]
    fn program_casing_collides_but_arguments_do_not() {
        let a = doom_signature("bash", r#"{"command":"Make test"}"#);
        let b = doom_signature("bash", r#"{"command":"make test"}"#);
        assert_eq!(a, b, "program-name casing is not intent");
        let c = doom_signature("bash", r#"{"command":"make check"}"#);
        assert_ne!(b, c, "different arguments remain distinct");
    }

    #[test]
    fn query_casing_and_spacing_collide() {
        let a = doom_signature("search_text", r#"{"query":"TODO  fix"}"#);
        let b = doom_signature("search_text", r#"{"query":"todo fix"}"#);
        assert_eq!(a, b);
        let c = doom_signature("search_text", r#"{"query":"todo refactor"}"#);
        assert_ne!(b, c);
    }

    #[test]
    fn file_pattern_order_does_not_change_search_intent() {
        let a = doom_signature(
            "find_files",
            r#"{"patterns":["*.rs","*.toml"],"path":"./src"}"#,
        );
        let b = doom_signature(
            "find_files",
            r#"{"path":"src/","patterns":["*.toml","*.rs"]}"#,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn path_decorations_collide() {
        let a = doom_signature("read_text", r#"{"path":"./src/"}"#);
        let b = doom_signature("read_text", r#"{"path":"src"}"#);
        assert_eq!(a, b);
    }

    #[test]
    fn normalized_variant_loop_is_blocked() {
        // End-to-end: a variant loop (the exact escape the normalization
        // closes) must trip the guard's pre-dispatch block.
        let mut g = DoomLoopGuard::new(enabled());
        let s = doom_signature("bash", r#"{"command":"sleep 1; make test"}"#);
        assert_eq!(
            g.check_ahead(std::slice::from_ref(&s)),
            GuardAction::Continue
        );
        // Same intent, different noise → same signature → blocked.
        let variant = doom_signature("bash", r#"{"command":"FOO=1 sleep 99; make test"}"#);
        assert_eq!(variant, s, "precondition: normalization collapses them");
        match g.check_ahead(std::slice::from_ref(&variant)) {
            GuardAction::Block { signatures, .. } => {
                assert_eq!(signatures, vec![s]);
            }
            other => panic!("expected block, got {other:?}"),
        }
    }
}
