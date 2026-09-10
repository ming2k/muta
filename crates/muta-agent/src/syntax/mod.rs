//! AST and Code Intelligence utilities powered by Tree-sitter (ADR-0211).
//!
//! Provides:
//! - 1ms incremental syntax verification (`verify_ast_syntax`)
//! - Structural symbol extraction for Repo Map generation (`generate_repo_map`)
//! - AST Symbol delta detection for dirty files (`extract_ast_deltas`)

use std::path::Path;
use tree_sitter::{Node, Parser};

/// Supported language for Tree-sitter parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportedLanguage {
    Rust,
    TypeScript,
    Tsx,
    Python,
    C,
    Cpp,
    Go,
}

impl SupportedLanguage {
    /// Detect supported language from a file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "rs" => Some(Self::Rust),
            "ts" | "js" | "mjs" | "cjs" => Some(Self::TypeScript),
            "tsx" | "jsx" => Some(Self::Tsx),
            "py" => Some(Self::Python),
            "c" | "h" => Some(Self::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some(Self::Cpp),
            "go" => Some(Self::Go),
            _ => None,
        }
    }

    /// Resolve the underlying Tree-sitter `Language`.
    pub fn tree_sitter_language(self) -> tree_sitter::Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }
}

/// Verify syntactic correctness of `content` using Tree-sitter.
///
/// Returns `Ok(())` if the syntax is valid or if the language is unsupported.
/// Returns `Err(message)` with line and column diagnostics if a syntax error is detected.
pub fn verify_ast_syntax(ext: &str, content: &str) -> Result<(), String> {
    let Some(lang) = SupportedLanguage::from_extension(ext) else {
        return Ok(());
    };

    let mut parser = Parser::new();
    if let Err(err) = parser.set_language(&lang.tree_sitter_language()) {
        return Err(format!("Tree-sitter failed to set language: {err}"));
    }

    let Some(tree) = parser.parse(content, None) else {
        return Err("Tree-sitter parser returned empty tree".to_string());
    };

    let root = tree.root_node();
    if root.has_error() {
        if let Some(err_node) = find_first_error(root) {
            let start = err_node.start_position();
            let row = start.row + 1;
            let col = start.column + 1;
            let snippet = node_snippet(err_node, content);
            return Err(format!(
                "Syntax error at line {row}, column {col} near '{snippet}': broken or unclosed syntax"
            ));
        }
        return Err("Syntax error detected: tree contains invalid syntax nodes".to_string());
    }

    Ok(())
}

/// Recursively find the first node with an error flag.
fn find_first_error(node: Node) -> Option<Node> {
    if node.is_error() || node.is_missing() {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.has_error() {
            return find_first_error(child);
        }
    }
    None
}

/// Short preview of the error node's text.
fn node_snippet<'a>(node: Node, source: &'a str) -> &'a str {
    let range = node.byte_range();
    if range.start < source.len() {
        let end = range.end.min(source.len());
        let slice = &source[range.start..end];
        let first_line = slice.lines().next().unwrap_or("").trim();
        if first_line.len() > 30 {
            &first_line[..30]
        } else {
            first_line
        }
    } else {
        ""
    }
}

/// Extract top-level symbol outline from source content.
pub fn extract_symbols(ext: &str, content: &str) -> Vec<String> {
    let Some(lang) = SupportedLanguage::from_extension(ext) else {
        return Vec::new();
    };

    let mut parser = Parser::new();
    if parser.set_language(&lang.tree_sitter_language()).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let root = tree.root_node();
    let mut symbols = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if let Some(sig) = format_symbol_signature(lang, child, content) {
            symbols.push(sig);
        }
    }

    symbols
}

/// Format a concise signature line for a top-level AST node.
fn format_symbol_signature(lang: SupportedLanguage, node: Node, source: &str) -> Option<String> {
    let kind = node.kind();
    match lang {
        SupportedLanguage::Rust => match kind {
            "function_item" | "struct_item" | "enum_item" | "trait_item" | "type_item" => {
                let text = node_text(node, source);
                let first_line = text.lines().next().unwrap_or("").trim();
                let clean = first_line.trim_end_matches('{').trim();
                Some(format!("  {clean}"))
            }
            "impl_item" => {
                let text = node_text(node, source);
                let first_line = text.lines().next().unwrap_or("").trim();
                let clean = first_line.trim_end_matches('{').trim();
                Some(format!("  {clean}"))
            }
            _ => None,
        },
        SupportedLanguage::TypeScript | SupportedLanguage::Tsx => match kind {
            "export_statement" => {
                let text = node_text(node, source);
                let first_line = text.lines().next().unwrap_or("").trim();
                let clean = first_line.trim_end_matches('{').trim();
                Some(format!("  {clean}"))
            }
            "function_declaration" | "class_declaration" | "interface_declaration"
            | "type_alias_declaration" => {
                let text = node_text(node, source);
                let first_line = text.lines().next().unwrap_or("").trim();
                let clean = first_line.trim_end_matches('{').trim();
                Some(format!("  {clean}"))
            }
            _ => None,
        },
        SupportedLanguage::Python => match kind {
            "class_definition" | "function_definition" => {
                let text = node_text(node, source);
                let first_line = text.lines().next().unwrap_or("").trim();
                let clean = first_line.trim_end_matches(':').trim();
                Some(format!("  {clean}"))
            }
            _ => None,
        },
        SupportedLanguage::C | SupportedLanguage::Cpp => match kind {
            "function_definition" | "declaration" | "class_specifier" | "struct_specifier" => {
                let text = node_text(node, source);
                let first_line = text.lines().next().unwrap_or("").trim();
                let clean = first_line.trim_end_matches('{').trim();
                if !clean.is_empty() {
                    Some(format!("  {clean}"))
                } else {
                    None
                }
            }
            _ => None,
        },
        SupportedLanguage::Go => match kind {
            "function_declaration" | "method_declaration" | "type_declaration" => {
                let text = node_text(node, source);
                let first_line = text.lines().next().unwrap_or("").trim();
                let clean = first_line.trim_end_matches('{').trim();
                Some(format!("  {clean}"))
            }
            _ => None,
        },
    }
}

