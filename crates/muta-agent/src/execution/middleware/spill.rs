//! Spill-to-disk middleware for handling large tool outputs.
//!
//! Prevents massive tool outputs (such as huge command logs or entire build dumps)
//! from overflowing the model's context window or wasting tokens.

use async_trait::async_trait;
use muta_contracts::ToolOutput;
use muta_contracts::execution::{ExecutionEnvironment, ToolMiddleware};

/// Middleware that offloads tool outputs exceeding a byte threshold to a file on disk.
#[derive(Debug, Clone)]
pub struct SpillMiddleware {
    /// Maximum bytes allowed inline in the model context before spilling (default 50,000).
    max_inline_bytes: usize,
    /// Head chars preserved for model visibility (default 2,000).
    head_chars: usize,
    /// Tail chars preserved for model visibility (default 1,000).
    tail_chars: usize,
}

impl Default for SpillMiddleware {
    fn default() -> Self {
        Self {
            max_inline_bytes: 50_000,
            head_chars: 2_000,
            tail_chars: 1_000,
        }
    }
}

impl SpillMiddleware {
    pub fn new(max_inline_bytes: usize) -> Self {
        Self {
            max_inline_bytes,
            ..Default::default()
        }
    }
}

#[async_trait]
impl ToolMiddleware for SpillMiddleware {
    async fn post_execute(
        &self,
        tool: &str,
        output: &mut ToolOutput,
        env: &dyn ExecutionEnvironment,
    ) -> Result<(), String> {
        let text = output.to_text();
        if text.len() <= self.max_inline_bytes {
            return Ok(());
        }

        // Generate deterministic/unique spill path inside .muta/spill or workspace
        let spill_dir = env.workspace_root().join(".muta").join("spill");
        let _ = env.fs().create_dir_all(&spill_dir).await;

        let filename = format!(
            "spill_{}_{}_{}.txt",
            tool,
            chrono::Utc::now().format("%Y%m%d_%H%M%S"),
            fastrand::u32(1000..9999)
        );
        let spill_path = spill_dir.join(&filename);

        // Write full content to disk through the execution environment's FsProvider
        if let Err(e) = env.fs().write(&spill_path, text.as_bytes()).await {
            tracing::warn!(?e, path = %spill_path.display(), "failed to write spill file");
            return Ok(());
        }

        // Generate truncated summary
        let total_chars = text.chars().count();
        let head: String = text.chars().take(self.head_chars).collect();
        let tail: String = text
            .chars()
            .skip(total_chars.saturating_sub(self.tail_chars))
            .collect();

        let rewritten = format!(
            "{head}\n\n\
             [... Output exceeded {max_bytes} bytes ({total_bytes} total bytes). \
             Full unabridged output saved to '{path}'. Use `read_text` or `grep` on this file to inspect specifics. ...]\n\n\
             {tail}",
            max_bytes = self.max_inline_bytes,
            total_bytes = text.len(),
            path = spill_path.display()
        );

        match output {
            ToolOutput::Text(t) => *t = rewritten,
            ToolOutput::Shell { stdout, .. } => *stdout = rewritten,
            ToolOutput::Code { text, .. } => *text = rewritten,
            _ => *output = ToolOutput::Text(rewritten),
        }

        Ok(())
    }
}
