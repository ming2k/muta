# 0189. Streaming prefix virtualization, in-flight bounded execution, and zero-allocation persistence

- Status: Accepted
- Date: 2026-09-07
- Deciders: ming
- Consulted: —
- Informed: —

## Context and Problem Statement

ADR-0187 and ADR-0188 eliminated the O(N²) quadratic persistence write curves and
model-request cloning. However, in-depth systematic audit identified remaining
linear O(N) degradation hot paths under long-running sessions and high-throughput workloads:

1. **Repetitive turn-boundary projection and wire allocations:** Every finished ReAct
   turn called `project_messages()` over the entire history, and `wire_eq` performed
   two deep clones per message (`x.to_wire() == y.to_wire()`) to test semantic equivalence.
2. **Session row checksum AST serialization:** `compute_checksum` serialized the entire
   `SessionData` (including large multi-megabyte transcripts) into an in-memory
   `serde_json::Value` tree only to drop the transcript immediately after.
3. **Full projection for latest prompt extraction:** `last_effective_prompt_from_data`
   executed a full transcript projection just to locate the most recent user prompt.
4. **Unbounded command output growth:** Tool command execution collected all stdout/stderr
   in unbounded memory buffers until process exit, and emitted unthrottled streaming
   events that saturated UI event loops.
5. **Streaming render O(N) scan fallback:** While live tail messages were streaming,
   the virtual index returned `None`, forcing a linear scan across all settled messages.

## Decision Drivers

- True zero-allocation hot paths for turn commits and comparisons.
- Strict bounded memory and throttled event emission during command execution.
- O(log N) viewport skipping for long transcripts during active streaming.
- Break clean with no legacy performance compromises.

## Decision Outcome

Chosen option: "end-to-end bounded streaming, prefix virtualization, and zero-allocation persistence":

1. **Zero-allocation wire equality (`semantic_wire_eq`):** Compares message semantics
   without allocating memory or cloning strings.
2. **Incremental projection cache (`SessionState::projected_cache`):** Caches projected
   messages across ReAct turns; incremental turns append directly to the cache, avoiding
   full-history reprojections.
3. **Zero-copy row checksum view (`SessionRowChecksumView`):** Directly streams row
   fields into CRC32C without serializing the transcript or building intermediate JSON ASTs.
4. **Reverse scan for effective prompt:** Locates the latest user prompt via reverse
   entry scan respecting compaction watermarks without projection materialization.
5. **In-flight bounded command collector:** Hard memory caps are enforced during execution,
   with 30ms/4KB streaming event batching to protect UI event loops.
6. **Streaming-aware prefix virtual index:** Builds exact layout indexes over settled
   history prefixes during streaming, skipping all preceding off-screen history in O(1)
   binary search time.

### Consequences

- Turn-boundary commits now perform zero temporary message allocations.
- Command execution memory is strictly capped at ≤ 1 MB regardless of output volume.
- UI event loops maintain smooth 60 fps during runaway command streams.
- TUI streaming renders skip thousands of historical messages with zero frame drops.
