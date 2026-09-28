---
id: ADR-0288
title: "Entity References Are Asset References: Canonical `@{namespace}:{target}` Resolution and Wire Envelopes"
status: accepted
date: 2026-09-28
scope: contracts/message, agent/conversation-context, skills/render, runtime/completion
superseded_by: null
negative_knowledge: true
---

# 0288. Entity References Are Asset References: Canonical `@{namespace}:{target}` Resolution and Wire Envelopes

- **Status:** Accepted
- **Date:** 2026-09-28
- **Scope:** `muta-contracts` (`Message`), `muta-agent` (`conversation_context`), `muta-skills` (`render`), `muta-runtime` (`input_completion`)
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0256](0256-unified-entity-mentions-lexical-escaping-and-two-tier-completion-pipeline.md) (unified entity mentions), [ADR-0213](0213-model-request-composition-and-context-lifecycle.md) (model request composition), [ADR-0050](0050-command-echo-non-driving-transcript-fidelity.md) (non-driving transcript fidelity), [ADR-0056](0056-model-context-assembly-boundary.md) (model-context assembly boundary), [ADR-0252](0252-unified-asset-identity-ttl-lifecycle-and-zero-bypass-attestation.md) (asset identity and locators)

---

## Context and Problem Statement

ADR-0256 unified the *surface* grammar of prompt entity references on `@{namespace}:{target}` (`@file:`, `@skill:`, `@session:`) and gave the composer lexical escaping and boundary guards. It did not settle the **transmission** contract: what a `@`-reference actually *is* once it leaves the composer.

The live behavior is injection-as-**addition**, never resolution-as-**substitution**:

1. The composer commits the *literal* address text. Accepting a file candidate splices `@file:src/main.rs ` verbatim (`crates/muta-runtime/src/input_completion.rs`, `path_item`) into `AgentRequest::Chat`, and nothing rewrites it afterwards.
2. Turn preparation resolves mentions and *appends* a hidden companion message:
   - `crates/muta-agent/src/conversation_context/files.rs` pushes `[File '<path>' loaded]\n<body>\n[/File]` (or `[File '<raw>' not loaded: <reason>]`).
   - `crates/muta-agent/src/conversation_context/skills.rs` pushes `<skill name="…" scope="…">…</skill>` from `muta-skills::render::format_skill_injection`.
3. `Message::to_wire()` (`crates/muta-contracts/src/message.rs`) strips `hidden` and `origin` and copies `content` byte-for-byte.

The consequence is that **the provider receives both the unresolved literal *and* the resolved asset**:

- The user message still reads `@file:src/main.rs` — a raw, uninterpreted token that the model must guess the meaning of.
- The address inside the companion message is a *display* string, not a canonical address: `@files:` and `@file:`, the `@skills:`/`@skill:`/bare-`@name`/`skill://` aliases, and percent or trailing punctuation variants all survive into the wire as distinct spellings of one asset.
- The backslash escape `\@file:x` is only *skipped* by the extractors; the backslash is never consumed, so the literal text the user intended (`@file:x`) never actually reaches the model in that form.
- `@session:{id}` / console `@N` is declared in ADR-0256 but has no implementation, and is a *dispatch* target — it has no business consuming payload bytes on the wire at all.
- Compounded for files: the truncation marker and rejection reason are prose baked into the body, so the model cannot distinguish "here is the asset" from "here is why there is no asset" without reading prose.

**The design defect is that `@` is being treated as content.** A `@`-reference is an *address*, and an address is not what a consumer of the wire should ever see — the consumer should see the **asset it addresses**, or an explicit statement that the address failed to resolve.

This is the same distinction the security plane already makes: `AssetLocator` (`crates/muta-contracts/src/security.rs`) governs an asset's *trust* identity, while `@{namespace}:{target}` governs its *reference* identity. The two must be symmetric: an address that resolves once produces exactly one canonical, machine-readable envelope.

---

## Decision Drivers

- **Reference, not content**: `@` is a reference operator. The wire must carry the referenced asset, never the bare address.
- **One address, one spelling**: `@file:`/`@files:`, `@skill:`/`@skills:`/bare-`@name`/`skill://` are input-surface aliases; on the wire they must collapse to one canonical form.
- **Machine-readable outcome**: success, truncation, and rejection must be *structured attributes*, not prose sentences the model has to interpret.
- **Transcript fidelity is not negotiable**: the durable transcript keeps exactly what the user typed (ADR-0050). Only the request projection is canonicalized.
- **No new wire field**: the resolution travels in the message content, where the model already reads it, so no `Message` schema change and no migration (ADR-0017 / ADR-0022 backward compatibility).
- **Dispatch references stay off the wire**: `@session:`/`@N` is control-plane routing; it is consumed by the harness and contributes zero bytes.

