//! Structural syntax validation defense guard for file modifications.
//!
//! Only formats with authoritative, non-heuristic parsers (e.g. JSON, TOML)
//! are verified in this whitelist guard. General programming language source files
//! (Rust, TS, JS, Python, etc.) are intentionally omitted to avoid heuristic
//! false-positives and to allow natural compiler/linter error feedback.

use std::path::Path;

/// Result of a pre/post-edit syntax check.
#[derive(Debug, PartialEq, Eq)]
pub enum SyntaxCheckResult {
    /// Content syntax is valid or the file format is not in the strict syntax validation whitelist.
    Valid,
    /// Content syntax is invalid with a diagnostic message and error detail.
    Invalid(String),
}

/// Verify syntactic integrity of `content` based on file extension.
///
/// Only whitelist-supported config/data formats with exact parsers are checked.
pub fn verify_syntax(path: &Path, content: &str) -> SyntaxCheckResult {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();

    match extension.as_str() {
        "json" => {
            if let Err(e) = serde_json::from_str::<serde_json::Value>(content) {
                return SyntaxCheckResult::Invalid(format!("Malformed JSON: {e}"));
            }
        }
        "toml" => {
            if let Err(e) = toml::from_str::<toml::Value>(content) {
                return SyntaxCheckResult::Invalid(format!("Malformed TOML: {e}"));
            }
        }
        _ => {}
    }

    SyntaxCheckResult::Valid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_syntax_validation() {
        let p = Path::new("config.json");
        assert_eq!(
            verify_syntax(p, r#"{"key": "value", "count": 42}"#),
            SyntaxCheckResult::Valid
        );
        assert!(matches!(
            verify_syntax(p, r#"{"key": "value", "count": }"#),
            SyntaxCheckResult::Invalid(_)
        ));
    }

    #[test]
    fn toml_syntax_validation() {
        let p = Path::new("Cargo.toml");
        assert_eq!(
            verify_syntax(p, "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n"),
            SyntaxCheckResult::Valid
        );
        assert!(matches!(
            verify_syntax(p, "[package\nname = \"pkg\""),
            SyntaxCheckResult::Invalid(_)
        ));
    }

    #[test]
    fn non_whitelisted_source_files_pass_through() {
        // Source files like Rust, TypeScript, Python are intentionally not blocked
        // by heuristic delimiter checks.
        let rs_path = Path::new("src/main.rs");
        assert_eq!(
            verify_syntax(rs_path, "fn main() { broken unclosed"),
            SyntaxCheckResult::Valid
        );

        let py_path = Path::new("script.py");
        assert_eq!(
            verify_syntax(py_path, "def foo():\n    return (unclosed"),
            SyntaxCheckResult::Valid
        );
    }
}
