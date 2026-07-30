# 0085. Config-time tool scoping: unified external-tool sources, no runtime discovery

- **Status:** Accepted
- **Date:** 2026-08-14
- **Revises:** the "progressive disclosure" direction noted (but never wired) in
  `crates/neenee-agent/src/lib.rs:135-138` (`disclosure_ledger` /
  `disclosure_bridge`, marked `#[allow(dead_code)]`)

## Context

Today every tool the model may call is loaded eagerly and its full JSON Schema
is sent on **every** provider request via `ModelRequest.tool_specs`. For the 16
builtin tools that is a fixed ~2.6k-token cost — acceptable, and a stable prefix
that helps prompt caching. The pressure comes entirely from *external* tools.

External tools today have exactly one source: MCP servers, configured **only** in
the global `~/.config/neenee/config.toml` under `[mcp.<name>]`
(`crates/neenee-persistence/src/config.rs:669`). Several facts constrain the
design space:

1. **MCP is connector-shaped, not tool-shaped.** An `[mcp.X]` entry declares a
   *connection* (`command`, `environment`, `enabled`, `read_only`); the actual
   tool list is discovered at runtime via `tools/list`
   (`McpRuntime::refresh_all`, `crates/neenee-agent/src/mcp/runtime.rs:217`).
   The tool count is unknowable at config time.

2. **`McpRuntime.configs` is frozen at construction.** There is no
   `set_configs` / `update_config` API (`runtime.rs:38-52`); adding, removing,
   or editing a server requires a process restart. The only live mutations are
   the `/mcp` modal's per-server enable/reconnect and the 10-minute catalog
   refresh of the *same* server set.

3. **Config is a single global file, read once.** `Config::load()`
   (`config.rs:1036`) reads `$XDG_CONFIG_HOME/neenee/config.toml`; nothing
   re-reads it during the process lifetime. There is no file watching and no
   per-project MCP config.

4. **A project-local `.neenee/` tree already exists** — but as *directory
   scanning*, not TOML config. `find_project_root`
   (`crates/neenee-skills/src/discovery.rs:212`) walks up looking for `.neenee`
   (among markers) and `.neenee/skills/<name>/SKILL.md` /
   `.neenee/commands/*.md` are the *highest*-priority skill/command sources
   (`discovery.rs:127`, `crates/neenee-transport/src/commands.rs:156`). So the
   "project overrides global" merge semantics already exist for knowledge — but
   not for tools, and not for MCP.

5. **No trust boundary for project-local config.** Because `.neenee/skills`
   ships knowledge (non-executing) it was safe to auto-load from a cloned repo.
   Project-local *tools* or *MCP servers* are a different matter: they execute
   processes. Auto-loading them from an untrusted clone is the same class of
   hazard npm postinstall and git `safe.directory` mitigate.

Two approaches were on the table for keeping context lean when many external
tools are configured:

- **Runtime progressive disclosure** — a `select_tools` meta-tool that lets the
  model discover/load tool schemas on demand. ADRs nowhere adopted this, but the
  machinery was ported from kimi-code and sits idle (`disclosure_ledger.rs`,
  `disclosure_bridge.rs`, both `#[allow(dead_code)]`).

- **Config-time scoping** — declare exactly which external tools an agent should
  see, and load nothing else. Trim the catalogue *before* it ever reaches the
  request, at config/launch time.

## Decision

Adopt **config-time scoping** as the policy for external tools, and explicitly
**reject runtime progressive disclosure**. External tools are a configured,
opt-in resource; if the model is drowning in tool schemas, that is a
configuration error to fix at the source, not a runtime discovery problem to
paper over.

### 1. The split lives in *configuration*, not in the `Tool` contract

The builtin / external distinction (and external's further split into
direct-configured vs MCP) is a **source / lifecycle** distinction, not a
**calling-contract** distinction. It is expressed in typed configuration and a
runtime source label — never by splitting the `Tool` trait.

