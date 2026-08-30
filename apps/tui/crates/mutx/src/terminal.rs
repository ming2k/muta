//! Raw-mode / alternate-screen lifecycle: initialization, graceful cleanup, signal-guard.

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use crossterm::{cursor, execute};

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static KEYBOARD_ENHANCEMENT_ENABLED: AtomicBool = AtomicBool::new(false);

/// Set up raw mode, alternate screen, mouse capture, bracketed paste, and
/// negotiate progressive keyboard enhancement if supported by the host terminal.
pub(super) fn enter_terminal() -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;

    // Progressive keyboard enhancement (Kitty keyboard protocol) allows modifier-bearing
    // keys (e.g. Ctrl+M vs Enter) to be disambiguated.
    // Query capability first so modern Windows Terminal/ConPTY and Linux/macOS
    // terminals receive exact protocol compliance without unsupported errors.
    if supports_keyboard_enhancement().unwrap_or(false)
        && execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok()
    {
        KEYBOARD_ENHANCEMENT_ENABLED.store(true, Ordering::Relaxed);
    }

    Ok(())
}

/// Undo raw mode, leave the alternate screen, disable bracketed paste, and turn off mouse tracking.
/// Used both on graceful shutdown and from the signal guard so an externally
/// killed process (e.g. `pkill muta`) does not strand the terminal in a
/// state where every mouse move spews SGR escape codes into the shell.
pub(super) fn restore_terminal() {
    let mut stdout = io::stdout();

    if KEYBOARD_ENHANCEMENT_ENABLED.swap(false, Ordering::Relaxed) {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }

    let _ = execute!(
        stdout,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        DisableMouseCapture,
        cursor::Show
    );
    let _ = disable_raw_mode();
    let _ = stdout.flush();
}

/// Catch termination signals and restore the terminal before exiting. Without
/// this, SIGTERM/SIGHUP (as sent by `pkill`) terminates the process without
/// running `run_tui`'s normal cleanup, leaving the host terminal in raw mode
/// with mouse capture enabled.
pub(super) fn spawn_signal_guard() {
    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut interrupt = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut hangup = match signal(SignalKind::hangup()) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut quit = match signal(SignalKind::quit()) {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
            _ = hangup.recv() => {}
            _ = quit.recv() => {}
        }
        restore_terminal();
        std::process::exit(130);
    });

    #[cfg(windows)]
    tokio::spawn(async move {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};
        let mut c = ctrl_c().ok();
        let mut b = ctrl_break().ok();
        let mut cl = ctrl_close().ok();
        let mut l = ctrl_logoff().ok();
        let mut s = ctrl_shutdown().ok();
        tokio::select! {
            _ = async { if let Some(ref mut c) = c { c.recv().await } else { std::future::pending().await } } => {}
            _ = async { if let Some(ref mut b) = b { b.recv().await } else { std::future::pending().await } } => {}
            _ = async { if let Some(ref mut cl) = cl { cl.recv().await } else { std::future::pending().await } } => {}
            _ = async { if let Some(ref mut l) = l { l.recv().await } else { std::future::pending().await } } => {}
            _ = async { if let Some(ref mut s) = s { s.recv().await } else { std::future::pending().await } } => {}
        }
        restore_terminal();
        std::process::exit(130);
    });
}
