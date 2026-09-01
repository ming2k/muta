# The Harness Cognitive Pipeline

The **Cognitive Pipeline** (`CognitivePipeline`, crate `muta-agent::cognitive`) is the internal out-of-band execution engine for the Agent Harness. It executes stateless, zero-tool, typed cognitive operations (`CognitiveTask`) that keep the execution environment safe, clean, and responsive without being modeled as artificial agent personas.

For the architectural decisions establishing the Worker-Station Model and the Cognitive Pipeline, see [ADR-0167](../../adr/0167-worker-station-agent-model-and-hypervisor.md).

---

## 1. Why a Stateless Cognitive Pipeline

In Muta's Worker-Station architecture:
- **`Master` and `Runner`** are the only two *agent archetypes*: they hold conversation context, execute tools, mutate files, and deliver features or daemon orchestration.
- **`Hypervisor`, `Session`, and `Subtask`** are *host stations*: where an agent is placed to perform its duty.
- **Harness Cognitive Tasks** are *internal utilities*: they exist entirely outside the agent delegation tree to service the Agent Harness state machine.

Instead of dressing internal LLM transformations as an artificial agent persona (the legacy "Steward" with persona "Offices"), the Cognitive Pipeline provides a clean, typed execution substrate for internal single-shot LLM tasks.

---

## 2. The `CognitiveTask` Contract

Every internal cognitive task implements the typed [`CognitiveTask`](../../../crates/muta-contracts/src/cognitive.rs) trait:

```rust
#[async_trait]
pub trait CognitiveTask: Send + Sync {
    type Input: Serialize + Send + Sync;
    type Output: DeserializeOwned + Send + Sync;

    fn name(&self) -> &'static str;
    fn system_prompt(&self) -> &'static str;
    fn render_prompt(&self, input: &Self::Input) -> String;
    fn model_preference(&self) -> CognitiveModelPreference;
    fn timeout_ms(&self) -> u64;
}
```

### Core Built-in Tasks

| Task | Input | Output | Purpose |
| :--- | :--- | :--- | :--- |
| **`StreamLoopReviewerTask`** | L1 candidate + channel + projected evidence | `StreamLoopVerdict` (`yes`/`no`) | Confirms or clears a mechanical stream-loop candidate before an early cutoff |
| **`SessionDigestTask`** | Transcript excerpt + optional previous digest | `SessionDigest` (title/intent/history) | Maintains the session's resume-time working-memory projection |

The session digest lifecycle: the first admitted user round generates it immediately (the opening request alone names title and intent), and later rounds refresh it once the transcript has grown past the stored anchor (8K chars). The digest is persisted with the session and rendered by the sessions picker's detail view, so resuming — or merely revisiting — a session reads as title, intent, and a history checklist instead of a raw transcript.

---

## 3. Reliability and Execution Invariants

1. **Tiered Execution (L1 Fast-path $\rightarrow$ L2 Cognitive Review)**:
   - Routine tool calls pass through L1 signature hashing (`DoomGuard`, 0ms); round-level loop defense is entirely deterministic (DoomLoopGuard + loop guard + stop gate).
   - Only in-flight stream-loop candidates escalate to L2 for one semantic confirmation before an early cut.
2. **Fail-Open by Default**:
   - Cognitive operations are bounded by strict timeouts (2s - 2.5s).
   - If a cognitive call fails, times out, or returns malformed output, the harness logs a warning and falls back to safe defaults (a stream review resolves `no`; a digest keeps its previous value). Consults run detached from the round loop — the stream keeps rendering while the cognitive review deliberates — so harness cognition **never** blocks the primary round loop.
3. **Resource & Model Isolation**:
   - Cognitive tasks default to lightweight, fast models (`FlashLite` / `Flash`).
   - Token accounting for cognitive calls is recorded under system governance rather than billed as user task tokens.

---

## See Also

- [ADR-0167: Worker-Station Agent Model and Hypervisor Station Placement](../../adr/0167-worker-station-agent-model-and-hypervisor.md)
- [Harness Architecture](harness.md)
- [Context Compaction](context-compaction.md)
- [Glossary](../../reference/glossary.md)
