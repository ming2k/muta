//! Local host execution environment implementation.
//!
//! Executes filesystem operations and subprocess commands directly on the host OS.

use async_trait::async_trait;
use neenee_contracts::execution::{
    DirEntry, ExecutionEnvironment, FsError, FsMetadata, FsProvider, ProcessOutput, ProcessRunner,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

/// Local host filesystem provider.
#[derive(Debug, Default, Clone)]
pub struct LocalFsProvider;

impl LocalFsProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl FsProvider for LocalFsProvider {
    async fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        tokio::fs::read(path).await.map_err(FsError::from)
    }

    async fn read_to_string(&self, path: &Path) -> Result<String, FsError> {
        tokio::fs::read_to_string(path).await.map_err(FsError::from)
    }

    async fn write(&self, path: &Path, content: &[u8]) -> Result<(), FsError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        // Atomic write via temp file and rename when possible
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let temp_name = format!(
            ".tmp_{}_{}",
            std::process::id(),
            fastrand::u64(..)
        );
        let temp_path = parent.join(temp_name);

        if let Err(e) = tokio::fs::write(&temp_path, content).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(FsError::from(e));
        }

        if let Err(_e) = tokio::fs::rename(&temp_path, path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            // Fallback direct write if rename across filesystem boundaries fails
            tokio::fs::write(path, content).await.map_err(FsError::from)?;
        }

        Ok(())
    }

    async fn exists(&self, path: &Path) -> bool {
        tokio::fs::try_exists(path).await.unwrap_or(false)
    }

    async fn is_dir(&self, path: &Path) -> bool {
        tokio::fs::metadata(path)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false)
    }

    async fn is_file(&self, path: &Path) -> bool {
        tokio::fs::metadata(path)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
    }

    async fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError> {
        let mut read_dir = tokio::fs::read_dir(path).await.map_err(FsError::from)?;
        let mut entries = Vec::new();

        while let Some(entry) = read_dir.next_entry().await.map_err(FsError::from)? {
            let entry_path = entry.path();
            let metadata = entry.metadata().await.map_err(FsError::from)?;
            let name = entry.file_name().to_string_lossy().to_string();

            entries.push(DirEntry {
                path: entry_path,
                name,
                is_dir: metadata.is_dir(),
                is_file: metadata.is_file(),
                size_bytes: metadata.len(),
            });
        }

        Ok(entries)
    }

    async fn create_dir_all(&self, path: &Path) -> Result<(), FsError> {
        tokio::fs::create_dir_all(path).await.map_err(FsError::from)
    }

    async fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        tokio::fs::remove_file(path).await.map_err(FsError::from)
    }

    async fn metadata(&self, path: &Path) -> Result<FsMetadata, FsError> {
        let meta = tokio::fs::symlink_metadata(path)
            .await
            .map_err(FsError::from)?;
        Ok(FsMetadata {
            is_dir: meta.is_dir(),
            is_file: meta.is_file(),
            is_symlink: meta.file_type().is_symlink(),
            len: meta.len(),
        })
    }
}

/// Local host subprocess runner.
#[derive(Debug, Default, Clone)]
pub struct LocalProcessRunner;

impl LocalProcessRunner {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ProcessRunner for LocalProcessRunner {
    async fn exec(
        &self,
        command: &str,
        cwd: &Path,
        env: Option<&HashMap<String, String>>,
        timeout: Duration,
    ) -> Result<ProcessOutput, String> {
        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(command);
        cmd.current_dir(cwd);

        #[cfg(unix)]
        cmd.process_group(0);

        if let Some(env_map) = env {
            cmd.envs(env_map);
        }

        let child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn bash: {e}"))?;

        #[cfg(unix)]
        let child_id = child.id();

        let wait_fut = child.wait_with_output();
        match tokio::time::timeout(timeout, wait_fut).await {
            Ok(Ok(output)) => Ok(ProcessOutput {
                exit_code: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
                timed_out: false,
            }),
            Ok(Err(e)) => Err(format!("Command execution failed: {e}")),
            Err(_) => {
                #[cfg(unix)]
                if let Some(pid) = child_id {
                    // SAFETY: kill process group using negative pid
                    unsafe {
                        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
                    }
                }

                Ok(ProcessOutput {
                    exit_code: None,
                    stdout: Vec::new(),
                    stderr: format!("Command timed out after {}s", timeout.as_secs()).into_bytes(),
                    timed_out: true,
                })
            }
        }
    }
}

/// Local execution environment tying together the local filesystem and local process runner.
#[derive(Debug, Clone)]
pub struct LocalExecutionEnvironment {
    fs: LocalFsProvider,
    process: LocalProcessRunner,
    workspace_root: PathBuf,
}

impl LocalExecutionEnvironment {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            fs: LocalFsProvider::new(),
            process: LocalProcessRunner::new(),
            workspace_root: workspace_root.into(),
        }
    }
}

impl ExecutionEnvironment for LocalExecutionEnvironment {
    fn fs(&self) -> &dyn FsProvider {
        &self.fs
    }

    fn process(&self) -> &dyn ProcessRunner {
        &self.process
    }

    fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}
