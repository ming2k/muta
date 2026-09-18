# 0254. Epistemic Observation Lifecycle, Ingestion Folding, and Near-Tail Invalidation

- **Status:** Proposed
- **Date:** 2026-10-19
- **Scope:** `core/contracts`, `core/runtime`, `muta-agent`, `muta-contracts`, `muta-persistence`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0021](0021-pruning-is-implicit-and-distinct-from-compaction.md) (pruning distinct from compaction), [ADR-0023](0023-relevance-aware-tiered-pruning-and-layered-token-accounting.md) (relevance-aware tiered pruning), [ADR-0044](0044-layered-token-accounting.md) (layered token accounting), [ADR-0230](0230-three-valued-vision-and-declared-only-gating.md) (three-valued vision capability)

---

## Context and Problem Statement

In autonomous coding and debugging workflows (such as troubleshooting C/C++ build suites, rendering pipelines, or Rust workspaces), the agent frequently executes build and test commands (e.g., `ninja -C build test`, `meson test`, `cargo nextest run`). A single run of a 134-test suite produces thousands of tokens of output—predominantly repetitive progress indicators (`[1/134] ... OK 0.01s`) and successful test logs.

Four critical architectural defects currently plague execution output and multimodal context management:

1. **Severe Signal-to-Noise Imbalance (~45% Repetitive Token Waste)**:
   In multi-round troubleshooting, command outputs are appended verbatim into the transcript tape. When an agent runs a build, inspects a single failure, edits code, and re-runs the build across 10–15 rounds, the historical command outputs are re-transmitted on every subsequent round. A 2,000-token test log compounding over 15 rounds consumes $\approx 240,000$ tokens of cumulative prompt volume, accounting for ~45% of total input tokens while carrying $<5\%$ real evidentiary signal.

2. **The "Prompt Cache vs. History Modification" Dilemma (The Deep-Water Bust Trap)**:
   Modern frontier models (Anthropic Claude, OpenAI, DeepSeek, Google Gemini) enforce strict longest-common-prefix matching for KV prompt caching (offering 80–90% cost discounts). Retroactively mutating historical messages in deep history (e.g., rewriting a 2,000-token build output at turn 2 when currently at turn 20) invalidates the KV cache from that turn forward. The uncached recomputation penalty on 50,000+ downstream tokens can cost up to 70× more than the marginal savings of pruning the 2,000 tokens, introducing 5–15s Time-to-First-Token (TTFT) latency spikes.

3. **Multimodal Visual Token Inflation & Phantom Attention**:
   Visual payloads (`ToolOutput::Image`, screenshots, UI renders) consume 1,000–3,000 tokens per image. Once an agent inspects a screenshot in Round $N$ and formulates a code edit, the bitmap's evidentiary value collapses to zero. Leaving high-entropy visual tokens in the history across subsequent rounds wastes thousands of tokens, stresses GPU KV-cache VRAM, and induces cross-attention visual hallucinations.

4. **The Tragedy of the Context Commons (AI Information Hoarding)**:
   Language models are opportunistic next-token prediction engines that exhibit greedy information-hoarding behaviors. Models will not voluntarily pass truncation flags (`fold: true`, piping to `tail`) or request visual eviction via prompt instructions alone. Instruction compliance drifts under heavy cognitive load. Context discipline must be enforced deterministically by harness infrastructure.

---

## Decision Drivers

- **Zero-Bust Ingestion Compression**: Maximize compression *at the moment of reception* so outputs enter the prompt cache pre-condensed, requiring zero retrospective mutations and preserving 100% KV-cache stability.
- **Asymmetric Cache Invalidation (Near-Tail vs. Deep-Water)**: Permit surgical cache invalidation only at the volatile near-tail (Round boundary, $<2,000$ downstream tokens), while strictly freezing deep-water history ($>10,000$ downstream tokens) until epoch-level summarization.
- **Strict Information Safety Net (No Loss of Truth)**: Output folding must be deterministic, pure-green only, and backed by 100% unabridged content-addressed storage (CAS) with demand-paged forensic probes (`inspect_log`, `search_text`).
- **Single-Round Multimodal Lifecycle**: Visual tokens are ephemeral to their active Round; they are stripped to structured textual tombstones upon Round completion.
- **Deterministic Harness Control**: Zero reliance on model prompt compliance; filtering and lifecycle state transitions are enforced transparently by the runtime pipeline.

