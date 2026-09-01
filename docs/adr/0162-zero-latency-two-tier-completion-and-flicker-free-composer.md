# 0162. Zero-Latency Two-Tier Composer Completion, Intent State Invariant, and Flicker-Free Rendering

- **Status:** Accepted
- **Date:** 2026-09-01

## Context

In the Mutx terminal interface, typing or editing slash commands and harness control-plane commands (e.g. `/plan`, `/reset`, `/models`, `/schedule`) suffered from a severe transient flicker (flash frames):

1. **Eager Cache Eviction on Keystroke**: On every keystroke, `refresh_backend_completion_request` in `apps/tui/crates/mutx/src/completion.rs` immediately wiped `self.backend_completions.clear()` and sent an asynchronous IPC request (`AgentRequest::CompleteComposer`) to the daemon.
2. **Synchronous Frame Drop (Frame 1, T=0ms)**: The keypress triggered an immediate synchronous frame render. Because the completion cache was cleared and the async daemon response was in-flight, `app.completions()` returned an empty list (`Vec::new()`). `completion_active` evaluated to `None`. For partially typed commands, `resolved_slash_command_len` returned `None`. Consequently, `compose_target` collapsed to `ComposeTarget::Prompt`, painting the bottom hint row as `Enter send prompt` and destroying the completion popup for that single frame.
3. **Async Correction Frame (Frame 2, T≈5ms)**: 1–10ms later, the daemon finished in-memory string matching over `CommandCatalog` and sent `AgentResponse::ComposerCompletions`. The event loop received this signal, marked the frame dirty, and repainted the UI with `ComposeTarget::Completion` (`Tab / Enter select  ↑↓ navigate  Esc dismiss`) and reopened the completion popup.
4. **Test Suite Masking**: In `apps/tui/crates/mutx/src/completion.rs`, unit tests under `#[cfg(test)]` ran synchronously via `complete_for_frontend_test`, masking the asynchronous dropout from test suites.

## Decision

1. **Two-Tier Completion Pipeline**:
   - **Tier 1 (Synchronous Fast-Path / Pure Domain)**: Slash and harness command matching over `CommandCatalog` is pure in-memory string computation with zero I/O. It runs synchronously within the frontend input event handler (`process_one_event`), guaranteeing 0ms latency and instant completion candidates on Frame 1 without waiting for daemon IPC.
   - **Tier 2 (Asynchronous Slow-Path / Daemon)**: Dynamic filesystem path mentions (`@file` / `@dir`) and workspace scans remain asynchronous to protect the 60/120 FPS event loop.
2. **Stale-While-Revalidate (SWR) & Cache Retention**:
   - Forbid clearing completion items on keystrokes (`backend_completions.clear()` removed).
   - Inflight asynchronous requests retain the previous completion list and apply client-side optimistic prefix narrowing.
   - Use monotonic request/generation identifiers; when a newer daemon response arrives, it atomically updates the state without intermediate blank frames.
3. **Intent-Driven State Machine Invariant**:
   - Disconnect `compose_target` from transient data emptiness. Whenever `input.starts_with('/')`, the composer intent is strictly bound to the `Command` domain (either `Completion` candidate selection or `Command` execution).
   - It is strictly forbidden for a `/`-prefixed input buffer to collapse to `ComposeTarget::Prompt`.
4. **Flicker-Free Render Coalescing**:
   - The synchronous Tier 1 execution ensures the keypress render pass emits the authoritative final frame. Redundant dirty signals from synchronous command completion are eliminated.

## Alternatives considered

- **Debouncing the Keypress Render Pass**: Delaying `terminal.draw` by 10–20ms on keypress to wait for daemon responses. *Rejected*: Introduces input lag for typing and compromises editor responsiveness.
- **Full Frontend-Only Completion**: Moving filesystem scanning into the frontend. *Rejected*: Violates ADR-0136's boundary where heavy filesystem indexing and daemon state belong in the background service.

## Consequences

- **Positive**: Complete elimination of transient flickering when typing or editing commands and subcommands in Mutx. Instant (0ms) zero-latency command completion.
- **Positive**: Predictable, rock-solid bottom hint bar that never jitters or changes colors mid-spelling.
- **Neutral**: `muta-runtime::input_completion::complete_slash_items` is exposed as a zero-I/O pure domain function shared cleanly between the daemon and Mutx.

## References

- [ADR-0038: In-house grid + diff rendering engine](0038-in-house-grid-diff-rendering-engine.md)
- [ADR-0136: Muta core and peer frontend apps](0136-muta-core-and-mutx-terminal-app.md)
- [ADR-0158: Native framed transport for local daemon IPC](0158-native-framed-transport-for-local-daemon-ipc.md)
