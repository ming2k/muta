use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

/// Long-running persistent shell session preserving environment and working directory.
pub struct PersistentTerminalSession {
    _child: Child,
    _process_tree: muta_platform::process::OwnedProcessTree,
    stdin: ChildStdin,
    stdout_lines: Lines<BufReader<ChildStdout>>,
}

impl PersistentTerminalSession {
    pub fn spawn(root: &Path) -> Result<Self, String> {
        let mut invocation = muta_platform::shell::persistent_shell_command();
        invocation
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(root);

        #[cfg(unix)]
        invocation.process_group(0);

        let (mut child, process_tree) = muta_platform::process::spawn_owned(&mut invocation)
            .map_err(|e| format!("Failed to spawn persistent terminal: {e}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or("Failed to open persistent shell stdin")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Failed to open persistent shell stdout")?;
        let stdout_lines = BufReader::new(stdout).lines();

        Ok(Self {
            _child: child,
            _process_tree: process_tree,
            stdin,
            stdout_lines,
        })
    }

    pub async fn run_command(
        &mut self,
        command: &str,
        timeout: Duration,
        on_stream: &mut (dyn FnMut(muta_contracts::ToolStream) + Send + '_),
    ) -> Result<muta_contracts::ToolOutput, String> {
        use muta_contracts::tool_output::{ShellLine, normalize_carriage_returns, strip_ansi};

        let sentinel_id = format!(
            "__MUTA_PERSISTENT_TERM_DONE_{}__",
            uuid::Uuid::new_v4().simple()
        );
        let cmd_payload = format!("{}\nprintf '\\n{}:%d\\n' $?\n", command, sentinel_id);

        self.stdin
            .write_all(cmd_payload.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to persistent terminal: {e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("Failed to flush persistent terminal: {e}"))?;

        let mut lines = Vec::new();
        let mut stdout_str = String::new();
        let mut exit_code = 0;
        let sentinel_prefix = format!("{}:", sentinel_id);

        let read_future = async {
            while let Ok(Some(line)) = self.stdout_lines.next_line().await {
                if let Some(rest) = line.strip_prefix(&sentinel_prefix) {
                    if let Ok(code) = rest.trim().parse::<i32>() {
                        exit_code = code;
                    }
                    break;
                }
                let clean = normalize_carriage_returns(&strip_ansi(&line));
                stdout_str.push_str(&clean);
                stdout_str.push('\n');
                lines.push(ShellLine {
                    stream: muta_contracts::tool_output::ShellStream::Out,
                    text: clean.clone(),
                });
                on_stream(muta_contracts::ToolStream::Stdout(format!("{}\n", clean)));
            }
            Ok::<_, String>(())
        };

        match tokio::time::timeout(timeout, read_future).await {
            Ok(Ok(())) => Ok(muta_contracts::ToolOutput::Shell {
                command: command.to_string(),
                stdout: stdout_str,
                stderr: String::new(),
                exit: Some(exit_code),
                truncated: false,
                termination: muta_contracts::ShellTermination::Exited,
                lines,
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(muta_contracts::ToolOutput::Shell {
                command: command.to_string(),
                stdout: stdout_str,
                stderr: String::new(),
                exit: None,
                truncated: false,
                termination: muta_contracts::ShellTermination::Timeout,
                lines,
            }),
        }
    }
}

static PERSISTENT_TERMINALS: LazyLock<
    Mutex<HashMap<String, Arc<Mutex<PersistentTerminalSession>>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub async fn run_persistent_command(
    root: &Path,
    term_id: &str,
    command: &str,
    timeout: Duration,
    on_stream: &mut (dyn FnMut(muta_contracts::ToolStream) + Send + '_),
) -> Result<muta_contracts::ToolOutput, String> {
    let session_arc = {
        let mut pool = PERSISTENT_TERMINALS.lock().await;
        if let Some(s) = pool.get(term_id) {
            s.clone()
        } else {
            let sess = PersistentTerminalSession::spawn(root)?;
            let arc = Arc::new(Mutex::new(sess));
            pool.insert(term_id.to_string(), arc.clone());
            arc
        }
    };
    let mut session = session_arc.lock().await;
    session.run_command(command, timeout, on_stream).await
}