There is exactly one tool contract — `neenee_core::Tool` (`fn name`, `fn call`,
`fn parameters`, …). By the time a tool reaches the dispatcher it is an
`Arc<dyn Tool>` and its origin is irrelevant to how it is invoked: builtin,
configured, and MCP tools are called identically, filtered by the same disabled
mask, and serialized into the same `tool_specs`. This uniformity is what
ADR-0060 bought ("Agent consumes tools, not JSON-RPC connections") and it is
preserved here.

The tiering instead lives in two *orthogonal* layers, neither of which touches
the contract:

- **A typed source label** — `ToolSource { Builtin, Configured, Mcp }`
  (`crates/neenee-agent/src/tool_manager.rs:42`). This is already a runtime tag
  driving name-collision priority (`builtin > user/configured > mcp`) and UI
  provenance, not a trait split. The existing `User` bucket is the home for the
  future "direct-configured" source; renaming it `Configured` (or keeping `User`
  as the label) is cosmetic and out of scope here.
- **Typed configuration tables** — one struct per source kind, each owning the
  fields that *only that source needs*:
  - `Builtin` has no config (compiled in via `register_tool!`).
  - `McpServerConfig` owns connection fields (`command`, `environment`,
    `enabled`, `read_only`, future `allow`) — the tool list is runtime-
    discovered, so it cannot live in config.
  - A future `ToolConfig` (direct-configured) would own its own definition
    (`run`, an inline `description`/`parameters` schema, `enabled`) — the tool
    list is fully known at config time.

So "internal vs external" and "configured vs MCP" are real distinctions, but
they constrain *how a tool is declared, loaded, trusted, and prioritized* —
not *how it is called*. Splitting the trait (e.g. `trait BuiltinTool: Tool` /
`trait ExternalTool: Tool`) would force the dispatcher to branch on origin and
then call both branches identically — a dead fork. The label + typed-config
shape captures the real difference without it.

### 2. Two tool tiers, one loading strategy

| Tier | Source | Loaded when | Schema delivery |
|------|--------|-------------|-----------------|
| **builtin** | Compiled in (`register_tool!`, `collect_toolset`) | Always | Full schema inline, every request (the stable ~2.6k prefix) |
| **external** | Configured (MCP today; static custom tools later) | Only when `enabled` and configured for this scope | Full schema inline, every request — same as builtin |

Both tiers are **eager and full-schema**. There is no "names-only, load on
demand" path. Builtin is never deferred because it is small, fixed, high-frequency,
and forms the cache-stable request prefix.

### 3. External tools are multi-source; MCP is one source, not special

External tools resolve from a merge of scopes, highest priority last so project
wins on name collision (matching the existing skill/command cascade):

```text
global  $XDG_CONFIG_HOME/neenee/config.toml   [mcp.*] (and, later, [tool.*])
project ./.neenee/config.toml                 [mcp.*] (and, later, [tool.*])
```

The configuration *shape* is shared across scopes and across source kinds — the
same `[mcp.<name>]` table in either file. What differs is the **trust posture**
(§5) and **merge precedence**. MCP is not promoted to a first-class concept
above any future static `[tool.<name>]`; it is simply the first external source
kind, and the only one whose tool count is runtime-discovered.

Because MCP discovers tools only after connecting, the per-server tool list
remains dynamic at runtime (`DynamicToolSink::replace`, unchanged from
ADR-0060). Config-time scoping therefore narrows the **server set** (which
servers connect), not the per-server tool subset. Per-server tool subsetting is
a future `[mcp.<name>].allow`/`deny` enhancement, not in scope here.

### 4. Merge semantics

- **Server name collision:** project `[mcp.X]` overrides global `[mcp.X]`
  wholesale (replace, not field-merge). Project specialization should be
  unambiguous; partial field-merge invites "which field came from where"
  confusion.
- **Tool name collision across servers:** unchanged — static > dynamic, dynamic
  resolves deterministically by source id (`mcp:<server>`), per ADR-0060.
