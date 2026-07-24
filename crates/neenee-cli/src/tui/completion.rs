//! Input-box completion pipeline: `/slash` commands and inline `@path` file
//! mentions. The pipeline is stateless on top of [`App::input`] — each
//! keystroke re-derives the candidates from the live text and the (cached)
//! recursive project scan. The render-facing data types ([`Completion`],
//! [`CompletionKind`], [`CompletionItemKind`]) live here too, shared with the
//! completion-menu renderer.

use crate::startup::BuiltinCmd;
use crate::tui::App;
use crate::tui::composer::{composer_text_width, composer_wrapped_pos};

/// Kind of completion menu the input box is currently offering. Drives the
/// keyboard shortcuts that cycle / accept entries: Tab, ↑/↓, and (for slash
/// only) plain Enter on a unique prefix. Path mentions only complete via Tab
/// so a plain Enter still sends the message as typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionKind {
    /// No completion menu is active.
    #[default]
    None,
    /// `/command` and subcommand completion (replaces the whole input).
    Slash,
    /// `@path` file mention completion (splices into the input at the cursor).
    Path,
}

/// What a [`Completion`] candidate represents, which controls the accept
/// semantics in `App::accept_completion`. Kept on the candidate itself (rather
/// than re-derived from the label) so absolute-path mention labels (which
/// legitimately start with `/`) are never confused with slash commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionItemKind {
    /// `/command` — replaces the whole input; a terminal accept (popup closes).
    #[default]
    Slash,
    /// `@path` file mention — the leading `@` trigger is dropped on accept
    /// (the concrete path is what enters the message context) and a trailing
    /// space is appended; a terminal accept.
    PathFile,
    /// `@path` directory mention from the project scan — the `@` trigger is
    /// kept so the popup can re-trigger on the directory's contents (descend
    /// navigation); no trailing space; stays live for further cycling.
    PathDir,
    /// `@path` mention resolved from an explicit prefix (`../`, `./`, `~/`,
    /// `/`) — expanded to an absolute path. Terminal on accept (the absolute
    /// path is concrete): the `@` is dropped, files get a trailing space.
    PathExplicit,
}

/// A single completion candidate rendered in the completion menu. The
/// `replace_start..replace_end` byte range is the slice of the current input
/// that gets overwritten by `label` when the candidate is accepted, so slash
/// commands (which replace the whole input) and inline `@path` mentions
/// (which replace the `@`-prefixed token) share one accept path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Text to insert at the replace range.
    pub label: String,
    /// Hint shown to the right of the label (e.g. "Set pursuit", "dir", "1.2k").
    pub description: String,
    /// Byte offset in `App::input` where the replacement starts.
    pub replace_start: usize,
    /// Byte offset in `App::input` where the replacement ends.
    pub replace_end: usize,
    /// What this candidate is and how accepting it behaves. See
    /// [`CompletionItemKind`].
    pub kind: CompletionItemKind,
}

impl Completion {
    /// Build a slash-command style completion that replaces the whole input
    /// (`replace_start = 0`, `replace_end = input_len`).
    pub fn whole_input(label: &str, description: &str, input_len: usize) -> Completion {
        Completion {
            label: label.to_string(),
            description: description.to_string(),
            replace_start: 0,
            replace_end: input_len,
            kind: CompletionItemKind::Slash,
        }
    }
}

// The built-in slash-command vocabulary (names + descriptions) lives in ONE
// place: `startup::BuiltinCmd::ALL`. Completion, `/help`, and the dispatch
// `match` in `main.rs` all derive from it, and that dispatch is a
// non-exhaustive match over `Option<BuiltinCmd>` — so a command added to the
// table without a handler arm (or vice versa) fails to compile. There is no
// second list here to drift out of sync.

/// Upper bound on the number of filesystem entries scanned for a single `@`
/// mention completion. Bounds the work on huge directories (e.g. generated
/// `node_modules`) so each keystroke stays imperceptible; the menu renders the
/// first six and cycles through the rest with ↑/↓.
const MAX_PATH_COMPLETIONS: usize = 200;

/// Cached recursive project listing for `@path` completion. Entries are
/// normalized to forward-slash paths relative to the captured cwd:
/// directories get a trailing `/`, files do not. Built once by
/// [`scan_project_files`] (ripgrep-first, manual walk fallback) and reused
/// across keystrokes, mirroring the per-directory picker cache in opencode's
/// TUI so each keystroke only filters instead of re-scanning.
#[derive(Debug, Clone)]
pub struct PathScan {
    pub entries: Vec<String>,
}

