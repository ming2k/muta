use async_trait::async_trait;
use muta_contracts::Tool;
use muta_tool_derive::ToolSchema;
use serde::Deserialize;

use crate::tools::helpers::{
    WorkspaceBase, env_from_root, execution_environment, json_string, resolve_workspace_path,
    workspace_base,
};

#[derive(ToolSchema, Deserialize)]
struct EditFileArgs {
    #[tool(desc = "Path to the file")]
    path: String,
    #[tool(desc = "The exact text to replace; must be unique in the file")]
    old_string: String,
    #[tool(desc = "The replacement text")]
    new_string: String,
}

/// Apply an edit to a file (safer than write_file — requires old_string match).
///
/// Relative paths resolve against the session's workspace root (captured at
/// factory time), not the daemon process's cwd — under the unified daemon
/// (ADR-0096) those differ whenever the daemon was first spawned from another
/// project, and an edit is exactly where that divergence does damage.
pub struct EditFileTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<std::sync::Arc<dyn muta_contracts::ExecutionEnvironment>>,
}

impl EditFileTool {
    pub fn new(root: WorkspaceBase) -> Self {
        Self { root, env: None }
    }

    pub fn with_env(env: std::sync::Arc<dyn muta_contracts::ExecutionEnvironment>) -> Self {
        let root = Some(env.workspace_root().to_path_buf());
        Self {
            root,
            env: Some(env),
        }
    }
}

/// Number of unchanged context lines to include above and below the edit in the
/// diff display (GitHub-style: 3 lines of surrounding context).
const DIFF_CONTEXT: usize = 3;

/// Extract up to [`DIFF_CONTEXT`] lines above and below a match in `content`,
/// returning the context-bracketed `old`/`new` snippets and an adjusted
/// `start_line` so the line-number gutter reflects true file positions.
fn contextual_patch(
    content: &str,
    match_offset: usize,
    old_str: &str,
    new_str: &str,
) -> (String, String, usize) {
    let match_end = match_offset + old_str.len();

    // Anchor the patch at the beginning of the line containing the match, then
    // walk back over complete source lines. Keeping byte slices instead of
    // splitting/rejoining with `str::lines()` preserves CRLF and prevents a
    // leading newline after the match from becoming a phantom context row.
    let mut context_start = content[..match_offset]
        .rfind('\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    for _ in 0..DIFF_CONTEXT {
        if context_start == 0 {
            break;
        }
        let previous_line_end = context_start - 1;
        context_start = content[..previous_line_end]
            .rfind('\n')
            .map(|idx| idx + 1)
            .unwrap_or(0);
    }

    // If the match includes its trailing newline, it already ends at a line
    // boundary. Otherwise include the unchanged suffix of the line containing
    // the match before walking forward over the requested context lines.
    let mut context_end = if old_str.as_bytes().last() == Some(&b'\n') {
        match_end
    } else {
        content[match_end..]
            .find('\n')
            .map(|rel| match_end + rel + 1)
            .unwrap_or(content.len())
    };
    for _ in 0..DIFF_CONTEXT {
        if context_end >= content.len() {
            break;
        }
        context_end = content[context_end..]
            .find('\n')
            .map(|rel| context_end + rel + 1)
            .unwrap_or(content.len());
    }

    let prefix = &content[context_start..match_offset];
    let suffix = &content[match_end..context_end];
    let build = |replacement: &str| {
        let mut snippet = String::with_capacity(prefix.len() + replacement.len() + suffix.len());
        snippet.push_str(prefix);
        snippet.push_str(replacement);
        snippet.push_str(suffix);
        snippet
    };
    let start_line = content[..context_start].matches('\n').count() + 1;

    (build(old_str), build(new_str), start_line)
}

/// Count non-overlapping occurrences of `needle` in `haystack`.
///
/// An empty `needle` reports zero matches — an empty `old_string` is never a
/// valid edit, and `str::matches("")` would otherwise enumerate every inter-char
/// position and overflow.
fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.matches(needle).count()
}

/// The components needed to display and persist a successful edit.
#[derive(Debug)]
struct AppliedEdit {
    old_ctx: String,
    new_ctx: String,
    ctx_start: usize,
    new_content: String,
}