- **Disabled mask:** name-level and scope-agnostic, exactly as today
  (`tool_manager.rs:206`). A tool disabled by name stays disabled regardless of
  which scope produced it.

### 5. Trust model for project-scope config (mandatory)

Global config is user-authored → trusted by default (it is the user's own
machine). Project config ships inside a working tree that may be cloned,
forked, or vendor-supplied → **untrusted until acknowledged**. Project-scope
external tools therefore require a one-time, per-project trust grant before they
connect, recorded as a remembered decision:

- On detecting a project `.neenee/config.toml` with any `[mcp.*]` (or future
  `[tool.*]`), the harness prompts once: *This project declares external tools.
  Trust and load them?* The answer is persisted keyed by the project root (a
  `trusted_projects` set, stored under XDG state, not inside the repo).
- While untrusted, project-scope external tools are **recorded but not
  connected** — they appear in the session modal as "blocked (untrusted)" and
  the user can trust them via `/trust` (new) or the modal.
- Revoking trust (`/untrust`) disconnects and re-prompts on next launch.

This mirrors git's `safe.directory` and macOS Gatekeeper: opt-in execution of
repo-supplied code paths. Global-only configs are unaffected.

### 6. Hot reload via explicit command, not file watching

Because `McpRuntime.configs` is frozen, editing config requires an explicit
reload today. We add a **user-triggered** reload, not an automatic one:

- A `/reload` slash command re-reads the merged config (global + trusted project)
  and applies the diff to the live `McpRuntime` via a new
  `McpRuntime::reconfigure(new_configs)`:
  - servers removed or changed → disconnect (`sink.remove`, drop handle)
  - servers added or changed → connect + publish
  - unchanged → untouched
  plus re-applies permissions / bash-policy / hooks from the new config.
- **No fs-watch auto-reload.** File watchers fire on every save (including
  editors that write-tmp-then-rename, `git checkout`, partial writes), would
  feed a half-written TOML to `reconfigure`, and could disconnect live MCP
  sessions mid-turn. The cost/risk is unjustified when the user knows when they
  finished editing. Reload is a deliberate, user-owned action.
- The tool layer needs no extra work: `DynamicToolRegistry` and the disabled
  mask are recomputed every request (`visible_tools`, `agent.rs:1995`), so a
  `reconfigure` that publishes/removes sources is reflected on the very next
  request.

### 7. Retire the disclosure machinery

`disclosure_ledger.rs` and `disclosure_bridge.rs` exist only to serve the
rejected `select_tools` path. Delete both modules (and their `lib.rs:135-138`
declarations). Keeping dead machinery for a decision we have explicitly rejected
is carrying cost and inviting future confusion.

## Alternatives considered

### Runtime progressive disclosure (`select_tools` meta-tool)

Rejected. It treats "too many tools configured" as a runtime search problem and
adds machinery — a disclosure ledger, a `loaded` set rebuilt from history, a
turn-boundary name announcement, a `deferred` marker, byte-stable-prefix
maintenance — to defer tool schemas until the model asks. For a coding agent
whose external tools are *explicitly configured per project*, "too many" is a
config error; the fix is to configure fewer, scope them, or toggle `enabled`,
not to make the model rummage at runtime. Runtime discovery also adds a
round-trip before a tool can first be used and reintroduces state that must
survive undo/compaction/resume. Config-time scoping achieves a lean context
with none of that. (This is also why the ported machinery was never wired: no
problem justified it.)

### Per-server tool subsetting at config time (`[mcp.X].allow`)

Deferred, not rejected. Letting a config declare "of github's 40 tools, only
load `create_issue` and `get_file`" is a legitimate config-time scoping lever
and consistent with this ADR. It is left for a follow-up because it requires
the tool list to be known before connect (either from a prior fetch cached on
disk, or an explicit name list the user maintains). The decision here is the
*framework* — external tools are configured, scoped, trusted, and eager — into
which that lever later fits.

### Field-level merge of global + project server config

