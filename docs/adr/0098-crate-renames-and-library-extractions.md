# 0098. Crate renames and library extractions: contracts, host, tui, mcp

- **Status:** Accepted
- **Date:** 2026-08-14
- **Revises:** ADR-0005 (crate names, not the layering rules), ADR-0057 (the
  boundary stands; the crate it bound is renamed), ADR-0076 (name chain
  extended), ADR-0079 (the re-merged view tree is extracted again, with the
  shell this time)

## Context

A full review of the workspace topology found four names/boundaries that no
longer matched what the crates contain, plus two hygiene issues:

1. **`neenee-core` was named by convention, not by content.** "core" carries
   no admission rule; the crate's own documentation described it as "shared
   domain and wire contracts" (ADR-0057). A name that states the rule defends
   the boundary better than a name that means "the important middle."
2. **`neenee-transport` was misnamed by the project's own standard.**
   ADR-0005 renamed `neenee-app` → `neenee-persistence` because a crate name
   must state purpose, not mechanism. The transport crate owned bootstrap
   assembly, the session registry, slash handlers, project scaffolding, and
   the daemon runtime — the session *host*; only its `serve` module is a
   transport.
3. **`neenee-cli` was a 68k-line, 107-file binary crate.** The whole
   terminal frontend (app shell + view tree) lived in the binary, making the
   default workspace member the largest single codegen unit and the TUI
   unconsumable as a library. ADR-0079 re-merged the view into the binary
   when churn was lockstep and a second consumer hypothetical; at the current
   size the compile-time and test-isolation argument dominates.
4. **The MCP connector was co-located with the agent without a recorded
   decision.** ADR-0060 extracted `neenee-mcp`; a later merge into
   `neenee-agent` (justified in a module comment by an external codebase's
   layout) undid it without an ADR. The module has zero dependencies on agent
   internals, and `neenee-host` — not the agent — owns the `McpRuntime`, so
   the boundary costs nothing.
5. Hygiene: `neenee-persistence` carried a dead `SearchHistoryTool` (a `Tool`
   implementation in the storage layer with no call sites) and depended on
   both `dirs` and `directories` (only the latter is used); several
   `neenee-core` module names violated snake_case (`colorschemeconfig`,
   `doomguardconfig`, `skillsconfig`, `webconfig`, `channelauth`).

## Decision

Apply the following renames and extractions; the dependency DAG stays acyclic
and the layering rules of ADR-0005 are unchanged.

1. Rename `neenee-core` → **`neenee-contracts`**, and normalize module names
   to snake_case: `color_scheme_config`, `doom_guard_config`, `skills_config`,
   `web_config`, `channel_auth`.
2. Rename `neenee-transport` → **`neenee-host`**.
3. Extract **`neenee-tui`** from `neenee-cli`: the entire `tui/` tree (app
   shell, view modules, snapshot tests) plus the debug-only `showcase` become
   a library crate; `neenee-cli` keeps only argument dispatch, the product
   identity/principal, the `status` verb's table rendering, and process
   wiring. The `neenee` binary name is unchanged.
4. Move the daemon client code (discovery, attach handshake, control verbs,
   monitor stream) from `neenee-cli` into **`neenee-host::client`**, so the
   client and server of the `serve::Wire` protocol live in the same crate and
   cannot drift.
5. Re-extract **`neenee-mcp`** from `neenee-agent` (restoring ADR-0060):
   stdio JSON-RPC client, server lifecycle, tool adapters, `McpRuntime`,
   `McpCatalog`. `neenee-agent` has no MCP protocol dependency; `neenee-host`
   depends on it directly. The stdio end-to-end test and its fixture move
   with the crate.
6. Delete `neenee-persistence::search_tool` (dead code in the wrong layer;
   git history preserves it) and drop the unused `dirs` dependency from
   `neenee-persistence`.
7. Move the orchestration integration tests from the `neenee-cli` binary into
   `neenee-agent/tests/`, with the code they exercise.

Not done, deliberately: `neenee-llm-client` keeps its name (it *is* the LLM
client; the protocol modules are its content, not a misnomer); `neenee-server`
stays a separate thin binary crate (its own identity module is the reason);
config-schema types stay in `neenee-contracts` (a config file schema is a
contract between the user and the application).

## Alternatives considered

- **Keep `core` and rely on ADR-0057 discipline.** Rejected: every
  `core`/`common`/`util` crate starts with a boundary doc; the name supplies
  no Schelling point against drift, and the boundary had already admitted
  presentation-adjacent schemas. A self-defending name is cheaper than
  permanent vigilance.
- **Merge `neenee-llm-client` into `neenee-providers`.** Rejected: the client
  has two consumers (`neenee-providers`, `neenee-agent`) and standalone
  protocol tests; the how/which split is real.
- **Extract the view tree only (restore `neenee-tui-view`).** Rejected: the
  shell (event loop, `App` state) is the bulk of the binary and the part that
  benefits from compile isolation; a view-only crate re-creates the ADR-0079
  friction without shrinking the binary.
- **Leave the MCP connector in the agent.** Rejected: zero coupling makes the
  extraction nearly free, the connector matches the `neenee-llm-client`
  precedent (a protocol adapter with its own crate), and the co-location was
  never recorded as a decision.

## Consequences

- Every `use neenee_core::` / `use neenee_transport::` path changes; the
  rename is mechanical and compiler-verified.
- `neenee-tui` snapshot file names embed the new crate/module path; the
  snapshots were regenerated with byte-identical payloads.
- The `neenee` binary, CLI surface, config schema, and wire protocol are
  unchanged — no user-visible behavior change.
- Old ADRs keep their historical names; `docs/reference/glossary.md` maps
  former names to current ones.
- `neenee-cli` shrinks from ~68k to ~840 lines; the default workspace member
  is now the fastest-compiling crate instead of the slowest.

## References

- [ADR-0005](0005-strict-layering-and-renames.md) — layering rules and the
  name-states-purpose standard this ADR extends.
- [ADR-0057](0057-contract-only-core-boundary.md) — the contract-only
  admission rule, unchanged in substance.
- [ADR-0060](0060-skills-and-mcp-extension-boundaries.md) — the original
  `neenee-mcp` extraction, now restored.
- [ADR-0079](0079-remerge-tui-view-into-binary.md) — the re-merge this ADR
  reverses at a larger scope.
- [Crate layering](../explanation/crate-layering.md) — the current topology.
