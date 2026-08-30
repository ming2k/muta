# muta-platform

Authoritative, zero-compromise Platform Abstraction Layer and Shim Layer (PAL & Shims) for muta.

## Subsystems

1. **`paths` (`PlatformPaths`, `StandardLayout`)**
   - Standardized cross-platform directory resolution compliant with Linux XDG, macOS Apple standard hierarchies, and Windows Known Folders (`%APPDATA%`, `%LOCALAPPDATA%`).

2. **`opener` (`SystemOpener`)**
   - Universal URL and file opener supporting macOS `open`, Windows `cmd /c start`, Linux `$BROWSER` / `wslview` / `xdg-open` / `gio`.
   - Automatic fallback shim formatting **OSC 8 terminal hyperlinks** in headless SSH / container environments.

3. **`clipboard` (`PlatformClipboard`)**
   - Asynchronous system clipboard reader and writer supporting Wayland (`wl-copy`/`wl-paste`), X11 (`xclip`), macOS AppKit, Windows `CF_HDROP`, and Terminal OSC 52 sequence injection.

4. **`shell` (`ShellDialect`)**
   - Explicit dialect modeling (POSIX `sh`/`bash`, PowerShell Core `pwsh` / Windows PowerShell), dialect-aware argument quoting (`quote_arg`), and UTF-8 console output enforcement shims.

5. **`process` (`spawn_owned`, `configure_owned`, `OwnedProcessTree`)**
   - Kernel-enforced process tree containment (Windows Job Objects with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and Unix Process Groups).
   - Executable binary image comparison (`process_image_matches_path`).

6. **`fs` (`symlink_file`, `symlink_dir`, `is_symlink`, `read_link`)**
   - Platform-neutral symbolic link creation and query operations.

7. **`secure_file` & `lock`**
   - Private file creation and atomic replacement (Unix `0600`/`0700` vs Windows SDDL/DACL).
   - Advisory cross-process file locking (Unix `flock` vs Windows `LockFileEx`).

8. **`ipc` (`LocalEndpoint`, `LocalListener`)**
   - Local IPC transport over Unix Domain Sockets and Windows Named Pipes.

9. **`workspace_sandbox` (`SandboxDriverKind`, `driver_kind`)**
   - Multi-driver workspace isolation HAL preserving strict Fail-Closed security.