Rejected for server entries. Merging `command` from global with `env` from
project produces a server whose definition is spread across two files and hard
to reason about. Project overrides global wholesale; a project that wants to
specialize re-states the full server entry. Simpler to document, simpler to
debug.

### Automatic fs-watch reload

Rejected. Reload is destructive (it tears down live connections and shifts the
tool set). Tying it to file-save events — which are frequent, multi-step
(write-tmp + rename), and include half-written states — risks applying a broken
config and disconnecting a session mid-turn. The user knows when they finished
editing; a `/reload` makes that intent explicit and safe. An optional
non-destructive "config changed, press R to reload" hint may be added later.

### Treat MCP as a special tier above other external tools

Rejected (reaffirms ADR-0060). MCP is a connector *protocol*, not a privilege
class. Promoting it would mean two parallel config/merge/trust stories. The
external-tool tier is uniform; MCP is merely its first inhabitant.

## Consequences

**Positive**

- A single, coherent loading story: builtin (always) + external (configured,
  scoped, trusted, eager). No runtime discovery state to keep consistent across
  undo/compaction/resume.
- Project teams can ship a curated `.neenee/config.toml` declaring exactly the
  MCP servers that project needs; newcomers get that set, not the global
  kitchen sink.
- Lean context by construction: if context is bloated, the user curates config
  (`enabled`, project scope, or the future per-server `allow`), with immediate
  effect rather than hoping the model selects the right tools at runtime.
- Hot reload (`/reload`) closes the "edited config, now restart" loop without
  the fragility of file watching.

**Negative**

- Deleting `disclosure_ledger.rs` / `disclosure_bridge.rs` removes ~240 lines of
  tested code. They are recoverable from history if a genuinely large,
  uncuratable tool universe ever appears (e.g. a marketplace), but until then
  they are pure carrying cost.
- Per-server tool subsetting is not solved here; an MCP server that exposes 40
  tools still injects all 40 once connected. The follow-up `[mcp.X].allow` is
  the mitigation.
- A new trust prompt adds one interaction for users who adopt project-scope
  config. The remembered-decision store is one more piece of persisted state.

**Migration**

This is a framework decision; implementation lands in stages:

1. Delete the dead disclosure modules and their `lib.rs` declarations.
2. Add `McpRuntime::reconfigure(new_configs)` + `/reload` (the hot-reload
   closure; usable with global-only config today).
3. Add project-scope `.neenee/config.toml` merging for `[mcp.*]` and the
   `trusted_projects` trust store + `/trust` / `/untrust`.
4. (Follow-up) `[mcp.<name>].allow` / `.deny` for per-server tool subsetting.

Each stage is independently shippable and does not change the model-facing wire
format (tools still arrive as full schemas in `tool_specs`).

## References

- [ADR-0060](0060-skills-and-mcp-extension-boundaries.md) — MCP as a connector
  with `DynamicToolSink`; this ADR keeps that boundary and layers scoping on
  top.
- [ADR-0013](0013-skills-xdg-paths-and-bundled-embed.md) — the XDG + project
  `.neenee/` cascade for skills/commands this ADR extends to tools.
- `crates/neenee-agent/src/mcp/runtime.rs` — `McpRuntime`, frozen `configs`,
  `start_background` / `refresh_all`.
- `crates/neenee-agent/src/tool_manager.rs` — three-bucket classification,
  per-request `loop_tools`, name-level disabled mask.
- `crates/neenee-agent/src/agent.rs:1995` — `visible_tools` recomputes every
  request (why the tool layer needs no reapply logic).
- `crates/neenee-persistence/src/config.rs:1036` — `Config::load`, single
  global file.
- `crates/neenee-skills/src/discovery.rs:212` — `find_project_root` and the
  project `.neenee/` marker.
- `crates/neenee-agent/src/disclosure_ledger.rs`,
  `crates/neenee-agent/src/disclosure_bridge.rs` — the rejected machinery to be
  deleted.
