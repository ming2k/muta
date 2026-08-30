//! Standardized cross-platform path and directory resolution.
//!
//! Provides platform-native directory resolution compliant with:
//! - Linux / BSD: XDG Base Directory Specification
//! - macOS: Standard Apple user library directories
//! - Windows: Known Folders (`%APPDATA%`, `%LOCALAPPDATA%`)

use std::path::{Path, PathBuf};

/// Base platform directory layout contract.
pub trait PlatformPaths {
    /// Directory for user configuration files.
    fn config_dir(&self) -> &Path;

    /// Directory for persistent user data.
    fn data_dir(&self) -> &Path;

    /// Directory for state files (logs, history, sockets).
    fn state_dir(&self) -> &Path;

    /// Directory for non-essential cached data.
    fn cache_dir(&self) -> &Path;

    /// Directory for runtime state (sockets, ephemeral locks, pidfiles).
    fn runtime_dir(&self) -> Option<&Path>;
}

/// Resolved standard directory layout for an application name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandardLayout {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub runtime_dir: Option<PathBuf>,
}

impl PlatformPaths for StandardLayout {
    fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    fn runtime_dir(&self) -> Option<&Path> {
        self.runtime_dir.as_deref()
    }
}

impl StandardLayout {
    /// Resolve the standard native directory layout for the given application name.
    #[must_use]
    pub fn for_app(app_name: &str) -> Self {
        let config_dir = resolve_config_dir(app_name);
        let data_dir = resolve_data_dir(app_name);
        let state_dir = resolve_state_dir(app_name);
        let cache_dir = resolve_cache_dir(app_name);
        let runtime_dir = resolve_runtime_dir(app_name);

        Self {
            config_dir,
            data_dir,
            state_dir,
            cache_dir,
            runtime_dir,
        }
    }

    /// Creates all standard directories on disk if they do not exist.
    pub fn ensure_all_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.state_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        if let Some(ref rt) = self.runtime_dir {
            std::fs::create_dir_all(rt)?;
        }
        Ok(())
    }
}

fn resolve_config_dir(app: &str) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(base) = dirs::config_dir() {
            return base.join(app);
        }
    }
    #[cfg(target_os = "macos")]
    {
        // On macOS, XDG or ~/Library/Application Support/<app>
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join(app);
            }
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".config").join(app);
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
            && !xdg.is_empty()
        {
            return PathBuf::from(xdg).join(app);
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".config").join(app);
        }
    }
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(app)
}

fn resolve_data_dir(app: &str) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(base) = dirs::data_local_dir() {
            return base.join(app);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join(app);
            }
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".local").join("share").join(app);
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
            && !xdg.is_empty()
        {
            return PathBuf::from(xdg).join(app);
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".local").join("share").join(app);
        }
    }
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(app)
}

fn resolve_state_dir(app: &str) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(base) = dirs::data_local_dir() {
            return base.join(app).join("state");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join(app);
            }
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".local").join("state").join(app);
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_STATE_HOME")
            && !xdg.is_empty()
        {
            return PathBuf::from(xdg).join(app);
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".local").join("state").join(app);
        }
    }
    dirs::state_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(app)
}

fn resolve_cache_dir(app: &str) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(base) = dirs::cache_dir() {
            return base.join(app);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join(app);
            }
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".cache").join(app);
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
            && !xdg.is_empty()
        {
            return PathBuf::from(xdg).join(app);
        }
        if let Some(home) = dirs::home_dir() {
            return home.join(".cache").join(app);
        }
    }
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(app)
}

fn resolve_runtime_dir(app: &str) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR")
            && !xdg.is_empty()
        {
            return Some(PathBuf::from(xdg).join(app));
        }
    }
    let _ = app;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_non_empty_layout() {
        let layout = StandardLayout::for_app("muta-test");
        assert!(!layout.config_dir.as_os_str().is_empty());
        assert!(!layout.data_dir.as_os_str().is_empty());
        assert!(!layout.state_dir.as_os_str().is_empty());
        assert!(!layout.cache_dir.as_os_str().is_empty());
        assert!(layout.config_dir.ends_with("muta-test"));
    }
}
