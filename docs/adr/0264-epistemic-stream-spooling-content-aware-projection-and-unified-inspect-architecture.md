# 0264. Epistemic Stream Spooling, Content-Aware Projection, and Unified Tool Call Inspect Architecture

- **Status:** Proposed
- **Date:** 2026-10-21
- **Scope:** `crates/muta-agent`, `crates/muta-contracts`, `crates/muta-runtime`, `crates/muta-persistence`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0254](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md) (Epistemic Observation Lifecycle), [ADR-0257](0257-unbounded-stream-guard-and-finite-execution-contract.md) (Unbounded Stream Guard), [ADR-0262](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md) (Demand-Paged Epistemic Memory), [ADR-0263](0263-axiom-of-linear-causality-and-pure-execution-primitives.md) (Axiom of Linear Causality)
- **Supersedes:** ADR-0254 §1.3 (ad-hoc file paths in folded output pointers)
- **Amends:** ADR-0257 §2 (stream truncation and cutoff diagnostics)

---

## Context and Problem Statement

When an autonomous agent executes commands against compiled artifacts, minified JavaScript bundles, reverse-engineering decompilers (`wasm2wat`, `javap`, `jadx`, `objdump`), or high-volume test and build pipelines, execution output frequently exceeds physical prompt budgets.

Currently, two diverging mechanisms handle oversized command output, resulting in an architectural fracture:

1. **Destructive Physical Truncation (StreamGuard / ADR-0257)**:
   When stdout exceeds line or byte ceilings (e.g. 50,000 bytes or 2,000 lines), StreamGuard abruptly severs the stream, discards all remaining bytes into the void, and appends a sterile string marker:
   ```text
   ... "aliyunCaptcha-window-float":"hmOyI","window-show":"tWKqi" ...
   [output truncated]
   ```
   **The Fracture**:
   - **Irreversible Information Loss**: The discarded text is destroyed permanently. Neither the LLM nor the user can recover the downstream content without re-executing the command.
   - **Zero Cognitive Actionability**: The model is left with no metadata regarding the volume discarded, no knowledge of file or stream topology, and no pointer to retrieve missing segments.
   - **Severe Attention Hallucination**: Receiving truncated, half-closed syntax tokens (such as cut-off JSON or unclosed AST brackets) triggers speculative completions and loop thrashing.

2. **Domain-Leaking Ad-Hoc Spooling (ADR-0254 §1.3)**:
   In test folding, raw text was dumped to `.muta/spill/shell_<ts>.log`, instructing the model to use file-system tools (`read_text`, `search_text`) to inspect the log.
   **The Fracture**:
   - **Violation of Tool Orthogonality ([ADR-0215](0215-tool-surface-consolidation.md), [ADR-0262](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md))**: Filesystem tools (`read_text`, `write_file`) are strictly for the user's workspace codebase. Leaking internal runtime execution spool logs into the workspace file domain pollutes file caches, introduces path traversal hazards, and confuses role boundaries.
   - **Divergence from ADR-0262 Epistemic Virtual Memory**: While pruned context and compacted nodes expose canonical `inspect` handles (`call:<id>`, `fold:<id>`), tool execution truncation bypasses `inspect` entirely.

3. **Content Blindness on High-Entropy Minified Code**:
   Current stream collectors treat all standard output as uniform LF-delimited plain text. When a build tool or decompiler emits a minified single-line bundle (e.g. 2MB on a single line containing CSS class mappings or mangled symbol tables), the stream collector pumps thousands of characters of zero-signal token noise into the active prompt cache before hitting the byte ceiling, completely destroying the turn's signal-to-noise ratio.

---

## Architectural Principles

1. **Axiom of Lossless Epistemic Ingestion**:
   At the tool execution boundary, no standard output or standard error emitted by a child process is ever discarded into `/dev/null`. If output was produced in a session, it belongs to that session's permanent epistemic memory.
2. **Context is L1 Cache; CAS is Epistemic Backing Store**:
   Prompt context is an ephemeral, bounded attention window (L1 Cache). Content-Addressed Storage (CAS BlobStore) is backing memory. Evicting bytes from L1 to satisfy token economics must never destroy the backing memory.
3. **Pristine Domain Orthogonality**:
   Session execution history and runtime-spooled artifacts must never be inspected via workspace filesystem tools (`read_text`, `find_files`). All off-stream session observations are addressed exclusively through the unified `inspect` channel.
