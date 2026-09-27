---
id: ADR-0285
title: "Multimodal Claim-Check Lifecycle, Epistemic Visual Invoices, and Lease-Bounded Rehydration"
status: accepted
date: 2026-10-26
scope: contracts/offstream, persistence/blobs, agent/tools, agent/orchestration
superseded_by: null
negative_knowledge: true
---

# 0285. Multimodal Claim-Check Lifecycle, Epistemic Visual Invoices, and Lease-Bounded Rehydration

- **Status:** Accepted
- **Date:** 2026-10-26
- **Scope:** `muta-contracts` (offstream content, media payloads, pressure), `muta-persistence` (blob stores, companion retrieval), `muta-agent` (inspect tool multimodal rehydration, round lifecycle), `muta-runtime` (offstream sources)
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0230](0230-three-valued-vision-and-declared-only-gating.md) (Three-valued vision capability), [ADR-0254](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md) (Epistemic observation lifecycle), [ADR-0262](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md) (Demand-paged epistemic memory), [ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md) (Scoped inspect retrieval, retention, and deletion), [ADR-0282](0282-tool-observation-claim-check-lifecycle-and-cas-backing-store.md) (Tool observation claim-check lifecycle), [ADR-0283](0283-anti-thrashing-context-scheduling-and-generative-horizon-compaction.md) (Anti-thrashing context scheduling), [ADR-0284](0284-separation-of-tool-egress-truncation-and-true-causal-staleness.md) (Separation of tool egress truncation)

---

## Context and Problem Statement

Multimodal capabilities (vision models inspecting code diagrams, user interfaces, screenshots, and visual diffs) introduce high-entropy payloads into LLM context windows. A single high-resolution image consumes 1,000 to 3,000 tokens across all attention layers.

Prior architectures suffered from four systemic design flaws when managing image lifecycles:

1. **The "Siamese Twins" Protocol Compromise**:
   Because the standard OpenAI Chat Completions protocol restricts `Role::Tool` messages strictly to text, tool-generated visual outputs (such as `read_image`) were split across two disjoint nodes: an empty/textual `Role::Tool` result followed by a companion `Role::User` message with `InjectionKind::ToolImage` carrying the Base64 payload.
2. **Opaque Visual Amnesia (Lossy Stripping)**:
   When context pressure triggered pruning, the companion user message had its images cleared (`images = None`) and was overwritten with an uninformative string: `[cleared image payload]`. This severed the connection to the originating tool call and gave the model zero metadata about what visual asset was removed.
3. **One-Way Ingestion (No Rehydration Path)**:
   While text outputs could be retrieved via `inspect(handle: "call:<id>")`, the `inspect` tool was strictly text-only (`ToolOutput::Text`). When a visual observation was pruned, the underlying image became completely inaccessible to the active model window. Models needing to re-examine a visual detail were forced to re-run tools, wasting round latency and compute.
4. **Retroactive Mutation vs. Cache Stability**:
   Attempting to restore images by editing past turns in-place destroys the provider KV-cache prefix for all subsequent turns, spiking prefill latency (TTFT) and token costs.

---

## Decision

We establish an uncompromised, future-proof **Multimodal Claim-Check and Lease-Bounded Rehydration Architecture**:

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ 1. Immutable Ingestion & Attestation                                        │
│    - Images (Tool companion or User upload) persisted to Transcript & CAS    │
│    - Linked by deterministic invariant handle: `call:<id>` or `artifact:<h>` │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                         Context Pressure Pruning / Eviction
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ 2. High-Fidelity Epistemic Visual Invoice                                   │
│    - High-entropy image payload stripped to relieve ~1,600 tokens           │
│    - Replaced with actionable, structured invoice tombstone:                │
│      "[cleared image payload (image/png) — rehydrate with inspect           │
│       handle \"call:<call_id>\"]"                                           │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                         Model invokes `inspect(handle: "call:<call_id>")`
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ 3. Lease-Bounded Epistemic Rehydration (Zero Cache Busting)                 │
│    - `PrunedToolSource` / `ArtifactSource` retrieves original `ImagePart`   │
│    - `inspect` returns `ToolOutput::Image { mime, data }`                   │
│    - Harnessed into the CURRENT turn's active tail (Lease = 1 round)        │
│    - Prior history remains 100% untouched: KV-Cache prefix fully intact!     │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 1. Epistemic Multimodal Contracts

