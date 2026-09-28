---
id: ADR-0291
title: "Single-Owner `@`-Reference Grammar Kernel: One Scanner, No Duplicated Lexical Logic"
status: accepted
date: 2026-09-28
scope: contracts/mention, agent/conversation-context, skills/render, runtime/completion
superseded_by: null
negative_knowledge: true
---

# 0291. Single-Owner `@`-Reference Grammar Kernel: One Scanner, No Duplicated Lexical Logic

- **Status:** Accepted
- **Date:** 2026-09-28
- **Scope:** `muta-contracts` (`mention`), `muta-agent` (`conversation_context`), `muta-skills` (`render`), `muta-runtime` (`input_completion`), `mutx`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0256](0256-unified-entity-mentions-lexical-escaping-and-two-tier-completion-pipeline.md) (unified entity mentions), [ADR-0288](0288-entity-references-are-asset-references-and-canonical-wire-envelopes.md) (references are asset references), [ADR-0290](0290-two-stage-at-completion-is-namespace-gated.md) (namespace-gated completion), [ADR-0057](0057-contract-only-core-boundary.md) (contract-only core boundary), [ADR-0059](0059-agent-tool-integration-boundary.md) (agent/skills edge)

---

## Context and Problem Statement

ADR-0256 defined the surface grammar of `@`-references (code-span masking, the
delimiter alphabets, the word-boundary and backslash-escape guards). ADR-0288
gave the grammar a semantic contract (a reference resolves to one canonical
envelope). Both were implemented, but the *grammar itself* was never given a
single owner. By the time this ADR was written, the same lexical logic existed
in five places:

1. `crates/muta-skills/src/render.rs` — `mask_code_spans`, `is_name_char`,
   `at_mention_names` (the `@` scanner), `skill_uris` (the `skill://` scanner),
   and the word-boundary/escape guard inline.
2. `crates/muta-agent/src/conversation_context/files.rs` — `mask_code_spans`,
   `is_path_char`, `parse_file_refs` (the `@` scanner), and the guard inline.
3. `crates/muta-agent/src/conversation_context/mentions.rs` — a third `@`
   scanner for address canonicalization, with its own copy of the guard.
4. `crates/muta-runtime/src/input_completion.rs` — `mention_range_at`.
5. `apps/terminal/crates/mutx/src/completion.rs` — `mention_range_at`,
   **duplicated verbatim** from (4).

This produced concrete defects:

- **The guard was triplicated** (`prev.is_whitespace() || prev == '(' | …`), so
  a grammar change had three edit sites that could silently diverge — a
  boundary fix in one extractor would not reach the other two.
- **`mention_range_at` was byte-identical in two crates**, meaning the daemon's
  notion of "the token the cursor is in" and the composer's could drift while
  every test still passed, because each crate tested its own copy.
- **`mask_code_spans` existed in two copies** before ADR-0288 partially
  consolidated it.
- Adding a namespace or a reference form required touching every scanner; there
  was no enumerated set of "what a reference is".

The root cause is that the grammar was treated as *local implementation detail*
of each consumer, when it is a **shared, stable vocabulary** — exactly the
category ADR-0057 admits to `muta-contracts`.

---

## Decision Drivers

- **One owner per rule.** Masking, the alphabets, the start-guard, the `@` scan,
  the `skill://` scan, and the cursor token-range each have exactly one
  implementation.
- **The grammar is shared vocabulary.** It is consumed by four independent
  layers, and it cannot live in any single one of them (see ADR-0057 argument
  below).
- **Recognition vs. policy.** The kernel *recognizes and segments* references;
  each consumer *filters* the scan and applies its own policy (skip escaped,
  load a file, match a skill, rewrite an address). Policy stays with its owner.
- **No legacy.** The duplicate scanners are deleted, not deprecated alongside
  the kernel.

---

## Decision Outcome

Chosen option: **one grammar kernel in `muta-contracts::mention`, consumed
everywhere.**

### 1. The kernel (`crates/muta-contracts/src/mention.rs`)

- `mask_code_spans` — code-span masking (single owner).
- `is_path_char` / `is_name_char` — the delimiter alphabets (single owner).
- `reference_start(text, at) -> Start` — the word-boundary/escape guard,
  returning `Boundary` / `Escaped` / `Rejected` (single owner).
- `scan_references(text) -> Vec<Reference>` — the one `@` **and** `skill://`
  scanner. A [`Reference`] is a typed value:
  `{ namespace: Namespace, form: Form, target: &str, start, end, escaped }`,
  with `Namespace ∈ {File, Skill}` and `Form ∈ {Qualified, Bare, Uri}`.
- `mention_range_at(input, cursor) -> Option<(usize, usize)>` — the cursor
  token-range (single owner).

### 2. Consumers become filters

- `muta-agent` `parse_file_refs` = filter `scan_references` on
  `namespace == File && !escaped`.
