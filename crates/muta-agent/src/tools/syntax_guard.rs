//! AST and structural syntax validation defense guard for file modifications.

use std::path::Path;

/// Result of a pre/post-edit syntax check.
#[derive(Debug, PartialEq, Eq)]
pub enum SyntaxCheckResult {
    /// Content syntax is valid or the file type does not require AST checking.
    Valid,
    /// Content syntax is invalid with a diagnostic message and error detail.
    Invalid(String),
}

/// Verify syntactic integrity of `content` based on file extension.
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
        "rs" => {
            if let Err(e) = verify_rust_delimiter_balance(content) {
                return SyntaxCheckResult::Invalid(format!("Rust syntax error: {e}"));
            }
        }
        "js" | "ts" | "jsx" | "tsx" | "py" => {
            if let Err(e) = verify_generic_delimiter_balance(content) {
                return SyntaxCheckResult::Invalid(format!("Unbalanced syntax delimiters: {e}"));
            }
        }
        _ => {}
    }

    SyntaxCheckResult::Valid
}

/// Fast verification of balanced braces, brackets, and parentheses in Rust source.
fn verify_rust_delimiter_balance(source: &str) -> Result<(), String> {
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut in_char = false;
    let mut in_line_comment = false;
    let mut in_block_comment = 0;
    let mut escape = false;

    let chars: Vec<char> = source.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];
        let next_ch = if i + 1 < len {
            Some(chars[i + 1])
        } else {
            None
        };

        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }

        if in_block_comment > 0 {
            if ch == '/' && next_ch == Some('*') {
                in_block_comment += 1;
                i += 2;
                continue;
            } else if ch == '*' && next_ch == Some('/') {
                in_block_comment -= 1;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if in_char {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '\'' {
                in_char = false;
            }
            i += 1;
            continue;
        }

        // Comment check
        if ch == '/' && next_ch == Some('/') {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if ch == '/' && next_ch == Some('*') {
            in_block_comment += 1;
            i += 2;
            continue;
        }

        // String / char literals
        if ch == '"' {
            in_string = true;
            i += 1;
            continue;
        }
        if ch == '\'' && next_ch.is_some() && chars.get(i + 2) == Some(&'\'') {
            in_char = true;
            i += 1;
            continue;
        }

        // Delimiters
        match ch {
            '{' | '(' | '[' => stack.push(ch),
            '}' if stack.pop() != Some('{') => {
                return Err("Unmatched closing brace '}'".to_string());
            }
            ')' if stack.pop() != Some('(') => {
                return Err("Unmatched closing parenthesis ')'".to_string());
            }
            ']' if stack.pop() != Some('[') => {
                return Err("Unmatched closing bracket ']'".to_string());
            }
            _ => {}
        }

        i += 1;
    }

    if let Some(unclosed) = stack.pop() {
        return Err(format!("Unclosed delimiter '{unclosed}'"));
    }

    Ok(())
}

/// Generic delimiter check for JS/TS/Python.
fn verify_generic_delimiter_balance(source: &str) -> Result<(), String> {
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut string_quote = '"';
    let mut escape = false;

    for ch in source.chars() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == string_quote {
                in_string = false;
            }
            continue;
        }

        if ch == '"' || ch == '\'' || ch == '`' {
            in_string = true;
            string_quote = ch;
            continue;
        }

        match ch {
            '{' | '(' | '[' => stack.push(ch),
            '}' if stack.pop() != Some('{') => {
                return Err("Unmatched '}'".to_string());
            }
            ')' if stack.pop() != Some('(') => {
                return Err("Unmatched ')'".to_string());
            }
            ']' if stack.pop() != Some('[') => {
                return Err("Unmatched ']'".to_string());
            }
            _ => {}
        }
    }

    if let Some(unclosed) = stack.pop() {
        return Err(format!("Unclosed delimiter '{unclosed}'"));
    }

    Ok(())
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
    fn rust_delimiter_validation() {
        let p = Path::new("src/main.rs");
        let valid_rs = r#"
            fn main() {
                println!("Hello world (test)");
                let arr = [1, 2, 3];
            }
        "#;
        assert_eq!(verify_syntax(p, valid_rs), SyntaxCheckResult::Valid);

        let broken_rs = r#"
            fn main() {
                println!("Hello world");
            // missing closing brace
        "#;
        assert!(matches!(
            verify_syntax(p, broken_rs),
            SyntaxCheckResult::Invalid(_)
        ));
    }
}
