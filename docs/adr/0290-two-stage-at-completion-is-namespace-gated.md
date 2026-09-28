---
id: ADR-0290
title: "Two-Stage `@` Completion Is Namespace-Gated: No Content Pass-Through"
status: accepted
date: 2026-09-28
scope: runtime/completion, terminal/composer
superseded_by: null
negative_knowledge: true
---

# 0290. Two-Stage `@` Completion Is Namespace-Gated: No Content Pass-Through

- **Status:** Accepted
- **Date:** 2026-09-28
- **Scope:** `muta-runtime` (`input_completion`), `mutx` (composer completion), `muta-contracts` (`ComposerCompletion`)
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0256](0256-unified-entity-mentions-lexical-escaping-and-two-tier-completion-pipeline.md) (unified entity mentions and the two-tier completion pipeline), [ADR-0288](0288-entity-references-are-asset-references-and-canonical-wire-envelopes.md) (entity references are asset references), [ADR-0162](0162-zero-latency-two-tier-completion-and-flicker-free-composer.md) (two-tier completion)

---

## Context and Problem Statement

ADR-0256 defined the composer's `@` pipeline as **two-stage**: a bare `@` offers
the entity *namespaces* (`@file:`, `@skill:`), and content (project files,
skills) appears only after a namespace has been committed. It also introduced
**"fuzzy-at-mention"** as an explicit ergonomics feature: typing a bare query
like `@creat` was said to "match files and skills simultaneously", and an
accepted row would "automatically commit the canonical `@file:…` representation".

The implemented behavior, however, does not match the two-stage claim. In the
daemon engine (`crates/muta-runtime/src/input_completion.rs`,
`complete_unified_entities`) and its frontend mirror
(`complete_for_frontend_test`), a query that is **not** a prefix of any
namespace name takes the *same* branch that is supposed to be Stage 1, and then
falls through to emit Stage-2 content:

```rust
// namespace-prefix items …
if query.is_empty() { return items; }
// then, for ANY non-empty bare query:
for skill in registry.list() { if skill.name.contains(query) { push(skill_item) } }
for path in scan_project_files(root) { if path.contains(query) { push(path_item) } }
```

The consequences contradict the design the same file claims to implement:

1. **The namespace gate is not a gate.** Typing `@xyz` — a query that no
   namespace matches — returns files and skills. Stage-2 content is reachable
   *without ever committing a namespace*, which is precisely what "two-stage"
   was defined to prevent.
2. **Undelimited content search is broad.** The content match is a substring
   `contains(query)` (`path_query_match` / `skill.name.contains`). `@main` pulls
   every path containing `main`; `@Cargo` pulls `Cargo.toml`, `Cargo.lock`, and
   any directory with those letters. A user typing an `@`-phrase mid-sentence
   gets an unbounded menu.
3. **Two code paths, one requirement, divergent.** The daemon engine
   (`complete_namespaces` after this ADR) and the test mirror
   (`complete_for_frontend_test`) duplicated the branch; both had the leak. The
   mirrored copy is what the terminal's own tests exercise, so the two could
   silently drift.
4. **The badge/aliasing rationale is dead.** ADR-0256 justified pass-through by
   "clear visual badges (`[File]`, `[Skill]`)" distinguishing a fuzzy row's
   origin. But the row already carries a kind (`PathFile`/`PathDir`) and the
   committed text; the badge does not require the surfaced row to *bypass* the
   namespace stage — a namespace-completed row (`@file:src/main.rs`) is
   unambiguous by construction and needs no disambiguation badge at all.

The reference semantics ADR-0288 settled make the tension sharper: an
`@`-reference is an *address* with a mandatory namespace, so a bare `@query`
that silently resolves to content would let the composer commit an address the
user never addressed.

---

## Decision Drivers

- **Two-stage means two stages.** Stage-2 content must be reachable only through
  a committed namespace, never through a bare query.
- **The namespace is the gate.** A query either names (a prefix of) a namespace —
  in which case Stage 1 offers it — or it names nothing, in which case the menu
  is empty. There is no third outcome.
- **Deliberate commitment.** Content is surfaced because the user asked for
  `file:` or `skill:`, not because a substring happened to appear in the menu.
- **One implementation.** The daemon engine and the frontend-test mirror must
  share the Stage-1 derivation so they cannot drift.

---

## Considered Options

- **Option 1: Keep fuzzy-at-mention, fix the claim.** Document that Stage 1 also
  fuzzy-matches content.
- **Option 2: Namespace-gated (remove pass-through).** A bare `@query` offers
  only the namespaces whose names it is a prefix of; content lives exclusively
  under `@file:` / `@skill:`.
