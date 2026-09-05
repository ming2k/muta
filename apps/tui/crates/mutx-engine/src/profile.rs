//! Standard-based terminal capability profiles and adaptive degradation.
//!
//! # Specification grounding (ADR-0180)
//!
//! Rather than ad-hoc terminal emulator sniffing (e.g. `is_kitty`, `is_iterm2`),
//! capabilities are anchored to formal international and hardware standards:
//!
//! - **ITU-T T.416 / ISO/IEC 8613-6**: Direct Color (`\x1b[38;2;r;g;bm`), 24-bit TrueColor.
//! - **ECMA-48 / ISO/IEC 6429**: 16-color SGR codes and standard text formatting.
//! - **ANSI X3.64 / DEC VT100**: Baseline physical terminal standard for serial/getty lines.
//!   Zero color codes, relying on `SGR 7` (Reverse Video), `SGR 4` (Underline), and
//!   `SGR 1` (Bold).
//! - **ANSI X3.4 (ASCII) vs ISO/IEC 10646 (UTF-8)**: Character set constraints.

use crate::{Color, Modifier, Style};

/// Standard terminal color reproduction capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorStandard {
    /// ITU-T T.416 Direct Color: 24-bit TrueColor via `\x1b[38;2;r;g;bm` and `\x1b[48;2;r;g;bm`.
    #[default]
    DirectColor,
    /// ECMA-48 5th Edition 8-color + aixterm 16-color (`\x1b[30..37m`, `\x1b[90..97m`).
    Ansi16,
    /// DEC VT100 Monochrome: Zero color codes emitted; visual emphasis uses SGR 7 (Reverse Video).
    Monochrome,
}

/// Standard terminal character set capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CharsetStandard {
    /// ISO/IEC 10646 (UTF-8) full character set including box drawing and symbols.
    #[default]
    Utf8,
    /// ANSI X3.4 7-bit US-ASCII safe fallback (+, -, |, etc.).
    Ascii,
}

/// A deterministic capability profile governing escape sequence emission and widget degradation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalProfile {
    pub color_standard: ColorStandard,
    pub charset_standard: CharsetStandard,
    pub supports_italic: bool,
    pub supports_sync_update: bool,
    pub supports_mouse: bool,
}

impl Default for TerminalProfile {
    fn default() -> Self {
        Self::detect()
    }
}

impl TerminalProfile {
    /// Profile 1: Full ITU-T T.416 Direct Color + UTF-8 + Mode 2026.
    pub const fn direct_color() -> Self {
        Self {
            color_standard: ColorStandard::DirectColor,
            charset_standard: CharsetStandard::Utf8,
            supports_italic: true,
            supports_sync_update: true,
            supports_mouse: true,
        }
    }

    /// Profile 2: ECMA-48 Standard 16-Color profile.
    pub const fn ecma48_ansi16() -> Self {
        Self {
            color_standard: ColorStandard::Ansi16,
            charset_standard: CharsetStandard::Utf8,
            supports_italic: false,
            supports_sync_update: false,
            supports_mouse: true,
        }
    }

    /// Profile 3: DEC VT100 Monochrome baseline profile (for getty, serial, rescue).
    pub const fn dec_vt100_monochrome() -> Self {
        Self {
            color_standard: ColorStandard::Monochrome,
            charset_standard: CharsetStandard::Ascii,
            supports_italic: false,
            supports_sync_update: false,
            supports_mouse: false,
        }
    }

    /// Detect terminal capability profile from environment variables and standards.
    pub fn detect() -> Self {
        let term = std::env::var("TERM").unwrap_or_default();
        let colorterm = std::env::var("COLORTERM").unwrap_or_default();
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let color_override = std::env::var("MUTA_COLOR_STANDARD").ok();
        let charset_override = std::env::var("MUTA_CHARSET_STANDARD").ok();

        Self::for_env(
            &term,
            &colorterm,
            no_color,
            color_override.as_deref(),
            charset_override.as_deref(),
        )
    }

