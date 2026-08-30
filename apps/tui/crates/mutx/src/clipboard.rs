//! Clipboard integration: OSC52 terminal sequences + system clipboard.
//!
//! This follows opencode's approach: the TUI framework manages copying,
//! not the terminal emulator.  When the user copies selected text, we
//! write it through both OSC52 (for remote/TTY sessions) and the
//! native system clipboard (arboard).

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOutcome {
    Native,
    Osc52,
}

/// Copy text through a native clipboard owner when possible, then fall back to
/// OSC52. Wayland needs a living owner for the selection, so `wl-copy` is
/// preferred over creating and immediately dropping an arboard clipboard.
pub async fn copy(text: &str) -> Result<CopyOutcome, String> {
    let mut errors = Vec::new();
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        match copy_with_command("wl-copy", &[], text).await {
            Ok(()) => return Ok(CopyOutcome::Native),
            Err(error) => errors.push(error),
        }
    }
    match copy_system(text).await {
        Ok(()) => return Ok(CopyOutcome::Native),
        Err(error) => errors.push(error.to_string()),
    }

    write_osc52(text)
        .map(|_| CopyOutcome::Osc52)
        .map_err(|osc_error| {
            format!(
                "native clipboard failed: {}; OSC52 failed: {}",
                errors.join("; "),
                osc_error
            )
        })
}

/// Write an OSC52 "copy to clipboard" escape sequence to stdout.
///
/// Sequence: `ESC ] 52 ; c ; <base64> BEL`
/// In tmux: wrapped with `ESC P tmux ; ESC ... ESC \\`
/// In screen: wrapped with `ESC P ... ESC \\`
fn write_osc52(text: &str) -> io::Result<()> {
    let encoded = base64_encode(text);
    let sequence = format!("\x1b]52;c;{}\x07", encoded);

    let output = if std::env::var("TMUX").is_ok() {
        format!("\x1bPtmux;\x1b{}\x1b\\", sequence)
    } else if std::env::var("STY").is_ok() {
        // GNU screen
        format!("\x1bP{}\x1b\\", sequence)
    } else {
        sequence
    };

    let mut stdout = io::stdout();
    stdout.write_all(output.as_bytes())?;
    stdout.flush()
}

async fn copy_with_command(command: &str, args: &[&str], text: &str) -> Result<(), String> {
    let mut child = tokio::process::Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        // Redirect stderr to /dev/null instead of piping it: helpers like
        // `wl-copy` fork a long-lived background daemon to hold the selection,
        // and that daemon inherits the stderr pipe. With a piped stderr,
        // `wait_with_output` would block until that daemon exits (i.e. until
        // the selection is replaced), making every copy appear to hang.
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to start {}: {}", command, error))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("{} stdin was unavailable", command))?;
    stdin
        .write_all(text.as_bytes())
        .await
        .map_err(|error| format!("failed to write to {}: {}", command, error))?;
    drop(stdin);
    // Wait only for the foreground process to exit. `wl-copy` daemonizes
    // after reading stdin and setting the selection, so this returns within
    // milliseconds; it must not wait for the background daemon (which would
    // block until the selection is replaced).
    let status = child
        .wait()
        .await
        .map_err(|error| format!("{} failed: {}", command, error))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} exited with {}", command, status))
    }
}

/// Copy text to the system clipboard using arboard.
async fn copy_system(text: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // arboard's Clipboard is not Send, so we do the work in a blocking task.
    let text = text.to_string();
    tokio::task::spawn_blocking(move || {
        let mut clipboard = arboard::Clipboard::new()?;
        clipboard.set_text(text)?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    Ok(())
}

/// Simple base64 encoder (no external crate needed).
fn base64_encode_bytes(input: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input;
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let b = match chunk.len() {
            1 => [chunk[0], 0, 0],
            2 => [chunk[0], chunk[1], 0],
            3 => [chunk[0], chunk[1], chunk[2]],
            _ => unreachable!(),
        };
        let n = (b[0] as usize) << 16 | (b[1] as usize) << 8 | (b[2] as usize);
        out.push(TABLE[(n >> 18) & 0x3F] as char);
        out.push(TABLE[(n >> 12) & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) & 0x3F]
        } else {
            b'='
        } as char);
        out.push(if chunk.len() > 2 {
            TABLE[n & 0x3F]
        } else {
            b'='
        } as char);
    }

    out
}

