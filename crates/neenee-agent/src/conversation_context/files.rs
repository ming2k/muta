//! Implicit file-content context injected when the latest visible user text
//! references a path via `@file:` / `@files:`.
//!
//! This is the file analogue of [`super::skills`]: a mention of
//! `@file:src/main.rs` reads that file (subject to a workspace sandbox and a
//! size cap) and appends its contents as a hidden user message, so the model
//! sees the referenced source without an explicit `read_text` call.
//!
//! ## Safety model
//!
//! Every candidate path is resolved against the agent's workspace root and
//! canonicalized before it is read. A path is rejected if:
//! - it is absolute or contains a parent component (`..`) before resolution,
//! - its canonicalized form is not **inside** the workspace root (after
//!   symlink hardening), or
//! - it is a directory, a binary file, or larger than the size cap.
//!
//! Rejections are surfaced as a single hidden error note (one per file) so the
//! model learns *why* the file was not loaded and can recover (switch to
//! `list_dir`, ask the user, etc.) instead of looping on the same path.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::{InjectionKind, Message, Role};

/// Hard upper bound on the number of bytes injected for a single `@file:`
/// reference. Mirrors the read tool's pagination page size; larger files are
/// truncated with a clear marker so the model knows there is more.
const MAX_FILE_BYTES: usize = 50 * 1024;

/// How many distinct `@file:` references a single round may inject. Caps the
/// worst case (a prompt full of file mentions) so one user turn cannot blow
/// the context budget before the model has even started.
const MAX_FILES_PER_ROUND: usize = 10;

/// Resolve `@file:` / `@files:` references in the latest visible user text and
/// append each file's contents as a hidden user message.
///
/// `workspace_root` is the base against which relative paths are resolved and
/// the sandbox they must stay inside; the caller (the agent) supplies the
/// persisted project root. Already-loaded files (a prior hidden
/// `[File '...' loaded]` note in this conversation) are skipped so a repeated
/// reference does not re-inject the body on every turn.
pub(crate) fn inject_mentioned_files(workspace_root: Option<&Path>, messages: &mut Vec<Message>) {
    let Some(root) = workspace_root else {
        // No persisted project root → file injection is disabled. Skill
        // injection still runs independently. Surfacing nothing (rather than
        // guessing `cwd`) keeps the sandbox deterministic and auditable.
        return;
    };

    let text = latest_visible_user_text(messages);
    if text.is_empty() {
        return;
    }

    let referenced = parse_file_refs(&text);
    if referenced.is_empty() {
        return;
    }

    let already_loaded: HashSet<String> = messages
        .iter()
        .filter(|message| message.role == Role::User && message.hidden)
        .filter_map(|message| {
            let prefix = "[File '";
            let start = message.content.find(prefix)? + prefix.len();
            let end = message.content[start..].find("' loaded]")?;
            Some(message.content[start..start + end].to_string())
        })
        .collect();

    let mut injected = 0usize;
    for raw in referenced {
        if injected >= MAX_FILES_PER_ROUND {
            push_error_note(
                messages,
                &raw,
                format!(
                    "not loaded: the per-round file-injection limit ({}) was reached. \
                     Read the rest explicitly with `read_text`.",
                    MAX_FILES_PER_ROUND
                ),
            );
            continue;
        }
        if already_loaded.contains(&raw) {
            continue;
        }
        match load_sandboxed(root, &raw) {
            Ok((resolved, bytes)) => {
                let display = path_display(root, &resolved);
                let content = render_file(&display, &bytes);
                messages.push(super::hidden_user_with_reason(
                    InjectionKind::ImplicitFile,
                    &display,
                    content,
                ));
                injected += 1;
            }
            Err(reason) => {
                push_error_note(messages, &raw, format!("not loaded: {reason}"));
                injected += 1;
            }
        }
    }
}

/// The newest non-empty visible user message, joined if a round carries
/// several. Mirrors [`super::skills`]'s definition of "the prompt that
/// mentions".
fn latest_visible_user_text(messages: &[Message]) -> String {
    messages
        .iter()
        .filter(|message| message.role == Role::User && !message.hidden)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract `@file:{path}` / `@files:{path}` references from `text`, in order,
/// deduplicated. The path runs until the first whitespace or any character
/// that cannot legally begin a relative path, so `@file:src/main.rs` and
/// `@file:src/main.rs.` (trailing period) both yield `src/main.rs`.
fn parse_file_refs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find('@') {
        let at = search_from + rel;
        let after_at = at + 1;
        let Some(rest) = text.get(after_at..) else {
            break;
        };
        let Some(stripped) = rest
            .strip_prefix("files:")
            .or_else(|| rest.strip_prefix("file:"))
        else {
            search_from = after_at;
            continue;
        };
        let path_start = after_at + (rest.len() - stripped.len());
        let mut end = path_start;
        while let Some(ch) = text[end..].chars().next()
            && is_path_char(ch)
        {
            end += ch.len_utf8();
        }
        if end > path_start {
            let raw = text[path_start..end].trim_end_matches('.').to_string();
            if !raw.is_empty() && seen.insert(raw.clone()) {
                out.push(raw);
            }
        }
        search_from = end.max(after_at);
    }
    out
}