/// Recursively list files (and synthesized directory entries) under `cwd`,
/// respecting `.gitignore` and `.ignore`. Hidden files are included by
/// default so the user can mention e.g. `.env`; `.git/` is always excluded.
///
/// Prefers `rg --files` (fast, gitignore-aware, already a project dep) and
/// falls back to a manual recursive walk when `rg` is unavailable so the
/// feature still works on stripped systems. Matches the ripgrep-fallback
/// behaviour opencode uses when its native `fff` picker is missing.
pub(super) fn scan_project_files(cwd: &std::path::Path) -> PathScan {
    let entries = try_ripgrep_scan(cwd).unwrap_or_else(|| manual_walk(cwd));
    PathScan { entries }
}

/// Ripgrep-backed project scan. Returns `None` if `rg` cannot be spawned or
/// exits non-zero so the caller can fall back to [`manual_walk`].
fn try_ripgrep_scan(cwd: &std::path::Path) -> Option<Vec<String>> {
    let output = std::process::Command::new("rg")
        .args([
            "--files",
            "--hidden",
            "--glob=!.git",
            "--color=never",
            "--no-messages",
        ])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let files: Vec<String> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.replace('\\', "/"))
        .collect();

    // Synthesize directory entries by walking each file's ancestor chain—
    // `rg --files` only emits files, so directories are derived. Matches
    // opencode's ripgrep-fallback behaviour.
    let mut dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for path in &files {
        let mut acc = String::new();
        let parts: Vec<&str> = path.split('/').collect();
        // All but the last segment (the filename) are directory ancestors.
        for part in &parts[..parts.len().saturating_sub(1)] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(part);
            dirs.insert(format!("{}/", acc));
        }
    }

    let mut entries: Vec<String> = files;
    entries.extend(dirs);
    // Dirs first (alphabetic), then files (alphabetic). Case-insensitive to
    // keep `README.md` and `readme.md` adjacent on case-insensitive FSes.
    entries.sort_by(|a, b| {
        let a_dir = a.ends_with('/');
        let b_dir = b.ends_with('/');
        b_dir
            .cmp(&a_dir)
            .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
    });
    entries.dedup();
    Some(entries)
}

/// Pure-Rust recursive directory walk used when `rg` is unavailable. Skips
/// `.git/` unconditionally; hidden files and other ignored directories are
/// included so users can still mention e.g. `.env` or `.github/workflows`.
pub(super) fn manual_walk(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: Vec<(std::path::PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel_prefix)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = match entry.file_name().to_str() {
                Some(s) => s.to_string(),
                None => continue,
            };
            // `.git/` is always skipped to avoid dumping the entire repo
            // internals into the completion list.
            if name == ".git" {
                continue;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let rel = if rel_prefix.is_empty() {
                name.clone()
            } else {
                format!("{}{}", rel_prefix, name)
            };
            if is_dir {
                let child_rel = format!("{}/", rel);
                stack.push((entry.path(), child_rel.clone()));
                out.push(child_rel);
            } else {
                out.push(rel);
            }
        }
    }
    out.sort_by(|a, b| {
        let a_dir = a.ends_with('/');
        let b_dir = b.ends_with('/');
        b_dir
            .cmp(&a_dir)
            .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
    });
    out
}

/// Decide whether a cached path entry should be shown for a given `@query`.
///
/// - Empty query: only top-level entries (immediate children of cwd), so the
///   initial menu is a small, useful overview instead of every nested file.
/// - Query without `/`: case-insensitive substring match anywhere in the
///   path, so `@foo` finds `src/foo.rs` and `Cargo.lock` alike.
/// - Query ending in `/` (e.g. `@src/`): case-insensitive prefix match,
///   listing that directory's descendants so the user can descend naturally.
/// - Other queries: case-insensitive substring match — covers `@src/foo` and
///   similar mid-path fragments.
pub(super) fn path_query_match(path: &str, query: &str) -> bool {
    if query.is_empty() {
        // Top-level: a path with no `/`, or a single trailing `/` and nothing
        // else (top-level directory).
        let trimmed = path.trim_end_matches('/');
        !trimmed.contains('/')
    } else if let Some(dir_prefix) = query.strip_suffix('/').filter(|_| query.contains('/')) {
        // Query is `@<dir>/`: descend, prefix match.
        path.to_lowercase().starts_with(&dir_prefix.to_lowercase())
    } else {
        path.to_lowercase().contains(&query.to_lowercase())
    }
}