---

## Considered Options

- **Option 1: Status quo — literal address plus prose companion.** Keep `@file:x` in the prompt and append `[File 'x' loaded]…`.
- **Option 2: Structured field on `Message`.** Add `asset_refs: Vec<AssetRef>` to the content contract, populated at injection time.
- **Option 3: Canonical XML envelopes in message content, with the bearer address carried as a `ref=` attribute.** Resolve each mention to a namespaced envelope (`<file …>`, `<skill …>`); canonicalize the visible address in the request projection; keep the transcript verbatim.

---

## Decision Outcome

Chosen option: **Option 3**, because it satisfies every driver without a schema change, without a migration, and without touching the durability contract.

### 1. The three-layer reference model

| Layer | Representation | Nature | Consumer |
|-------|----------------|--------|----------|
| **L0 — Surface** | `@{namespace}:{target}` (`@file:`, `@skill:`, `@session:`) | Human-readable **address** | User, composer |
| **L1 — Resolution** | address → asset (sandbox, read, size cap, dedup ledger, trust) | Address **resolved** | `conversation_context::{files,skills}` |
| **L2 — Projection** | asset **envelope** (+ canonical pointer in visible prose) | Asset **entity** | Provider |

**`[INV-REF-01]` No Unresolved Literal Crosses L2.** An `@{namespace}:{target}` token must never reach the provider as an unresolved, uninterpreted literal. At L2 every mention is either (a) represented by exactly one envelope carrying its canonical address, or (b) replaced in visible prose by a canonical, non-extractable pointer to that envelope.

### 2. Canonical envelopes (the wire shape)

One mention ⇒ exactly one envelope. Envelopes are namespaced, carry the canonical address as `ref=`, and express outcome as attributes:

```xml
<!-- resolved · file -->
<file path="src/main.rs" ref="@file:src/main.rs" bytes="10">
fn main() {}
</file>

<!-- resolved · truncated (attributes, not prose) -->
<file path="big.txt" ref="@file:big.txt" bytes="51200" total="102400" truncated="true">…</file>

<!-- resolved · skill (existing envelope gains the canonical address) -->
<skill name="rust-expert" scope="repo" ref="@skill:rust-expert">
  <system_guidance>…</system_guidance>
  <instructions>…</instructions>
</skill>

<!-- rejected (one per attempted mention; the recovery hint is the `reason` attribute) -->
<file ref="@file:missing.rs" status="rejected" reason="could not resolve under the workspace root: os error 2"/>
<file ref="@file:huge.rs"  status="deferred" reason="per-round file-injection limit (10) reached"/>
<file ref="../secret"     status="rejected" reason="`..` traversal is not allowed"/>
```

- `ref` is **always** the canonical full address (`@file:{path}`, `@skill:{name}`), never a display path or a plural/alias spelling.
- `status` is absent (success) or one of `rejected` / `deferred`.
- Truncation and size are attributes; the model needs no prose interpretation to know it holds a partial asset.

**`[INV-REF-02]` Envelope Cardinality.** A resolved or rejected mention produces exactly one envelope. No second "loaded" prose marker is emitted; envelope presence *is* the load record.

### 3. Canonicalization of the visible address (request projection only)

`[INV-REF-03]` **Aliases Collapse at L2.** The request projection rewrites every recognized address spelling to its canonical namespace: `@file:`/`@files:` → `@file:{path}`; `@skill:`/`@skills:`/bare-`@name`/`skill://{name}` → `@skill:{name}`. Trailing sentence punctuation is not part of the address and is not carried.

`[INV-REF-04]` **Escapes Are Consumed at L2.** `\@file:…` / `\@skill:…` become the literal text `@file:…` / `@skill:…` in the request projection. The escape exists to suppress resolution at L1; once resolution has run, the address is inert text and the backslash has no remaining meaning.

`[INV-REF-05]` **Transcript Verbatim.** L2 canonicalization applies to the *request projection* only (`Agent::model_request`). The durable transcript, event log, resume, and export keep the user's exact input, including aliases and backslashes. This is ADR-0050's non-driving-fidelity axis applied to references.

### 4. Dispatch references contribute zero bytes

`[INV-REF-06]` **Routing Is Not Payload.** `@session:{id}` / console `@N` is a dispatch reference consumed by the harness at L1. It is never rendered into a provider request. Until a dispatcher exists, the composer must not advertise the namespace (ADR-0256 declared it ahead of its implementation).