/// Characters permitted inside a raw `@file:` reference. Relative paths may
/// contain path separators and the usual filename alphabet; whitespace, quotes,
/// commas, and sentence punctuation terminate the reference so prose like
/// "see @file:src/main.rs." reads cleanly.
fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '\\' | '_' | '-' | '.' | '+' | '~' | '@')
}

/// Resolve `raw` against `root`, sandbox it, and read it. Returns the
/// canonicalized path and the file bytes (already capped to
/// [`MAX_FILE_BYTES`]). Returns `Err(reason)` for every reject case so the
/// caller can surface a single actionable note.
fn load_sandboxed(root: &Path, raw: &str) -> Result<(PathBuf, Vec<u8>), String> {
    // Reject anything that is not a plain relative path *before* touching the
    // filesystem: an absolute path (`/etc/passwd`) or a parent traversal
    // (`../secret`) cannot live under the workspace by construction.
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        return Err(
            "absolute paths are not allowed — reference a path relative to the workspace root"
                .to_string(),
        );
    }
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("`..` traversal is not allowed — stay within the workspace root".to_string());
    }
    if raw.is_empty() || raw == "." {
        return Err("empty path".to_string());
    }

    let joined = root.join(candidate);
    let canonical = joined.canonicalize().map_err(|e| {
        format!(
            "could not resolve '{}' under the workspace root: {}",
            raw, e
        )
    })?;

    // Symlink-hardened containment: the canonicalized path must start with the
    // canonicalized root. This catches a relative path that resolves through a
    // symlink out of the workspace.
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("workspace root is not resolvable: {}", e))?;
    if !canonical.starts_with(&canonical_root) {
        return Err("path escapes the workspace root".to_string());
    }

    if canonical.is_dir() {
        return Err(format!(
            "'{}' is a directory — use `list_dir` to inspect its contents",
            path_display(root, &canonical)
        ));
    }

    // Read the head first so an oversized binary is refused before the whole
    // file is buffered. Mirrors the read tool's sniff-then-read discipline.
    let mut head = [0u8; 4096];
    {
        use std::io::Read;
        let mut file = std::fs::File::open(&canonical)
            .map_err(|e| format!("could not open '{}': {}", path_display(root, &canonical), e))?;
        let n = file
            .read(&mut head)
            .map_err(|e| format!("could not read '{}': {}", path_display(root, &canonical), e))?;
        if is_binary_content(&head[..n]) {
            return Err(format!(
                "'{}' looks like a binary file and will not be injected",
                path_display(root, &canonical)
            ));
        }
    }

    let bytes = std::fs::read(&canonical)
        .map_err(|e| format!("could not read '{}': {}", path_display(root, &canonical), e))?;
    if bytes.len() > MAX_FILE_BYTES {
        // Truncate rather than refuse: a large source file is still useful in
        // context; the model just needs to know it is partial.
        let mut truncated = bytes[..MAX_FILE_BYTES].to_vec();
        truncated.extend_from_slice(
            format!(
                "\n\n[... file truncated at {} bytes; {} total bytes — read the rest with `read_text`]",
                MAX_FILE_BYTES,
                bytes.len()
            )
            .as_bytes(),
        );
        return Ok((canonical, truncated));
    }
    Ok((canonical, bytes))
}

/// NUL or a disproportionate run of control bytes ⇒ binary. Same heuristic as
/// the read tool, kept local so this module owns its whole reject path.
fn is_binary_content(buf: &[u8]) -> bool {
    if buf.is_empty() {
        return false;
    }
    if buf.contains(&0) {
        return true;
    }
    let control = buf
        .iter()
        .filter(|b| **b < 0x20 && **b != b'\n' && **b != b'\r' && **b != b'\t')
        .count();
    control * 10 > buf.len()
}

/// Render the loaded file as the hidden context message body.
fn render_file(display_path: &str, bytes: &[u8]) -> String {
    let body = String::from_utf8_lossy(bytes);
    format!("[File '{display_path}' loaded]\n{body}\n[/File]")
}

/// Append a hidden note explaining why a referenced file was not loaded, so
/// the model can recover instead of looping on the same path.
fn push_error_note(messages: &mut Vec<Message>, raw: &str, reason: String) {
    messages.push(super::hidden_user_with_reason(
        InjectionKind::ImplicitFile,
        raw,
        format!("[File '{raw}' {reason}]"),
    ));
}

