# Archived Architecture Decision Records (Cold Tier)

- **Tier:** COLD
- **Lifecycle:** Archived
- **Audience:** Auditor / Forensic
- **Parent registry:** [`docs/adr/index.md`](../index.md)

## Charter

This directory is the cold tier for architectural decision records. It holds
records that have been superseded, deprecated, or compacted, per `[INV-TEMP-03]`
(No Direct Deletion) and the COLD tier of the 4D taxonomy.

## Rules

- Records here are **immutable**: never edited in place except for pointer
  corrections in header metadata (`[INV-TEMP-04]`).
- Records here are **excluded from routine AI context loading and recursive
  searches** (`[INV-TEMP-02]`). Consult them only for historical forensics and
  negative-knowledge audits.
- A record is moved here only once a superseding, deprecating, or compacting
  decision exists. The binding rule lives in that successor (or in the living
  snapshot under `docs/architecture/`), never here.
- The registry rows in [`docs/adr/index.md`](../index.md) point at the archived
  path for every record listed below.

## Archived Records

| ADR | Title | Status |
|-----|-------|--------|
| [0003](0003-extract-neenee-app-crate.md) | Extract `neenee-app` from the binary crate | Superseded by ADR-0004 |
| [0004](0004-six-crate-topology.md) | Six-crate topology: core / app / providers / tools / harness / cli | Superseded by ADR-0005 |
| [0006](0006-plan-mode-v2.md) | Plan mode v2: approval gate, active plan path, proposed-plan rendering | Superseded by ADR-0027 |
| [0007](0007-plan-progress-panel.md) | Plan progress sticky panel above input box | Superseded by ADR-0020 |
| [0010](0010-slim-goal-primitive.md) | Slim the goal primitive (drop status machine, token budget, time accounting) | Superseded by ADR-0082 |
| [0013](0013-skills-xdg-paths-and-bundled-embed.md) | Skills & commands: XDG paths + compile-time-embedded bundled skills | Superseded by ADR-0058 |
| [0015](0015-pursue-stop-gate-and-repeat-cron.md) | Pursue stop-gate + repeat cron scheduler (replace `/goal` + `/loop`) | Superseded by ADR-0082 |
| [0024](0024-pragmatic-sqlite-migrations.md) | Pragmatic SQLite migrations via `PRAGMA user_version` | Superseded (SQLite removed) |
| [0026](0026-plan-progression-forcing-functions.md) | Plan progression forcing functions (plan-exit nudge, todo-continuation nudge, approval-handoff instruction; re-order verify-nudge after the todo list drains) | Superseded by ADR-0033 |
| [0027](0027-plan-as-subagent.md) | Plan as a subagent (replace Plan mode with a `PLAN` profile + a `plan` tool; supersedes ADR-0006, revises ADR-0026; depends on ADR-0028/0029) | Superseded by ADR-0033 |
| [0028](0028-capability-allocation-scoped-writes.md) | Capability allocation: scoped filesystem writes (`WriteScope` per agent + `write_paths` grant on `ToolPolicy`; decouples write admission from the `ToolAccess` ceiling) | Superseded by ADR-0084 |
| [0031](0031-pursuit-tools-removed.md) | Remove the pursuit tools (`get_pursuit` / `start_pursuit` / `complete_pursuit`); the `/pursue` slash command, stop-gate, and `[NEENEE_PURSUIT_COMPLETE]` marker own the lifecycle (reverses the tool-keeping sub-decisions of ADR-0010/0015) | Superseded by ADR-0082 |
| [0032](0032-fold-pursuit-into-session-store.md) | Fold pursuit persistence into `SessionStore` (delete `PursuitStore` / `PursuitService` / `pursuits.db`; move `pursuit` onto `SessionData` + `SessionEvent::PursuitSet`; drop the `pursuit_service` field from `Agent` and every turn context) | Superseded by ADR-0082 |
| [0035](0035-application-layer-split.md) | Application-layer split: `neenee-code` + `neenee-quant` (rename cli→code; add the quant application crate + `QUANT` profile) | Superseded by ADR-0073 / ADR-0075 |
| [0036](0036-cjk-wide-char-ghost-cells.md) | Heal CJK wide-character "ghost" cells with a whole-row re-emitting backend wrapper (`WideHealBackend`) so wide-glyph trailing columns stay fresh through tmux | Superseded by ADR-0038 |
| [0039](0039-unified-prompt-registry.md) | Unified prompt registry: declarative system-channel composition via `PromptSection` (one trait + one registry keyed by `InjectionKind` replace the ad-hoc `format!`/`push_str` system-prompt assembly; the two duplicated turn-loop prep funnels collapse to one; two latent sub-agent system-message clobber defects fixed). User channel and store prompts investigated and deliberately not migrated | Superseded by ADR-0056 |
| [0042](0042-principal-envoy-role-vocabulary.md) | Principal / Envoy role vocabulary: keep `agent` as the umbrella engine term; name the top-level role `Principal` (`[principal]` config) and the spawned child role `Envoy` (renames the `subagent` tool/types/files); hard rename, no config alias | Superseded by ADR-0144 |
| [0045](0045-extract-neenee-tui-view.md) | Extract `neenee-tui-view` (widgets + semantic document model) from the `neenee-code` app shell; three-layer engine/view/shell topology with a one-way `TranscriptView<'a>` seam enforced by the compiler | Superseded by ADR-0079 |
| [0062](0062-longport-openapi-quant-adapter.md) | Direct LongPort OpenAPI adapter for quantitative trading | Superseded by ADR-0073 |
| [0063](0063-intelligence-workbench-and-expert-council.md) | Intelligence workbench and expert council boundary | Superseded by ADR-0073 |
| [0064](0064-product-family-workspace-layout.md) | Product-family workspace layout | Superseded by ADR-0073 |
| [0067](0067-modular-prompt-cache-control.md) | Modular prompt-cache control policy: a pure-domain `CachePolicy` classifier (`Breakpoints`/`SessionKey`/`Automatic`) per model family, a shared `read_cached_tokens` helper so OpenAI/Gemini/Moonshot discounts surface in the token-source report (not just Anthropic), and session-id-keyed `prompt_cache_key` injection for Moonshot/Kimi | Superseded by ADR-0161 |
| [0069](0069-pursuit-budgets-and-stats.md) | Pursuit budgets and runtime stats: optional opt-in `PursuitBudget` (turns/tokens/wall-clock) set via `/pursue budget`, session-scoped `PursuitStats` accumulated each round and surfaced in `/pursue status`, budget hard-stop with `terminal_reason`, and a ≥75% convergence reminder — all while keeping the marker-based stop-gate (no LLM judge) | Superseded by ADR-0082 |
| [0080](0080-rename-neenee-to-neenee-cli.md) | Rename `neenee` → `neenee-cli` (package); the command stays `neenee` | Superseded by ADR-0136 |
| [0083](0083-crash-consistent-pursuit-attempt-accounting.md) | Crash-consistent pursuit attempt accounting: persist pass/token/time counters with the armed runtime, align checkpoints and the 50-pass cap to one unit, type checkpoint status, and record terminal reasons on every non-completion path (supersedes ADR-0069's in-memory-statistics decision) | Superseded by ADR-0082 |
| [0095](0095-standalone-session-mirroring.md) | Standalone session mirroring: a bare `neenee` TUI reports its session into the project host over a read-only `Mirror` channel (`MirrorHello` + `MonitoredSession` rows, `hosting: mirrored`), so the control plane sees every session without changing the ownership model; liveness = connection lifetime | Superseded by ADR-0096 |
| [0102](0102-unified-binary-and-runtime-rename.md) | Unified single-binary architecture and `neenee-runtime` rename: `neenee-host` → `neenee-runtime`, delete `neenee-server` crate, unify background daemon execution into `neenee serve` / `neenee serve --detach` with single-binary `current_exe()` auto-spawning | Superseded by ADR-0136 |
| [0107](0107-trust-gate-covers-project-skills-and-commands.md) | Historical path-based project trust gate for skills and commands; replaced by content-bound workspace extension admission in ADR-0140 | Superseded |
| [0133](0133-view-surfaces-buffer-like-lifecycle-and-quick-switch.md) | Views as retained, buffer-like surfaces: every browse modal becomes a retained `View` whose scroll/index/sub-layer state survives hide (first-open initialisation replaces the per-open reset ritual), navigation becomes one bounded MRU stack (replacing the hand-mirrored deepest-first Esc chain, `editor_return_to`, and the hard-coded return links) with a shared hide/close/switch vocabulary, composer-line views park drafts per-view instead of one global `stashed_input`, and a Ctrl+L quick switcher lists all views fuzzy-MRU | Superseded by ADR-0139 |
| [0138](0138-actor-model-subagent-isolation-and-mcp-sandboxing.md) | Actor-model subagent isolation and MCP sandboxing: lean principal agent with permanently invariant core toolset (100% KV cache stability in 200k+ sessions), dynamic/heavy MCP tool sandboxing inside ephemeral Envoy subagents, zero-pollution noise/error boundary, and parallel actor execution | Superseded by ADR-0144 |
| [0140](0140-workspace-authority-and-content-bound-extension-trust.md) | Workspace authority and content-bound extension trust: execution profiles and project contributions become independent axes; autopilot is interaction-only; unknown unattended work fails preflight; extension changes re-quarantine by digest; product filesystem and shell execution move behind a fail-closed physical workspace sandbox | Superseded by ADR-0145 |
| [0145](0145-decoupled-workspace-asset-trust-and-tool-hazard-model.md) | Decoupled workspace asset trust and tool hazard model; replaced by the three-plane model and domain-specific trust in ADR-0147 | Superseded by ADR-0147 |
| [0169](0169-session-view-dual-mode-and-unified-leader-keyboard-architecture.md) | Session View dual-mode confinement, device-agnostic input state machine, and unified `Ctrl+X` leader keyboard architecture: strictly isolate dual-mode to Session View, eliminate `Ctrl+C` leader deadlocks, align Which-Key and Help registry SSOT, and enforce proximity whitespace layout | Superseded by ADR-0170 |
