# Workspace Security and Trust Architecture

Muta's security architecture enforces safe autonomous agent operations through
three strictly orthogonal domains: **Authority & Trust**, **Interaction Posture**,
and **Execution Runtime & Containment**.

```text
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│ 1. Authority & Trust Domain  ──  Canonical Command: /trust                               │
│    • What authority does this project have?                                             │
│    • Split into Workspace Execution Authority and Content-Bound Extension Trust         │
├─────────────────────────────────────────────────────────────────────────────────────────┤
│ 2. Interaction Posture Domain  ──  Canonical Command: /autopilot [on|off]               │
│    • How does the agent interact with humans during tool execution?                     │
│    • Decides whether missing authority prompts interactively or fails immediately       │
├─────────────────────────────────────────────────────────────────────────────────────────┤
│ 3. Execution Runtime & Containment Domain  ──  Runtime Environment                      │
│    • How does the underlying operating system spawn and isolate process execution?      │
│    • Host execution (Native Shell) vs. Physical Sandbox (Linux bubblewrap)              │
└─────────────────────────────────────────────────────────────────────────────────────────┘
```

---

## 1. The Three Orthogonal Domains

### Domain 1: Authority & Trust (`/trust`)
Authority determines **what resources and actions are permitted**. Muta decouples
authority into two independent axes:
- **Workspace Execution Authority**: Governs file modifications, test runs,
  compilation, and dependency management within the workspace root.
- **Project Extension Trust**: Governs whether project-authored control-plane
  contributions (`.muta/config.toml` MCP servers and hooks, `.muta/skills/`,
  `.agents/skills/`, `.claude/skills/`, and `.muta/commands/`) are loaded.

Authority decisions are durable and persisted in `workspace_security.json`, keyed
by the canonical exact workspace root.

### Domain 2: Interaction Posture (`/autopilot`)
Interaction posture determines **whether human intervention is solicited**.
- In **attended mode** (`/autopilot off`), missing grants prompt the human
  operator via interactive modals (once/always/reject).
- In **autopilot mode** (`/autopilot on`), the agent advances autonomously
  through approved tools; missing grants fail immediately without deadlocking
  the unattended run.

> **Core Invariant**: Autopilot is an interaction modifier, **never** an authority
> grant. An operation that is denied or unauthorized in attended mode remains
> denied in autopilot mode.

### Domain 3: Execution Runtime & Containment
Execution runtime determines **how processes execute at the OS level**:
- **Host Execution (Native Shell)**: The default for trusted workspace development.
  Commands execute in the workspace root with access to the developer's installed
  toolchains (`rustup`, `cargo`, `nvm`, `python`, `pnpm`), global package caches,
  and local daemons.
- **Physical Workspace Sandbox (bubblewrap)**: An optional fail-closed physical
  process sandbox on Linux using unprivileged user/PID/IPC/network namespaces,
  dropped capabilities, and an ephemeral home directory.

---

## 2. Two-Axis Authority Model

Traditional IDEs (e.g. VS Code Workspace Trust) rely on binary, path-only trust.
If a developer trusts a folder, any script or task introduced later via a `git pull`
or malicious PR receives unconditional execution privileges.

Muta eliminates this vulnerability by separating execution authority from
content-bound extension trust:

```text
                        ┌───────────────────────────────┐
                        │   Workspace Security Model    │
                        └───────────────┬───────────────┘
                                        │
             ┌──────────────────────────┴──────────────────────────┐
             ▼                                                     ▼
┌─────────────────────────────┐                       ┌─────────────────────────────┐
│ Workspace Execution Profile │                       │  Project Extension Trust    │
├─────────────────────────────┤                       ├─────────────────────────────┤
│ • unknown     (unconfigured)│                       │ • absent       (no ext)     │
│ • restricted  (read-only)   │                       │ • quarantined  (untrusted)  │
│ • development (full dev)    │                       │ • trusted      (attested)   │
│                             │                       │ • changed      (hash drift) │
└─────────────────────────────┘                       └─────────────────────────────┘
```