/// Find a **unique** match of `old` in `content`, build the contextual diff
/// patch, and produce replacement content with exactly that one occurrence
/// swapped for `new`.
///
/// Return value:
/// - `Ok(Some(_))` — exactly one match; safe to apply.
/// - `Ok(None)` — no match (caller may try a fallback or report not-found).
/// - `Err(_)` — the match is *ambiguous* (`count > 1`). This is an error, never
///   a silent global replace: an edit intended for one site must not rewrite
///   every look-alike occurrence.
fn apply_unique_edit(
    content: &str,
    old: &str,
    new: &str,
    path: &str,
) -> Result<Option<AppliedEdit>, String> {
    match count_occurrences(content, old) {
        0 => Ok(None),
        1 => {
            // `find` is guaranteed to return `Some` here (count is exactly 1),
            // but guard with `let … else` so the function stays panic-free even
            // if the invariant above is ever weakened.
            let Some(offset) = content.find(old) else {
                return Ok(None);
            };
            let (old_ctx, new_ctx, ctx_start) = contextual_patch(content, offset, old, new);
            // Replace only this single occurrence by stitching the prefix, the
            // new text, and the suffix back together — *not* `str::replace`,
            // which would rewrite every occurrence.
            let mut new_content = String::with_capacity(content.len() - old.len() + new.len());
            new_content.push_str(&content[..offset]);
            new_content.push_str(new);
            new_content.push_str(&content[offset + old.len()..]);
            Ok(Some(AppliedEdit {
                old_ctx,
                new_ctx,
                ctx_start,
                new_content,
            }))
        }
        n => Err(format!(
            "old_string matches {n} places in '{path}'. Add more surrounding context so the match is unique."
        )),
    }
}