- `muta-agent` `canonicalize_addresses` = rewrite `scan_references` targets
  (policy: qualified → canonical, bare untouched, escape consumed).
- `muta-skills` `resolve_mentions` = filter `scan_references` on
  `namespace == Skill`, then name/URI equality.
- `muta-runtime` and `mutx` `mention_range_at` = delegates to the kernel.

No consumer holds a scanner, a masker, or the guard.

### 3. Why the kernel lives in `muta-contracts` (ADR-0057 admission)

ADR-0057 admits an item to the bottom crate only when it is **exchanged by
multiple independent layers**, **breaks a dependency cycle**, or is **stable
wire/domain vocabulary**. The kernel satisfies the first and third, and the
second is decisive: `muta-skills` consumes the grammar but **must not depend on
`muta-agent`** (that would invert the `agent → skills` edge, ADR-0059), and
`muta-runtime` and the terminal both need the identical guard. No other crate is
simultaneously below all of them. `muta-contracts` is therefore the only
acyclic single-source home — the same reasoning that places `Message` and
`ToolSpec` there. The kernel remains pure and I/O-free: it resolves nothing.

---

### Invariants & Behavioral Boundaries

- **`[INV-GRAMMAR-01]` Single Scanner**: exactly one `@`/`skill://` scanner
  exists in the workspace (`mention::scan_references`). Every extractor is a
  filter over it.
- **`[INV-GRAMMAR-02]` Single Guard**: the word-boundary/escape decision is
  computed only by `mention::reference_start`; no consumer re-implements
  `is_whitespace() || matches!(prev, '(' | …)`.
- **`[INV-GRAMMAR-03]` Single Token-Range**: `mention::mention_range_at` is the
  only implementation of the cursor mention range.
- **`[INV-GRAMMAR-04]` Recognition ≠ Resolution**: the kernel performs no I/O
  and holds no registry; loading, matching, and canonicalization are consumer
  policy.

### Positive Consequences

- A grammar change (new namespace, new form, a boundary tweak) is a one-file
  edit that provably reaches every consumer.
- The daemon and the composer can no longer disagree about the mention range —
  they call the same function.
- The set of "what a reference is" is an enumerated type (`Namespace`, `Form`,
  `Reference`), not an emergent property of five scanners.

### Negative Consequences & Trade-offs

- **The kernel is a public contract surface.** Its types (`Reference`,
  `Namespace`, `Form`, `Start`) are now `pub`. Mitigation: they are small,
  closed, and stable; widening them is itself an ADR-worthy change.
- **A one-crate-wide rebuild** when the grammar changes. Accepted: grammar
  changes are rare and the correctness win dominates.

---

## Rejected Alternatives & Negative Knowledge

### Keep per-consumer scanners (status quo) (rejected)

- **Why considered**: zero migration; each consumer stays self-contained.
- **Why rejected**: it is the defect. The guard was triplicated and
  `mention_range_at` was byte-duplicated across two crates; the copies pass
  their own tests while diverging, which is the worst failure mode (green
  tests, wrong behavior at the seam).

### Put the kernel in `muta-agent` and let `muta-skills` reach through it (rejected)

- **Why considered**: the agent is the primary consumer and owns reference
  *lifecycle*.
- **Why rejected**: `muta-skills` cannot depend on `muta-agent` without
  inverting the `agent → skills` edge (ADR-0059) and creating a cycle. It would
  force skills to either duplicate the grammar again or collapse the skills
  capability into the agent.

### A dedicated `muta-mentions` micro-crate (rejected)

- **Why considered**: maximal separation; a tiny focused crate.
- **Why rejected**: ADR-0057 explicitly warns against splitting the bottom
  crate for label-based reasons; a crate holding three pure functions and one
  scanner adds a manifest, a re-export surface, and compile fan-out for no
  demonstrated dependency boundary. The `muta-contracts` module is the right
  granularity; promote it only when a real independent consumer demands it.

### Keep a thin per-crate wrapper with a doc comment (partially adopted, bounded)

- **Why considered**: preserves a stable local call site and test import while
  delegating.
- **Why rejected as a general pattern / bounded where used**: a wrapper that
  *delegates* (one line, no logic) is acceptable for import ergonomics in
  `mutx`; a wrapper that *duplicates* logic is the thing this ADR forbids
  (`[INV-GRAMMAR-01]`–`03`).

---

## Links

- Consolidates the grammar ADR-0256 introduced and the semantics ADR-0288
  defined; complements ADR-0290's namespace gate.
- Admission rationale: [ADR-0057](0057-contract-only-core-boundary.md);
  the edge it protects: [ADR-0059](0059-agent-tool-integration-boundary.md).
- Implements: `crates/muta-contracts/src/mention.rs` (kernel),
  `crates/muta-agent/src/conversation_context/{files,mentions}.rs`,
  `crates/muta-skills/src/render.rs`,
  `crates/muta-runtime/src/input_completion.rs`,
  `apps/terminal/crates/mutx/src/completion.rs`.
