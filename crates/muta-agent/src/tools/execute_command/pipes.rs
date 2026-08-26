use muta_contracts::tool_output::{
    ShellLine, ShellStream, normalize_carriage_returns, strip_ansi, truncate_utf8,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::task::JoinHandle;

pub const SHELL_COLLECT_MAX_CHARS: usize = muta_contracts::tool_output::SHELL_MAX_OUTPUT_CHARS * 8;
pub const SHELL_COLLECT_MAX_LINES: usize = 5_000;

/// Background reader tasks draining child stdout and stderr concurrently into a merged channel.
pub struct StreamReaders {
    pub rx: UnboundedReceiver<(ShellStream, String)>,
    pub stdout_task: JoinHandle<()>,
    pub stderr_task: JoinHandle<()>,
}

/// Spawn the command with piped stdout/stderr and merge both streams into a
/// single, arrival-ordered line buffer so the renderer never has to choose
/// the "all-stdout-then-all-stderr" split.
pub fn spawn_stream_readers(stdout: ChildStdout, stderr: ChildStderr) -> StreamReaders {
    let (tx, rx) = unbounded_channel::<(ShellStream, String)>();

    let tx_err = tx.clone();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx_err.send((
                ShellStream::Err,
                normalize_carriage_returns(&strip_ansi(&line)),
            ));
        }
    });

    let tx_out = tx.clone();
    let stdout_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx_out.send((
                ShellStream::Out,
                normalize_carriage_returns(&strip_ansi(&line)),
            ));
        }
    });
    drop(tx);

    StreamReaders {
        rx,
        stdout_task,
        stderr_task,
    }
}

/// In-memory collection buffer for lines, stdout, and stderr.
#[derive(Default)]
pub struct OutputCollector {
    pub stdout_buf: String,
    pub stderr_buf: String,
    pub lines: Vec<ShellLine>,
}

impl OutputCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_line(
        &mut self,
        stream: ShellStream,
        text: String,
        on_stream: &mut (dyn FnMut(muta_contracts::ToolStream) + Send + '_),
    ) {
        match stream {
            ShellStream::Out => {
                self.stdout_buf.push_str(&text);
                self.stdout_buf.push('\n');
                on_stream(muta_contracts::ToolStream::Stdout(format!("{}\n", text)));
            }
            ShellStream::Err => {
                self.stderr_buf.push_str(&text);
                self.stderr_buf.push('\n');
                on_stream(muta_contracts::ToolStream::Stderr(format!("{}\n", text)));
            }
        }
        self.lines.push(ShellLine { stream, text });
    }

    pub fn drain_remaining_rx(&mut self, rx: &mut UnboundedReceiver<(ShellStream, String)>) {
        while let Ok((stream, text)) = rx.try_recv() {
            match stream {
                ShellStream::Out => {
                    self.stdout_buf.push_str(&text);
                    self.stdout_buf.push('\n');
                }
                ShellStream::Err => {
                    self.stderr_buf.push_str(&text);
                    self.stderr_buf.push('\n');
                }
            }
            self.lines.push(ShellLine { stream, text });
        }
    }

    /// Apply head+tail byte caps and line count caps to prevent unbound memory growth.
    pub fn apply_caps(mut self, exit: Option<i32>) -> (String, String, Vec<ShellLine>, bool) {
        let mut collection_truncated = false;
        if self.stdout_buf.len() > SHELL_COLLECT_MAX_CHARS {
            self.stdout_buf = head_tail(&self.stdout_buf, SHELL_COLLECT_MAX_CHARS / 2);
            collection_truncated = true;
        }
        if self.stderr_buf.len() > SHELL_COLLECT_MAX_CHARS {
            self.stderr_buf = head_tail(&self.stderr_buf, SHELL_COLLECT_MAX_CHARS / 2);
            collection_truncated = true;
        }
        if self.lines.len() > SHELL_COLLECT_MAX_LINES {
            let half = SHELL_COLLECT_MAX_LINES / 2;
            let dropped = self.lines.len() - (half * 2);
            let marker = ShellLine {
                stream: ShellStream::Err,
                text: format!("⋯ {dropped} lines dropped (collection cap)"),
            };
            let mut capped: Vec<ShellLine> = self.lines.drain(..half).collect();
            capped.push(marker);
            capped.extend(self.lines.drain(self.lines.len() - half..));
            self.lines = capped;
            collection_truncated = true;
        }

        let truncated = collection_truncated
            || muta_contracts::tool_output::shell_inner_text(
                &self.stdout_buf,
                &self.stderr_buf,
                exit,
            )
            .len()
                > muta_contracts::tool_output::SHELL_MAX_OUTPUT_CHARS;

        (self.stdout_buf, self.stderr_buf, self.lines, truncated)
    }
}

/// Keep the first `head` and last `head` bytes of `s` (UTF-8-safe, without
/// splitting a character), joining them with a marker row.
pub fn head_tail(s: &str, head: usize) -> String {
    if s.len() <= head * 2 {
        return s.to_string();
    }
    let total = s.len();
    format!(
        "{}\n⋯ {} bytes dropped (collection cap)\n{}",
        truncate_utf8(s, head),
        total - head * 2,
        truncate_utf8(&s[total - head..], head)
    )
}
