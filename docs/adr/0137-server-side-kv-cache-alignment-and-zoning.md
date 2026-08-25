# 0137. Server-Side KV Cache Alignment and Zoning Architecture

- **Status:** Accepted
- **Date:** 2026-08-25

## Context

Modern frontier large language model (LLM) serving architectures (Google Gemini, Anthropic Claude, OpenAI, DeepSeek, vLLM, SGLang) leverage **PagedAttention** and **Radix Tree (Prefix Tree) KV caching** in GPU High-Bandwidth Memory (HBM).

In Transformer self-attention:
$$\text{Attention}(Q, K, V) = \text{softmax}\left(\frac{Q K^T}{\sqrt{d_k}}\right) V$$

When processing large contexts (e.g. 50k–200k tokens), computing the Key ($K$) and Value ($V$) activation matrices across dozens of transformer layers requires significant compute and power. Caching these matrices allows the inference engine to bypass recomputation of the common prompt prefix, reducing Time-To-First-Token (TTFT) from seconds down to 50–150 ms and lowering input token pricing by up to 90%.

However, KV caching is **strictly left-to-right (0-indexed prefix matching)**. If a single character or token order changes at position $K$, all subsequent KV blocks from position $K+1$ to $N$ are invalidated.

Prior naive agent implementations suffered from several cache-invalidation anti-patterns:
1. **Unordered tool serialization**: iterating over `HashMap<String, Tool>` serialized tools in nondeterministic order, changing the prompt prefix on every invocation.
2. **Dynamic tool removal**: removing tools (e.g. `ask_user` in autopilot mode) mid-session altered the tool schema prefix, completely invalidating prior multi-turn conversation KV caches.
3. **Dynamic timestamp pollution in system prompt headers**: embedding `Current Time: ...` in the initial system instructions broke $Token_0$, rendering multi-turn KV caching impossible.
4. **Discarding prior thinking signatures**: omitting previous turn reasoning blocks or cryptographic signatures (e.g. Anthropic `thinking_signature`) on multi-turn replays broke protocol cache linearity.
5. **Unbounded context bloat**: accumulating multi-megabyte raw tool outputs from dozens of turns ago slowed down Attention calculation without adding semantic value.

## Decision

We establish a **Three-Zone Cache Architecture** across `muta-contracts`, `muta-agent`, and `muta-llm-client`:

```
Left (Token 0) ──────────────────────────────────────────────────────────> Right (Token N)
┌───────────────────────────┬───────────────────────────────┬────────────────────────────┐
│   Zone 1: Static Prefix   │   Zone 2: Monotonic History   │   Zone 3: Ephemeral Tail   │
│  (Static Invariant Prefix)│  (Monotonic History Stream)   │  (Ephemeral Dynamic Tail)  │
├───────────────────────────┼───────────────────────────────┼────────────────────────────┤
│ • Deterministic Tool Specs│ • User & Assistant Messages   │ • Current local timestamp  │
│   (Sorted lexicographical)│ • Thinking (with signatures)  │ • Runtime transient state  │
│ • System Identity         │ • ToolCalls & ToolResults     │ • Ephemeral directives     │
│ • Guidelines & Guidance   │ • Injected Skills & Files     │                            │
├───────────────────────────┼───────────────────────────────┼────────────────────────────┤
│ 💡 100% Shared Across Turns│ 💡 Monotonic Append Only      │ 💡 Confined to Tail End    │
└───────────────────────────┴───────────────────────────────┴────────────────────────────┘
```

### 1. Zone 1: Deterministic Static Invariant Prefix
- **Deterministic Tool Ordering**: `ModelRequest::with_tools` unconditionally sorts `tool_specs` by `name` alphabetically.
- **Superset Tooling & Runtime Gating**: Tool schemas remain stable throughout the session. Tool access restrictions (e.g. autopilot mode) are enforced at runtime via `PermissionPolicy` and `PreToolHook`, not by mutating the exposed tool definition list.
- **Pure Static System Prompts**: `SystemPromptContext` contains only invariant policies (`Identity`, `Guidelines`, `ModelGuidance`, `ProviderGuidance`). Breakpoint 1 is stamped on the last tool, and Breakpoint 2 on the system block for Anthropic.