### 5. Compatibility

`[INV-REF-07]` **Legacy Markers Remain Readable.** Deduplication must recognize both the canonical envelopes and the legacy `[File '…' loaded]` / `[Skill '…' loaded]` / `<skill name="…">` markers, so sessions persisted by earlier versions neither re-inject nor diverge (ADR-0017 / ADR-0022).

### 6. Grammar ownership

The lexical grammar these references are recognized by (masking, alphabets, the
word-boundary/escape guard, and the `@`/`skill://` scanner) is owned by a single
kernel in `muta-contracts::mention`, consumed by every extractor as a filter.
See [ADR-0291](0291-single-owner-mention-grammar-kernel.md) — the recognition
half of this ADR's resolution contract.

---

### Positive Consequences

- The model reads the asset, not an address plus a guess: no token in the prompt has to be interpreted by the model.
- Success, truncation, and rejection become machine-readable attributes — the recovery contract (`read_text`, `list_dir`, narrow the path) is a structured `reason`, not a sentence to parse.
- One canonical spelling per asset on the wire removes an entire class of prompt-cache prefix divergence between sessions that spelled the same reference differently.
- Zero schema change, zero migration: resolution still travels in message content, so every existing consumer (transcript renderer, export, web panel) keeps working.
- The reference address and the trust locator become visibly symmetric, which makes future address-driven trust lookups (a mention resolving to an `AssetLocator`) a natural extension rather than a redesign.

### Negative Consequences & Trade-offs

- **XML envelopes cost more tokens than the bracketed marker.** Mitigation: the marker and the body were already separate lines; `ref=` adds one attribute per mention, bounded by the per-round caps (10 files) that already exist.
- **Two rendering paths in `conversation_context`** (canonical emit, legacy accept). Mitigation: the legacy reader is confined to the dedup extractor and dies when no persisted session predates this ADR.

---

## Rejected Alternatives & Negative Knowledge

### Option 1 — Status quo: literal address plus prose companion (rejected)

- **Why considered**: Zero implementation cost; every existing test already asserts the `[File '…' loaded]` shape.
- **Why rejected**: It is precisely the defect. It ships an uninterpreted token (`@file:src/main.rs`) *and* an asset, leaves alias spellings divergent on the wire, never consumes escapes, and forces the model to parse prose ("file truncated at N bytes") to learn the state of the asset it was handed.

### Option 2 — Structured `asset_refs: Vec<AssetRef>` field on `Message` (rejected)

- **Why considered**: Strongest typing; a reference would be a first-class value rather than text.
- **Why rejected**: It splits the resolution across two channels the model reads differently. The model consumes *content*; an adjacent structured field would be invisible to it and dead weight on the wire, while still requiring the content to describe the asset anyway. It also forces a schema change on the most load-bearing durability type for no behavioral gain, violating the ADR-0017/ADR-0022 compatibility contract for a purely presentational improvement.

### Deleting the address from the visible prompt entirely (rejected)

- **Why considered**: If the envelope carries the asset, the address is redundant and could be dropped, saving tokens.
- **Why rejected**: The user's sentence must remain coherent and auditable — "please review @file:src/main.rs" must still read as a sentence about that file, and hidden-message provenance must stay attributable to the address the user typed. Removing the pointer breaks readability, breaks dedup provenance, and hides from the model *why* the envelope exists. Canonicalization, not deletion, is the correct operation.

### Keeping `@session:` advertised before its dispatcher exists (rejected)

- **Why considered**: Documented in ADR-0256 as part of the namespace set.
- **Why rejected**: A namespace with no consumer is worse than an absent one: the composer would offer a reference the harness ignores, producing exactly the unresolved-literal-on-the-wire failure this ADR prohibits.

---

## Links

- Supersedes nothing; extends [ADR-0256](0256-unified-entity-mentions-lexical-escaping-and-two-tier-completion-pipeline.md) from surface grammar to transmission contract.
- Amendment note appended to ADR-0256 §Decision.
- Implements: `crates/muta-agent/src/conversation_context/files.rs`, `crates/muta-agent/src/conversation_context/skills.rs`, `crates/muta-skills/src/render.rs`.
- Sibling concept: `AssetLocator` / `AssetSpec` in `crates/muta-contracts/src/security.rs` ([ADR-0252](0252-unified-asset-identity-ttl-lifecycle-and-zero-bypass-attestation.md)).
- Related: [ADR-0213](0213-model-request-composition-and-context-lifecycle.md), [ADR-0217](0217-request-components-and-derived-cache-plan.md), [ADR-0050](0050-command-echo-non-driving-transcript-fidelity.md).
