# 0188. Runtime hot-path degradation elimination

- Status: Accepted
- Date: 2026-09-07
- Deciders: ming
- Consulted: —
- Informed: —

## Context and Problem Statement

The persistence correctness work (ADR-0187) removed the O(N²) durable-write
curve, but the evaluation that motivated it also identified per-turn costs in
the request hot path and the streaming renderer that grow with session
length: every turn deep-cloned the whole window twice before assembly, every
estimate pass re-serialized and re-tokenized every visible tool schema, usage
records forced an O(records) row serialization and an O(records²) commit
diff, and every streaming delta performed a reverse linear scan over the
entire rendered transcript.

## Decision Drivers

- Per-turn cost must scale with the turn's delta, not the session.
- Caches must be content-addressed or identity-validated, never trust-on-age.
- Correctness boundaries (what the provider sees) must not move.

## Decision Outcome

Chosen option: "content-addressed or validated hot paths", implemented as:

1. **Single-clone assembly** (`Agent::model_request`): the provider-relevant
   window is filtered while cloning once (system rows first), skill
   injection keeps its pre-echo-filter scan semantics, and the echo/
   empty-assistant filters run in place. The assembler takes ownership via
   `assemble_prepared` — the second full clone per turn is gone.
2. **One assembly per turn**: the context-pressure gate receives the
   already-assembled request (`project_context_if_needed` returns whether
   the window changed), so estimate and provider call share one assembly;
   the duplicated post-turn gate is removed (the gate lives at the
   request-assembly boundary, and projection commits its own durable
   window).
3. **Tool-schema weight cache** (`ToolSchemaWeights`): BPE weights of tool
   specs are content-addressed by (name, description, schema) — a stable
   toolset costs one tokenization, not one per estimate pass.
4. **Usage ledger table**: per-attempt usage records move out of the session
   row into a key-addressed `usage_records` table; a commit upserts only the
   attempts it actually changed (HashMap diff, O(records) once instead of
   O(records²)), and the session row no longer serializes the whole list on
   every save. Wholesale replacement persists as a full rewrite.
5. **Streaming patch cursor** (TUI): `apply_transcript_patch_with_cursor`
   resolves the streaming target O(1) via an id-validated (id, index)
   cursor; a stale cursor degrades to the scan, never to a wrong message.

### Positive Consequences

- Per-turn hot-path work is O(delta) for cloning, schema weighting, usage
  commits, and delta targeting.
- ContextTokens telemetry reflects one consistent assembly per turn.

### Negative Consequences

- Context pressure now resolves after a turn's persist rather than before
  it; the projected window is committed by the projection's own save, so
  the durable transcript remains consistent — a crash between the two
  leaves one extra uncompacted turn, not a divergence.
- The streaming renderer retains one O(rendered-transcript) cached-height
  walk per frame by design (ADR-0184); the cursor removes the per-delta
  scan, while an incremental flex-prefix index remains future work.

## Links

- Builds on [ADR-0187](0187-persistence-v2-incremental-append-and-blob-reference-ledger.md).
- Paint-path contract: [ADR-0184](0184-incremental-streaming-pipeline-and-single-parse-hot-path.md).