/// Whether a `@`-mention query is an *explicit* path prefix that should be
/// resolved against the real filesystem (and expanded to an absolute path)
/// rather than filtered against the recursive project scan. Covers the
/// conventions shells and editors use:
/// - `../` / `..` — parent of the project root
/// - `./` / `.`   — explicit current dir (rare, but unambiguous)
/// - `~/` / `~`   — the user's home directory
/// - `/`          — filesystem root
///
/// A plain relative segment like `src/` is NOT explicit and keeps using the
/// project scan, so the common case is unaffected.
pub(super) fn is_explicit_path_prefix(query: &str) -> bool {
    query.starts_with("../")
        || query == ".."
        || query.starts_with("./")
        || query == "."
        || query.starts_with("~/")
        || query == "~"
        || query.starts_with('/')
}

/// Expand an explicit-path query into `(absolute_dir, name_prefix)`: the
/// directory whose immediate children we should list (absolute, canonicalized
/// where possible so candidates render as clean absolute paths) and the
/// filename fragment the user is still typing. Splits the query at its last
/// `/`; the directory portion is resolved against the matching base — `~` →
/// home, `/` → root, `.`/`..` → the supplied project `cwd` — and the trailing
/// segment becomes the prefix filter. `None` only when the `~` base cannot be
/// resolved at all.
///
/// Examples (cwd `/proj`):
/// - `../src/fo` → (`/src`, `"fo"`)
/// - `~/notes/a` → (`<home>/notes`, `"a"`)
/// - `/etc/h`     → (`/etc`, `"h"`)
pub(super) fn resolve_explicit_dir(
    query: &str,
    cwd: &std::path::Path,
) -> Option<(std::path::PathBuf, String)> {
    use std::path::PathBuf;

    // The base the leading prefix anchors to, plus the remainder of the query
    // that still travels with it (e.g. `../src/fo` → base cwd, remainder
    // `../src/fo`; `~/notes/a` → base home, remainder `notes/a`).
    let (base, remainder): (PathBuf, &str) =
        if let Some(rest) = query.strip_prefix("~/").or_else(|| query.strip_prefix("~")) {
            let home = dirs::home_dir().or_else(|| std::env::var_os("HOME").map(PathBuf::from));
            (home.unwrap_or_default(), rest)
        } else if let Some(rest) = query.strip_prefix('/') {
            (PathBuf::from("/"), rest)
        } else {
            // `../`, `./`, bare `..`/`.` — resolve relative to the project root
            // captured at startup (NOT the live process cwd, which can drift).
            (cwd.to_path_buf(), query)
        };

    // Split the remainder into its directory portion + trailing name prefix.
    // `../src/fo` → dir `../src`, prefix `fo`. A remainder with no `/` (e.g.
    // `@~foo` after stripping `~`) means everything is the prefix and the
    // directory is just the base.
    let last_sep = remainder.rfind('/');
    let dir = match last_sep {
        Some(idx) => base.join(&remainder[..=idx]),
        None => base,
    };
    let name_prefix = match last_sep {
        Some(idx) => remainder[idx + 1..].to_string(),
        None => remainder.to_string(),
    };

    // Canonicalize so the candidates we render are clean absolute paths with
    // no `..`/`.` segments. Fall back to the raw join when canonicalization
    // fails (e.g. the parent dir is unreadable) rather than dropping the menu.
    let canonical = dir.canonicalize().unwrap_or(dir);
    Some((canonical, name_prefix))
}

/// Build a project-relative [`Completion`] for a cached scan entry `label`.
/// `at_start`/`cursor_end` are the inclusive `(@..cursor)` byte range; the
/// replacement covers only the path portion (after the `@`). Directory labels
/// (trailing `/`) keep the `@` so the popup can descend; files drop the `@`
/// on accept since the trigger has served its purpose.
pub(super) fn path_completion(label: &str, at_start: usize, cursor_end: usize) -> Completion {
    let is_dir = label.ends_with('/');
    Completion {
        label: label.to_string(),
        description: String::new(),
        replace_start: at_start + 1,
        replace_end: cursor_end,
        kind: if is_dir {
            CompletionItemKind::PathDir
        } else {
            CompletionItemKind::PathFile
        },
    }
}