### 2. Zone 2: Monotonic History Stream & Trajectory Compaction
- **Thinking & Signature Preservation**: Historical assistant `reasoning_content` and protocol-specific credentials (`metadata["thinking_signature"]`) are preserved and replayed verbatim, keeping the Radix Tree branch continuous.
- **Lossless Trajectory Compaction (`compact_historical_tool_outputs`)**: Recent turns (last 6 messages) retain full output fidelity. Bulky tool outputs (>1200 characters or >25 lines) from older historical turns are deterministically compacted to a representative head and structured summary notice, bounding context size without modifying the semantic trajectory.
- **Breakpoints 3 & 4**: Stamped on the second-to-latest message and latest assistant turn for providers supporting explicit multi-tier cache control.

### 3. Zone 3: Ephemeral Dynamic Tail
- All transient metadata (local time, dynamic session switches, ephemeral autopilot reminders) are strictly appended to the tail of the final user turn (`<ADDITIONAL_METADATA>` or `SystemReminder`), ensuring they never shift or invalidate the preceding 99.9% of the prefix tokens.

### 4. Zero-Overhead In-Process Tooling & Concurrency Support
- **In-Process Native Search (`native_grep`)**: Integrated directory walking and regex matching via `walkdir` and `regex` to eliminate subprocess `fork/exec` overhead while providing seamless fallback when `rg` is unavailable.
- **Persistent PTY Terminal Sessions (`PersistentTerminalSession`)**: Reusable long-running shell instances with sentinel delimiter tracking, preserving environment variables, aliases, and working directories across tool executions.
- **Inflight Request Deduplication (`Inflight<K, V>`)**: Thread-safe task deduplicator merging concurrent identical async requests (e.g. parallel AST scans or file reads) into a single shared Future.

## Alternatives considered

1. **Per-turn dynamic tool subsetting**: Dropping unused tools per turn to minimize prompt token count.
   * *Rejected*: Saving ~200 schema tokens destroys ~50,000+ tokens of cached KV memory, causing a 10× latency and cost regression.
2. **Injecting time/state in the System Prompt header**:
   * *Rejected*: Modifying the first few tokens invalidates all downstream KV blocks on every turn.
3. **Uncapped raw history replay**: Replaying full unabbreviated megabytes of past grep/bash outputs forever.
   * *Rejected*: Increases GPU memory bandwidth saturation and quadratic attention latency on long multi-turn sessions.

## Consequences

### Positive
- **Dramatic TTFT Reduction**: Time-To-First-Token drops by 70%~90% across multi-turn sessions.
- **Major Cost Reduction**: Up to 90% discount on cached input tokens for Anthropic Claude, OpenAI, DeepSeek, and Google Gemini.
- **Deterministic Prompt Hash**: Byte-for-byte reproducibility in system and tool headers.
- **Resilient Tooling**: Subprocess-free native search and persistent shell state across multi-step commands.

### Neutral
- Old historical tool outputs (>6 turns ago) show structured compaction notices rather than megabytes of redundant raw text.

## References

- [ADR-0056](0056-agent-owned-declarative-system-prompt-composition.md): Declarative system prompt composition
- [ADR-0120](0120-tokens-first-class-unit.md): Tokens as first-class units
- [ADR-0130](0130-native-platform-capability-boundary.md): Native platform capability boundary
- Google Antigravity / Jetski `cumulative_prompt_handler` & `StreamingReplaceFileContentParser` architecture
- vLLM PagedAttention & RadixAttention Paper (Sheng et al., 2023)
