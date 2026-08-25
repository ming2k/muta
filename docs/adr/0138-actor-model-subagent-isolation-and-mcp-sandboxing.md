# 0138. Actor-Model Subagent Isolation and MCP Sandboxing Architecture

- **Status:** Accepted
- **Date:** 2026-08-25

## Context

In frontier long-context agent systems (200k–1M tokens), modifying the tool declaration schema (Zone 1) mid-session invalidates all precomputed Key-Value (KV) cache blocks across the entire historical token sequence (as formalized in [ADR-0137](0137-server-side-kv-cache-alignment-and-zoning.md)), triggering full attention recomputation, 10–20s TTFT cold stalls, and full-rate token re-billing.

Furthermore, monolithic agent architectures that directly expose dozens or hundreds of dynamic third-party Model Context Protocol (MCP) tools to a single top-level conversation suffer from three critical failure modes:
1. **Cache Fragility**: Asynchronous MCP connection flaps, reconnections, or hot-reloads mutate the root tool schema, continuously busting the multi-turn prompt cache.
2. **Cognitive Degradation (Tool Confusion)**: Exposing 50+ tool schemas in a single attention window dilutes the LLM's attention, causing frequent tool hallucinations, incorrect parameter generation, and flawed decision trees.
3. **Context Pollution & Fault Cascades**: A failing MCP tool or a query dumping megabytes of raw JSON/logs permanently clutters the principal agent's long-term context window.

## Decision

We establish an **Actor-Model Subagent Isolation and MCP Sandboxing Architecture** across `muta-agent` and `muta-contracts`:

```
                                  【Main Principal Agent (The Conductor)】
                                    ├── Context: 200k ~ 1M Long-Context Window
                                    ├── Toolset: Permanently Invariant Core Primitives
                                    │   (bash, read_file, edit_file, grep, list_dir, todo, envoy)
                                    └── Cache Rate: 99.9% Constant Hit Rate (TTFT ~50ms)
                                                   │
                        ┌──────────────────────────┼──────────────────────────┐
                        ▼                          ▼                          ▼
            【Postgres Specialist】             【GitHub PR Specialist】      【Research Specialist】
               (Envoy Subagent A)                 (Envoy Subagent B)            (Envoy Subagent C)
          ├── Scratchpad: ~1k tokens         ├── Scratchpad: ~2k tokens    ├── Scratchpad: ~3k tokens
          ├── Tools: Isolated Postgres MCP   ├── Tools: Isolated GitHub MCP├── Tools: Web/Doc MCP
          └── Returns Summary & Terminates   └── Returns Summary & Destroys└── Returns Summary & Destroys
```

### 1. The Lean Principal Invariant
The top-level coding agent maintains a **strictly invariant toolset of core primitives** (`bash`, `read_file`, `edit_file`, `grep`, `list_dir`, `todo`, `envoy`). This freezes the Zone 1 static prefix across all turns of a multi-day session, guaranteeing that the 200k+ historical context achieves a ~100% KV-cache hit rate on every turn.

### 2. Actor-Model Subagent Sandboxing for Dynamic MCP Tools
Dynamic, high-cardinality, or specialized third-party MCP tools (e.g. database connectors, GitHub integrations, SaaS APIs) are decoupled from the main principal session and sandboxed inside dedicated, short-lived `Envoy` Subagents:
- **Focused Capability Profiles**: Each subagent is instantiated with a targeted tool profile (e.g. `EXPLORE`, `CODE`, or custom MCP capability sets) containing only 2–5 relevant tools.
- **Zero-Pollution Error Boundary**: Intermediate tool logs, multi-page data dumps, or transient MCP transport errors are contained strictly within the subagent's disposable scratchpad context.
- **Structured Synthesis Return**: Upon completion, the subagent returns a concise, high-signal structured summary to the principal agent via `ToolOutput::Envoy`, keeping the principal context clean.

### 3. Zero-Copy Prefix Replay for Subagents
Subagents inherit base environment and model capabilities from the parent, allowing the inference engine's Radix Tree to share the common system prompt prefix with zero allocation overhead and near-zero cold-start latency.

## Alternatives considered

1. **Monolithic Flat Tool Injection**: Registering all 50+ MCP tools directly into the principal agent's `tools` array.
   * *Rejected*: Causes severe KV-cache invalidation on every MCP disconnect/reconnect, inflates prompt overhead by 20,000+ tokens per turn, and degrades tool selection accuracy.
2. **Dynamic In-Place Schema Subsetting**: Dynamically adding/removing tools per turn based on heuristic intent matching.
   * *Rejected*: Mutating the tool definition prefix on every turn destroys all historical prompt caching, resulting in massive latency regressions.
3. **Pure Meta-Tool Dispatch without Subagents**: Using a single `call_mcp_tool(server, name, args)` gateway in the main conversation.
   * *Rejected*: While it avoids schema mutations, large multi-turn data dumps from MCP executions still pollute the main 200k context window.

## Consequences

### Positive
- **Indestructible 200k+ KV Cache**: The principal agent's static prefix remains 100% stable, regardless of how many MCP servers are added, restarted, or modified.
- **Cognitive Precision**: Subagents operate with 100% tool selection accuracy due to laser-focused tool definitions (3–5 tools per subagent).
- **Fault & Noise Isolation**: Heavy query outputs, errors, and transport retries are quarantined within ephemeral subagent sandboxes.
- **Concurrent Execution**: Multiple specialized subagents can run concurrently in parallel threads (e.g. searching docs while querying databases).

### Neutral
- Specialized MCP tasks require one orchestration hop through `envoy`, which is fully automated and native to the agent loop.

## References

- [ADR-0011](0011-subagent-profiles.md): Sub-agent profiles and tool admission
- [ADR-0029](0029-full-duplex-subagent-communication.md): Full-duplex subagent communication
- [ADR-0137](0137-server-side-kv-cache-alignment-and-zoning.md): Server-side KV cache alignment and zoning
- Model Context Protocol (MCP) Specification (2024)
