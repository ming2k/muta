use super::base::truncate_to_width;
use super::payloads::tool_summary_line;
use mutx_engine::Color;

#[test]
fn truncate_to_width_stops_at_newline() {
    assert_eq!(
        truncate_to_width("Run python3 -c\ns=open(...)", 50),
        "Run python3 -c…"
    );
    assert_eq!(truncate_to_width("abc\r\ndef", 50), "abc…");
    assert_eq!(truncate_to_width("single line", 50), "single line");
    assert_eq!(
        truncate_to_width("very long single line that exceeds width", 10),
        "very long…"
    );
}

#[test]
fn tool_summary_line_produces_single_row_span_without_newline() {
    let line = tool_summary_line(
        "+",
        "Run python3 -c\ns=open(...)",
        Color::White,
        Color::Black,
        40,
    );
    for span in &line.spans {
        assert!(
            !span.content.contains('\n'),
            "span must never contain newline"
        );
        assert!(
            !span.content.contains('\r'),
            "span must never contain carriage return"
        );
    }
}

#[test]
fn project_frags_to_wrapped_preserves_highlights_across_soft_wraps() {
    use super::payloads::project_frags_to_wrapped;
    use crate::tools::DiffFrag;

    // Line: "let x = very_long_changed_identifier_value;"
    // frags:
    // 0: "let x = " (changed: false)
    // 1: "very_long_changed_identifier_value" (changed: true)
    // 2: ";\n" (changed: false)
    let frags = vec![
        DiffFrag {
            text: "let x = ".to_string(),
            changed: false,
        },
        DiffFrag {
            text: "very_long_changed_identifier_value".to_string(),
            changed: true,
        },
        DiffFrag {
            text: ";\n".to_string(),
            changed: false,
        },
    ];
    let full = "let x = very_long_changed_identifier_value;\n";

    // Simulate wrapped into two lines:
    // Row 1: "let x = very_long_" (bytes 0..18)
    // Row 2: "changed_identifier_value;\n" (bytes 18..44)
    let row1 = project_frags_to_wrapped(full, &frags, 0, 18);
    assert_eq!(
        row1,
        vec![
            ("let x = ", false),
            ("very_long_", true),
        ]
    );

    let row2 = project_frags_to_wrapped(full, &frags, 18, 44);
    assert_eq!(
        row2,
        vec![
            ("changed_identifier_value", true),
            (";", false),
        ]
    );
}

#[test]
fn project_syntax_diff_frags_merges_syntax_and_diff_layers() {
    use super::payloads::project_syntax_diff_frags;
    use crate::syntax::{Language, SyntaxKind, tokenize_line};
    use crate::tools::DiffFrag;

    // Line: "let count = 10;" -> modified "10" to "20"
    let full = "let count = 20;\n";
    let syntax = tokenize_line(full, Language::Rust);
    let frags = vec![
        DiffFrag {
            text: "let count = ".to_string(),
            changed: false,
        },
        DiffFrag {
            text: "20".to_string(),
            changed: true,
        },
        DiffFrag {
            text: ";\n".to_string(),
            changed: false,
        },
    ];

    let slices = project_syntax_diff_frags(full, &frags, &syntax, 0, full.len());
    
    // "let" should be Keyword, not changed
    let let_slice = slices.iter().find(|s| s.text == "let").unwrap();
    assert_eq!(let_slice.kind, SyntaxKind::Keyword);
    assert!(!let_slice.changed);

    // "20" should be Number, AND changed = true (so it gets highlighted background!)
    let num_slice = slices.iter().find(|s| s.text == "20").unwrap();
    assert_eq!(num_slice.kind, SyntaxKind::Number);
    assert!(num_slice.changed);
}

#[test]
fn test_diff_renderer_applies_syntax_colors_to_code() {
    use crate::syntax::SyntaxKind;
    use crate::theme::Theme;

    let theme = Theme::default();
    let kw_color = theme.syntax_color(SyntaxKind::Keyword);
    let num_color = theme.syntax_color(SyntaxKind::Number);
    let str_color = theme.syntax_color(SyntaxKind::String);

    assert_eq!(kw_color, theme.brand());
    assert_eq!(num_color, theme.ok());
    assert_eq!(str_color, theme.warn());
}

#[test]
fn project_syntax_slice_splits_code_block_tokens() {
    use super::payloads::project_syntax_slice;
    use crate::syntax::{Language, SyntaxKind, tokenize_line};

    let line = "pub fn add(a: i32, b: i32) -> i32 {\n";
    let tokens = tokenize_line(line, Language::Rust);
    let slices = project_syntax_slice(line, &tokens, 0, line.len());

    let pub_tok = slices.iter().find(|(text, _)| *text == "pub").unwrap();
    assert_eq!(pub_tok.1, SyntaxKind::Keyword);

    let fn_tok = slices.iter().find(|(text, _)| *text == "fn").unwrap();
    assert_eq!(fn_tok.1, SyntaxKind::Keyword);

    let add_tok = slices.iter().find(|(text, _)| *text == "add").unwrap();
    assert_eq!(add_tok.1, SyntaxKind::Function);

    let i32_toks: Vec<_> = slices.iter().filter(|(text, _)| *text == "i32").collect();
    assert_eq!(i32_toks.len(), 3);
    for t in i32_toks {
        assert_eq!(t.1, SyntaxKind::Type);
    }
}
