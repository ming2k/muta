use std::sync::Arc;

use async_trait::async_trait;
use muta_contracts::{ExecutionEnvironment, Tool, ToolAccesses};
use muta_tool_derive::ToolSchema;
use serde::Deserialize;
use std::time::{Duration, Instant};

use crate::tools::file_search::{
    build_file_walker, build_include_matcher, include_allows, resolve_search_root, search_limit,
    search_path_argument,
};
use crate::tools::helpers::{WorkspaceBase, env_from_root, execution_environment};

const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
const SEARCH_MAX_BYTES: usize = 32 * 1024;
const SEARCH_MAX_MATCHES_PER_FILE: usize = 50;

/// Regex or literal search over file contents.
pub struct SearchTextTool {
    env: Arc<dyn ExecutionEnvironment>,
}

impl SearchTextTool {
    pub fn new(root: WorkspaceBase) -> Self {
        Self {
            env: env_from_root(&root),
        }
    }

    pub fn with_env(env: Arc<dyn ExecutionEnvironment>) -> Self {
        Self { env }
    }
}

#[derive(Debug, ToolSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchTextArgs {
    #[tool(desc = "Regex to search for, or exact text when literal is true")]
    query: String,
    #[tool(
        desc = "File or directory to search; relative paths use the primary workspace (default '.')"
    )]
    path: Option<String>,
    #[tool(desc = "File globs relative to path; pass alternatives as separate array items (OR)")]
    include: Option<Vec<String>>,
    #[tool(desc = "File globs to exclude from the search")]
    exclude: Option<Vec<String>>,
    #[tool(desc = "Treat query as exact text instead of regex (default false)")]
    literal: Option<bool>,
    #[tool(desc = "Context lines around each match (default 0)")]
    context: Option<u64>,
    #[tool(desc = "Maximum returned lines (default 200)")]
    limit: Option<u64>,
}

#[async_trait]
impl Tool for SearchTextTool {
    fn name(&self) -> &str {
        "search_text"
    }

    fn description(&self) -> &str {
        "Search text in files with a regex or literal query. Returns path:line:content matches."
    }

    fn parameters(&self) -> serde_json::Value {
        SearchTextArgs::parameters_schema()
    }

    fn accesses(&self, arguments: &str) -> ToolAccesses {
        ToolAccesses::search_tree(search_path_argument(arguments))
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: SearchTextArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments: {error}"))?;
        if args.query.is_empty() {
            return Err("'query' must not be empty".to_string());
        }
        let context = args.context.unwrap_or(0).min(10) as usize;
        let limit = search_limit(args.limit)?;
        let path = args.path.as_deref().unwrap_or(".");
        let workspace = self.env.workspace_root().to_path_buf();
        let search_root = resolve_search_root(self.env.as_ref(), path)?;
        self.env
            .fs()
            .metadata(&search_root)
            .await
            .map_err(|error| format!("Cannot search '{path}': {error}"))?;

        let query = args.query;
        let include = args.include.unwrap_or_default();
        let exclude = args.exclude.unwrap_or_default();
        let literal = args.literal.unwrap_or(false);
        tokio::task::spawn_blocking(move || {
            native_search(NativeSearchParams {
                workspace: &workspace,
                search_root: &search_root,
                query: &query,
                include: &include,
                exclude: &exclude,
                literal,
                context,
                limit,
                deadline: Instant::now() + SEARCH_TIMEOUT,
            })
        })
        .await
        .map_err(|error| format!("Text search task failed: {error}"))?
    }

    async fn call_structured(&self, arguments: &str) -> Result<muta_contracts::ToolOutput, String> {
        let output = self.call(arguments).await?;
        let query = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|value| value["query"].as_str().map(str::to_string))
            .unwrap_or_default();
        Ok(muta_contracts::ToolOutput::Matches {
            pattern: query,
            lines: output.lines().map(str::to_string).collect(),
        })
    }
}

muta_contracts::register_tool!(SearchTextFactory => |ctx| SearchTextTool {
    env: execution_environment(ctx),
});

struct NativeSearchParams<'a> {
    workspace: &'a std::path::Path,
    search_root: &'a std::path::Path,
    query: &'a str,
    include: &'a [String],
    exclude: &'a [String],
    literal: bool,
    context: usize,
    limit: usize,
    deadline: Instant,
}

