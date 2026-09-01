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