fn node_text<'a>(node: Node, source: &'a str) -> &'a str {
    let range = node.byte_range();
    if range.end <= source.len() {
        &source[range.start..range.end]
    } else {
        ""
    }
}

/// Generate a compact repository outline within the given token budget (ADR-0211).
///
/// Estimated at ~4 characters per token. Default budget is 1024 tokens (~4000 chars).
pub fn generate_repo_map(workspace_root: &Path, token_budget: usize) -> Option<String> {
    let char_budget = token_budget.saturating_mul(4);
    let mut accumulated = String::new();
    let mut file_count = 0;

    let walker = ignore::WalkBuilder::new(workspace_root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .max_depth(Some(4))
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if SupportedLanguage::from_extension(&ext).is_none() {
            continue;
        }

        // Exclude test directories or vendor folders
        let rel_path = path
            .strip_prefix(workspace_root)
            .unwrap_or(path)
            .to_string_lossy();

        if rel_path.starts_with("target")
            || rel_path.starts_with("node_modules")
            || rel_path.starts_with(".git")
            || rel_path.contains("tests/")
        {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };

        let symbols = extract_symbols(&ext, &content);
        if symbols.is_empty() {
            continue;
        }

        let mut block = format!("{rel_path}:\n");
        for sym in symbols.iter().take(8) {
            block.push_str(sym);
            block.push('\n');
        }

        if accumulated.len() + block.len() > char_budget {
            break;
        }

        accumulated.push_str(&block);
        file_count += 1;
        if file_count >= 20 {
            break;
        }
    }

    if accumulated.is_empty() {
        None
    } else {
        Some(accumulated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_rust_syntax_passes() {
        let code = r#"
            pub fn add(a: i32, b: i32) -> i32 {
                a + b
            }
        "#;
        assert_eq!(verify_ast_syntax("rs", code), Ok(()));
    }

    #[test]
    fn broken_rust_syntax_is_rejected() {
        let code = "fn broken( { let x = 1; ";
        assert!(verify_ast_syntax("rs", code).is_err());
    }

    #[test]
    fn valid_python_syntax_passes() {
        let code = "def hello():\n    return 42\n";
        assert_eq!(verify_ast_syntax("py", code), Ok(()));
    }

    #[test]
    fn broken_python_syntax_is_rejected() {
        let code = "def hello(\nreturn 42\n";
        assert!(verify_ast_syntax("py", code).is_err());
    }

    #[test]
    fn symbol_extraction_extracts_rust_items() {
        let code = r#"
            pub struct User { id: u64 }
            pub trait Greeter { fn greet(&self); }
            pub fn run() {}
        "#;
        let symbols = extract_symbols("rs", code);
        assert_eq!(symbols.len(), 3);
        assert!(symbols[0].contains("pub struct User"));
        assert!(symbols[1].contains("pub trait Greeter"));
        assert!(symbols[2].contains("pub fn run()"));
    }

    #[test]
    fn valid_c_cpp_syntax_passes() {
        let c_code = "int main(void) { return 0; }";
        assert_eq!(verify_ast_syntax("c", c_code), Ok(()));

        let cpp_code = "class Engine { public: void start(); };";
        assert_eq!(verify_ast_syntax("cpp", cpp_code), Ok(()));
    }

    #[test]
    fn broken_c_cpp_syntax_is_rejected() {
        let broken_c = "int main(void { return 0;";
        assert!(verify_ast_syntax("c", broken_c).is_err());
    }

    #[test]
    fn valid_go_syntax_passes() {
        let go_code = "package main\nfunc main() {}\n";
        assert_eq!(verify_ast_syntax("go", go_code), Ok(()));
    }

    #[test]
    fn broken_go_syntax_is_rejected() {
        let broken_go = "package main\nfunc main( {}\n";
        assert!(verify_ast_syntax("go", broken_go).is_err());
    }
}