---

## Decision Outcome

We establish the **Active Epistemic Context Architecture (AECA)** across `muta-agent`, `muta-contracts`, and `muta-runtime`.

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ 1. Ingestion-Time Semantic Folding (pipes.rs)                               │
│    • Raw Stream ──► ANSI Strip ──► Protocol Classifier (Ninja/Meson/Nextest)│
│    • Pure-Green Runs Collapsed: [1..133/134] OK ──► 1 single summary line   │
│    • Critical Failures & Stderr preserved 100% verbatim                     │
│    • Unabridged raw log mirrored to CAS (.muta/spill/<id>.log)              │
│    • Zero-Bust: Enters context pre-compressed (150 tokens vs 2,200 tokens)  │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Pre-Compressed Signal
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 2. Tool Execution Contract & Escape Hatch (ExecuteCommandTool)              │
│    • Default: Semantic folding enabled (`raw: false`)                       │
│    • Escape Hatch: `raw: true` parameter for model-requested raw traces     │
│    • Structured Output includes CAS pointer + forensic probe hint           │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Session History
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 3. Near-Tail Causal Invalidation & Multimodal Eviction (pressure.rs)         │
│    • Round Boundary Gate: Fast-path near-tail (<2,000 tokens downstream)    │
│    • Command Supersession: Prior failed test run superseded by newer run    │
│    • Mutation Invalidation: Prior test failure invalidated by code edit     │
│    • Image Eviction: ToolOutput::Image payload stripped to metadata tombstone│
│    • Deep-Water Protection: History >10,000 tokens downstream frozen         │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

### 1. Ingestion-Time Semantic Folding (Zero-Bust Ingestion)

In `crates/muta-agent/src/tools/execute_command/pipes.rs`, output collection is upgraded from a dumb line buffer to a **Protocol-Aware Stream Distillation Pipeline**:

1. **Pure-Green Purity Gate**:
   Consecutive test executions matching known runner success patterns (Ninja `[X/Y] ... OK`, Meson `OK (X.XXs)`, Cargo Nextest `PASS [ ... ]`) are collapsed into a single run-length counter:
   ```text
   ✓ 133/134 tests passed (output folded)
   FAILED: test_embed_pipeline (0.05s)
   AssertionError: ... [full stack trace and failure diagnostic preserved verbatim]
   ```
2. **Purity Invariant**:
   A line is eligible for folding **if and only if**:
   - It matches a known success pattern;
   - It is produced on `stdout` with no accompanying `stderr`;
   - It contains no suspicious keywords (`WARN`, `WARNING`, `DEPRECATED`, `LEAK`, `PANIC`);
   - Process exit code is zero, or the failed segment is isolated.
   Any test that emits unexpected stdout/stderr or warnings is immediately exempted from folding and printed verbatim.
3. **CAS Mirroring & Forensic Pointer**:
   The full unabridged raw stream is always written to disk (`.muta/spill/shell_<timestamp>_<rand>.log`). The folded tool output concludes with a structured demand-paged pointer:
   ```text
   [Full unabridged output (134 tests, 2,240 tokens) saved to '.muta/spill/shell_...log'. Use search_text or read_text to inspect specific parts.]
   ```

---

### 2. Command Escape Hatch (`raw: Option<bool>`)

`ExecuteCommandArgs` admits an explicit `raw: Option<bool>` parameter:
- **Default (`false` / `None`)**: Harness-level semantic folding is active.
- **Explicit (`true`)**: Bypasses semantic folding, emitting raw stdout/stderr up to the standard collection cap.

This gives the agent (or human operator) an escape hatch when debugging unusual harnesses without making raw output the wasteful default.

---

### 3. Causal Near-Tail Invalidation

In `crates/muta-contracts/src/pressure.rs`, context relief is extended with **Command-Level Causal Invalidation**:

1. **Command Canonicalization**:
   Commands are parsed into canonical signatures (e.g. `ninja -C build test` $\to$ `ninja:test`, `cargo test` $\to$ `cargo:test`).