/// Detailed diagnostic when `old_string` cannot be found in `content`.
/// Provides actionable hints for whitespace mismatches, line divergence, or missing lines.
fn diagnose_edit_failure(content: &str, old_str: &str, path: &str) -> String {
    let old_lines: Vec<&str> = old_str.lines().collect();
    let content_lines: Vec<&str> = content.lines().collect();

    if old_lines.is_empty() {
        return format!("Could not find old_string in '{path}': old_string is empty.");
    }

    // 1. Check if trailing-whitespace trimmed version matches
    let trimmed_old: Vec<&str> = old_lines.iter().map(|l| l.trim_end()).collect();
    let trimmed_content: Vec<&str> = content_lines.iter().map(|l| l.trim_end()).collect();
    let mut ws_matches = 0usize;
    let mut ws_match_line = 0usize;
    if !trimmed_old.is_empty() && trimmed_old.len() <= trimmed_content.len() {
        for i in 0..=trimmed_content.len() - trimmed_old.len() {
            if trimmed_content[i..i + trimmed_old.len()] == trimmed_old[..] {
                ws_matches += 1;
                ws_match_line = i + 1;
            }
        }
    }
    if ws_matches == 1 {
        return format!(
            "Could not find exact match for `old_string` in '{path}', but found a match at line {ws_match_line} \
             when ignoring trailing whitespace / line endings. Please check trailing whitespace or indentation around line {ws_match_line}."
        );
    }

    // 2. First-line anchor search & divergence detection
    let first_line = old_lines[0].trim_end();
    if !first_line.is_empty() {
        let candidate_starts: Vec<usize> = content_lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.trim_end() == first_line)
            .map(|(idx, _)| idx)
            .collect();

        if candidate_starts.len() == 1 {
            let start_idx = candidate_starts[0];
            let mut match_count = 0usize;
            while match_count < old_lines.len()
                && start_idx + match_count < content_lines.len()
                && content_lines[start_idx + match_count].trim_end()
                    == old_lines[match_count].trim_end()
            {
                match_count += 1;
            }
            if match_count < old_lines.len() {
                let diverge_old = old_lines[match_count];
                let diverge_actual = if start_idx + match_count < content_lines.len() {
                    content_lines[start_idx + match_count]
                } else {
                    "<end of file>"
                };
                return format!(
                    "Could not find exact match for `old_string` in '{path}'. \
                     Found matching start at line {} (first {} line{} matched), but diverged at line {}:\n\
                     Expected: `{diverge_old}`\n\
                     File has: `{diverge_actual}`\n\
                     Please re-read '{path}' around line {} to get the latest content.",
                    start_idx + 1,
                    match_count,
                    if match_count == 1 { "" } else { "s" },
                    start_idx + match_count + 1,
                    start_idx + 1
                );
            }
        } else if candidate_starts.len() > 1 {
            let lines_str = candidate_starts
                .iter()
                .take(5)
                .map(|idx| (idx + 1).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return format!(
                "Could not find exact match for `old_string` in '{path}'. \
                 The first line `{first_line}` appears multiple times (lines {lines_str}), \
                 but subsequent lines did not match. Please provide more surrounding context or re-read '{path}'."
            );
        }
    }

    format!(
        "Could not find exact match for `old_string` in '{path}' (0 matches found). \
         The content of '{path}' may have changed. Please use `read_text` to inspect the latest file contents before editing."
    )
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }
    fn description(&self) -> &str {
        "Replace a unique block of text (old_string) with new_string in an existing file. old_string must match exactly one location."
    }
    fn parameters(&self) -> serde_json::Value {
        EditFileArgs::parameters_schema()
    }
    fn scope_target(&self, arguments: &str) -> muta_contracts::ScopeTarget {
        muta_contracts::ScopeTarget::Path(std::path::PathBuf::from(json_string(arguments, "path")))
    }
    fn hazard_level(&self) -> muta_contracts::HazardLevel {
        muta_contracts::HazardLevel::FileModification
    }
    fn permission_submission(
        &self,
        arguments: &str,
    ) -> Option<muta_contracts::ToolPermissionSubmission> {
        let path = json_string(arguments, "path");
        Some(muta_contracts::ToolPermissionSubmission {
            hazard_level: muta_contracts::HazardLevel::FileModification,
            label: format!("Edit file `{path}`"),
            description: format!("Modifies content within file `{path}`."),
            scope: path.clone(),
            payload: muta_contracts::ToolPermissionPayload::FileEdit {
                paths: vec![path],
                operation: "edit_file".to_string(),
            },
        })
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_structured(arguments).await.map(|o| o.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<muta_contracts::ToolOutput, String> {
        let args: EditFileArgs =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let path = &args.path;
        let old_str = &args.old_string;
        let new_str = &args.new_string;
        let env = self
            .env
            .clone()
            .unwrap_or_else(|| env_from_root(&self.root));
        let resolved = resolve_workspace_path(&self.root, path);

        let content = env
            .fs()
            .read_to_string(&resolved)
            .await
            .map_err(|e| format!("Failed to read '{}': {}", path, e))?;

        // Exact match first; fall back to a CRLF-normalized comparison so an
        // edit authored with LF line endings works against a CRLF file. Either
        // path requires the match to be *unique* — an ambiguous old_string is an
        // error, never a silent global replace.
        let edit = match apply_unique_edit(&content, old_str, new_str, path)? {
            Some(e) => e,
            None => {
                let normalized_content = content.replace("\r\n", "\n");
                let normalized_old = old_str.replace("\r\n", "\n");
                match apply_unique_edit(&normalized_content, &normalized_old, new_str, path)? {
                    Some(e) => e,
                    None => {
                        return Err(diagnose_edit_failure(&content, old_str, path));
                    }
                }
            }
        };

        // Syntax defense guard: verify syntactic integrity before committing changes to disk.
        if let super::syntax_guard::SyntaxCheckResult::Invalid(err) =
            super::syntax_guard::verify_syntax(&resolved, &edit.new_content)
        {
            return Err(format!(
                "Syntax check failed for '{}': {err}. The edit was NOT applied. Please fix the syntax error and re-apply.",
                path
            ));
        }

        // Atomically commit the new content (temp file + fsync + rename) so an
        // interrupted edit never corrupts the file in place.
        env.fs()
            .write(&resolved, edit.new_content.as_bytes())
            .await
            .map_err(|e| format!("Failed to write '{}': {}", path, e))?;
        Ok(muta_contracts::ToolOutput::Patch {
            path: path.to_string(),
            op: muta_contracts::PatchOp::Edit,
            old: edit.old_ctx,
            new: edit.new_ctx,
            start_line: edit.ctx_start,
        })
    }
}
muta_contracts::register_tool!(EditFileFactory => |ctx| EditFileTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_occurrences_is_zero_for_empty_needle() {
        assert_eq!(count_occurrences("abc", ""), 0);
    }

    #[test]
    fn count_occurrences_handles_overlapping_and_repeats() {
        assert_eq!(count_occurrences("aaa", "a"), 3);
        // `str::matches` is non-overlapping, so "aaaa"/"aa" is 2, not 3.
        assert_eq!(count_occurrences("aaaa", "aa"), 2);
        assert_eq!(count_occurrences("abcabc", "abc"), 2);
        assert_eq!(count_occurrences("abc", "z"), 0);
    }

    #[test]
    fn apply_unique_edit_replaces_exactly_one_occurrence() {
        let content = "foo\nbar\nbaz".to_string();
        let edit = apply_unique_edit(&content, "bar", "qux", "f.txt")
            .unwrap()
            .expect("single match should apply");
        assert_eq!(edit.new_content, "foo\nqux\nbaz");
        assert_eq!(edit.old_ctx, "foo\nbar\nbaz");
        assert_eq!(edit.new_ctx, "foo\nqux\nbaz");
        assert_eq!(edit.ctx_start, 1);
    }

    #[test]
    fn contextual_patch_keeps_three_real_lines_without_phantom_blank() {
        let content = "above 1\nabove 2\nabove 3\nold\nbelow 1\nbelow 2\nbelow 3\nbelow 4\n";
        let offset = content.find("old").unwrap();
        let (old, new, start_line) = contextual_patch(content, offset, "old", "new");

        assert_eq!(
            old,
            "above 1\nabove 2\nabove 3\nold\nbelow 1\nbelow 2\nbelow 3\n"
        );
        assert_eq!(
            new,
            "above 1\nabove 2\nabove 3\nnew\nbelow 1\nbelow 2\nbelow 3\n"
        );
        assert_eq!(start_line, 1);
    }

    #[test]
    fn contextual_patch_preserves_inline_prefix_suffix_and_line_number() {
        let content =
            "discard\nheader\nkeep 1\nkeep 2\nlet value = old;\nafter 1\nafter 2\nafter 3\n";
        let offset = content.find("old").unwrap();
        let (old, new, start_line) = contextual_patch(content, offset, "old", "new");

        assert_eq!(
            old,
            "header\nkeep 1\nkeep 2\nlet value = old;\nafter 1\nafter 2\nafter 3\n"
        );
        assert_eq!(
            new,
            "header\nkeep 1\nkeep 2\nlet value = new;\nafter 1\nafter 2\nafter 3\n"
        );
        assert_eq!(start_line, 2);
    }

    #[test]
    fn contextual_patch_preserves_crlf_and_trailing_newline_match() {
        let content = "before\r\nold\r\nafter 1\r\nafter 2\r\nafter 3\r\nafter 4\r\n";
        let offset = content.find("old\r\n").unwrap();
        let (old, new, start_line) = contextual_patch(content, offset, "old\r\n", "new\r\n");

        assert_eq!(old, "before\r\nold\r\nafter 1\r\nafter 2\r\nafter 3\r\n");
        assert_eq!(new, "before\r\nnew\r\nafter 1\r\nafter 2\r\nafter 3\r\n");
        assert_eq!(start_line, 1);
    }

    #[test]
    fn apply_unique_edit_errors_on_ambiguous_match() {
        let content = "dup\ndup\nother".to_string();
        let err = apply_unique_edit(&content, "dup", "x", "f.txt").unwrap_err();
        assert!(
            err.contains("2 places"),
            "ambiguous match must report count: {err}"
        );
    }

    #[test]
    fn apply_unique_edit_returns_none_when_absent() {
        let content = "hello world".to_string();
        assert!(
            apply_unique_edit(&content, "goodbye", "x", "f.txt")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn apply_unique_edit_replaces_between_occurrences() {
        // The replacement must not touch the second occurrence (regression for
        // the old `str::replace`-replaces-all behaviour).
        let content = "x KEEP x".to_string();
        let edit = apply_unique_edit(&content, "x", "Y", "f.txt").unwrap_err(); // ambiguous (2 occurrences) -> error
        assert!(edit.contains("2 places"));
    }

    #[test]
    fn diagnose_edit_failure_detects_whitespace_mismatch() {
        let content = "fn foo() {\n    let x = 1;  \n}\n";
        let old = "    let x = 1;\n";
        let diag = diagnose_edit_failure(content, old, "test.rs");
        assert!(
            diag.contains("when ignoring trailing whitespace"),
            "diagnostic: {diag}"
        );
        assert!(diag.contains("line 2"));
    }

    #[test]
    fn diagnose_edit_failure_detects_divergence() {
        let content = "fn foo() {\n    let x = 1;\n    let y = 200;\n    let z = 3;\n}\n";
        let old = "    let x = 1;\n    let y = 2;\n    let z = 3;";
        let diag = diagnose_edit_failure(content, old, "test.rs");
        assert!(diag.contains("diverged at line 3"), "diagnostic: {diag}");
        assert!(diag.contains("let y = 200"));
    }

    #[test]
    fn diagnose_edit_failure_reports_zero_matches() {
        let content = "fn foo() {}\n";
        let old = "fn bar() {}";
        let diag = diagnose_edit_failure(content, old, "test.rs");
        assert!(diag.contains("0 matches found"), "diagnostic: {diag}");
    }
}