/// Encode a UTF-8 string to base64.
fn base64_encode(input: &str) -> String {
    base64_encode_bytes(input.as_bytes())
}

/// What `read()` found on the system clipboard.
#[derive(Debug, Clone)]
pub enum ClipboardRead {
    /// An image (PNG bytes) plus its MIME type.
    Image { data: Vec<u8>, mime: String },
    /// File references (e.g. files copied in a file manager). These are
    /// absolute local paths; remote or unresolvable entries are dropped at
    /// read time.
    Files(Vec<PathBuf>),
    /// Plain text.
    Text(String),
    /// The clipboard is empty or unreadable.
    Empty,
}

/// Read the system clipboard, preferring an image, then file references,
/// then text (mirrors opencode).
///
/// Image bytes come straight from the platform clipboard owner (`wl-paste` on
/// Wayland, `xclip` on X11, `osascript` on macOS) as PNG, so no re-encoding is
/// needed. File references are read from the file-list flavors a file manager
/// puts on the clipboard when you "copy" a file (`text/uri-list` on Linux,
/// Finder's file URL on macOS, `CF_HDROP` via PowerShell on Windows). Text
/// falls back to `arboard`. Everything runs off the event loop: external
/// commands are awaited asynchronously and arboard (which is `!Send`) runs in
/// a blocking task.
pub async fn read() -> ClipboardRead {
    if let Some(bytes) = read_image_bytes().await {
        return ClipboardRead::Image {
            data: bytes,
            mime: "image/png".to_string(),
        };
    }
    let files = read_file_paths().await;
    if !files.is_empty() {
        return ClipboardRead::Files(files);
    }
    match read_text().await {
        Ok(Some(text)) if !text.is_empty() => ClipboardRead::Text(text),
        _ => ClipboardRead::Empty,
    }
}

/// Encode raw bytes as a base64 string (used to build image data URLs/parts).
pub fn base64_image(bytes: &[u8]) -> String {
    base64_encode_bytes(bytes)
}

async fn read_image_bytes() -> Option<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        if let Some(bytes) = read_command_output("wl-paste", &["-t", "image/png"]).await
            && !bytes.is_empty()
        {
            return Some(bytes);
        }
        if let Some(bytes) = read_command_output(
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        )
        .await
            && !bytes.is_empty()
        {
            return Some(bytes);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(bytes) = read_macos_png().await {
            return Some(bytes);
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = ();
    }
    None
}

/// Read file references from the system clipboard, as produced by "Copy" on
/// one or more files in a file manager. Returns an empty vec when the
/// clipboard holds no recognizable local file list (e.g. plain text from an
/// editor), so callers can fall through to text.
///
/// Per-platform channels (no crate covers all of them — `arboard` has no
/// file-list API and the X11-only crates miss Wayland):
/// - Linux: `text/uri-list` via `wl-paste` / `xclip`, plus GNOME's
///   `x-special/gnome-copied-files` ("copy\nfile://…") fallback.
/// - macOS: Finder's file URLs via `osascript`.
/// - Windows: `CF_HDROP` via PowerShell's `Get-Clipboard -Format FileDropList`.
async fn read_file_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        // Prefer uri-list (both KDE and GNOME provide it). GNOME also
        // exposes a line-prefixed flavor; try it when the uri-list target
        // is absent. A bare-text fallback catches KDE builds that only put
        // a plain local path on the clipboard.
        if let Some(bytes) = read_command_output("wl-paste", &["-t", "text/uri-list"]).await
            && let Some(paths) = parse_uri_list(&String::from_utf8_lossy(&bytes))
        {
            return paths;
        }
        if let Some(bytes) = read_command_output(
            "xclip",
            &["-selection", "clipboard", "-t", "text/uri-list", "-o"],
        )
        .await
            && let Some(paths) = parse_uri_list(&String::from_utf8_lossy(&bytes))
        {
            return paths;
        }
        for (command, args) in [
            ("wl-paste", &["-t", "x-special/gnome-copied-files"][..]),
            (
                "xclip",
                &[
                    "-selection",
                    "clipboard",
                    "-t",
                    "x-special/gnome-copied-files",
                    "-o",
                ][..],
            ),
        ] {
            if let Some(bytes) = read_command_output(command, args).await
                && let Some(paths) = parse_gnome_copied_files(&String::from_utf8_lossy(&bytes))
            {
                return paths;
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(paths) = read_macos_file_urls().await {
            return paths;
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(bytes) = read_command_output(
            "powershell",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$d = Get-Clipboard -Format FileDropList; if ($d) { $d | ForEach-Object { $_.FullName } }",
            ],
        )
        .await
        {
            return String::from_utf8_lossy(&bytes)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .filter(|path| path.is_absolute() && path.exists())
                .collect();
        }
    }
    Vec::new()
}