fn native_search(params: NativeSearchParams<'_>) -> Result<String, String> {
    let NativeSearchParams {
        workspace,
        search_root,
        query,
        include,
        exclude,
        literal,
        context,
        limit,
        deadline,
    } = params;
    let expression = if literal {
        regex::escape(query)
    } else {
        query.to_string()
    };
    let regex = regex::Regex::new(&expression)
        .map_err(|error| format!("Invalid regular expression: {error}"))?;
    // Include globs filter after project-ignore pruning (see file_search.rs).
    let include_matcher = build_include_matcher(search_root, include, exclude)?;
    let walker = build_file_walker(search_root, exclude, None)?.build();
    let mut matches = Vec::new();
    let mut total_bytes = 0usize;
    let mut truncated = false;

    for entry in walker {
        if Instant::now() >= deadline {
            return Err(format!(
                "Text search timed out after {} seconds",
                SEARCH_TIMEOUT.as_secs()
            ));
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if !include_allows(
            &include_matcher,
            !include.is_empty(),
            search_root,
            entry.path(),
        ) {
            continue;
        }
        if let Ok(metadata) = entry.metadata()
            && metadata.len() > 10 * 1024 * 1024
        {
            continue;
        }
        let content = match std::fs::read_to_string(entry.path()) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let display = entry
            .path()
            .strip_prefix(workspace)
            .unwrap_or_else(|_| entry.path())
            .to_string_lossy();
        let lines: Vec<&str> = content.lines().collect();
        let mut file_matches = 0usize;
        for (index, line) in lines.iter().enumerate() {
            if Instant::now() >= deadline {
                return Err(format!(
                    "Text search timed out after {} seconds",
                    SEARCH_TIMEOUT.as_secs()
                ));
            }
            if !regex.is_match(line) {
                continue;
            }
            file_matches += 1;
            if file_matches > SEARCH_MAX_MATCHES_PER_FILE {
                break;
            }
            let start = index.saturating_sub(context);
            let end = (index + context + 1).min(lines.len());
            for (context_index, context_line) in lines.iter().enumerate().take(end).skip(start) {
                let separator = if context_index == index { ':' } else { '-' };
                let formatted = format!(
                    "{display}{separator}{}{separator}{}",
                    context_index + 1,
                    context_line
                );
                if matches.len() >= limit || total_bytes + formatted.len() + 1 > SEARCH_MAX_BYTES {
                    truncated = true;
                    break;
                }
                total_bytes += formatted.len() + 1;
                matches.push(formatted);
            }
            if truncated {
                break;
            }
        }
        if truncated {
            break;
        }
    }

    if matches.is_empty() {
        return Ok("No matches found.".to_string());
    }
    let mut output = matches.join("\n");
    if truncated {
        output.push_str("\n\n[Output truncated — narrow query, path, or file globs.]");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_search_applies_the_global_line_limit() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("many.txt"), "hit\n".repeat(10)).unwrap();
        let output = native_search(NativeSearchParams {
            workspace: tmp.path(),
            search_root: tmp.path(),
            query: "hit",
            include: &[],
            exclude: &[],
            literal: false,
            context: 0,
            limit: 3,
            deadline: Instant::now() + SEARCH_TIMEOUT,
        })
        .unwrap();
        assert_eq!(
            output.lines().filter(|line| line.contains(":hit")).count(),
            3
        );
        assert!(output.contains("[Output truncated"));
    }

    #[test]
    fn native_search_supports_file_globs_and_literal_queries() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "fn hello.world() {}\n").unwrap();
        std::fs::write(tmp.path().join("src/main.py"), "hello.world\n").unwrap();

        let output = native_search(NativeSearchParams {
            workspace: tmp.path(),
            search_root: tmp.path(),
            query: "hello.world",
            include: &["*.rs".to_string()],
            exclude: &[],
            literal: true,
            context: 0,
            limit: 20,
            deadline: Instant::now() + SEARCH_TIMEOUT,
        })
        .unwrap();
        assert!(output.contains("src/main.rs:1:"));
        assert!(!output.contains("main.py"));
    }

    #[tokio::test]
    async fn tool_call_combines_content_and_file_selection() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "needle.value\n").unwrap();
        std::fs::write(tmp.path().join("src/main.py"), "needle.value\n").unwrap();
        let tool = SearchTextTool::new(Some(tmp.path().to_path_buf()));

        let output = tool
            .call(r#"{"query":"needle.value","include":["*.rs"],"literal":true}"#)
            .await
            .unwrap();
        assert!(output.contains("src/main.rs:1:"), "{output}");
        assert!(!output.contains("main.py"), "{output}");

        let exclude_only = tool
            .call(r#"{"query":"needle","exclude":["*.py"]}"#)
            .await
            .unwrap();
        assert!(exclude_only.contains("src/main.rs:1:"), "{exclude_only}");
        assert!(!exclude_only.contains("main.py"), "{exclude_only}");
    }

    #[tokio::test]
    async fn tool_search_admits_outside_path_when_unconfined() {
        let ws_dir = tempfile::tempdir().unwrap();
        let out_dir = tempfile::tempdir().unwrap();
        let outside_file = out_dir.path().join("outside.txt");
        std::fs::write(&outside_file, "secret_content_123\n").unwrap();

        let env = std::sync::Arc::new(crate::execution::WorkspaceExecutionEnvironment::new(
            ws_dir.path(),
        ));
        let tool = SearchTextTool::with_env(env.clone());

        env.shared_unconfined().set_unconfined(true);
        let res_unconfined = tool
            .call(&format!(
                r#"{{"query":"secret_content_123","path":"{}"}}"#,
                out_dir.path().display()
            ))
            .await
            .unwrap();
        assert!(res_unconfined.contains("outside.txt:1:secret_content_123"));
    }
}
