//! Explicit native shell selection.

use tokio::process::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellDialect {
    Posix,
    PowerShell,
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
    #[cfg(unix)]
    {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        command
    }
    #[cfg(windows)]
    {
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

/// Build a long-running persistent native shell command that reads commands from stdin.
pub fn persistent_shell_command() -> Command {
    #[cfg(unix)]
    {
        let mut command = Command::new("sh");
        command.arg("-s");
        command
    }
    #[cfg(windows)]
    {
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