/// Decode a percent-encoded URI component (`%20` etc.). Invalid escapes pass
/// through verbatim; UTF-8 is lossy-decoded. Operates on raw bytes so a
/// truncated escape before a multi-byte character can never slice across a
/// char boundary. Small inline helper keeps the reader dependency-free.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(high << 4 | low);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Hex digit value, or `None` for anything else.
fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Convert one `file://` URI to a local absolute path, or `None` when it
/// points at a remote host / isn't a file URI.
fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Split `{host}/{path}` at the first slash: `file:///a` has an empty
    // host and path `/a`; `file://server/share` is remote and dropped.
    let slash = rest.find('/')?;
    let host = &rest[..slash];
    if !host.is_empty() && host != "localhost" {
        return None;
    }
    let decoded = percent_decode(&rest[slash..]);
    // file:///C:/… decodes to `/C:/…`, which is not a rooted Windows path;
    // drop the leading slash when a drive letter follows.
    #[cfg(target_os = "windows")]
    let decoded = {
        if let Some(drive) = decoded.strip_prefix('/')
            && drive
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
            && drive.as_bytes().get(1) == Some(&b':')
        {
            drive.to_string()
        } else {
            decoded
        }
    };
    let path = Path::new(&decoded);
    path.is_absolute().then(|| path.to_path_buf())
}

/// Parse an RFC 2483 `text/uri-list` payload into existing local file paths.
/// Returns `None` when nothing in the payload is a resolvable local file —
/// that lets callers fall through to other flavors instead of swallowing a
/// non-file paste as an empty attachment.
#[allow(dead_code)]
fn parse_uri_list(payload: &str) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for line in payload.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(path) = file_uri_to_path(line)
            && path.exists()
        {
            paths.push(path);
        }
    }
    (!paths.is_empty()).then_some(paths)
}

/// Parse GNOME's `x-special/gnome-copied-files` payload: an operation verb
/// ("copy" / "cut") followed by one URI per line.
#[allow(dead_code)]
fn parse_gnome_copied_files(payload: &str) -> Option<Vec<PathBuf>> {
    let mut lines = payload
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "copy" && *line != "cut");
    let mut paths = Vec::new();
    for line in lines.by_ref() {
        if let Some(path) = file_uri_to_path(line)
            && path.exists()
        {
            paths.push(path);
        }
    }
    (!paths.is_empty()).then_some(paths)
}

/// Interpret a paste *text* payload as file references.
///
/// Bracketed paste (Ctrl+Shift+V — the terminal emulator's own paste,
/// delivered as an `Event::Paste`) is a text-only channel: an image copied
/// in a file manager arrives as the clipboard's text flavor — a `file://`
/// URI or bare absolute path — instead of as image bytes. Returns the
/// referenced files only when *every* non-empty line resolves to an
/// existing local file, so prose or mixed payloads stay ordinary text
/// pastes. Callers gate on at least one supported image before treating
/// the result as an attachment list.
pub(crate) fn paste_text_as_file_paths(payload: &str) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for line in payload.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A line qualifies when it is a `file://` URI or a bare absolute
        // path; anything else makes the payload prose.
        let path = match file_uri_to_path(line) {
            Some(path) => path,
            None => {
                let bare = PathBuf::from(line);
                if !bare.is_absolute() {
                    return None;
                }
                bare
            }
        };
        if !path.is_file() {
            return None;
        }
        paths.push(path);
    }
    (!paths.is_empty()).then_some(paths)
}

