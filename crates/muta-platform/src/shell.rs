//! Explicit native shell selection.

use tokio::process::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellDialect {
    Posix,
    PowerShell,
}

impl ShellDialect {
    /// Short identifier for the dialect (e.g. "posix" or "powershell").
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Posix => "posix",
            Self::PowerShell => "powershell",
        }
    }

    /// Human-readable description of the shell.
    pub const fn description(&self) -> &'static str {
        match self {
            Self::Posix => "POSIX sh / bash",
            Self::PowerShell => "PowerShell (powershell.exe)",
        }
    }

    /// Build a non-interactive episodic shell command invocation for user script text.
    pub fn build_episodic_command(&self, script: &str) -> Command {
        match self {
            Self::Posix => {
                let mut command = Command::new("sh");
                command.arg("-c").arg(script);
                command
            }
            Self::PowerShell => {
                let mut command = Command::new("powershell.exe");
                command
                    .arg("-NoLogo")
                    .arg("-NoProfile")
                    .arg("-NonInteractive")
                    .arg("-ExecutionPolicy")
                    .arg("Bypass")
                    .arg("-Command")
                    .arg(script);
                command
            }
        }
    }

    /// Build a persistent interactive-like shell command reading from stdin.
    pub fn build_persistent_command(&self) -> Command {
        match self {
            Self::Posix => {
                let mut command = Command::new("sh");
                command.arg("-s");
                command
            }
            Self::PowerShell => {
                let mut command = Command::new("powershell.exe");
                command
                    .arg("-NoLogo")
                    .arg("-NoProfile")
                    .arg("-NonInteractive")
                    .arg("-ExecutionPolicy")
                    .arg("Bypass")
                    .arg("-Command")
                    .arg("-");
                command
            }
        }
    }

    /// Format a sentinel payload string written into stdin to execute `command`
    /// and reliably print the exit status together with `sentinel_id`.
    pub fn format_sentinel_command(&self, command: &str, sentinel_id: &str) -> String {
        match self {
            Self::Posix => {
                format!("{}\nprintf '\\n{}:%d\\n' $?\n", command, sentinel_id)
            }
            Self::PowerShell => {
                // In PowerShell:
                // If $LASTEXITCODE is set (native process), use it;
                // else if $? is false, exit code is 1, otherwise 0.
                format!(
                    "{}\r\n$__muta_ec = if ($null -ne $LASTEXITCODE) {{ $LASTEXITCODE }} elseif ($?) {{ 0 }} else {{ 1 }}; Write-Output \"`r`n{}:$__muta_ec`r`n\"\r\n",
                    command, sentinel_id
                )
            }
        }
    }
}

pub const fn native_shell_dialect() -> ShellDialect {
    #[cfg(unix)]
    return ShellDialect::Posix;
    #[cfg(windows)]
    return ShellDialect::PowerShell;
}

/// Build a non-interactive native-shell invocation for user-authored script
/// text. Internal commands should still use argv directly.
pub fn native_shell(script: &str) -> Command {
    native_shell_dialect().build_episodic_command(script)
}

/// Build a long-running persistent native shell command that reads commands from stdin.
pub fn persistent_shell_command() -> Command {
    native_shell_dialect().build_persistent_command()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_sentinel_format() {
        let formatted = ShellDialect::Posix.format_sentinel_command("echo test", "TOKEN123");
        assert!(formatted.contains("printf '\\nTOKEN123:%d\\n' $?"));
    }

    #[test]
    fn powershell_sentinel_format() {
        let formatted =
            ShellDialect::PowerShell.format_sentinel_command("Write-Output test", "TOKEN123");
        assert!(formatted.contains("TOKEN123:$__muta_ec"));
        assert!(formatted.contains("Write-Output"));
    }
}