In `muta-contracts`:
- `PagedOffstreamContent` is extended with an optional media payload:
  ```rust
  pub struct PagedOffstreamContent {
      pub text: String,
      pub next_cursor: Option<String>,
      pub total_lines: usize,
      pub media: Option<ImagePart>,
  }
  ```
- Tool image pruning replaces the generic `[cleared image payload]` with a structured, informative invoice:
  ```text
  [cleared image payload (<mime>) — rehydrate with inspect handle "call:<call_id>"]
  ```

### 2. Multimodal Rehydration in `InspectTool`

In `muta-agent`:
- `InspectTool::call_structured` checks for `paged.media`.
- When an offstream artifact carries an image, `InspectTool` directly yields `ToolOutput::Image { mime, data }`.
- Through the harness's existing `ToolOutput::Image` lowering pipeline (`rounds.rs`), the image is injected into the **current turn's tail**, granting immediate visual observation for the active step without modifying historical turns.

### 3. Offstream Multimodal Sources

In `muta-runtime`:
- `PrunedToolSource` resolves `call:<call_id>`:
  - Scans persistent transcript and Session IR history.
  - When the tool call is associated with a companion `ToolImage` entry or tool image payload, extracts the unpruned `ImagePart`.
  - Emits `PagedOffstreamContent` with `media: Some(image)`.
- `ArtifactSource` resolves `artifact:<sha256>`:
  - Fetches the raw content-addressed blob from `BlobStore`.
  - Determines media type (e.g. PNG, JPEG, WebP, GIF) or parses stored media envelopes.
  - Emits the rehydrated `ImagePart`.

---

## Invariants & Behavioral Boundaries

- **`[INV-MM-01]` Zero Historical Mutation on Rehydration**: Rehydrating a visual artifact MUST NOT mutate past historical turns or retroactively insert Base64 into pruned nodes. It must append to the active execution tail.
- **`[INV-MM-02]` Invariant Media Claim-Check**: Pruned visual companions must declare their rehydration handle (`call:<call_id>` or `artifact:<sha256>`).
- **`[INV-MM-03]` Modality Parity in Epistemic Memory**: The `inspect` channel is first-class multimodal: text, code slices, and visual images share the same offstream lifecycle and access governance.

---

## Negative Knowledge: Rejected Alternatives (`[INV-AGENT-01]`)

### 1. In-Place Historical Re-hydration
- **Approach**: Modifying past pruned user messages to put the Base64 image back into its original slot when the model requests it.
- **Why Failed**: Completely invalidated the provider-side KV-cache prefix from that turn forward. A single re-read caused massive TTFT latency spikes and destroyed prompt-caching economic benefits.

### 2. Downsampling Images In-Place Instead of Evicting
- **Approach**: Downscaling images to tiny thumbnails (e.g. 64x64) and keeping them in context forever.
- **Why Failed**: 64x64 images still consume hundreds of vision tokens while rendering text, diagrams, and UI details illegible, inducing model visual hallucinations without solving context bloat.

### 3. Separate `inspect_image` Tool
- **Approach**: Introducing an independent `inspect_image` tool alongside `inspect`.
- **Why Failed**: Violates orthogonal tool consolidation (ADR-0179, ADR-0262). Models struggled to disambiguate handles and chose the wrong tool. Epistemic memory inspection must remain a single, unified channel.

---

## References

- [ADR-0230: Three-Valued Vision Capability](0230-three-valued-vision-and-declared-only-gating.md)
- [ADR-0254: Epistemic Observation Lifecycle](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md)
- [ADR-0262: Demand-Paged Epistemic Memory and Unified Inspect Channel](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md)
- [ADR-0279: Scoped Inspect Retrieval, Retention, and Deletion](0279-scoped-inspect-retrieval-retention-and-deletion.md)
- [ADR-0282: Tool Observation Claim-Check Lifecycle and Content-Addressed Backing Store](0282-tool-observation-claim-check-lifecycle-and-cas-backing-store.md)
- [ADR-0283: Anti-Thrashing Context Scheduling and Generative Horizon Compaction](0283-anti-thrashing-context-scheduling-and-generative-horizon-compaction.md)
- [ADR-0284: Separation of Tool Egress Truncation and True Causal Staleness](0284-separation-of-tool-egress-truncation-and-true-causal-staleness.md)