/// Build an absolute-path [`Completion`] for an explicit-path entry. Because
/// the user asked to descend the real filesystem, every candidate is terminal
/// on accept (the absolute path is concrete): the `@` trigger is dropped and,
/// for files, a trailing space is appended by the accept path. All explicit
/// candidates share [`CompletionItemKind::PathExplicit`]; directories keep a
/// trailing `/` label only for display.
pub(super) fn path_completion_abs(
    label: &str,
    at_start: usize,
    cursor_end: usize,
    is_dir: bool,
) -> Completion {
    let label = if is_dir {
        format!("{}/", label.trim_end_matches('/'))
    } else {
        label.to_string()
    };
    Completion {
        label,
        description: String::new(),
        replace_start: at_start + 1,
        replace_end: cursor_end,
        kind: CompletionItemKind::PathExplicit,
    }
}

/// Stable ordering for path completions: directories first (alphabetic), then
/// files (alphabetic), case-insensitively so `README.md`/`readme.md` stay
/// adjacent on case-insensitive filesystems. Mirrors the scan sort.
pub(super) fn sort_path_completions(comps: &mut [Completion]) {
    comps.sort_by(|a, b| {
        let a_dir = a.label.ends_with('/');
        let b_dir = b.label.ends_with('/');
        b_dir
            .cmp(&a_dir)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
}

/// Column (absolute screen x) the completion popup's leading edge should sit
/// at so the menu hangs off the token it completes:
///
/// - `/command` — the trigger starts the input, so the popup aligns with the
///   start of the composer's text area (the prompt prefix is
///   `COMPOSER_PROMPT_PREFIX_COLS = 2` columns, matched below rather than
///   imported so the `pub(super)` design constant stays private to the view
///   crate).
/// - `@path` — the `@` trigger's column, mapped through the composer's
///   wrapped-line layout so the popup follows the token even when the input
///   wraps (the mention scanner rejects tokens containing newlines, so the
///   `@` is always in the text the wrapper sees).
pub fn completion_anchor_x(
    input: &str,
    byte_cursor: usize,
    input_rect: neenee_tui_engine::Rect,
    kind: CompletionKind,
) -> u16 {
    const COMPOSER_PROMPT_PREFIX_COLS: u16 = 2;
    let text_width = composer_text_width(input_rect.width as usize);
    let trigger_byte = match kind {
        CompletionKind::Path => mention_range_at(input, byte_cursor)
            .map(|(start, _)| start)
            .unwrap_or(0),
        _ => 0,
    };
    let (_, col) = composer_wrapped_pos(input, text_width, trigger_byte);
    input_rect.x + COMPOSER_PROMPT_PREFIX_COLS + col.min(text_width) as u16
}

/// Byte length of the leading `/command` token when the input names a
/// *known* command — a built-in (`BuiltinCmd::from_slash`) or one of the
/// user-defined `custom_commands` — so the composer can accent it and the
/// user can tell a resolved command apart from plain prose (or from an
/// unrecognized `/`-prefix, which stays in the normal text color). Only the
/// command token is covered, never the argument tail. `None` for non-slash
/// input.
pub fn resolved_slash_command_len(
    input: &str,
    custom_commands: &[(String, String)],
) -> Option<usize> {
    if !input.starts_with('/') {
        return None;
    }
    // `split_custom_command` trims the input and strips the leading `/` from
    // the name it returns, so compare against a re-slashed token. The token
    // length in the original input is the name length plus the `/`.
    let (bare, _args) = crate::startup::split_custom_command(input);
    if bare.is_empty() {
        return None;
    }
    let slashed = format!("/{bare}");
    let known = BuiltinCmd::from_slash(&slashed).is_some()
        || custom_commands.iter().any(|(cmd, _)| cmd == &slashed);
    known.then_some(slashed.len())
}

/// Pure core of [`App::active_mention_range`]. Given the input bytes and a
/// byte offset sitting at the caret, return the inclusive `(start, end)` range
/// of the `@mention` token the caret is inside, or `None` when no token is
/// active. See the method docs for the rules.
pub(super) fn mention_range_at(input: &str, cursor_byte: usize) -> Option<(usize, usize)> {
    if cursor_byte > input.len() {
        return None;
    }
    let before = &input[..cursor_byte];
    // Walk back over chars from the cursor looking for an `@` without
    // crossing whitespace. `char_indices` gives byte offsets so the range we
    // return can be sliced straight out of the input.
    let mut chars_before: Vec<(usize, char)> = before.char_indices().collect();
    while let Some((idx, c)) = chars_before.pop() {
        if c.is_whitespace() {
            return None;
        }
        if c == '@' {
            let preceding_whitespace = chars_before
                .last()
                .map(|(_, prev_c)| prev_c.is_whitespace())
                .unwrap_or(true);
            return if preceding_whitespace {
                Some((idx, cursor_byte))
            } else {
                None
            };
        }
    }
    None
}

impl App {
    /// Classify which completion menu, if any, should be shown for the current
    /// input + cursor state. Slash commands take priority over `@path` mentions
    /// because a slash input is a command-in-progress and never carries inline
    /// file references.
    pub fn completion_kind(&self) -> CompletionKind {
        if self.input.starts_with('/') {
            CompletionKind::Slash
        } else if self.active_mention_range().is_some() {
            CompletionKind::Path
        } else {
            CompletionKind::None
        }
    }

    /// Compute the live completion candidates for the current input + cursor.
    /// Returns an empty `Vec` when no menu should be shown. See [`Completion`]
    /// for the slash-vs-path replace-range semantics. Takes `&mut self` so the
    /// `@path` scan can populate [`App::path_scan_cache`] on first use.
    pub fn completions(&mut self) -> Vec<Completion> {
        let current = self.input.to_lowercase();

        // Subcommand completion for /pursue
        if let Some(after) = current.strip_prefix("/pursue ") {
            return [
                ("/pursue status", "Show the current pursuit"),
                ("/pursue stop", "Stop the active pursuit"),
                ("/pursue done", "Mark the pursuit completed"),
                ("/pursue clear", "Remove the pursuit"),
            ]
            .iter()
            .filter(|(cmd, _)| {
                cmd.strip_prefix("/pursue ")
                    .map(|sub| sub.starts_with(after))
                    .unwrap_or(false)
            })
            .map(|(cmd, desc)| Completion::whole_input(cmd, desc, self.input.len()))
            .collect();
        }

        if let Some(after) = current.strip_prefix("/permissions ") {
            return [(
                "/permissions clear",
                "Clear process-local always-allow rules",
            )]
            .iter()
            .filter(|(cmd, _)| {
                cmd.strip_prefix("/permissions ")
                    .map(|sub| sub.starts_with(after))
                    .unwrap_or(false)
            })
            .map(|(cmd, desc)| Completion::whole_input(cmd, desc, self.input.len()))
            .collect();
        }

        if let Some(after) = current.strip_prefix("/session ") {
            return [
                ("/session status", "Show session id and loop checkpoint"),
                ("/session list", "List durable session branches"),
                (
                    "/session resume",
                    "Resume the most recent or selected session",
                ),
                ("/session fork", "Fork the current conversation"),
                ("/session new", "Start a new durable session"),
            ]
            .iter()
            .filter(|(cmd, _)| {
                cmd.strip_prefix("/session ")
                    .map(|sub| sub.starts_with(after))
                    .unwrap_or(false)
            })
            .map(|(cmd, desc)| Completion::whole_input(cmd, desc, self.input.len()))
            .collect();
        }

        if current.starts_with('/') {
            return BuiltinCmd::ALL
                .iter()
                .filter(|(cmd, _)| cmd.starts_with(&current))
                .map(|(cmd, desc)| Completion::whole_input(cmd, desc, self.input.len()))
                .chain(self.custom_commands.iter().filter_map(|(command, desc)| {
                    if command.starts_with(&current) {
                        Some(Completion::whole_input(
                            command.as_str(),
                            desc.as_str(),
                            self.input.len(),
                        ))
                    } else {
                        None
                    }
                }))
                .collect();
        }

        // Inline `@path` file mention completion.
        if let Some(range) = self.active_mention_range() {
            let (at_start, cursor_end) = range;
            // The path text after the `@` trigger.
            let query = &self.input[at_start + 1..cursor_end];
            // Explicit path prefixes (`@../`, `@./`, `@~/`, `@/`) resolve
            // against the real filesystem and expand to absolute paths, so
            // the user can mention files outside the project scan (req: `@`
            // should support `@../`-style completion, expanded to absolute).
            if is_explicit_path_prefix(query) {
                return self.enumerate_explicit_path_completions(range);
            }
            return self.enumerate_path_completions(range);
        }

        Vec::new()
    }

    /// Locate the `@mention` token the cursor is currently inside, if any.
    /// Returns the byte range `(start, end)` of the token inclusive of the
    /// leading `@`. A mention only triggers completion when:
    ///
    /// - The `@` is at the start of the input or preceded by whitespace, so it
    ///   is not confused with e.g. `user@example` in pasted prose.
    /// - The cursor sits somewhere inside the `@`-prefixed run, not after a
    ///   whitespace that terminated it.
    /// - The text between `@` and the cursor contains no whitespace.
    pub fn active_mention_range(&self) -> Option<(usize, usize)> {
        mention_range_at(&self.input, self.byte_cursor())
    }

    /// Enumerate filesystem entries that extend the `@path` prefix the cursor
    /// is currently in. `mention_range` is the inclusive `(@..cursor)` byte
    /// range produced by [`Self::active_mention_range`]. Pulls from the cached
    /// recursive project scan (populated on first use) and filters with
    /// [`path_query_match`], so each keystroke only filters — it never touches
    /// the filesystem. Empty descriptions match opencode's minimal aesthetic;
    /// directories are distinguished by their trailing `/` label.
    fn enumerate_path_completions(&mut self, mention_range: (usize, usize)) -> Vec<Completion> {
        let (at_start, cursor_end) = mention_range;
        // Skip the `@` itself — only the path portion is replaced/extended.
        // Clone into an owned String so the borrow on `self.input` ends before
        // we mutably borrow `self` for the cache populate below.
        let after_at = self.input[at_start + 1..cursor_end].to_string();

        // Lazy-populate the cache on first `@` mention; subsequent calls reuse
        // it. `path_scan()` is `&mut self`, so clone the entries out to avoid
        // holding a borrow across the iterator below.
        let entries: Vec<String> = self.path_scan().entries.clone();

        let mut comps: Vec<Completion> = entries
            .iter()
            .filter(|p| path_query_match(p, &after_at))
            .take(MAX_PATH_COMPLETIONS)
            .map(|p| path_completion(p, at_start, cursor_end))
            .collect();
        // path_query_match + scan already sort, but the take() may have
        // shuffled entries between filter phases; re-sort for stability.
        sort_path_completions(&mut comps);
        comps
    }

    /// Enumerate filesystem entries for an **explicit** path prefix — one
    /// starting with `../`, `./`, `~/`, or `/`. Unlike the project-scan
    /// pipeline, this reads the real directory at the resolved prefix and
    /// expands candidates to absolute paths, so the user can mention files
    /// *outside* the project tree (the project scan only covers cwd
    /// descendants). Every candidate is terminal on accept (the absolute path
    /// is concrete): the `@` trigger is dropped and, for files, a trailing
    /// space is appended by the accept path — matching the requirement that
    /// an explicit `@../`-style mention expand to a clean absolute path.
    fn enumerate_explicit_path_completions(
        &mut self,
        mention_range: (usize, usize),
    ) -> Vec<Completion> {
        let (at_start, cursor_end) = mention_range;
        let query = self.input[at_start + 1..cursor_end].to_string();
        let Some((dir, name_prefix)) = resolve_explicit_dir(&query, &self.cwd) else {
            return Vec::new();
        };
        let entries = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => return Vec::new(),
        };
        let mut comps: Vec<Completion> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                if name == ".git" {
                    return None;
                }
                if !name.to_lowercase().starts_with(&name_prefix.to_lowercase()) {
                    return None;
                }
                let is_dir = entry.file_type().ok()?.is_dir();
                // `read_dir` was given an already-canonicalized absolute dir,
                // so `entry.path()` yields the absolute target. `path_completion_abs`
                // adds the trailing `/` for directories and tags the candidate
                // terminal (PathExplicit).
                let abs = entry.path();
                let label = abs.to_str()?.to_string();
                Some(path_completion_abs(&label, at_start, cursor_end, is_dir))
            })
            .take(MAX_PATH_COMPLETIONS)
            .collect();
        sort_path_completions(&mut comps);
        comps
    }

    /// Borrow the cached recursive project listing, populating it on first
    /// access. Mirrors opencode's per-directory picker cache: one
    /// [`scan_project_files`] call per App session, then pure filtering.
    fn path_scan(&mut self) -> &PathScan {
        self.path_scan_cache
            .get_or_insert_with(|| scan_project_files(&self.cwd))
    }
}