2. **Supersession Rule**:
   When a newer build/test command is executed, all prior build/test command results in the near-tail volatile zone ($<2,000$ tokens downstream from the candidate) are classified as `stale` and replaced with an informative tombstone:
   ```text
   [Prior command output cleared: `ninja -C build test` (superseded by later run)]
   ```
3. **Mutation Invalidation Rule**:
   When an `edit_text` or `write_file` mutation occurs, prior failed build/test outputs in the near-tail are marked stale:
   ```text
   [Prior build failure cleared: `ninja -C build` (invalidated by code edit)]
   ```
4. **Deep-Water Freeze**:
   If a historical command result is separated from the current round by $>10,000$ tokens of history, it is **frozen** to protect the KV-cache. It is only compacted when global compaction triggers at the epoch level (ADR-0019 / ADR-0021).

---

### 4. Multimodal Single-Round Ephemeral Eviction

Visual payloads (`ToolOutput::Image`) are classified as **Single-Round Ephemeral**:

1. **Intra-Round Liveness**:
   During the active Round (across all turns/steps within the round), the image payload remains intact in memory to enable multi-step visual reasoning (e.g., inspect image $\to$ inspect shader $\to$ apply fix).
2. **Round-Boundary Eviction**:
   Upon completion of the Round (when the agent returns its final response to the user), or upon subsequent compaction passes, the raw Base64/image payload is stripped from historical messages and converted into a textual tombstone:
   ```text
   [Image artifact: mime=image/png, resolution=1920x1080 | Inspected in Round 2. Raw payload cleared from context.]
   ```
3. **Amortized Reset Economics**:
   Busting the cache at the Round boundary for an image re-caches only the few hundred tokens produced in that round's final answer, while saving 1,000–3,000 tokens on every subsequent round across the remainder of the session.

---

## Rejected Alternatives & Negative Knowledge

- **Rely on Model Prompting / Self-Folding (`fold: true` via system instructions)**:
  *Rejected*. LLMs suffer from the Tragedy of the Context Commons: they greedily request full logs under uncertainty and suffer instruction drift under cognitive pressure.
- **Retroactive In-Place Invalidation in Deep-Water History**:
  *Rejected*. Mutating command outputs located 50,000 tokens in the past busts the entire downstream KV cache, causing a 70× financial cost spike and 5–15s latency penalty for negligible token reclamation.
- **Turn-Level Image Eviction (Mid-Round Dropping)**:
  *Rejected*. Dropping images immediately after the turn that fetched them destroys intra-round reasoning when the agent needs to reference the visual error while writing code edits across subsequent tool steps within the same round.
- **Heuristic / LLM-Generated Log Summarization at Ingestion**:
  *Rejected*. Running an auxiliary LLM call to summarize build logs introduces latency, cost, and hallucination risks (potentially omitting subtle compiler warnings). Ingestion folding must remain 100% deterministic, rule-based, and pure-green only.

---

## Consequences

### Positive
- **Drastic Token Reduction**: Eliminates ~45% of redundant prompt token volume in iterative build/test/render debugging sessions.
- **Attention Preservation**: Keeps the signal-to-noise ratio high, avoiding the "lost in the middle" degradation caused by thousands of passing tests.
- **Cache-Economic Harmony**: Combines zero-mutation ingestion folding (100% cache hit) with near-tail amortized invalidation (minimal recomputation penalty).
- **Multi-Modal Sustainability**: Prevents visual token runaway in multi-turn screenshot-driven workflows.
- **Zero Loss of Ground Truth**: 100% of raw stream logs and original images are durably preserved in CAS and probe-accessible.

### Negative & Neutral
- Introduces protocol-specific line parsers in `pipes.rs` that must be maintained for common build tools (Ninja, Meson, Nextest).
- Causes a bounded, intentional one-time cache re-evaluation at Round boundaries when near-tail invalidation or image eviction triggers (amortized to substantial net positive).

---

## References

- ADR-0019: Model-relative context compaction
- ADR-0021: Pruning is implicit and distinct from compaction
- ADR-0023: Relevance-aware, tiered pruning and layered token accounting
- ADR-0044: Layered token accounting
- ADR-0047: Round contains turn (vocabulary swap)
- ADR-0230: Three-valued vision capability and declared-only gating
