//! Local host execution environment implementation.
//!
//! Executes filesystem operations and subprocess commands directly on the host OS.

use async_trait::async_trait;
use muta_contracts::execution::{
    DirEntry, ExecutionEnvironment, FsError, FsMetadata, FsProvider, ProcessOutput, ProcessRunner,
    ShellIsolation,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
        let temp_name = format!(".tmp_{}_{}", std::process::id(), fastrand::u64(..));
        let temp_path = parent.join(temp_name);

        if let Err(e) = tokio::fs::write(&temp_path, content).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(FsError::from(e));
        }

        if let Err(_e) = tokio::fs::rename(&temp_path, path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            // Fallback direct write if rename across filesystem boundaries fails
            tokio::fs::write(path, content)
                .await
                .map_err(FsError::from)?;
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
        let mut cmd = muta_platform::shell::native_shell(command);
        cmd.current_dir(cwd);
        if let Some(env_map) = env {
            cmd.envs(env_map);
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let (child, process_tree) = muta_platform::process::spawn_owned(&mut cmd)
            .map_err(|e| format!("failed to spawn and contain native shell: {e}"))?;

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
                let _ = process_tree.terminate();

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

/// Filesystem provider that physically confines every operation to one root.
#[derive(Debug, Clone)]
struct WorkspaceFsProvider {
    inner: LocalFsProvider,
    root: PathBuf,
}

impl WorkspaceFsProvider {
    fn new(root: PathBuf) -> Self {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        Self {
            inner: LocalFsProvider::new(),
            root,
        }
    }

    fn confined(&self, path: &Path) -> Result<PathBuf, FsError> {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let resolved = resolve_existing_ancestor(&candidate).ok_or_else(|| {
            FsError::PermissionDenied(format!(
                "workspace sandbox could not resolve '{}'",
                path.display()
            ))
        })?;
        if resolved.starts_with(&self.root) {
            // Operate on the checked path, not the original spelling. This
            // collapses `..` and already-resolved symlink parents so the I/O
            // call cannot reinterpret a different lexical route.
            Ok(resolved)
        } else {
            Err(FsError::PermissionDenied(format!(
                "'{}' is outside workspace '{}'",
                path.display(),
                self.root.display()
            )))
        }
    }
}

fn resolve_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut cursor = path;
    let mut suffix = Vec::new();
    while !cursor.exists() {
        suffix.push(cursor.file_name()?.to_os_string());
        cursor = cursor.parent()?;
    }
    let mut resolved = std::fs::canonicalize(cursor).ok()?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Some(resolved)
}

#[async_trait]
impl FsProvider for WorkspaceFsProvider {
    async fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        self.inner.read(&self.confined(path)?).await
    }

    async fn read_to_string(&self, path: &Path) -> Result<String, FsError> {
        self.inner.read_to_string(&self.confined(path)?).await
    }

    async fn write(&self, path: &Path, content: &[u8]) -> Result<(), FsError> {
        self.inner.write(&self.confined(path)?, content).await
    }

    async fn exists(&self, path: &Path) -> bool {
        match self.confined(path) {
            Ok(path) => self.inner.exists(&path).await,
            Err(_) => false,
        }
    }

    async fn is_dir(&self, path: &Path) -> bool {
        match self.confined(path) {
            Ok(path) => self.inner.is_dir(&path).await,
            Err(_) => false,
        }
    }

    async fn is_file(&self, path: &Path) -> bool {
        match self.confined(path) {
            Ok(path) => self.inner.is_file(&path).await,
            Err(_) => false,
        }
    }

    async fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError> {
        self.inner.list_dir(&self.confined(path)?).await
    }

    async fn create_dir_all(&self, path: &Path) -> Result<(), FsError> {
        self.inner.create_dir_all(&self.confined(path)?).await
    }

    async fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        self.inner.remove_file(&self.confined(path)?).await
    }

    async fn metadata(&self, path: &Path) -> Result<FsMetadata, FsError> {
        self.inner.metadata(&self.confined(path)?).await
    }
}

#[derive(Debug, Clone, Default)]
struct WorkspaceProcessRunner;

#[async_trait]
impl ProcessRunner for WorkspaceProcessRunner {
    async fn exec(
        &self,
        _command: &str,
        _cwd: &Path,
        _env: Option<&HashMap<String, String>>,
        _timeout: Duration,
    ) -> Result<ProcessOutput, String> {
        Err("Direct process execution is disabled in the workspace sandbox; use the sandbox-aware shell capability.".to_string())
    }
}