4. **Content-Aware Ingestion Gates**:
   The runtime stream observer must distinguish between structured multi-line text and minified single-line code, preventing token floods before they corrupt attention weights.

---

## Decision Outcome

We establish **Lossless Epistemic Stream Spooling, Content-Aware Projection, and Unified `call:` Handle Convergence** across `muta-agent`, `muta-runtime`, and `muta-persistence`.

```text
                       Child Process (run_command)
                                   │
                                   ▼ (stdout / stderr stream)
                    ┌──────────────────────────────┐
                    │      StreamGuard Splitter    │
                    └──────────────┬───────────────┘
                                   │
          ┌────────────────────────┴────────────────────────┐
          │ (100% Unabridged Raw Stream)                    │ (Budget-Aware Observer)
          ▼                                                 ▼
┌───────────────────────────────┐                 ┌───────────────────────────────┐
│     CAS Epistemic Spooler     │                 │   Content-Aware Ingestion     │
│   (muta-persistence::blobs)   │                 │   - Long-line minification    │
│   Stored under content hash   │                 │   - Pure-green folding        │
│   Keyed to ToolCallId in DB   │                 │   - StreamGuard budget cutoff │
└──────────────┬────────────────┘                 └───────────────┬───────────────┘
               │                                                  │
               │ backing store                                    ▼ Projected L1 Preview
               │                                      ┌───────────────────────────────┐
               │                                      │   Self-Describing Projection  │
               │                                      │   [Output Truncated ...]      │
               │                                      │   inspect(handle="call:...")  │
               │                                      └───────────────┬───────────────┘
               │                                                      │
               ▼                                                      ▼
┌───────────────────────────────┐                        Active Prompt Context
│         inspect Tool          │ ◄───────────────────── (Demand-Paged Recall)
│  inspect(handle="call:...",   │
│          query="...",         │
│          offset=N, limit=M)   │
└───────────────────────────────┘
```

---

## Technical Architecture

### 1. The StreamGuard Zero-Loss Splitter Pipe (`muta-agent::pipes`)

`run_command` output processing in `crates/muta-agent/src/tools/execute_command/pipes.rs` is upgraded to a duplex tee-stream:

1. **Unabridged Branch (Data Plane)**:
   The entire byte stream (up to hard disk safety quotas, e.g. 500MB) is streamed directly into an append-only spill writer backed by CAS blob storage (`muta_persistence::BlobStore`).
   Upon command termination, the final SHA-256 content digest is registered in the session ledger, bound directly to the active `ToolCallId`.
2. **Projected Branch (Control Plane / L1 Attention Window)**:
   The stream passes through the Content-Aware Ingestion Filter before entering the model transcript.

### 2. Content-Aware Stream Heuristics

Before accumulating characters into the context buffer, the stream collector evaluates content profiles:

1. **Minification Detector**:
   - If any single line exceeds `MAX_UNWRAPPED_LINE_LEN = 4,096` bytes without whitespace break, the stream is flagged as `StreamProfile::MinifiedArtifact`.
   - **Policy**: Direct streaming into L1 context is halted immediately for that line. The line is committed to CAS storage, and an inline structural marker is emitted:
     ```text
     [Single-line minified artifact detected: 485,210 bytes. Inline rendering suppressed to preserve context budget.]
     ```
2. **High-Entropy / Binary Gate**:
   - If null bytes or non-text control characters are detected, the stream is categorized as binary. The raw output is spooled to CAS, and the preview displays file magic/metadata only.

### 3. Self-Describing Inline `call:` Projection Envelope

When StreamGuard halts streaming due to line/byte quotas, or when minified code is suppressed, the truncated preview **must conclude with a structured, actionable epistemic envelope**:

```text
... [last valid lines within token budget] ...

[OUTPUT TRUNCATED BY STREAMGUARD]
- Reason: Output exceeded safety budget (captured 50,000 bytes / ~12,500 tokens).
- Stream Profile: 3,842,109 bytes (~960,000 tokens) total across 18,400 lines (98.7% preserved off-stream).
- Epistemic Asset: call:call_019a3b4c
- Actionable Guidance:
  DO NOT re-run this command expecting full output. Full stream is stored in epistemic virtual memory.
  Retrieve specific sections on demand via:
    inspect(handle="call:call_019a3b4c", query="pattern", limit=100)
    inspect(handle="call:call_019a3b4c", offset=1200, limit=200)
```

