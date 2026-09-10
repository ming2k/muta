# 0221. Custom external tools: evaluated and rejected

- **Status:** Rejected
- **Date:** 2026-09-10
- **Superseded by:** none; its cleanup mandate is carried by
  [ADR-0222](0222-retire-configured-external-tool-source.md)

## Context

Users asked for a way to teach the agent project-specific capabilities —
wrapping a repo script, a linter, an internal API, a domain CLI — without
forking `muta-agent` and recompiling. ADR-0085 had reserved a future static
`[tool.<name>]` shape for exactly this, and the `ToolSource::User` bucket
existed as its runtime home (`crates/muta-agent/src/tool_manager.rs:42`). A
proposal was drafted (the former contents of this record) to make that tier
concrete: declarative `[tool.*]` definitions executing out-of-process through
`ProcessRunner`, plus a reserved capability manifest and a staged roadmap
toward scripts, WASM plugins, and a host ABI.

This record evaluates that proposal and records why it is **rejected**, so the
same ground is not re-trodden.

## Decision

**Do not add a configured/custom external tool source — not now, and not as a
planned future.** The `[tool.*]` reservation and the `ToolSource::User` bucket
are retired; see
[ADR-0222](0222-retire-configured-external-tool-source.md).

### The proposed value increments all have a superior existing owner

A custom tool's capability increment is approximately zero: `run_command` with
a PTY already makes the action space Turing-complete. The only genuine
increments, and why each collapses:

| Proposed increment | Existing owner that is strictly better |
|---|---|
| Approval convergence | `[permissions] allow {tool, scope}` and `bash_policy` command rules already pre-authorize narrow operations. |
| Determinism (fix the recipe) | A checked-in `scripts/verify.sh` plus a skill/instruction. It is shared with humans and CI, versioned, reviewable, and testable — a `[tool.*]` entry is none of these. |
| Output economy | Command-level shaping (`jq`, `--format=json`), plus `SpillMiddleware` and tool-result pruning. MCP returns structured output natively. |
| Credential/protocol encapsulation | MCP. A real external service is a connector, and MCP already owns transport, lifecycle, and dynamic discovery (ADR-0060). |

The residual argument — *discoverability*, that a tool appears in the schema
while a skill must be mentioned — is net-negative: eager tool schemas cost
tokens on **every** request and pollute the model's decision space, whereas
skills are progressively disclosed and demand-loaded. A project recipe is
knowledge, and ADR-0085 already assigns knowledge to skills and executable
capability to tools; putting a recipe in the tool surface fights the
architecture.

### There is no structural future in which it clears the bar

The three forces that close the gap keep improving:

- **Stronger models** erode the determinism increment: the more reliable the
  agent is at shell and at following skills, the less a fixed incantation buys.
- **A maturing MCP ecosystem** absorbs the external/typed-service increment
  completely.
- **Evolving skills** (progressive disclosure, remote sources) absorb the
  knowledge increment.

The space is pinned from both ends: simple → script + skill; complex/external →
MCP. There is no durable niche in between.

The one would-be future rationale — a **capability manifest** for
least-privilege agents — does not belong to this feature. Enforcement is not a
property of a tool *definition*; it lives in the workspace sandbox,
`bash_policy`, permission scopes, and (for untrusted code) a WASM ABI. For
non-WASM tools those enforcement planes already exist and are independent of
custom tools. The manifest only becomes a real boundary in an
untrusted-third-party-plugin world, which is a separate and speculative product
bet (ADR-0160 lists WASM only as a possible platform adapter; ADR-0195 lists
plugin ABIs among things deliberately *not* built). Configured tools are
therefore not the vehicle for it.

A final asymmetry: the population able to author `[tool.*]` plus an inline JSON
Schema is a **subset** of the population able to write a shell script. The
feature asks the more capable author to use the clumsier mechanism. MCP earned
its place because a *third-party author* exists; configured tools have no such
division of labor.

### Falsification conditions (what would reopen this)

This rejection should be revisited only if concrete evidence appears:

1. Users repeatedly demand a capability while being unable or unwilling to
   write a script or an MCP server.
2. A real "shell disabled, only typed operations permitted" deployment mode is
   actually used.
3. A class of operations measurably reduces failure rates with schema-typed
   arguments versus shell + skill.
4. There is a demonstrated need to share tool definitions across projects
   without touching code.
5. An external cross-harness standard for declarative tools emerges — adopting
   a standard is parity, not speculation, and should be evaluated on its own
   terms.

Absent these, building it is YAGNI.

## Alternatives considered

### The full tiered roadmap (configured → script → WASM → host ABI)

Rejected as a plan. It conflated a *capability ceiling* with a *build order*.
Each tier must independently clear a demonstrated need before a later tier is
contemplated; tier 1 does not clear it, so the ladder is moot.

### Embedded Lua (or other in-process VM)

Rejected. In-process VM `io`/`os.execute`/FFI either bypasses
`ExecutionEnvironment`, `WorkspaceJail`, and `SecretScrub`, or requires
sandboxing an entire stdlib — a large, error-prone security surface. It also
forces async/cancellation/timeout bridging against a synchronous VM, and its
arbitrary behavior cannot be mapped to a `HazardLevel` or `ScopeTarget`.

### Only MCP (wrap everything as an MCP server)

Rejected as a *general* answer, but MCP *is* the answer for external/typed
services. It is connector-shaped, which is friction for a one-line CLI wrapper —
which is precisely why that wrapper should be a script + skill, not a new tool
source.

### Runtime progressive disclosure (`select_tools` meta-tool)

Rejected, reaffirming ADR-0085.

### Config-time `[tool.*]` as originally reserved

Rejected. See the value-increment analysis above.

### Inferring `hazard` / permissive defaults

Rejected in the proposal, and now moot: with no configured tool source there
is no classification surface to get wrong.

## Consequences

**Positive**

- The extension story is a single, coherent pair: **MCP** for executable
  external tools and **skills** for project knowledge. No third mechanism to
  document, secure, or keep consistent.
- No speculative VM, plugin ABI, or eager per-request schema tax.
- The reserved hooks that invited accidental implementation are removed
  (ADR-0222), preventing the dead-opening drift that ADR-0215's ghost-tool
  defect already demonstrated in this codebase.

**Negative**

- Users who want a narrowly pre-authorized typed operation must express it as a
  `bash_policy`/permission rule over a checked-in script, or an MCP server —
  more ceremony for the simplest wrappers.
- If condition (5) — a cross-harness standard — materializes, the decision must
  be revisited rather than inherited.

**Migration**

1. Retire the `[tool.*]` reservation from ADR-0085's design (this record's
   rejection + ADR-0222).
2. Delete `ToolSource::User` and its plumbing/comments (ADR-0222).
3. No data migration: nothing ever populated the bucket.

## References

- [ADR-0222](0222-retire-configured-external-tool-source.md) — the cleanup this
  rejection mandates.
- [ADR-0085](0085-config-time-tool-scoping.md) — config-time scoping and the
  now-void `[tool.*]` reservation.
- [ADR-0060](0060-skills-and-mcp-extension-boundaries.md) — MCP and skills as
  the two extension boundaries.
- [ADR-0195](0195-retained-tui-runtime-and-component-lifecycle.md) — plugin
  ABIs deliberately not built.
- [ADR-0160](0160-platform-abstraction-and-shim-layer.md) — WASM as a possible
  platform adapter target, not a tool-scripting engine.
- `crates/muta-agent/src/tool_manager.rs:42` — the retired `ToolSource::User`
  bucket.
