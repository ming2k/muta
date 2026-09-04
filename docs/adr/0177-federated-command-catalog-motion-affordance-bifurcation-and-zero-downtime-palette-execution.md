# 0177. Federated Command Catalog, Motion-Affordance Bifurcation, and Zero-Downtime Palette Execution

- **Status:** Accepted
- **Date:** 2026-09-07
- **Builds on:** ADR-0170 (composer-first & command palette), ADR-0172 (per-surface keybinding schemes), ADR-0176 (blurred-composer routing)
- **Amends / Partially Supersedes:** ADR-0170 §4 and §5 (retires the static single-source assumption in favor of a federated catalog; purges editor motions and dead contextual actions from `CommandId` and the Command Palette).

## Context and Problem Statement

Prior iterations of the keyboard and command architecture (notably ADR-0170) sought to establish a single source of truth (SSOT) for all application actions through `COMMAND_REGISTRY`. However, in real implementation this created three compounding architectural contradictions:

1. **Split-Brain Command Vocabulary**:
   The session daemon published `muta_runtime::command_catalog` (comprising all `BuiltinCmd` specifications like `/compact`, `/undo`, `/diff`, `/new`, `/delegate`, `/jail`, `/schedule`, `/jobs`, `/trust`, alongside project-local `.muta/commands/*.md` definitions) for slash completion. Simultaneously, `mutx` maintained a disconnected, static `COMMAND_REGISTRY` in `keymap.rs`. The Command Palette (`Ctrl+L` / `Ctrl+P`) filtered solely against this static array, making the daemon's core control-plane and workspace-authored commands completely unsearchable and unexecutable from the palette.
2. **Category Mistake (Motions Conflated with Commands)**:
   Viewport scrolling (`PageUp`/`PageDown`), step focus stepping (`Up`/`Down`), line breaks (`Alt+Enter`), and dialog dismissal (`Esc`) were declared as commands in `COMMAND_REGISTRY` (`ScrollTranscriptUp`, `TranscriptMoveUp`, `InsertNewline`, etc.). These are context-dependent input affordances and motions owned by active widgets, not schedulable application verbs. Exposing them in the palette broke focus invariants when invoked out of context.
3. **Contextless Ghost Actions**:
   Modal-local actions (such as reconnecting an MCP server, toggling tools, or editing connections) were registered as static commands with `avail_always` availability. In `actions.rs::execute_command_by_id`, their execution arms were empty no-op stubs (`{}`), creating ghost entries in the palette that produced zero user feedback.
4. **Inverted Search Ranking**:
   In `command_palette.rs`, `fuzzy_match` was invoked with candidate target and query inverted (`fuzzy_match(clean_query, &match_target)`), returning `None` whenever the user's query was shorter than the match target string.

## Decision

### 1. Motion-Affordance Bifurcation
- **Strict Domain Boundary**: Viewport scrolling, cursor motion, and text editing primitives are classified as **Focus Affordances** and removed from `CommandId` and `COMMAND_REGISTRY`. They are owned and consumed directly by the active widget's event handler or surface key scheme (ADR-0172, ADR-0176).
- **Purge of Ghost Actions**: Modal-local verbs (`McpReconnectSelected`, `McpToggleSelected`, `ToolsToggleSelected`, `PermissionsRevokeSelected`, `SkillsToggleDetail`, `ProviderEditSelected`, `ProviderDeleteSelected`, `ProviderToggleFavorite`) are removed from `CommandId` and `COMMAND_REGISTRY`. Surface-local actions are exposed exclusively via contextual Footer Hints in their respective modals.
- **Scope Consolidation**: Redundant scopes (`Scope::Transcript`, `Scope::BrowsePanel`, `Scope::BlockingDialog`) are retired; `Scope` is simplified to `Global`, `Session`, and `Composer`.

### 2. Federated Command Catalog in Command Palette
`filter_palette_commands` merges two distinct command sources into a unified `PaletteEntry` list:
1. **Client UI & Router Commands**: High-level application lifecycle and navigation commands declared in `COMMAND_REGISTRY` (`CommandId::OpenModels`, `NavigateSettings`, `NavigateDashboard`, `Help`, `Quit`, etc.).
2. **Daemon Harness Commands**: Real-time business control plane commands from `app.command_catalog` (`/compact`, `/undo`, `/diff`, `/new`, `/delegate`, `/jail`, `/schedule`, `/jobs`, `/trust`, and workspace custom commands).

**Deduplication & Preference Rule**: If a command exists in both catalogs (e.g. `/models` or `/settings`), the client UI action takes precedence, preserving native interactive modals and global shortcuts while eliminating duplicate rows.

### 3. Closed-Loop Palette Execution Contract
When an entry is accepted in the Command Palette:
- **Client Actions (`PaletteAction::Client(CommandId)`)**: Dismiss the palette and dispatch directly via `execute_command_by_id`.
- **Zero-Arg Harness Commands (`requires_args: false`)**: (e.g. `/compact`, `/undo`, `/diff`, `/new`, `/export`, `/retry`). Dismiss the palette and execute immediately via `handle_send_slash(app, runtime, session, slash)`.
- **Argument-Bearing Harness Commands (`requires_args: true`)**: (e.g. `/search <query>`, `/schedule <cron> <prompt>`, `/master <role>`). Dismiss the palette, switch to the chat composer, prefill `"/cmd "` into `app.input`, and set the cursor at the end, allowing the user to seamlessly type the required arguments.

### 4. Canonical Fuzzy Argument Ordering
Ensure `fuzzy_match` receives `(&match_target, clean_query)` so candidate strings serve as haystack and search queries as needle, restoring sub-string and fuzzy prefix matching across all commands, hints, and intent keywords.

## Consequences

- **Unified Surface Discovery**: The Command Palette is a true single pane of glass for all application, router, and daemon control-plane commands.
- **Zero Ghost Entries**: Every item in the palette maps to a functional execution path or composer prefill; no empty `{}` no-ops remain.
- **Clean Focus Model**: Viewport and readline mechanics remain localized to active widgets without polluting the global command catalog.
- **Backwards Compatibility**: Existing global chords, slash auto-completions, and per-surface modal hint rows remain fully functional and guarded by test assertions.
