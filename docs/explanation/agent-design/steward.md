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

## 1.1 Offices: the Steward corps, by station of duty

"Steward" is a collective noun, like Runner. Each [`StewardTask`](../../../crates/muta-contracts/src/steward.rs) therefore declares an **office** (`StewardOffice`) — the station whose judgment it performs:

| Office | Title | Task | Charter |
|---|---|---|---|
| `StreamSentinel` | Stream Sentinel | `StreamLoopReviewerTask` | Confirms/clears mechanical stream-loop candidates; one bare-token verdict, fail-open |
| `Chronicler` | Chronicler | `SessionDigestTask` | Distills transcripts into the session digest (title/intent/history); describes, never judges next actions |

The consult system prompt is identity-first: the office charter opens it, anchored by the collective Steward mission. Offices also pin default model staffing (the Stream Sentinel at `Flash`, the Chronicler at `FlashLite`). The zero-tool invariant is unchanged — an office is a *name and a charter*, not a tool grant.

Two offices from the original design — a Round Sentinel (semantic doom-loop review) and a Sanity Warden (payload audits) — were retired before ever being wired: round-trajectory loop defense is fully covered by the deterministic `DoomLoopGuard` / loop-guard / stop-gate machinery, and payload safety by the permission broker. An LLM office is only staffed when a judgment genuinely needs semantics that rules cannot express; today that is exactly the in-flight stream review and the digest distillation.

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
| **`StreamLoopReviewerTask`** | L1 candidate + channel + projected evidence | `StreamLoopVerdict` (`yes`/`no`) | Confirms or clears a mechanical stream-loop candidate before a cut |
| **`SessionDigestTask`** | Transcript excerpt + optional previous digest | `SessionDigest` (title/intent/history) | Maintains the session's resume-time working-memory projection |

The Chronicler's digest lifecycle: the first admitted user round generates it immediately (the opening request alone names title and intent), and later rounds refresh it once the transcript has grown past the stored anchor (8K chars). The digest is persisted with the session and rendered by the sessions picker's detail view, so resuming — or merely revisiting — a session reads as title, intent, and a history checklist instead of a raw transcript.

---

## 3. Reliability and Execution Invariants

1. **Tiered Execution (L1 Fast-path $\rightarrow$ L2 Steward)**:
   - Routine tool calls pass through L1 signature hashing (`DoomGuard`, 0ms); round-level loop defense is entirely deterministic (DoomLoopGuard + loop guard + stop gate).
   - Only in-flight stream-loop candidates escalate to the L2 Stream Sentinel for one semantic confirmation before a cut.
2. **Fail-Open by Default**:
   - Steward operations are bounded by strict timeouts (2s - 2.5s).
   - If a Steward call fails, times out, or returns malformed output, the harness logs a warning and falls back to safe defaults (a stream review resolves `no`; a digest keeps its previous value). Consults run detached from the round loop — the stream keeps rendering while the Stream Sentinel deliberates — so harness cognition **never** blocks the primary round loop.
3. **Resource & Model Isolation**:
   - Steward defaults to lightweight, fast models (`FlashLite`).
   - Token accounting for Steward calls is recorded under system governance rather than billed as user task tokens.

---

## See Also

- [ADR-0150: Two-Axis Agent Architecture and the Harness Steward](../../adr/0150-two-axis-agent-architecture-and-harness-steward.md)
- [Harness Architecture](harness.md)
- [Context Compaction](context-compaction.md)
- [Glossary](../../reference/glossary.md)
