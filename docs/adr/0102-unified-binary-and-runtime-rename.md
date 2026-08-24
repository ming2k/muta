# 0102. Unified single-binary architecture and `neenee-runtime` rename

- **Status:** Superseded by ADR-0136
- **Date:** 2026-08-15
- **Revises:** ADR-0080 (single binary restored), ADR-0081 (the separate server binary is retired in favor of subcommands), ADR-0098 (crate rename chain extended), ADR-0099 (vocabulary refined)

## Context

Following the multi-session daemon work (ADR-0096) and crate boundary extractions (ADR-0098), two architectural frictions remained:

1. **`neenee-host` was ambiguous and confusing in isolation.** The name "host" is overloaded across computing (operating system host, network host, VM host). The crate's actual purpose is providing the session state machine (`SessionDriver`), control-plane IPC protocol (`serve`), background task supervision, and multi-session daemon runtime. Renaming it to `neenee-runtime` immediately clarifies its role as the execution runtime engine.
2. **`neenee-server` was a redundant thin binary crate.** `neenee-server` contained fewer than 100 lines of code whose sole purpose was parsing CLI flags and calling `neenee_host::host::run_daemon`. Having two separate binaries (`neenee` and `neenee-server`) introduced packaging complexity, discovery fragile lookups, and conceptual confusion over why a separate server binary existed when `neenee` itself was already capable of subcommands.

## Decision

1. **Rename `neenee-host` → `neenee-runtime`.**
   - Package and directory name: `neenee-runtime`.
   - Rust import path: `neenee_runtime`.
   - Clear architectural definition: the crate is the session runtime, control plane IPC wire protocol, and daemon supervisor.

2. **Delete `crates/neenee-server` and unify into a single binary (`neenee`).**
   - The workspace produces exactly one binary artifact: `neenee` (from `crates/neenee-cli`).
   - `neenee serve`: Runs the headless daemon runtime in the foreground.
   - `neenee serve --detach`: Spawns the daemon in the background detached from the current terminal.
   - Client on-demand spawning (`neenee_runtime::client::spawn_daemon`) invokes `std::env::current_exe() serve` directly, eliminating any requirement to locate a secondary binary on `$PATH` or next to the executable.

3. **Update service unit definitions.**
   - Systemd user service (`assets/neenee.service`) invokes `neenee serve --idle-exit 0 --grace 20`.

## Consequences

- **Single Binary Artifact:** Distribution and installation are simplified to packaging only the `neenee` executable.
- **Zero Ambiguity in Layering:** The crate dependency chain is strictly acyclic: `neenee-cli` (application binary) → `neenee-tui` (UI) / `neenee-runtime` (runtime engine) → `neenee-agent` (orchestration) → `neenee-contracts` (pure domain contracts).
