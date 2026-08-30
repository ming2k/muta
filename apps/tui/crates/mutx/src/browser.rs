//! Cross-platform URL opener with robust Linux / WSL / desktop fallbacks.

use std::process::Stdio;

/// Check whether the current system is running inside WSL (Windows Subsystem for Linux).
fn is_wsl() -> bool {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WSL_DISTRO_NAME").is_some()
            || std::env::var_os("WSL_INTEROP").is_some()
        {
            return true;
        }
        if let Ok(version) = std::fs::read_to_string("/proc/version") {
            let lower = version.to_lowercase();
            if lower.contains("microsoft") || lower.contains("wsl") {
                return true;
            }
        }
    }
    false
}

/// Open a URL in the user's default browser with extensive fallbacks for Linux desktop & WSL.
pub fn open_browser(url: &str) -> Result<(), String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!("Unsupported URL scheme: {url}"));
    }

    #[cfg(target_os = "linux")]
    {
        if is_wsl() {
            // 1. WSL-specific utilities
            if try_spawn("wslview", &[url]).is_ok() {
                return Ok(());
            }
            if try_spawn("cmd.exe", &["/c", "start", "", url]).is_ok() {
                return Ok(());
            }
            if try_spawn("powershell.exe", &["-c", &format!("Start-Process '{url}'")]).is_ok() {
                return Ok(());
            }
        }

        // 2. $BROWSER environment variable
        if let Ok(browser_env) = std::env::var("BROWSER") {
            let mut parts = browser_env.split_whitespace();
            if let Some(cmd) = parts.next() {
                let mut args: Vec<&str> = parts.collect();
                args.push(url);
                if try_spawn(cmd, &args).is_ok() {
                    return Ok(());
                }
            }
        }

        // 3. Standard desktop openers
        if try_spawn("xdg-open", &[url]).is_ok() {
            return Ok(());
        }
        if try_spawn("gio", &["open", url]).is_ok() {
            return Ok(());
        }

        // 4. Fallback to webbrowser crate
        if webbrowser::open(url).is_ok() {
            return Ok(());
        }

        // 5. Common desktop browsers in PATH
        let direct_browsers = [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "firefox",
            "brave-browser",
            "microsoft-edge",
            "x-www-browser",
        ];
        for b in direct_browsers {
            if try_spawn(b, &[url]).is_ok() {
                return Ok(());
            }
        }

        Err("Could not find a working browser opener (tried xdg-open, gio, wslview, common browsers)".to_string())
    }

    #[cfg(target_os = "macos")]
    {
        if try_spawn("open", &[url]).is_ok() {
            return Ok(());
        }
        webbrowser::open(url).map_err(|e| e.to_string())
    }

    #[cfg(target_os = "windows")]
    {
        if try_spawn("cmd.exe", &["/c", "start", "", url]).is_ok() {
            return Ok(());
        }
        webbrowser::open(url).map_err(|e| e.to_string())
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        webbrowser::open(url).map_err(|e| e.to_string())
    }
}

/// Helper to spawn a detached command without capturing or blocking stdio.
fn try_spawn(cmd: &str, args: &[&str]) -> Result<(), std::io::Error> {
    let mut child = std::process::Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_browser_rejects_invalid_scheme() {
        assert!(open_browser("ftp://example.com").is_err());
        assert!(open_browser("file:///etc/passwd").is_err());
        assert!(open_browser("javascript:alert(1)").is_err());
    }
}
