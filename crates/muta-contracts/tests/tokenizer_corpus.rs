//! Cross-validation of the native BPE tokenizer against reference counts
//! generated offline by a pure-python reimplementation of tiktoken's
//! cl100k_base (pretokenize + `byte_pair_merge`), itself checked against
//! published tiktoken outputs. Regenerate via the recipe in
//! `vendor/README.md`.
//!
//! File-derived cases assert counts over each file's first 40 KB with a 2%
//! tolerance: any edit to those files shifts the true count, and the point is
//! detecting algorithmic drift, not freezing the corpus.

use muta_contracts::tokenizer::Tokenizer;

#[test]
fn inline_corpus_matches_reference_exactly() {
    // (name, text, expected cl100k_base token count)
    let cases: &[(&str, &str, usize)] = &[
        (
            "zh_short",
            "针对当前的token预测，我们应该改变目前这种粗暴的方案。",
            27,
        ),
        (
            "zh_long",
            "上下文压力是根据消息列表估算的 token 数与模型上下文窗口的比较结果，用于触发修剪和压缩。上下文压力是根据消息列表估算的 token 数与模型上下文窗口的比较结果，用于触发修剪和压缩。上下文压力是根据消息列表估算的 token 数与模型上下文窗口的比较结果，用于触发修剪和压缩。上下文压力是根据消息列表估算的 token 数与模型上下文窗口的比较结果，用于触发修剪和压缩。",
            188,
        ),
        (
            "mixed",
            "在 `crates/muta-contracts/src/pressure.rs` 中，`count_tokens` 对每个 Unicode 字符分类并加权（CJK ≈ 1.0 token/char，ASCII ≈ 0.25）。",
            53,
        ),
        (
            "code_snip",
            "fn main() { let x: Vec<u32> = (0..100).map(|i| i * i).collect(); println!(\"{x:?}\"); }",
            35,
        ),
        (
            "english",
            "The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. ",
            201,
        ),
        (
            "json_tool",
            "{\"path\":\"/home/user/project/src/main.rs\",\"line\":42,\"content\":\"fn main() {}\"}",
            26,
        ),
        (
            "punct",
            "!!!???...,,,;;;:::'''\"\"\"(){}[]<>/*-+=&^%$#@!~`|\\",
            25,
        ),
        (
            "digits",
            "3.14159265358979323846264338327950288419716939937510582097494459",
            29,
        ),
        ("emoji", "🦀🚀✨🔥💡👍❤️😂😀😃😄😅😊🙂🎉🎯⚡🌟💥🌈", 50),
        ("empty", "", 0),
        ("whitespace", "   \n\t  \n\n   \t\t", 4),
        (
            "korean",
            "안녕하세요, 반갑습니다. 컨텍스트 압력을 계산합니다.",
            24,
        ),
        (
            "japanese",
            "こんにちは、世界。トークンを数えます。ひらがなとカタカナと漢字。",
            30,
        ),
        ("russian", "Привет, мир! Как дела? Считаем токены.", 19),
        ("accents", "éàèùçâêîôûëïüöäßñåøæ", 19),
        ("arrows", "→←↑↓⇒⇐⟶⟵⇔↔↕⇕➜➡⬅⬆⬇", 37),
        ("fullwidth", "ＡＢＣ１２３ａｂｃ！？：；（）【】《》", 25),
        (
            "url",
            "https://openaipublic.blob.core.windows.net/encodings/cl100k_base.tiktoken",
            27,
        ),
        (
            "xml",
            "<tool_call><name>bash</name><arguments>{\"command\":\"ls -la\"}</arguments></tool_call>",
            26,
        ),
    ];
    let t = Tokenizer::new();
    for (name, text, expected) in cases {
        let got = t.count(text);
        assert_eq!(&got, expected, "case {name}");
    }
}

#[test]
fn file_corpus_stays_within_two_percent() {
    // (path relative to this crate, reference count of the first 40 KB)
    let cases: &[(&str, usize)] = &[
        ("../../crates/muta-agent/src/agent/mod.rs", 10_478),
        ("../../crates/muta-contracts/src/pressure.rs", 10_855),
        ("../../crates/muta-agent/src/orchestration.rs", 10_411),
        ("../../README.zh-CN.md", 1_098),
        ("../../CHANGELOG.md", 10_516),
        ("../../docs/adr/0044-layered-token-accounting.md", 1_742),
        (
            "../../docs/explanation/agent-design/token-accounting.md",
            5_553,
        ),
    ];
    let t = Tokenizer::new();
    for (path, expected) in cases {
        // A missing corpus file must FAIL (see the in-crate twin of this
        // test): a silently-skipped corpus is a test that asserts nothing.
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("corpus file {path} unreadable: {e}"));
        // Pin the corpus to the LF form used to obtain the reference counts;
        // a Windows checkout's CRLF materialization is not tokenizer drift.
        let text = text.replace("\r\n", "\n");
        let text = &text[..text.len().min(40_000)];
        let got = t.count(text);
        let drift = (got as f64 - *expected as f64).abs() / *expected as f64;
        assert!(drift < 0.02, "{path}: {got} vs {expected} ({drift:.3})");
    }
}