    /// Pure resolution logic for testability and deterministic overrides.
    pub fn for_env(
        term: &str,
        colorterm: &str,
        no_color: bool,
        color_override: Option<&str>,
        charset_override: Option<&str>,
    ) -> Self {
        // Resolve ColorStandard
        let color_standard = if let Some(ov) = color_override {
            match ov {
                "direct" | "truecolor" | "24bit" => ColorStandard::DirectColor,
                "ansi16" | "16" | "basic" => ColorStandard::Ansi16,
                "monochrome" | "mono" | "0" => ColorStandard::Monochrome,
                _ => ColorStandard::DirectColor,
            }
        } else if no_color {
            // https://no-color.org standard
            ColorStandard::Monochrome
        } else if is_vt100_or_serial(term) {
            ColorStandard::Monochrome
        } else if colorterm == "truecolor" || colorterm == "24bit" {
            ColorStandard::DirectColor
        } else if term == "linux" || term.contains("16color") || term == "xterm" {
            ColorStandard::Ansi16
        } else if term.is_empty() || term == "dumb" {
            ColorStandard::Monochrome
        } else {
            // Default to DirectColor for modern pseudo-terminals (xterm-256color, tmux, etc.)
            ColorStandard::DirectColor
        };

        // Resolve CharsetStandard
        let charset_standard = if let Some(ov) = charset_override {
            match ov {
                "ascii" => CharsetStandard::Ascii,
                "utf8" => CharsetStandard::Utf8,
                _ => CharsetStandard::Utf8,
            }
        } else if is_vt100_or_serial(term) || term == "dumb" {
            CharsetStandard::Ascii
        } else {
            CharsetStandard::Utf8
        };

        let supports_italic = matches!(color_standard, ColorStandard::DirectColor)
            && term != "linux"
            && !term.starts_with("vt");

        let supports_sync_update = matches!(color_standard, ColorStandard::DirectColor)
            && !is_vt100_or_serial(term)
            && term != "dumb";

        let supports_mouse = !is_vt100_or_serial(term) && term != "dumb";

        Self {
            color_standard,
            charset_standard,
            supports_italic,
            supports_sync_update,
            supports_mouse,
        }
    }

    /// Sanitize a requested [`Style`] to strictly adhere to the terminal profile's constraints.
    pub fn sanitize_style(&self, mut s: Style) -> Style {
        match self.color_standard {
            ColorStandard::DirectColor => {
                // Passthrough
            }
            ColorStandard::Ansi16 => {
                s.fg = quantize_to_ansi16(s.fg);
                s.bg = quantize_to_ansi16(s.bg);
            }
            ColorStandard::Monochrome => {
                // In DEC VT100 Monochrome:
                // No color codes may be emitted. If the cell has visual emphasis
                // (non-reset background, or explicit REVERSE), transform to Reverse Video.
                let has_emphasis = s.bg != Color::Reset || s.add.contains(Modifier::REVERSE);
                s.fg = Color::Reset;
                s.bg = Color::Reset;
                if has_emphasis {
                    s.add.insert(Modifier::REVERSE);
                }
                // Strip DIM in monochrome as it is unsupported on VT100
                s.add.remove(Modifier::DIM);
            }
        }

        if !self.supports_italic && s.add.contains(Modifier::ITALIC) {
            s.add.remove(Modifier::ITALIC);
            s.add.insert(Modifier::UNDERLINE);
        }

        s
    }
}

/// Helper to identify VT100 physical terminals, serial devices, and unadorned consoles.
fn is_vt100_or_serial(term: &str) -> bool {
    let lower = term.to_ascii_lowercase();
    lower == "vt100"
        || lower == "vt102"
        || lower == "vt220"
        || lower.starts_with("ttys")
        || lower.starts_with("ttyusb")
        || lower == "serial"
}

/// Quantize any [`Color`] down to the ECMA-48 / aixterm 16 ANSI color space.
pub fn quantize_to_ansi16(color: Color) -> Color {
    match color {
        Color::Reset => Color::Reset,
        Color::Rgb(r, g, b) => nearest_ansi16(r, g, b),
        named => named,
    }
}

