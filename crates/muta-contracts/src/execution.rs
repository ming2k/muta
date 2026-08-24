//! Execution environment and capability seams.
//!
//! Separates the agent's high-level intent (tools, prompts, ReAct loop) from
//! physical execution (local OS, Docker sandbox, remote E2B VM, or memory mock).

use async_trait::async_trait;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Errors produced by a [`FsProvider`] implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsError {
    NotFound(String),
    PermissionDenied(String),
    IsADirectory(String),
    NotADirectory(String),
    AlreadyExists(String),
    Io(String),
    Other(String),
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(p) => write!(f, "File or directory not found: {p}"),
            Self::PermissionDenied(p) => write!(f, "Permission denied: {p}"),
            Self::IsADirectory(p) => write!(f, "'{p}' is a directory, not a file"),
            Self::NotADirectory(p) => write!(f, "'{p}' is not a directory"),
            Self::AlreadyExists(p) => write!(f, "File already exists: {p}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FsError {}

impl From<std::io::Error> for FsError {
    fn from(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound(err.to_string()),
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied(err.to_string()),
            std::io::ErrorKind::AlreadyExists => Self::AlreadyExists(err.to_string()),
            _ => Self::Io(err.to_string()),
        }
    }
}

/// Metadata describing a file system node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsMetadata {
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub len: u64,
}

/// One directory entry returned by [`FsProvider::list_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub is_file: bool,
    pub size_bytes: u64,
}

/// The file system capability seam.
///
/// Implementations can back onto the local OS file system, an in-memory VFS,
/// a Docker container overlay, or a remote cloud sandbox (e.g. E2B).
#[async_trait]
pub trait FsProvider: Send + Sync {
    /// Read entire file into raw bytes.
    async fn read(&self, path: &Path) -> Result<Vec<u8>, FsError>;

    /// Read entire file as UTF-8 string.
    async fn read_to_string(&self, path: &Path) -> Result<String, FsError> {
        let bytes = self.read(path).await?;
        String::from_utf8(bytes).map_err(|e| FsError::Other(format!("Invalid UTF-8: {e}")))
    }

    /// Write byte contents to a file atomically or in-place.
    async fn write(&self, path: &Path, content: &[u8]) -> Result<(), FsError>;

    /// Write text string to a file.
    async fn write_str(&self, path: &Path, content: &str) -> Result<(), FsError> {
        self.write(path, content.as_bytes()).await
    }

    /// Check if a path exists.
    async fn exists(&self, path: &Path) -> bool;

    /// Check if a path is a directory.
    async fn is_dir(&self, path: &Path) -> bool;

    /// Check if a path is a regular file.
    async fn is_file(&self, path: &Path) -> bool;

    /// Read directory entries.
    async fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError>;

    /// Create directory recursively.
    async fn create_dir_all(&self, path: &Path) -> Result<(), FsError>;

    /// Remove a file.
    async fn remove_file(&self, path: &Path) -> Result<(), FsError>;

    /// Query metadata for a path.
    async fn metadata(&self, path: &Path) -> Result<FsMetadata, FsError>;
}

/// The outcome of running a subprocess command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    /// The process exit code, if terminated normally by exit code.
    pub exit_code: Option<i32>,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
    /// Whether execution was terminated because of timeout.
    pub timed_out: bool,
}

impl ProcessOutput {
    /// Format stdout as lossy UTF-8 string.
    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }

    /// Format stderr as lossy UTF-8 string.
    pub fn stderr_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr).to_string()
    }

    /// Return combined output (stdout + stderr), preserving order where possible.
    pub fn combined_output_lossy(&self) -> String {
        let stdout = self.stdout_lossy();
        let stderr = self.stderr_lossy();
        if stderr.is_empty() {
            stdout
        } else if stdout.is_empty() {
            stderr
        } else {
            format!("{stdout}\n{stderr}")
        }
    }

    /// True if the process exited with code 0 and did not time out.
    pub fn is_success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }
}

/// The subprocess execution capability seam.
#[async_trait]
pub trait ProcessRunner: Send + Sync {
    /// Execute a shell/process command within a working directory, with optional
    /// environment overrides and a timeout deadline.
    async fn exec(
        &self,
        command: &str,
        cwd: &Path,
        env: Option<&HashMap<String, String>>,
        timeout: Duration,
    ) -> Result<ProcessOutput, String>;
}

/// A unified execution environment bundling filesystem access, process
/// execution, and a canonical workspace root.
pub trait ExecutionEnvironment: Send + Sync {
    /// The filesystem provider for this environment.
    fn fs(&self) -> &dyn FsProvider;

    /// The process runner for this environment.
    fn process(&self) -> &dyn ProcessRunner;

    /// The canonical root directory for this environment.
    fn workspace_root(&self) -> &Path;
}

/// Interceptor middleware for the tool execution pipeline.
///
/// Enables cross-cutting concerns like large output spilling (Spill-to-Disk),
/// secret scrubbing, and permission guards without modifying individual tool bodies.
#[async_trait]
pub trait ToolMiddleware: Send + Sync {
    /// Pre-execution inspection: inspect or reject a tool call before invocation.
    async fn pre_execute(
        &self,
        _tool: &str,
        _arguments: &serde_json::Value,
        _env: &dyn ExecutionEnvironment,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Post-execution transformation: scrub secrets, spill large outputs, or append metadata.
    async fn post_execute(
        &self,
        _tool: &str,
        _output: &mut crate::ToolOutput,
        _env: &dyn ExecutionEnvironment,
    ) -> Result<(), String> {
        Ok(())
    }
}
