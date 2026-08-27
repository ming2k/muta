# The Harness Steward

The `Steward` is the internal cognitive attendant of the Agent Harness. It executes out-of-band, stateless, zero-tool cognitive operations that keep the execution environment safe, clean, and active.

For the architectural decision establishing the Two-Axis, Three-Plane model, see [ADR-0150](../../adr/0150-two-axis-agent-architecture-and-harness-steward.md).

---

## 1. Why a Steward

In the Muta agent architecture:
- **`Master` and `Runner`** are *operational actors*: they hold conversation context, mutate files, run shell commands, and deliver user-facing features.
- **`Supervisor`** is the *fleet coordinator*: it tracks sessions across the daemon and orchestrates multi-session joint debugging (联调).
- **`Steward`** is the *harness cognitive attendant*: it exists entirely outside the user's task delegation tree to service the Agent Harness itself.

Instead of polluting the `Runner` pool with non-tool internal prompts (like session titling) or relying solely on brittle heuristic regexes for loop detection, the Steward provides a unified, typed execution substrate for all harness-internal LLM invocations.

---

## 2. The `StewardTask` Contract

Every internal cognitive task implements the typed [`StewardTask`](../../../crates/muta-contracts/src/steward.rs) trait:

```rust
#[async_trait]
pub trait StewardTask: Send + Sync {
    type Input: Serialize + Send + Sync;
    type Output: DeserializeOwned + Send + Sync;

    fn name(&self) -> &'static str;
    fn system_prompt(&self) -> &'static str;
    fn render_prompt(&self, input: &Self::Input) -> String;
    fn model_preference(&self) -> StewardModelPreference;
    fn timeout_ms(&self) -> u64;
}
```

### Core Built-in Tasks

| Task | Input | Output | Purpose |
| :--- | :--- | :--- | :--- |
| **`SemanticLoopSentinelTask`** | Recent tool signatures + thoughts | `SemanticLoopVerdict` | Detects semantic thrashing & returns a prescriptive `remedy_nudge` |
| **`SanityVerifierTask`** | Action type + payload + justification | `SanityCheckVerdict` | Validates safety and rationality of critical commands/diffs |
| **`SessionTitlerTask`** | Opening transcript excerpt | `SessionTitleOutput` | Generates a 3-7 word concise title for the session |
| **`TranscriptCompactorTask`** | Overflowing message history | `CompactedProjection` | Produces an anchored summary when context budget reaches ~85% |

---

## 3. Reliability and Execution Invariants

1. **Tiered Execution (L1 Fast-path $\rightarrow$ L2 Steward)**:
   - Routine tool calls pass through L1 signature hashing (`DoomGuard`, 0ms).
   - Only suspicious or oscillating patterns escalate to the L2 semantic Steward.
2. **Fail-Open by Default**:
   - Steward operations are bounded by strict timeouts (1.5s - 2.5s).
   - If a Steward call fails, times out, or returns malformed JSON, the harness logs a warning and falls back to safe defaults (e.g. `is_loop: false`, `is_sane: true`). It **never** blocks the primary round loop.
3. **Resource & Model Isolation**:
   - Steward defaults to lightweight, fast models (`FlashLite`).
   - Token accounting for Steward calls is recorded under system governance rather than billed as user task tokens.

---

## See Also

- [ADR-0150: Two-Axis Agent Architecture and the Harness Steward](../../adr/0150-two-axis-agent-architecture-and-harness-steward.md)
- [Harness Architecture](harness.md)
- [Context Compaction](context-compaction.md)
- [Glossary](../../reference/glossary.md)
