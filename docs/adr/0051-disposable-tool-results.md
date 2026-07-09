# 0051. Disposable tool results: per-tool declaration for early pruning

- **Status:** Accepted
- **Date:** 2026-07-09

## Context

Context pruning (`prune_tool_results`, `neenee-core/src/pressure.rs`) relieves
pressure by degrading old `Tool`-role results in tiers. Its selection policy is
shaped around **durable** results — file reads, shell output, search dumps —
that stay relevant for many rounds. Two protections keep such results alive:

- **Recency protection** shields the newest `protect_recent_chars` of tool
  output verbatim.
- **Keep-alive** spares a fresh result whose file target is still referenced
  after it was produced.

A `Tool`-role message with no `file_key` (e.g. `list_skills`) falls through
both protections only by aging out of the recency window, and is otherwise
cleared only once pressure crosses the 65% gate. For a long, low-pressure
session this means a once-only lookup — a skill catalog the model consults to
pick one skill and then never needs verbatim again — squats on context tokens
for the rest of the session. The skill catalog case motivated this, but the same
shape recurs for any index/listing tool (`search_history`, `glob`, …).

The risk was solving it narrowly: hard-coding a `list_skills`-supersedes rule
into the staleness pass. That would make pruning a growing table of per-tool
special cases — one entry per future disposable tool — with no single place that
declares "this result is throwaway".

## Decision

Introduce a framework-level convergence point: a per-tool **disposable-result
declaration** with pruning as its single consumer.

1. **`Tool::result_disposable(&self) -> bool`** (default `false`) on the `Tool`
   trait (`neenee-core/src/capability.rs`). A tool overrides it to `true` when
   its result is a once-only catalog/index the model consults and then no longer
   needs verbatim.

2. **`Message::disposable: bool`** (default `false`, `#[serde(default)]` so
   legacy `session.json` loads as durable with no migration) on the `Tool`-role
   result message (`neenee-core/src/message.rs`). The declaration is *stamped*
   onto the result message at the recording choke point, not threaded through
   the `ToolOutput` enum — so pruning, a pure function over `&mut [Message]`,
   never needs access to live `Tool` objects.

3. **`Agent::tool_result_disposable(&self, name) -> bool`**
   (`neenee-agent/src/agent.rs`) resolves the declaration by name from the
   registered toolset, mirroring the existing `tool_target_is_unspecified`
   lookup. `record_tool_result` calls it and applies `.with_disposable()` to the
   result message when true. Envoy results are never disposable.

4. **`plan_prune`** (`neenee-core/src/pressure.rs`) is the single consumer. A
   `disposable` result:
   - is **excluded from recency protection** (never enters the `protected` set),
   - **bypasses keep-alive** (no file target to keep alive), and
   - degrades **straight to an informative clear**, skipping the gentler
     truncate tier (a head/tail slice of a throwaway catalog is pointless).

   The durable transcript, the `tool_call_id` chain, and the informative
   `[cleared tool result: list_skills (N lines, M chars)]` placeholder are all
   unchanged — the model can always re-invoke the tool.

5. **`ListSkillsTool::result_disposable` → `true`**
   (`neenee-agent/src/skills/tools.rs`) is the first consumer. Future disposable
   tools (e.g. `search_history`, directory listings) need only the same
   one-line override.

## Alternatives considered

- **Hard-code a `list_skills` supersede rule in the staleness pass.** Rejected:
  one special case per future disposable tool, with no single declaration point.
  The staleness pass is keyed on `file_key` (a shared file); `list_skills` has
  none, so it would need a parallel, name-based branch — exactly the divergence
  the range-aware staleness rewrite (ADR-0034) worked to avoid.

- **Thread the disposable flag through `ToolOutput`.** Rejected: `ToolOutput` is
  a 10-variant enum; adding the flag to every variant bloats the type and its
  serialization, and the flag is not a property of the *result data* but of the
  *tool's result lifetime*. Stamping it on the `Message` at the recording choke
  point keeps the data flow clean: the declaration lives on the tool, the signal
  lives on the message, pruning reads the message.

- **Make `list_skills` results transient (never persisted).** Rejected: breaks
  session-resume faithfulness (the replayed transcript would lack the catalog
  the model actually saw) and forces the model to re-`list_skills` on every
  resume. The disposable approach keeps full durability while reclaiming the
  model-window bytes.

- **Pass `&Tool` into `record_tool_result` to query the declaration there.**
  Rejected: pollutes the signature chain (`execute_tool` → result →
  `record_tool_result`) with a borrow that is only used for one boolean. The
  name-based lookup (`tool_result_disposable`) matches the established
  `tool_target_is_unspecified` pattern and keeps `record_tool_result`'s
  signature unchanged.

## Consequences

**Positive**

- One declaration point (`Tool::result_disposable`), one consumer
  (`plan_prune`): adding a new disposable tool is a one-line trait override with
  no pruning-logic change. This is the framework convergence the design sought.
- `list_skills` catalog results no longer squat on context in long sessions —
  they clear at the first prune gate with an informative placeholder.
- Pruning stays a pure function over `&mut [Message]`; it learns about tool
  intent only through the stamped `disposable` flag, never through live `Tool`
  objects, so the layering (`neenee-core` does not depend on `neenee-agent`) is
  intact.
- Full durability is preserved: the durable transcript, `tool_call_id` chain,
  and informative placeholders are unchanged; only model-window bytes are
  reclaimed earlier.

**Negative / neutral**

- `Message` gains one `bool` field, so every struct-literal construction site
  across the workspace initializes it (mechanical; `..Message::new(...)`
  spread sites need no change).
- A disposable result clears *at the prune gate* (65% pressure), not
  immediately after use. Aggressive immediate eviction was considered and
  rejected to avoid churn on sessions that stay low-pressure — the placeholder
  is cheap, and the gate is the single, well-understood relief trigger.

## References

- `crates/neenee-core/src/capability.rs` — `Tool::result_disposable`.
- `crates/neenee-core/src/message.rs` — `Message::disposable`,
  `Message::with_disposable`.
- `crates/neenee-core/src/pressure.rs` — `plan_prune` disposable handling.
- `crates/neenee-agent/src/agent.rs` — `tool_result_disposable`,
  `record_tool_result` stamping.
- `crates/neenee-agent/src/skills/tools.rs` — `ListSkillsTool` override.
- [Context pruning](../explanation/agent-design/context-pruning.md) — the
  disposable-results pass.
- ADR-0023 — relevance-aware tiered pruning (this adds the disposable axis).
- ADR-0034 — range-aware staleness (the precedent for a pure-function pruning
  policy that avoids name-based special cases).