/// Find nearest ANSI 16 color using perceptual weighted Euclidean distance.
fn nearest_ansi16(r: u8, g: u8, b: u8) -> Color {
    const ANSI16_TABLE: &[(Color, u8, u8, u8)] = &[
        (Color::Black, 0, 0, 0),
        (Color::Red, 170, 0, 0),
        (Color::Green, 0, 170, 0),
        (Color::Yellow, 170, 85, 0),
        (Color::Blue, 0, 0, 170),
        (Color::Magenta, 170, 0, 170),
        (Color::Cyan, 0, 170, 170),
        (Color::Gray, 170, 170, 170),
        (Color::DarkGray, 85, 85, 85),
        (Color::LightRed, 255, 85, 85),
        (Color::LightGreen, 85, 255, 85),
        (Color::LightYellow, 255, 255, 85),
        (Color::LightBlue, 85, 85, 255),
        (Color::LightMagenta, 255, 85, 255),
        (Color::LightCyan, 85, 255, 255),
        (Color::White, 255, 255, 255),
    ];

    let mut best_color = Color::White;
    let mut best_dist = u32::MAX;

    for &(c, pr, pg, pb) in ANSI16_TABLE {
        // Redmean-style weighted perceptual distance:
        let r_mean = (r as i32 + pr as i32) / 2;
        let dr = r as i32 - pr as i32;
        let dg = g as i32 - pg as i32;
        let db = b as i32 - pb as i32;
        let dist = (((512 + r_mean) * dr * dr) >> 8)
            + 4 * dg * dg
            + (((767 - r_mean) * db * db) >> 8);

        let dist_u32 = dist as u32;
        if dist_u32 < best_dist {
            best_dist = dist_u32;
            best_color = c;
        }
    }

    best_color
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vt100_detection() {
        let p = TerminalProfile::for_env("vt100", "", false, None, None);
        assert_eq!(p.color_standard, ColorStandard::Monochrome);
        assert_eq!(p.charset_standard, CharsetStandard::Ascii);
        assert!(!p.supports_italic);
        assert!(!p.supports_sync_update);
        assert!(!p.supports_mouse);
    }

    #[test]
    fn test_no_color_forces_monochrome() {
        let p = TerminalProfile::for_env("xterm-256color", "truecolor", true, None, None);
        assert_eq!(p.color_standard, ColorStandard::Monochrome);
    }

    #[test]
    fn test_linux_console_detection() {
        let p = TerminalProfile::for_env("linux", "", false, None, None);
        assert_eq!(p.color_standard, ColorStandard::Ansi16);
        assert!(!p.supports_italic);
        assert!(!p.supports_sync_update);
    }

    #[test]
    fn test_direct_color_detection() {
        let p = TerminalProfile::for_env("xterm-256color", "truecolor", false, None, None);
        assert_eq!(p.color_standard, ColorStandard::DirectColor);
        assert_eq!(p.charset_standard, CharsetStandard::Utf8);
        assert!(p.supports_italic);
        assert!(p.supports_sync_update);
        assert!(p.supports_mouse);
    }

    #[test]
    fn test_monochrome_sanitizes_colors_to_reverse() {
        let p = TerminalProfile::dec_vt100_monochrome();
        let s = Style::default()
            .fg(Color::Rgb(255, 0, 0))
            .bg(Color::Rgb(10, 20, 30));
        let sanitized = p.sanitize_style(s);
        assert_eq!(sanitized.fg, Color::Reset);
        assert_eq!(sanitized.bg, Color::Reset);
        assert!(sanitized.add.contains(Modifier::REVERSE));
    }

    #[test]
    fn test_italic_converts_to_underline_when_unsupported() {
        let p = TerminalProfile::ecma48_ansi16();
        let s = Style::default().add_modifier(Modifier::ITALIC);
        let sanitized = p.sanitize_style(s);
        assert!(!sanitized.add.contains(Modifier::ITALIC));
        assert!(sanitized.add.contains(Modifier::UNDERLINE));
    }

    #[test]
    fn test_quantize_rgb_to_ansi16() {
        assert_eq!(quantize_to_ansi16(Color::Rgb(255, 255, 255)), Color::White);
        assert_eq!(quantize_to_ansi16(Color::Rgb(0, 0, 0)), Color::Black);
        assert_eq!(quantize_to_ansi16(Color::Rgb(170, 0, 0)), Color::Red);
        assert_eq!(quantize_to_ansi16(Color::Rgb(255, 85, 85)), Color::LightRed);
        assert_eq!(quantize_to_ansi16(Color::Reset), Color::Reset);
    }
}
