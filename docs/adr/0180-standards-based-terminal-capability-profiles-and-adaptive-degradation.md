# 0180. Standards-Based Terminal Capability Profiles and Adaptive Degradation

- **Status:** Accepted
- **Date:** 2026-04-18

## Context

`mutx-engine` (ADR-0038) decoupled terminal buffer diffing and escape code emission into an in-house retained grid engine. However, both `mutx-engine` and the application layer `mutx` were built under an unconstrained, modern terminal assumption:
1. **Unconditional ITU-T T.416 Direct Color (24-bit TrueColor):** The engine unconditionally emitted `\x1b[38;2;r;g;bm` and `\x1b[48;2;r;g;bm` whenever a cell carried `Color::Rgb`. In legacy or constrained terminal environments—specifically physical serial lines (`/dev/ttyS*`), Linux Virtual Terminals (`/dev/tty1..6`, `TERM=linux`), cloud serial consoles (AWS/GCP/Proxmox Serial Console), and disaster recovery `getty` sessions (`TERM=vt100`, `vt102`, `vt220`, `dumb`)—these escape sequences are either unrecognized (dumping raw parameter text onto the screen and destroying layout) or silently dropped (leaving foreground text identical to background, rendering output invisible).
2. **Fragile SGR Attributes:** Modern formatting such as italics (`\x1b[3m`, ECMA-48 SGR 3) and faint/dim (`\x1b[2m`, SGR 2) was emitted unconditionally. Linux consoles and VT100 physical terminals have no italic font capability (often rendering it as garbled reverse video or underline) and fail to distinguish faint text from standard text.
3. **Unchecked Terminal Protocol Sequences:** `terminal.rs` and `backend.rs` unconditionally emitted DEC 2026 Synchronized Update sequences (`\x1b[?2026h`) and mouse event capture (`\x1b[?1000h` etc.). On serial getty sessions, mouse tracking sequences trigger corrupted input streams due to unhandled terminal echoes, and unrecognised DEC mode 2026 sequences pollute the visible screen.
4. **Hardcoded Modern Unicode Box Drawing:** Widgets hardcoded heavy Unicode box characters such as `┃` (U+2503) and rounded corners (`╭╮╰╯`). On Linux VT environments using VGA console fonts (limited to CP437/ISO-8859-1) or 7-bit serial terminals, these characters degrade into unreadable replacement glyphs (`?` or `□`).

Ad-hoc "emulator-sniffing" (checking if the terminal is named Kitty, iTerm2, or PowerShell) is rejected as an anti-pattern. Terminal architecture must instead be anchored to formal international and hardware standards: **ECMA-48 / ISO/IEC 6429**, **ITU-T T.416 / ISO/IEC 8613-6**, **ANSI X3.64 / DEC VT100**, and **ANSI X3.4 (ASCII) vs ISO/IEC 10646 (UTF-8)**.

## Decision

Establish formal **Terminal Capability Profiles** in `mutx-engine` and an **Adaptive Degradation Pipeline** spanning both engine and application layers.

### 1. The Three Standard Capability Profiles

1. **`Profile 1: DirectColor` (ITU-T T.416 / UTF-8 / DEC 2026):**
   - Active when `COLORTERM=truecolor | 24bit`.
   - Full 24-bit TrueColor RGB pass-through.
   - DEC 2026 synchronized updates enabled.
   - Complete ECMA-48 SGR attributes (Bold, Dim, Italic, Underline).
   - Full ISO/IEC 10646 UTF-8 glyphs, smooth cosine luminance breathing, and modal alpha-dimming.

2. **`Profile 2: Ansi16` (ECMA-48 / aixterm 16-color):**
   - Active on `TERM=linux` or 16-color capable terminals without TrueColor.
   - **Zero TrueColor emission:** Engine quantizes `Color::Rgb` to the nearest standard 16 ANSI colors (`\x1b[30..37m`, `\x1b[90..97m`).
   - `SGR 3` (Italic) is safely mapped to `SGR 4` (Underline) per Unix documentation tradition.
   - DEC 2026 is disabled (treated as no-op).