- **Option 3: Namespace-gated, plus a separate explicit content-search affordance**
  (e.g. `@?query`).

---

## Decision Outcome

Chosen option: **Option 2**. We remove the content pass-through and make the
namespace the sole gate to Stage 2.

### 1. Stage 1 is namespace-only

A bare `@` (or `@prefix`) yields **only** the namespace rows whose name is a
prefix of what was typed:

- `@`          → `@file:`, `@skill:`
- `@f`, `@fi`  → `@file:`
- `@s`, `@sk`  → `@skill:`
- `@xyz`       → *(empty)*

`[INV-COMPLETE-01]` **Namespace Gate.** A query that is not a prefix of a
namespace name yields no completions. Stage-2 content (files, skills, explicit
paths) is never returned for a bare `@query`.

### 2. Stage 2 is entered only by a committed namespace

Content appears only when the buffer the user is editing already carries a
namespace:

- `@file:` / `@files:` → project files and directories.
- `@skill:` / `@skills:` → skill names.
- `@./`, `@../`, `@~/`, `@/` → explicit filesystem paths (raw path completion,
  outside the workspace sandbox; unchanged).

`[INV-COMPLETE-02]` **Content Only Under a Namespace.** Every Stage-2 candidate's
edit range (`replace_start..replace_end`) begins at or after a committed
namespace token, so accepting it always produces a canonical
`@file:…` / `@skill:…` address (ADR-0288 `[INV-REF-03]`).

### 3. Single Stage-1 derivation

The daemon engine and the frontend-test mirror share one function
(`namespace_items`) that computes the Stage-1 rows. The mirror exists only to
serve tests; it must not grow a second copy of the stage logic.

`[INV-COMPLETE-03]` **No Parallel Stage Logic.** The `@` stage decision lives in
exactly one place per process (`InputCompletionEngine` + the documented
frontend-test mirror that delegates to the same helper).

---

### Positive Consequences

- The composer behaves as documented: a bare `@` never dumps content; the
  namespace is a real commitment step.
- Typing an `@`-phrase in prose no longer opens an unbounded substring menu over
  every skill and path.
- The daemon engine and the terminal's tests can no longer diverge on what
  Stage 1 returns — they call the same helper.

### Negative Consequences & Trade-offs

- **One extra keystroke for the shortest path to a file.** A user who wants
  `@file:` must now commit the namespace before file names appear. Mitigation:
  the namespace rows are offered from the first `@`, and `@f`/`@s` narrow to a
  single row, so committing is one `Tab`/`Enter`.

---

## Rejected Alternatives & Negative Knowledge

### Option 1 — Keep fuzzy-at-mention, fix the claim (rejected)

- **Why considered**: It was the status quo and the ADR-0256 ergonomics rationale.
- **Why rejected**: It does not preserve the design's central property (Stage 2
  is gated), and it requires the substring `contains` search that produces the
  unbounded menus the feature was never specified to produce. It also
  contradicts ADR-0288's "an `@`-reference is an address with a namespace": a
  bare query would commit an address the user never wrote.

### Option 3 — Namespace gate plus `@?query` content-search (rejected)

- **Why considered**: Keeps a discovery affordance for users who half-remember a
  skill or file name.
- **Why rejected**: It introduces a fourth sigil whose meaning overlaps the two
  existing namespaces, and the same discovery is already reachable by a two-tap
  path (`@f` → `Tab` → type). A new trigger for a need that the gate already
  serves is speculative surface area; reject until a concrete workflow demands
  it.

### Ordering the fuzzy content *after* matching namespace rows (rejected)

- **Why considered**: A "best of both" that keeps content visible while
  namespace rows lead the list.
- **Why rejected**: The namespaces would still be bypassable — the content rows
  would still be accepted without commitment, which is the exact behavior being
  removed. Ordering does not restore the gate; only removal does.

---

## Links

- Reverses the "fuzzy-at-mention" alternative recorded in
  [ADR-0256](0256-unified-entity-mentions-lexical-escaping-and-two-tier-completion-pipeline.md);
  the two-stage grammar and the completion types it introduced remain.
- Depends on: [ADR-0288](0288-entity-references-are-asset-references-and-canonical-wire-envelopes.md)
  (canonical addresses are the only thing an accepted row may commit).
- Implements: `crates/muta-runtime/src/input_completion.rs`
  (`namespace_items`, `complete_namespaces`), `mutx` composer completion.
- Related: [ADR-0162](0162-zero-latency-two-tier-completion-and-flicker-free-composer.md)
  (the latency tiers these stages ride on).
