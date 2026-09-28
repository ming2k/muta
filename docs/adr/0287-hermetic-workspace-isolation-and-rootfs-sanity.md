---
id: ADR-0287
title: "Hermetic Workspace Containment: Retiring Fragile Distro-Path Lists in Favor of Read-Only Host Overlays and Native Container Architecture"
status: accepted
date: 2026-10-28
scope: platform/sandbox, agent/tools, contracts/execution
superseded_by: null
negative_knowledge: true
---

# 0287. Hermetic Workspace Containment: Retiring Fragile Distro-Path Lists in Favor of Read-Only Host Overlays and Native Container Architecture

- **Status:** Accepted
- **Date:** 2026-10-28
- **Scope:** `muta-platform` (`workspace_sandbox`), `muta-agent` (`execute_command`), `muta-contracts` (`execution`)
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0160](0160-platform-abstraction-and-shim-layer.md) (Platform abstraction and sandbox HAL), [ADR-0286](0286-hermetic-headless-execution-and-interactive-process-containment.md) (Hermetic headless execution and terminal isolation)

---

## Context and Problem Statement

`muta-platform::workspace_sandbox` was originally introduced to provide fail-closed filesystem and process isolation for unattended commands. However, its implementation in Linux via `bubblewrap` (`bwrap`) suffered from severe architectural overreach:

1. **The "Homebrew Container Runtime" Anti-Pattern**:
   `workspace_sandbox.rs` attempted to assemble an ad-hoc Linux root filesystem from an empty `tmpfs` root (`--tmpfs /`) by hardcoding piecemeal lists of host directories and configuration files:
   - Dynamic library paths: `/usr`, `/bin`, `/lib`, `/lib64`, `/lib32`
   - Hardcoded distro-specific configuration files: `/etc/alternatives` (Debian), `/etc/crypto-policies` (Fedora), `/etc/pki` (RHEL), `/etc/ssl/certs`, `/etc/resolv.conf`.
2. **Distro Fragility**:
   Because Linux distributions diverge widely in filesystem layouts, this hand-assembled list breaks completely on non-traditional systems (such as NixOS, Guix, and Alpine), where system binaries reside outside `/usr/bin` (e.g. `/nix/store`).
3. **Developer Toolchain Amnesia**:
   By resetting `$HOME` to `/tmp/muta-home`, stripping ambient environment variables, and restricting `$PATH` to `/usr/bin`, user-installed development toolchains (`~/.cargo/bin`, `~/.rustup`, `~/.nvm`, `~/.pyenv`, `~/.local/bin`) become completely missing. Agent tasks attempting to run `cargo test` or `npm test` inside the sandbox fail with `command not found`.
4. **Glibc NSS & User Identity Failure**:
   Hiding `/etc/passwd` breaks standard C library identity calls (`getpwuid(getuid())`). Git commits fail with identity auto-detection errors, and Cargo fails to determine current user credentials.
5. **False Multi-Driver Claims**:
   The code defined `SandboxDriverKind` variants for `MacosSeatbelt` and `WindowsRestrictedToken`, but both unconditionally returned `Unavailable`, creating dead boilerplate and misleading API contracts.

Maintaining a bespoke, hardcoded Linux filesystem catalog inside an application-level agent repository is an unmaintainable boundary violation.

---

## Decision

We replace the fragile, handcrafted file-by-file assembly with an uncompromised, two-tier architecture:

### 1. Read-Only Host Overlay (`--ro-bind / /`) for Local Sandbox Containment
When local `bubblewrap` process containment is active:
- **Immutable Host Base**: The entire host filesystem is mounted strictly read-only (`--ro-bind / /`).
  - Guarantees 100% compatibility across all Linux distributions (NixOS, Fedora, Arch, Debian, Ubuntu) without maintaining any hardcoded distro path lists.
  - Naturally preserves glibc NSS databases (`/etc/passwd`, `/etc/hosts`), system SSL certificates, and user development toolchains (`cargo`, `node`, `python`).
  - Physically prevents any modification to host binaries, libraries, or system files (`EROFS: Read-only file system`).
- **Writable Workspace Slices**: Only the canonical workspace root and explicitly configured additional roots are bind-mounted read-write (`--bind <root> <root>`).
- **Ephemeral Scratch Storage**: `/tmp` is mounted as an isolated memory tmpfs (`--tmpfs /tmp`).
- **Namespace & Capability Isolation**:
  - Drops all Linux capabilities (`--cap-drop ALL`).
  - Unshares PID, IPC, and UTS namespaces (`--unshare-pid`, `--unshare-ipc`, `--unshare-uts`).
  - Network isolation (`--unshare-net`) remains enforceable on demand.
  - Process lifecycle bounded to parent (`--die-with-parent`).

### 2. Elimination of Phantom Driver Boilerplate
- Remove misleading stubs for `MacosSeatbelt` and `WindowsRestrictedToken` that provided zero implementation.
- Real multi-tenant or untrusted external isolation is explicitly delegated to standard OCI containers (Docker / Podman) with complete container images, rather than emulating container runtimes in user space.

---

## Alternatives Considered

- **Continuing to expand the hardcoded file allowlist**:
  *Rejected*: An infinite maintenance sink. Every new Linux distribution, SSL certificate path, or toolchain manager introduces new failure modes.
- **Requiring Docker/Podman for all commands**:
  *Rejected*: Excessive overhead for local interactive development. Local developer workflows should operate via ADR-0286 Hermetic Host Execution, with `bwrap` providing lightweight overlay protection when requested.

---

## Consequences

### Positive
- Works universally on all Linux distributions (including NixOS, Arch, Fedora, and Debian) with zero hardcoded path lists.
- Full access to developer toolchains (`cargo`, `node`, `python`, `git`) while keeping the entire host filesystem mathematically read-only.
- Eliminates hundreds of lines of fragile boilerplate and parameters.
- 100% fail-closed containment: writes outside the workspace return `Read-only file system`.