3. **`Profile 3: Monochrome` (DEC VT100 / ANSI X3.64 Baseline / getty standard):**
   - Active on `TERM=vt100`, `vt102`, `vt220`, `dumb`, serial devices (`ttyS*`), or when `NO_COLOR` is present.
   - **Zero color emission:** Strips all foreground and background SGR color escapes (`Color::Reset`).
   - **`SGR 7` (Reverse Video) as canonical interaction focus:** In DEC VT100 specifications, reverse video is the universal standard for active selection, focused buttons, and cursor highlights. Cells carrying background tint or highlight are transformed into reverse video.
   - **ASCII character set fallback:** Borders render as `|`, `-`, `+`; spinners render as ASCII rotation sticks `|/-\`.
   - **Zero mouse capture & zero synchronized updates:** Prevents input corruption and escape garbage over serial connections.

### 2. Architecture & Layering

- **`mutx-engine::driver`**: Formalizes the **Terminal Driver Pattern**. Protocol emission is decoupled from the `Backend` coordinator into an explicit `TerminalDriver` enum (`DirectColorDriver`, `Ansi16Driver`, `MonochromeDriver`) implementing `EscapeEmitter`.
  - `DirectColorDriver`: Emits 24-bit TrueColor and DEC Mode 2026 sync updates.
  - `Ansi16Driver`: Quantizes colors to 16 ANSI codes and maps unsupported italics to underline.
  - `MonochromeDriver`: Strictly emits DEC VT100 physical states (SGR 0/1/4/7) with zero color code emission.
- **`mutx-engine::backend`**: `Backend<W>` acts as a pure I/O and diff coordinator, delegating all escape generation to its configured `TerminalDriver`. Hot-loop rendering runs with zero runtime capability branches.
- **`mutx-engine::glyph`**: Introduces centralized [`GlyphSet`] (`UNICODE_GLYPHS` and `ASCII_GLYPHS`). Decouples all box borders, spinners, status dots, and indicators from hardcoded string literals.
- **`mutx-engine::widgets`**: `Block` natively supports `glyph_v` injection and `BorderType::Ascii`.
- **`mutx::terminal`**: Binds `enter_terminal` to `TerminalProfile::detect()`, omitting mouse capture when unsupported.
- **`mutx::theme`**: `Theme` carries `pub glyphs: GlyphSet`, providing high-contrast `Theme::ansi16()` and `Theme::monochrome()` presets while seamlessly propagating adaptive glyphs across all view components.

## Alternatives considered

1. **Vendor-specific emulator sniffing (e.g. `if term.contains("kitty")`):** Rejected. Fragile, violates POSIX and terminal standards, creates vendor lock-in, and fails across SSH multiplexers or non-standard wrappers.
2. **Application-level manual color branching in every view:** Rejected. Requiring hundreds of view components to conditionally pick ANSI vs RGB colors creates massive code churn. Pushing sanitization down into `mutx-engine::Backend` guarantees zero regressions across existing views while preserving high-definition rendering on modern terminals.

## Consequences

### Positive
- **Guaranteed getty & serial line survival:** Running muta over `/dev/ttyS0`, emergency rescue shells, or Linux virtual consoles produces zero escape garbage, no invisible black-on-black text, and robust layout integrity.
- **Compliance with standard `NO_COLOR`:** Fully respects the cross-tool `NO_COLOR` standard.
- **Clean architectural boundary:** All protocol sanitization is isolated within `mutx-engine`, keeping UI components purely declarative.

### Negative / Neutral
- On monochrome terminals, subtle background color distinction is compressed into reverse video and borders.
- Quantization of arbitrary RGB colors to 16 ANSI colors can reduce visual fidelity on legacy Linux virtual consoles, mitigated by the dedicated high-contrast `Theme::ansi16()` preset.

## References

- ECMA-48 5th Edition: *Control Functions for Coded Character Sets*
- ITU-T Recommendation T.416 / ISO/IEC 8613-6: *Character Content Architectures*
- ANSI X3.64-1977 / DEC VT100 User Guide (EK-VT100-UG-003)
- Terminal Working Group: DEC Mode 2026 Synchronized Output
- ADR-0038: *Replace ratatui with an in-house grid + diff rendering engine*
- NO_COLOR specification: <https://no-color.org>
