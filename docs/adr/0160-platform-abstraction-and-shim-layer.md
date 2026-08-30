# 0160. Platform Abstraction and Shim Layer (PAL & Shims)

- **Status:** Accepted
- **Date:** 2026-09-25

## Context

Previous iterations of the codebase suffered from platform leakages across business layers (`muta-agent`, `muta-runtime`, `mutx`):
1. **Scattered Platform Conditions**: Hand-rolled `#[cfg(unix)]` / `#[cfg(windows)]` checks were scattered in command runners, process spawning, terminal sessions, and file operations.
2. **Duplicated Subsystem Implementations**: `mutx` contained 700+ lines of low-level clipboard handling (Wayland, X11, AppKit, Windows CF_HDROP, OSC 52) and 130+ lines of browser launching logic, neither of which was accessible to other crates (e.g. `muta-providers` OAuth or daemon services).
3. **Implicit Workarounds and Incomplete Shims**: Inode inspections (`/proc/<pid>/exe`), process group allocations (`process_group(0)`), symlink operations, and path resolution across Linux XDG, macOS Apple hierarchies, and Windows Known Folders lacked a unified architectural foundation.

## Decision

Establish an authoritative, zero-compromise **Platform Abstraction Layer and Shim Layer (PAL & Shims)** in `muta-platform` and enforce strict separation across the entire workspace:

1. **Normalized Directory Layout (`muta-platform::paths`)**:
   - Introduce `PlatformPaths` trait and `StandardLayout` resolving standard user paths for Linux (XDG), macOS (Apple standard directories), and Windows (`%APPDATA%`, `%LOCALAPPDATA%`).
   - Eliminate direct ad-hoc path manipulation in business layers.

2. **Universal System Opener with Headless Fallback (`muta-platform::opener`)**:
   - Centralize URL and file opening across macOS (`open`), Windows (`cmd /c start`), Linux/WSL (`wslview`, `xdg-open`, `gio`, browsers).
   - In headless, SSH, or container environments, automatically provide a fallback shim formatting **OSC 8 terminal hyperlinks** and returning `OpenOutcome::Headless`.
   - Remove redundant `webbrowser` dependency from `mutx`.

3. **Platform Clipboard Subsystem (`muta-platform::clipboard`)**:
   - Downstream low-level clipboard operations to `muta-platform::clipboard`, supporting text, PNG image extraction, and file drop lists across Wayland, X11, macOS AppKit, Windows, and Terminal OSC 52.
   - `mutx` implements `UiBridge` by clean delegation to the platform layer.

4. **Cross-Platform Shell Dialects and Quoting (`muta-platform::shell`)**:
   - Formally model `ShellDialect` (`Posix`, `PowerShell`) with dialect-aware argument quoting (`quote_arg`) and sentinel protocol formatting.
   - Inject UTF-8 console output encoding shims on PowerShell (`[Console]::OutputEncoding = [System.Text.Encoding]::UTF8`) to prevent encoding corruption.

5. **Kernel-Enforced Process Tree Lifecycle (`muta-platform::process`)**:
   - Unify owned process tree execution (`spawn_owned`) with Windows **Job Objects** (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) and Unix Process Groups (`setpgid`).
   - Move process executable image verification (`process_image_matches_path`) into the platform layer, removing raw `/proc/<pid>/exe` logic from `muta-runtime`.

6. **Cross-Platform Filesystem & Symlink Shims (`muta-platform::fs`)**:
   - Provide platform-agnostic `symlink_file`, `symlink_dir`, `is_symlink`, and `read_link`, eliminating OS-specific `std::os::unix/windows` imports from business logic.

7. **Multi-Driver Sandbox HAL (`muta-platform::workspace_sandbox`)**:
   - Introduce `SandboxDriverKind` for explicit capability negotiation (`LinuxBubblewrap`, `MacosSeatbelt`, `WindowsRestrictedToken`, `Unavailable`) while preserving strict Fail-Closed security guarantees.

## Consequences

- **Zero Platform Leakage**: Business crates (`muta-agent`, `muta-runtime`, `mutx`, `muta-persistence`) operate exclusively over semantic traits and platform-neutral APIs.
- **Maintainability & Portability**: New OS targets or execution environments (e.g. WASM, micro-VMs) only require implementing the corresponding PAL adapter/driver.
- **Fail-Closed Security**: Eliminates silent no-op fallbacks and ensures consistent security semantics on all supported platforms.