**Zero Ambiguity**: The agent immediately recognizes:
1. Exact scale of the total output vs. what it received.
2. The reason for truncation.
3. The exact canonical `inspect` handle ready for immediate, targeted recall.

### 4. Native `inspect` Expansion for `call:` Handles

In compliance with [ADR-0262](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md), the `inspect` tool resolver (`crates/muta-agent/src/tools/inspect.rs`) natively handles `call:<tool_call_id>`:

```rust
impl InspectTool {
    async fn resolve_call(
        &self,
        call_id: &str,
        query: Option<&str>,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<InspectPayload, ToolError> {
        // 1. Fetch blob digest bound to this tool call from Session IR
        let blob_digest = self.session_repo.get_tool_output_blob(call_id).await?
            .ok_or_else(|| ToolError::NotFound(format!("No spooled output for {}", call_id)))?;

        // 2. Open lock-free reader on BlobStore
        let reader = self.blob_store.open_reader(&blob_digest).await?;

        // 3. Apply demand-paged window or ripgrep regex filter
        let content = if let Some(pattern) = query {
            reader.grep(pattern, offset.unwrap_or(0), limit.unwrap_or(100))?
        } else {
            reader.slice_lines(offset.unwrap_or(0), limit.unwrap_or(200))?
        };

        Ok(InspectPayload {
            handle: format!("call:{}", call_id),
            total_lines: reader.total_lines(),
            returned_lines: content.lines().count(),
            has_more: reader.has_more_beyond(offset, limit),
            content,
        })
    }
}
```

### 5. Eradication of Ad-Hoc Spool Paths

- **Retirement**: The `.muta/spill/` filesystem directory and pointers in tool descriptions referencing `read_text` for internal logs are completely removed.
- All spooled data resides within the internal, content-addressed database or engine storage (`.muta/storage/blobs/`), invisible to filesystem tools (`find_files`, `read_text`, `list_dir`) and accessible strictly through `inspect`.

---

## Invariants & Behavioral Boundaries

- **`[INV-SPOOL-01]` Zero Physical Discard**: No child process standard output or standard error may be truncated without committing the unabridged stream to CAS.
- **`[INV-SPOOL-02]` Universal Inspect Exclusivity**: Spooled execution outputs MUST NOT be exposed as filesystem paths. The agent toolset MUST NOT direct models to workspace file tools to inspect session runtime history.
- **`[INV-SPOOL-03]` Structural Transparency**: Every truncation, compression, or folding event MUST embed the total byte count, omitted byte count, and valid `call:` handle.
- **`[INV-SPOOL-04]` Subagent Quarantine Continuity**: In accordance with `[INV-OFFSTREAM-04]` (ADR-0262), subagents cannot invoke `inspect`. Subagents encountering truncated output must rely on standard CLI piping/grepping within their sandbox, while parent orchestrators hold full demand-paging rights over all child and own tool calls.

---

## Consequences

### Positive
- **100% Information Retention**: Long outputs, build logs, and reverse-engineered files are never lost. An agent can pinpoint exact functions in 50MB decompiled files without blowing up context.
- **KV-Cache Stability**: Context remains small and clean (L1 Cache), preventing catastrophic prompt cache invalidations.
- **Ergonomic Cognitive Interface**: The model stops thrashing or re-running failed `cat`/dump commands; it naturally falls back to `inspect(handle="call:...", query="...")`.
- **Workspace Hygiene**: Zero runtime temp files spilling into developer workspace folders.

### Negative / Trade-offs
- **Storage Consumption**: Unabridged logs take disk space in CAS storage. Managed by a deterministic LRU garbage collector during session archival or DB vacuuming.
- **Tool Implementation Scope**: Requires wiring the output collector pipe directly into the persistence `BlobStore`.

---

## References

- [ADR-0215: Tool Surface Consolidation](0215-tool-surface-consolidation.md)
- [ADR-0254: Epistemic Observation Lifecycle, Ingestion Folding, and Near-Tail Invalidation](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md)
- [ADR-0257: Unbounded Stream Guard and Finite Execution Contract](0257-unbounded-stream-guard-and-finite-execution-contract.md)
- [ADR-0262: Demand-Paged Epistemic Memory and Unified Inspect Channel](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md)
- [ADR-0263: The Axiom of Linear Causality and Pure Execution Primitives](0263-axiom-of-linear-causality-and-pure-execution-primitives.md)
