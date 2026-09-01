#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use crate::tools::*;
    use muta_contracts::{Tool, WebSearchConfig, truncate_utf8};

    #[test]
    fn html_to_text_handles_multibyte_before_script_tags() {
        let html = "αβ<script>hidden</script>γδ<style>.x{}</style>εζ";

        assert_eq!(html_to_text(html), "αβγδεζ");
    }

    #[test]
    fn truncate_utf8_does_not_split_multibyte_chars() {
        let text = "prefix ’ suffix";
        let inside_curly_quote = text.find('’').unwrap() + 1;

        assert_eq!(truncate_utf8(text, inside_curly_quote), "prefix ");
    }

    #[test]
    fn websearch_config_defaults_to_exa() {
        let cfg = WebSearchConfig::default();
        assert_eq!(cfg.provider, "exa");
        assert!(cfg.proxy.is_none());
        assert_eq!(cfg.timeout_secs, 20);
    }

    #[test]
    fn websearch_config_round_trips_through_toml() {
        let toml = r#"
            provider = "searxng"
            proxy = "socks5h://127.0.0.1:1080"
            timeout_secs = 8
            searxng_url = "http://localhost:8080/search"
        "#;
        let cfg: WebSearchConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.provider, "searxng");
        assert_eq!(cfg.proxy.as_deref(), Some("socks5h://127.0.0.1:1080"));
        assert_eq!(cfg.timeout_secs, 8);
        assert_eq!(
            cfg.searxng_url.as_deref(),
            Some("http://localhost:8080/search")
        );
    }

    #[test]
    fn bocha_backend_parses_from_toml_and_builds() {
        let toml = r#"
            provider = "bocha"
            bocha_api_key = "sk-test-bocha"
        "#;
        let cfg: WebSearchConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.provider, "bocha");
        assert_eq!(
            cfg.bocha_api_key
                .as_ref()
                .map(|k| k.expose_secret().to_string())
                .as_deref(),
            Some("sk-test-bocha")
        );
        assert_eq!(
            crate::tools::search::build_provider(&cfg, "bocha").name(),
            "Bocha"
        );
    }

    #[test]
    fn reader_field_parses_and_defaults_to_none() {
        let cfg = WebSearchConfig::default();
        assert_eq!(cfg.reader, "none");
        assert!(cfg.jina_api_key.is_none());

        let toml = r#"
            reader = "jina"
            jina_api_key = "jina-test-key"
        "#;
        let cfg: WebSearchConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.reader, "jina");
        assert_eq!(
            cfg.jina_api_key
                .as_ref()
                .map(|k| k.expose_secret().to_string())
                .as_deref(),
            Some("jina-test-key"),
            "deserialization still accepts the inline spelling (legacy files)"
        );
        // Secrets never serialize into config.toml (behavior-only contract);
        // they persist in credentials.toml instead. A config round-trip
        // therefore drops them by design — see the persistence crate's
        // websearch-keys migration tests for the full path.
        let reencoded = toml::to_string(&cfg).unwrap();
        assert!(
            !reencoded.contains("jina-test-key"),
            "secret leaked through config serialization: {reencoded}"
        );
        let reloaded: WebSearchConfig = toml::from_str(&reencoded).unwrap();
        assert_eq!(reloaded.reader, "jina");
        assert!(reloaded.jina_api_key.is_none());
    }

    #[test]
    fn write_and_edit_tools_allow_plan_paths_in_plan_mode() {
        // Plan-mode path exemption was removed (ADR-0027/0028): scoped writes
        // are now expressed per-agent via `WriteScope`, not via an
        // `allowed_in_plan_mode` override on the write tools. This test is
        // kept as a placeholder guard that the write tools still build; the
        // scoping behavior is covered by muta-contracts's WriteScope tests.
        let _write = WriteFileTool::new(None);
        let _edit = EditFileTool::new(None);
    }

    #[tokio::test]
    async fn read_text_carries_offset_as_start_line() {
        // The structured `Code::start_line` is the contract the renderer relies
        // on to number an offset snippet from its true file line. A read with
        // `offset: 3` must surface `start_line: 3` (and only the post-offset
        // content), while a plain read reports `start_line: 1`.
        let dir =
            std::env::temp_dir().join(format!("muta-read-start-line-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lines.txt");
        std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").unwrap();

        let tool = ReadTextTool::new(None);

        let full_arguments = serde_json::json!({ "path": &path }).to_string();
        let full = tool.call_structured(&full_arguments).await.unwrap();
        match full {
            muta_contracts::ToolOutput::Code {
                start_line, text, ..
            } => {
                assert_eq!(start_line, 1);
                assert!(text.starts_with("one"));
            }
            _ => panic!("expected Code"),
        }

        let offset_arguments = serde_json::json!({ "path": &path, "offset": 3 }).to_string();
        let offset = tool.call_structured(&offset_arguments).await.unwrap();
        match offset {
            muta_contracts::ToolOutput::Code {
                start_line, text, ..
            } => {
                assert_eq!(start_line, 3);
                assert_eq!(text, "three\nfour\nfive");
            }
            _ => panic!("expected Code"),
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Pull `(text, prefix, suffix)` out of a `Code` output for assertions.
    fn code_parts(out: muta_contracts::ToolOutput) -> (String, Option<String>, Option<String>) {
        match out {
            muta_contracts::ToolOutput::Code {
                text,
                prefix,
                suffix,
                ..
            } => (text, prefix, suffix),
            _ => panic!("expected Code output"),
        }
    }

    /// A file whose every line is exactly `line_width` chars so the byte-budget
    /// math is predictable in the pagination tests below.
    fn make_fixed_width_file(line_count: usize) -> (std::path::PathBuf, Vec<String>) {
        let dir = std::env::temp_dir().join(format!("muta-read-paginate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.txt");
        let lines: Vec<String> = (1..=line_count).map(|n| format!("line{n:05}")).collect();
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
        (path, lines)
    }

    #[tokio::test]
    async fn plain_small_read_has_no_framing() {
        // The common case stays byte-identical to the legacy model output:
        // no prefix/suffix, so we don't tax every small read.
        let dir = std::env::temp_dir().join(format!("muta-read-plain-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("small.txt");
        std::fs::write(&path, "a\nb\nc\n").unwrap();

        let arguments = serde_json::json!({ "path": &path }).to_string();
        let out = ReadTextTool::new(None)
            .call_structured(&arguments)
            .await
            .unwrap();
        let (text, prefix, suffix) = code_parts(out);
        assert_eq!(text, "a\nb\nc");
        assert!(prefix.is_none());
        assert!(suffix.is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn large_read_paginates_with_concrete_non_overlapping_continuation() {
        // 6000 lines × 10 bytes ("lineNNNNN\n") = 60KB. The 50 000-byte budget
        // holds ~5000 lines per page. The tool MUST return whole lines, declare
        // the range, and give an exact next offset — and following that offset
        // must continue without overlap or gap (the loop-safety contract).
        const LINES: usize = 6000;
        const PAGE: usize = 5000; // 50_000 / (9 + 1)
        let (path, _lines) = make_fixed_width_file(LINES);
        let tool = ReadTextTool::new(None);
        let arg =
            |offset: usize| serde_json::json!({ "path": &path, "offset": offset }).to_string();

        // Page 1: lines 1..=5000, continuation offset = 5001.
        let (text1, pre1, suf1) = code_parts(tool.call_structured(&arg(1)).await.unwrap());
        assert_eq!(
            pre1,
            Some(format!(
                "[{}: lines 1-{} of {}]",
                path.to_string_lossy(),
                PAGE,
                LINES
            ))
        );
        let suf1 = suf1.expect("page 1 has a continuation suffix");
        assert!(
            suf1.contains("offset=5001"),
            "suffix must name the exact next offset, got: {suf1}"
        );
        assert_eq!(text1.lines().count(), PAGE);
        assert_eq!(text1.lines().next().unwrap(), "line00001");
        assert_eq!(text1.lines().last().unwrap(), &format!("line{:05}", PAGE));

        // Page 2 from the advertised offset: must start exactly at 5001 (no gap)
        // and not repeat line 5000 (no overlap) — this is what breaks the loop.
        let (text2, _pre2, suf2) = code_parts(tool.call_structured(&arg(5001)).await.unwrap());
        assert_eq!(text2.lines().next().unwrap(), "line05001", "no gap");
        assert!(
            !text2.lines().any(|l| l == "line05000"),
            "no overlap with previous page"
        );
        // Page 2 is the final page (1000 lines remaining).
        assert!(suf2.is_none(), "page 2 reaches EOF, no suffix");

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn oversized_limit_is_line_bounded_not_re_truncated() {
        // Regression for the real infinite-loop trap: requesting a huge `limit`
        // on a big file used to keep the slice over budget, re-truncate the
        // same window, and emit a generic "use offset/limit" with no number.
        // Now the window is line-bounded and the continuation is concrete, so
        // the model advances instead of looping.
        const LINES: usize = 6000;
        let (path, _lines) = make_fixed_width_file(LINES);
        let arg = serde_json::json!({ "path": &path, "limit": LINES }).to_string();
        let (text, _pre, suf) =
            code_parts(ReadTextTool::new(None).call_structured(&arg).await.unwrap());
        // Far fewer than the requested 6000 lines — bounded by the budget.
        assert!(text.lines().count() < LINES);
        assert!(
            suf.expect("oversized limit still paginates")
                .contains("offset="),
            "gives a concrete next offset rather than a generic hint"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn empty_and_past_eof_reads_explain_themselves() {
        // Both cases used to return a bare empty string, which a model can
        // mistake for a failure and re-read in a loop. They now carry an
        // explicit note via the model-facing prefix.
        let dir = std::env::temp_dir().join(format!("muta-read-edge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let empty = dir.join("empty.txt");
        std::fs::write(&empty, "").unwrap();
        let empty_arguments = serde_json::json!({ "path": &empty }).to_string();
        let (text, pre, suf) = code_parts(
            ReadTextTool::new(None)
                .call_structured(&empty_arguments)
                .await
                .unwrap(),
        );
        assert!(text.is_empty());
        assert!(
            pre.as_ref().is_some_and(|p| p.contains("empty file")),
            "pre={pre:?}"
        );
        assert!(suf.is_none());

        let small = dir.join("small.txt");
        std::fs::write(&small, "a\nb\n").unwrap();
        let past_eof_arguments = serde_json::json!({ "path": &small, "offset": 99 }).to_string();
        let (text, pre, suf) = code_parts(
            ReadTextTool::new(None)
                .call_structured(&past_eof_arguments)
                .await
                .unwrap(),
        );
        assert!(text.is_empty());
        assert!(
            pre.as_ref().is_some_and(|p| p.contains("past end of file")),
            "pre={pre:?}"
        );
        assert!(suf.is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn reading_a_directory_suggests_list_dir() {
        // A directory read used to surface the raw OS error ("Is a directory
        // (os error 21)"), which gives the model no hint about what to do.
        // Now it gets an explicit, actionable message naming `list_dir`, which
        // breaks any retry loop.
        let dir = std::env::temp_dir().join(format!("muta-read-isdir-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let arguments = serde_json::json!({ "path": &dir }).to_string();
        let err = ReadTextTool::new(None).call(&arguments).await.unwrap_err();
        assert!(
            err.contains("list_dir"),
            "should point to list_dir, got: {err}"
        );
        assert!(
            !err.contains("os error"),
            "should not leak the raw OS error, got: {err}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn cross_tool_additional_roots_consistency() {
        let tmp = tempfile::tempdir().unwrap();
        let primary_root = tmp.path().join("workspace");
        let sibling_root = tmp.path().join("optics");
        // Not under the tempdir: temp dirs are implicitly admitted, so the
        // unadmitted denial fixture must live outside every admitted root.
        let unadmitted_root =
            crate::execution::workspace_tests_outside_scratch("denied-cross-tool");
        let unadmitted_secret = unadmitted_root.join("secret.txt");

        std::fs::create_dir_all(primary_root.join("src")).unwrap();
        std::fs::create_dir_all(sibling_root.join("src")).unwrap();

        std::fs::write(
            primary_root.join("src/main.rs"),
            "fn main() { println!(\"workspace\"); }",
        )
        .unwrap();
        std::fs::write(sibling_root.join("src/lib.rs"), "pub fn optics() {}").unwrap();
        std::fs::write(&unadmitted_secret, "secret").unwrap();

        let unadmitted_abs = unadmitted_secret.to_string_lossy().into_owned();
        let unadmitted_dir_abs = unadmitted_root.to_string_lossy().into_owned();

        let env = std::sync::Arc::new(
            crate::execution::WorkspaceExecutionEnvironment::with_additional_roots(
                primary_root.clone(),
                vec![sibling_root.clone()],
            ),
        );

        // 1. list_dir
        let list_tool = ListDirTool::with_env(env.clone());
        let list_sibling = list_tool
            .call(&serde_json::json!({ "path": "../optics" }).to_string())
            .await
            .unwrap();
        assert!(list_sibling.contains("src/"));
        let list_unadmitted = list_tool
            .call(&serde_json::json!({ "path": unadmitted_dir_abs }).to_string())
            .await;
        assert!(list_unadmitted.is_err());

        // 2. find_files
        let find_tool = FindFilesTool::with_env(env.clone());
        let find_sibling = find_tool
            .call(&serde_json::json!({ "path": "../optics", "patterns": ["*.rs"] }).to_string())
            .await
            .unwrap();
        assert!(find_sibling.contains("lib.rs"));
        let find_unadmitted = find_tool
            .call(&serde_json::json!({ "path": unadmitted_dir_abs, "patterns": ["*"] }).to_string())
            .await;
        assert!(find_unadmitted.is_err());

        // 3. search_text
        let search_tool = SearchTextTool::with_env(env.clone());
        let search_sibling = search_tool
            .call(&serde_json::json!({ "path": "../optics", "query": "optics" }).to_string())
            .await
            .unwrap();
        assert!(search_sibling.contains("pub fn optics"));
        let search_unadmitted = search_tool
            .call(&serde_json::json!({ "path": unadmitted_dir_abs, "query": "secret" }).to_string())
            .await;
        assert!(search_unadmitted.is_err());

        // 4. read_text
        let read_tool = ReadTextTool::with_env(env.clone());
        let read_sibling = read_tool
            .call(&serde_json::json!({ "path": "../optics/src/lib.rs" }).to_string())
            .await
            .unwrap();
        assert!(read_sibling.contains("pub fn optics"));
        let read_unadmitted = read_tool
            .call(&serde_json::json!({ "path": unadmitted_abs }).to_string())
            .await;
        assert!(read_unadmitted.is_err());

        // 5. write_file
        let write_tool = WriteFileTool::with_env(env.clone());
        let write_sibling = write_tool.call(&serde_json::json!({ "path": "../optics/src/new.rs", "content": "// new optics file" }).to_string()).await.unwrap();
        assert!(write_sibling.contains("new.rs"));
        let write_unadmitted = write_tool
            .call(
                &serde_json::json!({ "path": unadmitted_root.join("new.rs"), "content": "bad" })
                    .to_string(),
            )
            .await;
        assert!(write_unadmitted.is_err());

        // 6. edit_file
        let edit_tool = EditFileTool::with_env(env.clone());
        let edit_sibling = edit_tool.call(&serde_json::json!({ "path": "../optics/src/lib.rs", "old_string": "pub fn optics() {}", "new_string": "pub fn optics_v2() {}" }).to_string()).await.unwrap();
        assert!(edit_sibling.contains("Edited"));
        assert_eq!(
            std::fs::read_to_string(sibling_root.join("src/lib.rs")).unwrap(),
            "pub fn optics_v2() {}"
        );
        let edit_unadmitted = edit_tool.call(&serde_json::json!({ "path": unadmitted_abs, "old_string": "secret", "new_string": "hacked" }).to_string()).await;
        assert!(edit_unadmitted.is_err());

        std::fs::remove_dir_all(&unadmitted_root).ok();
    }
}