/// Display path relative to the workspace root when possible (cleaner in
/// context than an absolute path), falling back to the canonical form.
fn path_display(root: &Path, canonical: &Path) -> String {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    canonical
        .strip_prefix(&canonical_root)
        .map(|rel| rel.to_string_lossy().into_owned())
        .unwrap_or_else(|_| canonical.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_file_ref() {
        assert_eq!(
            parse_file_refs("refactor @file:src/main.rs now"),
            vec!["src/main.rs"]
        );
    }

    #[test]
    fn parses_files_plural_and_dedups() {
        let refs = parse_file_refs("@files:a.rs @files:a.rs and @file:b.rs");
        assert_eq!(refs, vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn strips_trailing_punctuation() {
        // A sentence-ending period must not become part of the path.
        assert_eq!(parse_file_refs("see @file:src/lib.rs."), vec!["src/lib.rs"]);
        assert_eq!(parse_file_refs("(@file:x.txt,)",), vec!["x.txt"]);
    }

    #[test]
    fn ignores_bare_at_mention_without_namespace() {
        // `@some_user` is not a file reference — only `@file:`/`@files:` are.
        assert!(parse_file_refs("ping @some_user about @file:real.rs").len() == 1);
        assert_eq!(
            parse_file_refs("ping @some_user about @file:real.rs"),
            vec!["real.rs"]
        );
    }

    #[test]
    fn rejects_absolute_path() {
        let tmp = tempdir();
        let absolute = std::env::temp_dir().join("neenee-absolute-path-probe");
        let err = load_sandboxed(&tmp, absolute.to_str().unwrap()).unwrap_err();
        assert!(err.contains("absolute paths are not allowed"));
    }

    #[test]
    fn rejects_parent_traversal() {
        let tmp = tempdir();
        let err = load_sandboxed(&tmp, "../secret").unwrap_err();
        assert!(err.contains("`..` traversal is not allowed"));
    }

    #[test]
    fn loads_file_inside_root() {
        let tmp = tempdir();
        let target = tmp.join("src").join("main.rs");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "fn main() {}").unwrap();
        let (resolved, bytes) = load_sandboxed(&tmp, "src/main.rs").unwrap();
        assert!(resolved.ends_with("main.rs"));
        assert_eq!(bytes, b"fn main() {}");
    }

    #[test]
    fn rejects_symlink_escape() {
        let tmp = tempdir();
        // An outside file the workspace has no business reading.
        let outside = std::env::temp_dir().join(format!(
            "neenee-file-inject-outside-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&outside, "secret").unwrap();
        // A symlink inside the workspace that points outside.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, tmp.join("escape")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, tmp.join("escape")).unwrap();

        let err = load_sandboxed(&tmp, "escape").unwrap_err();
        assert!(err.contains("escapes the workspace root"), "got: {err}");
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn rejects_directory() {
        let tmp = tempdir();
        std::fs::create_dir_all(tmp.join("dir")).unwrap();
        let err = load_sandboxed(&tmp, "dir").unwrap_err();
        assert!(err.contains("directory"));
    }

    #[test]
    fn rejects_binary() {
        let tmp = tempdir();
        std::fs::write(tmp.join("blob.bin"), [0u8, 1, 2, 0, 4, 5]).unwrap();
        let err = load_sandboxed(&tmp, "blob.bin").unwrap_err();
        assert!(err.contains("binary"));
    }

    #[test]
    fn truncates_oversize_file() {
        let tmp = tempdir();
        // Double the cap, all ASCII so it is not flagged binary.
        let big = "a".repeat(MAX_FILE_BYTES * 2);
        std::fs::write(tmp.join("big.txt"), &big).unwrap();
        let (_, bytes) = load_sandboxed(&tmp, "big.txt").unwrap();
        assert!(bytes.len() > MAX_FILE_BYTES);
        assert!(bytes.len() < MAX_FILE_BYTES * 2);
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("truncated"),
            "should carry the truncation marker"
        );
    }

    #[test]
    fn inject_appends_hidden_message_for_file_ref() {
        let tmp = tempdir();
        std::fs::write(tmp.join("lib.rs"), "pub fn x() {}").unwrap();
        let mut messages = vec![Message::new(Role::User, "review @file:lib.rs".to_string())];
        inject_mentioned_files(Some(&tmp), &mut messages);
        assert_eq!(messages.len(), 2);
        assert!(messages[1].hidden);
        assert!(messages[1].content.contains("[File 'lib.rs' loaded]"));
        assert!(messages[1].content.contains("pub fn x() {}"));
    }

    #[test]
    fn inject_skips_already_loaded_file() {
        let tmp = tempdir();
        std::fs::write(tmp.join("lib.rs"), "pub fn x() {}").unwrap();
        // First turn: loads it.
        let mut messages = vec![Message::new(Role::User, "@file:lib.rs".to_string())];
        inject_mentioned_files(Some(&tmp), &mut messages);
        assert_eq!(messages.len(), 2);
        // Second turn: mention it again — must NOT re-inject.
        messages.push(Message::new(
            Role::User,
            "and @file:lib.rs again".to_string(),
        ));
        inject_mentioned_files(Some(&tmp), &mut messages);
        // One user turn each + exactly one hidden load.
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages
                .iter()
                .filter(|m| m.hidden && m.content.contains("[File 'lib.rs' loaded]"))
                .count(),
            1
        );
    }

    #[test]
    fn inject_without_workspace_root_is_noop() {
        let mut messages = vec![Message::new(Role::User, "@file:lib.rs".to_string())];
        inject_mentioned_files(None, &mut messages);
        assert_eq!(messages.len(), 1);
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neenee-file-inject-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