/// Product runtime environment: filesystem tools are physically confined and
/// shell tools are required to enter a workspace sandbox.
#[derive(Debug, Clone)]
pub struct WorkspaceExecutionEnvironment {
    fs: WorkspaceFsProvider,
    process: WorkspaceProcessRunner,
    workspace_root: PathBuf,
}

impl WorkspaceExecutionEnvironment {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            fs: WorkspaceFsProvider::new(workspace_root.clone()),
            process: WorkspaceProcessRunner,
            workspace_root,
        }
    }
}

/// Probe the physical sandbox once. Existence is insufficient: distributions
/// may install bubblewrap while disabling unprivileged user namespaces.
pub fn workspace_sandbox_available() -> bool {
    muta_platform::workspace_sandbox::available()
}

impl ExecutionEnvironment for WorkspaceExecutionEnvironment {
    fn fs(&self) -> &dyn FsProvider {
        &self.fs
    }

    fn process(&self) -> &dyn ProcessRunner {
        &self.process
    }

    fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    fn shell_isolation(&self) -> ShellIsolation {
        ShellIsolation::Workspace
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::*;

    fn scratch() -> PathBuf {
        let root = std::env::temp_dir().join(format!("muta-workspace-fs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[tokio::test]
    async fn workspace_fs_allows_inside_and_denies_outside() {
        let root = scratch();
        let outside = scratch().join("secret.txt");
        std::fs::write(&outside, "secret").unwrap();
        let env = WorkspaceExecutionEnvironment::new(&root);

        env.fs()
            .write(Path::new("inside.txt"), b"ok")
            .await
            .unwrap();
        assert_eq!(
            env.fs().read(&root.join("inside.txt")).await.unwrap(),
            b"ok"
        );
        assert!(matches!(
            env.fs().read(&outside).await,
            Err(FsError::PermissionDenied(_))
        ));
        assert_eq!(env.shell_isolation(), ShellIsolation::Workspace);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_fs_denies_symlink_escape() {
        let root = scratch();
        let outside = scratch().join("secret.txt");
        std::fs::write(&outside, "secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        let env = WorkspaceExecutionEnvironment::new(&root);
        assert!(matches!(
            env.fs().read(&root.join("escape")).await,
            Err(FsError::PermissionDenied(_))
        ));
    }

    #[tokio::test]
    async fn workspace_process_runner_fails_closed() {
        let env = WorkspaceExecutionEnvironment::new(scratch());
        let error = env
            .process()
            .exec(
                "echo unsafe",
                env.workspace_root(),
                None,
                Duration::from_secs(1),
            )
            .await
            .unwrap_err();
        assert!(error.contains("disabled in the workspace sandbox"));
    }

    #[tokio::test]
    async fn every_builtin_file_discovery_path_rejects_workspace_escape() {
        use crate::tools::{FindTool, GlobTool, GrepTool, ListDirTool, ReadImageTool};
        use muta_contracts::Tool;

        let root = scratch();
        let outside_root = scratch();
        let outside_text = outside_root.join("secret.txt");
        let outside_image = outside_root.join("secret.png");
        std::fs::write(&outside_text, "secret").unwrap();
        std::fs::write(&outside_image, b"not actually an image").unwrap();
        let env: std::sync::Arc<dyn ExecutionEnvironment> =
            std::sync::Arc::new(WorkspaceExecutionEnvironment::new(&root));

        let find = FindTool::with_env(env.clone());
        let glob = GlobTool::with_env(env.clone());
        let grep = GrepTool::with_env(env.clone());
        let list = ListDirTool::with_env(env.clone());
        let image = ReadImageTool::with_env(env);

        assert!(
            find.call(&serde_json::json!({ "path": outside_root }).to_string())
                .await
                .unwrap_err()
                .contains("outside workspace")
        );
        assert!(
            glob.call(&serde_json::json!({ "pattern": "*", "path": outside_root }).to_string())
                .await
                .unwrap_err()
                .contains("outside workspace")
        );
        assert!(
            grep.call(
                &serde_json::json!({ "pattern": "secret", "path": outside_text }).to_string()
            )
            .await
            .unwrap_err()
            .contains("outside workspace")
        );
        assert!(
            list.call(&serde_json::json!({ "path": outside_root }).to_string())
                .await
                .unwrap_err()
                .contains("outside workspace")
        );
        assert!(
            image
                .call(&serde_json::json!({ "path": outside_image }).to_string())
                .await
                .unwrap_err()
                .contains("outside workspace")
        );
        assert!(
            glob.call(r#"{"pattern":"../*","path":"."}"#)
                .await
                .unwrap_err()
                .contains("must stay relative")
        );
        assert!(
            list.call(r#"{"pattern":"../*","path":"."}"#)
                .await
                .unwrap_err()
                .contains("must stay relative")
        );
    }
}