### Axis 1: Workspace Execution Profiles
1. **`unknown`**: Initial state for newly opened directories. Preflight check
   prevents model rounds or direct-shell executions until the user makes an
   explicit choice (`/trust` or `/trust readonly`).
2. **`restricted`**: Read-oriented posture. Safe for surveying codebases.
   Modifications or command executions require explicit individual permission.
3. **`development`**: Standard development posture. File writes, dependency
   installation, and test execution within the workspace are pre-authorized.

### Axis 2: Content-Bound Extension Trust
Project-authored extensions (MCP servers, lifecycle hooks, custom skills, prompt
commands) can alter the agent's core decision loop. Muta binds extension trust to
a cryptographic **SHA-256 content digest**:
- **Cryptographic Attestation**: The hash includes all files, permissions, and
  contents across `.muta/`, `.agents/skills/`, and `.claude/skills/`. Symlinks are
  rejected to prevent target escape.
- **Automatic Quarantine on Drift**: If a team member or upstream PR modifies a
  hook or MCP configuration, Muta detects the digest mismatch upon the next run
  and automatically transitions from `trusted` to `changed` (quarantined).

---

## 3. The `/trust` Command Reference

The `/trust` command provides an ergonomic, unified interface to manage workspace
security without exposing low-level container details:

| Command | Action & Scope |
| :--- | :--- |
| **`/trust`** (or `/trust all`) | **Full Development Trust**: Sets execution profile to `development` and trusts project extensions if present. |
| **`/trust workspace`** | **Execution Only**: Pre-authorizes development while keeping project MCP servers, hooks, and skills quarantined. |
| **`/trust extensions`** | **Extensions Only**: Attests and trusts the exact current content of project MCP servers, hooks, and skills. |
| **`/trust readonly`** | **Restricted Mode**: Sets workspace to restricted read-only analysis without execution authority. |
| **`/trust status`** | **Security Panel**: Displays comprehensive status (root path, execution profile, extension digest state, and sandbox capability). |
| **`/trust revoke`** (or `/untrust`) | **Revoke All**: Resets execution profile to `unknown` and revokes extension trust. |

---

## 4. Execution Runtime: Host Execution vs Physical Sandbox

### Why Host Execution is the Default for Trusted Projects
When a developer works on their own trusted codebase, they require seamless
integration with local developer tooling:
- Toolchain binaries installed in user homes (`~/.cargo/bin/rustc`, `~/.nvm`,
  `~/.local/bin`) are immediately accessible.
- Global compilation and package caches (`~/.cargo/registry`, `~/.npm`,
  `~/.cache`) prevent redundant, expensive re-downloads.
- Local communication with development services (Docker daemons, local databases,
  test runners) operates natively.

### The Physical Sandbox Containment Layer
On Linux hosts supporting `bubblewrap` (`bwrap`), Muta provides a hard physical
boundary:
- **Filesystem Isolation**: Starts from an empty `tmpfs` root; binds `/usr`
  and minimal system libraries as read-only; mounts only the exact workspace
  root as writable; sets `$HOME` to an ephemeral `/tmp/muta-home`.
- **Process & Capability Stripping**: Unshares User, PID, IPC, UTS, and cgroup
  namespaces; drops all Linux capabilities (`--cap-drop ALL`); disables user
  namespace nesting.
- **Environment Cleansing**: Runs `--clearenv` to scrub ambient host environment
  variables and blocks injection of `LD_PRELOAD`, `LD_LIBRARY_PATH`, or `BASH_ENV`.

---

## 5. Persistence and Zero-Friction Workflow

1. **Atomic Persistence**:
   Trust records are stored at `~/.local/share/muta/workspace_security.json` with
   file-level locking (`fsutil::FileLock`) to prevent cross-session race conditions.
2. **First-Run Consultation (No Silent Blockers)**:
   When entering an unconfigured workspace (`unknown` profile), Muta presents a
   friendly startup trust banner inviting the developer to run `/trust`.
3. **Subsequent Frictionless Sessions**:
   Once trusted, subsequent launches in the same workspace automatically resolve
   the `development` profile from disk, passing preflight immediately without
   repeated manual confirmations.