#[cfg(target_os = "macos")]
async fn read_macos_file_urls() -> Option<Vec<PathBuf>> {
    let script = "POSIX path of (the clipboard as «class furl»)";
    let output = tokio::process::Command::new("osascript")
        .args(["-e", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // Multi-file copies: Finder leaves the furl flavor holding only the
    // first item, which is good enough for single attachments; treat every
    // returned line as a candidate path.
    let text = String::from_utf8_lossy(&output.stdout);
    let mut paths = Vec::new();
    for line in text.lines() {
        let path = PathBuf::from(line.trim());
        if path.is_absolute() && path.exists() {
            paths.push(path);
        }
    }
    (!paths.is_empty()).then_some(paths)
}

/// Capture a command's stdout as bytes, returning `None` if the command is
/// missing or exits non-zero (e.g. the clipboard holds no image).
#[cfg(any(target_os = "linux", target_os = "windows"))]
async fn read_command_output(command: &str, args: &[&str]) -> Option<Vec<u8>> {
    let output = tokio::process::Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if output.status.success() {
        Some(output.stdout)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
async fn read_macos_png() -> Option<Vec<u8>> {
    let file = std::env::temp_dir().join("mutxpboard.png");
    let path = file.to_str()?.to_string();
    let script = format!(
        "set imageData to the clipboard as \"PNGf\"\n\
         set fileRef to open for access POSIX file \"{path}\" with write permission\n\
         set eof fileRef to 0\n\
         write imageData to fileRef\n\
         close access fileRef"
    );
    let status = tokio::process::Command::new("osascript")
        .args(["-e", &script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .ok()?;
    let result = if status.success() {
        std::fs::read(&file).ok().filter(|bytes| !bytes.is_empty())
    } else {
        None
    };
    let _ = std::fs::remove_file(&file);
    result
}

/// Read plain text from the system clipboard. On Linux the platform-native
/// readers (`wl-paste` on Wayland, `xclip` on X11) are tried first because
/// `arboard` does not reliably see selection contents set through the
/// wl-clipboard protocol (which the copy path uses via `wl-copy`) or some
/// X11 clipboard managers. macOS and other platforms fall through to
/// `arboard`, which talks to NSPasteboard / Win32 directly.
async fn read_text() -> Result<Option<String>, ()> {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some()
            && let Some(bytes) = read_command_output("wl-paste", &[]).await
            && let Ok(text) = String::from_utf8(bytes)
            && !text.is_empty()
        {
            return Ok(Some(text));
        }
        if let Some(bytes) = read_command_output("xclip", &["-selection", "clipboard", "-o"]).await
            && let Ok(text) = String::from_utf8(bytes)
            && !text.is_empty()
        {
            return Ok(Some(text));
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // arboard is the only option on macOS / Windows; the Linux branch
        // above falls through to it too as a last-resort reader.
    }
    tokio::task::spawn_blocking(|| {
        let mut clipboard = arboard::Clipboard::new().map_err(|_| ())?;
        match clipboard.get_text() {
            Ok(text) => Ok(Some(text)),
            Err(_) => Ok(None),
        }
    })
    .await
    .map_err(|_| ())?
}

/// Adapter implementing [`muta_runtime::UiBridge`] by delegating to the TUI's
/// real clipboard path (arboard / wl-copy / OSC52). Used by the slash-command
/// dispatcher (ADR-0037 step 3) so it stays frontend-agnostic.
pub struct TuiClipboard;

#[async_trait::async_trait]
impl muta_runtime::UiBridge for TuiClipboard {
    async fn copy_to_clipboard(&self, text: &str) -> Result<muta_runtime::CopyOutcome, String> {
        copy(text).await.map(|outcome| match outcome {
            CopyOutcome::Native => muta_runtime::CopyOutcome::Native,
            CopyOutcome::Osc52 => muta_runtime::CopyOutcome::Osc52,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64() {
        assert_eq!(base64_encode("hello"), "aGVsbG8=");
        assert_eq!(base64_encode("hello world"), "aGVsbG8gd29ybGQ=");
        assert_eq!(base64_encode(""), "");
    }

    #[tokio::test]
    async fn command_clipboard_receives_utf8_input() {
        copy_with_command("cat", &[], "test😀").await.unwrap();
    }

    #[test]
    fn percent_decode_decodes_escapes_and_passes_malformed_through() {
        assert_eq!(
            percent_decode("/home/user/my%20shot.png"),
            "/home/user/my shot.png"
        );
        // Multi-byte UTF-8 (%E4%B8%AD = 中) survives the byte-level decode.
        assert_eq!(percent_decode("/tmp/%E4%B8%AD.png"), "/tmp/中.png");
        // Truncated or malformed escapes pass through verbatim.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%2"), "%2");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("/plain.png"), "/plain.png");
        // A malformed escape directly before a multi-byte character must not
        // panic slicing mid-char (regression: byte-indexed parser).
        assert_eq!(percent_decode("%zz%E4%B8%AD"), "%zz中");
    }

    #[test]
    fn file_uri_rejects_non_file_and_remote_forms() {
        assert_eq!(
            file_uri_to_path("file:///home/u/pic.png"),
            Some(PathBuf::from("/home/u/pic.png"))
        );
        assert_eq!(file_uri_to_path("https://example.com/a.png"), None);
        // Remote host form (file://host/path) is dropped.
        assert_eq!(file_uri_to_path("file://server/share/pic.png"), None);
        // Percent-encoded spaces resolve to real paths.
        assert_eq!(
            file_uri_to_path("file:///home/u/my%20pic.png"),
            Some(PathBuf::from("/home/u/my pic.png"))
        );
    }

    #[test]
    fn uri_list_parses_comments_blank_lines_and_skips_missing_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("real.txt");
        std::fs::write(&file, b"test").expect("write");
        let payload = format!(
            "# comment line\n\nfile://{}\nfile:///nonexistent/missing.png\n",
            file.display()
        );
        let parsed = parse_uri_list(&payload).expect("at least one resolvable path");
        assert_eq!(parsed, vec![file]);
        // No resolvable local file at all → None so callers fall through.
        assert_eq!(parse_uri_list("file:///nonexistent/missing.png\n"), None);
        assert_eq!(parse_uri_list("# only a comment\n"), None);
    }

    #[test]
    fn gnome_copied_files_strips_operation_verb() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("real.txt");
        std::fs::write(&file, b"test").expect("write");
        let payload = format!("copy\nfile://{}\n", file.display());
        let parsed = parse_gnome_copied_files(&payload).expect("verb + one path");
        assert_eq!(parsed, vec![file]);
        let cut = parse_gnome_copied_files("cut\nfile:///nonexistent/x.png\n");
        assert_eq!(cut, None, "unresolvable-only payload falls through");
    }

    #[test]
    fn paste_text_as_file_paths_requires_every_line_to_be_an_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let png = dir.path().join("shot.png");
        std::fs::write(&png, b"png").expect("write file");
        let note = dir.path().join("note.txt");
        std::fs::write(&note, b"x").expect("write file");

        // Bare path and `file://` URI forms both resolve; trailing newline
        // and multi-line lists are fine.
        assert_eq!(
            paste_text_as_file_paths(png.to_str().unwrap()).as_deref(),
            Some(&[png.clone()][..])
        );
        assert_eq!(
            paste_text_as_file_paths(&format!("file://{}\n", png.display())).as_deref(),
            Some(&[png.clone()][..])
        );
        assert_eq!(
            paste_text_as_file_paths(&format!("{}\n{}", png.display(), note.display())).as_deref(),
            Some(&[png.clone(), note.clone()][..])
        );
        // Prose, missing files, directories, relative paths, URLs and
        // empty payloads all stay text pastes.
        assert_eq!(
            paste_text_as_file_paths(&format!("see {} please", png.display())),
            None
        );
        assert_eq!(
            paste_text_as_file_paths(dir.path().join("missing.png").to_str().unwrap()),
            None
        );
        assert_eq!(paste_text_as_file_paths(dir.path().to_str().unwrap()), None);
        assert_eq!(paste_text_as_file_paths("relative/shot.png"), None);
        assert_eq!(
            paste_text_as_file_paths("https://example.com/shot.png"),
            None
        );
        assert_eq!(paste_text_as_file_paths(""), None);
        assert_eq!(paste_text_as_file_paths("\n  \n"), None);
    }
}
