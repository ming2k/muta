# 0237. One structural query tool, versioned mutations, and an enforced role contract

- **Status:** Accepted
- **Date:** 2026-09-11
- **Amends:** [ADR-0214](0214-on-demand-code-structure-context-and-mutation-freshness.md) — replaces its `get_outline` delivery surface and supplies the missing executor for its freshness invariants. Does not alter ADR-0214's decision against ambient repository maps.
- **Supersedes:** nothing. [ADR-0215](0215-tool-surface-consolidation.md) is still `Proposed`; this record applies its one-tool-per-lifecycle rule to the structural surface without deciding ADR-0215's remaining scope.
- **Scope:** agent/code intelligence, filesystem tools, mutation precondition, subagent dispatch contract
- **Deciders:** Maintainer (delegated to the assistant under an unattended session, 2026-11-20)

## Context and Problem Statement

ADR-0214 established that code structure enters model context through a scoped,
bounded, **versioned** on-demand query rather than an ambient repository map. Two
things were left unsettled, and both turned out to be load-bearing.

**1. The delivery surface was a single file summary.** `get_outline(path)`
(`crates/muta-agent/src/tools/get_outline.rs`) renders one signature line per
top-level item. It answers "what is in this file?" and nothing else. A model
that wants a *particular* declaration has no structural way to ask for it: it
must read the whole file and pay for it. Worse, the tool returns a
content-addressed `version` that **nothing consumes** — no tool, no precondition,
no check. The provenance ADR-0214 mandated for every structural result (§4) was
emitted and discarded.

**2. The freshness invariants had no executor.** ADR-0214 §5 requires that a
successful built-in mutation invalidate derived state, and its acceptance
criteria require that stale evidence cannot be silently reused as current. With
parse-on-demand and no cache, invalidation is automatic — but the *read* side of
that contract was still unenforced. A model may read a file, reason for twenty
turns, and then issue `write_file`, which overwrites the file wholesale. Nothing
compared the model's snapshot against the bytes on disk. `edit_text` is partly
protected — its `old_string` must match uniquely — but that anchor pins only the
edited *region*, so a change elsewhere in the same file is invisible to it.

**3. The dispatch contract was advertised but not enforced.** `spawn_agent`
advertised `role: ["explore", "code", "mcp", "skill"]` in its JSON schema, but
the resolver (`subagent_tool.rs`) did `SubagentPresetPool::find(requested).unwrap_or(self.profile)`.
An unadvertised or misspelled role was **silently downgraded** to the bound
default, and `role: "title"` — a harness-internal role for session titling —
was accepted through the model-facing surface. A caller that asked for a
write-capable child could receive a read-only one without being told.

The measurement that frames the decisions below: the built-in surface is paid
for in **every** request. Measured with the project's own accounting (cl100k BPE
via `ToolSchemaWeights`, ADR-0117 / ADR-0188), the surface was **1579
tokens/request** across 14 tools, of which `spawn_agent` alone was 174 and
`get_outline` 88.

## Decision Drivers

- A structural query must return the evidence the caller actually asked for,
  not a file summary it must then re-read in full.
- The version every structural result carries must have a consumer, or it is
  decoration.
- A write must be refusable when its basis snapshot is gone — fail closed, not
  best effort.
- Every advertised input contract must be enforced; an unenforced enum is a
  silent downgrade waiting to happen.
- The published surface is a recurring cost, so its growth must be a deliberate,
  measured, reviewable act.

## Considered Options

1. Keep `get_outline`; add `get_symbol_context`, `find_syntax`,
   `replace_function_body`, and `check_changes` as four further tools.
2. Keep `get_outline`; add only a freshness tool.
3. Replace `get_outline` with one three-mode `code_query`, add an
   `expected_version` precondition to the existing mutation tools, and enforce
   the dispatch role contract.
4. Build a persistent incremental index first, then revisit the surface.

## Decision Outcome

Chosen option 3.

### 1. `code_query` replaces `get_outline`

One tool, three modes, one parser
(`crates/muta-agent/src/tools/code_query.rs`):

| Mode | Question | Returns |
| :--- | :--- | :--- |
| `outline` | What is in this file? | Indented declaration signatures with start lines |
| `symbol` | Give me this declaration | The declaration's **source**, with path, line span, kind, containers, and version |
| `find` | Where are declarations matching this shape? | One line per match, with kind, qualified name, path, span |

`find` takes a closed grammar over a closed kind vocabulary
(`crates/muta-agent/src/syntax/query.rs`):

```text
pattern := clause ( ( ',' | whitespace ) clause )*
clause  := kind [ ':' name_glob ]
kind    := fn | method | struct | enum | trait | impl | class
         | interface | type | const | static | mod | macro
```

Three semantic choices are binding:

- **The vocabulary is closed and a wrong kind is an error naming the list.** The
  model never writes a tree-sitter S-expression, so a miss is a miss against a
  small word list rather than a runtime parse failure it cannot diagnose.
- **`fn` also matches `method`.** "Where is this function defined?" is the
  question a caller has; whether the function happens to sit inside an `impl` or
  `class` is an implementation detail of the caller's mental model, not of its
  intent. Silently missing a definition is the worst failure mode a structural
  search can have. `method` stays exact for when the member position is the
  point.
- **Nesting is indexed.** Members are reachable through their containers
  (`Service::run`, `Greeter::greet`), one bounded level in per container up to a
  depth of 3, so a method is a first-class target rather than invisible.

Every mode obeys the same three contracts: **bounded** (`limit` entries,
`budget` bytes, 2 MiB per file, 2000 files / 64 MiB per unscoped query, 400
lines per symbol, truncation always disclosed), **versioned** (each result names
the content version of the bytes it described), and **honest** (unsupported
extensions, oversized inputs, and empty scopes are reported as such).

One renderer exists. `extract_symbols` and `format_symbol_signature` are deleted
in favour of `syntax::extract_declarations`, so an outline, a `find` hit, and a
`symbol` span are three projections of one index — they cannot disagree.

### 2. Versioned mutation preconditions

`edit_text` and `write_file` accept an optional `expected_version`
(`check_expected_version`, `crates/muta-agent/src/tools/helpers.rs`), the value
a `code_query` / `read_text` result reported. The check is a whole-file content
digest comparison run **before** any matching or writing work, and it fails
closed in every direction:

- no `expected_version` → no precondition (previous behaviour, unchanged);
- version supplied and the file vanished → refused, rather than silently
  recreating content the caller cannot see;
- version supplied and content differs → refused, naming both versions and
  directing the caller to re-read.

This gives ADR-0214's freshness invariants the executor they lacked. The
coarseness is the point: `old_string` pins the edited region, `expected_version`
pins the file. `write_file` needs it most — a full overwrite has no anchor at
all — but both tools carry it so the mechanism is one concept rather than a
special case.

### 3. The dispatch role contract is enforced

`DISPATCH_ROLES` (`crates/muta-agent/src/subagent_tool.rs`) is the single source
of truth for both the schema's `role` enum and the runtime check. An explicit
role must be advertised, or the call is refused with the dispatchable set named;
an absent role resolves to the **bound profile verbatim**, deliberately without
consulting the preset pool, because a dispatch tool may legitimately be bound to
a caller-supplied preset with no pool entry. `title` is not dispatchable.

### Invariants & Behavioral Boundaries

1. **One structural query tool.** Code structure is retrieved through
   `code_query` alone. A new structural capability is a mode of it, not a
   sibling tool, unless it operates on a different resource (ADR-0143's
   boundary). `get_outline` is retired with no alias (ADR-0135).
2. **One structural index and one renderer.** Outline, symbol, and find results
   derive from `syntax::extract_declarations`. A second extraction or rendering
   path is prohibited: divergence between them is unobservable until it
   misleads.
3. **Query patterns stay closed.** A query names a kind from
   `DECLARATION_KINDS`, optionally with a `*`/`?` name glob. Tree-sitter queries,
   regexes, character classes, brace alternation, and `**` are refused with the
   legal vocabulary named. An unknown kind is an error, never an empty result.
4. **Every structural result carries a version.** The version is the
   content-addressed digest of the exact bytes read, and it is the value
   `expected_version` accepts. No structural result may be reported without it.
5. **Mutations may be made conditional, and the check fails closed.** When
   `expected_version` is supplied, a mismatch or a missing file refuses the
   write before it happens. The check must never be skipped because a read
   failed, and it must never downgrade to a warning.
6. **Advertised inputs are enforced.** No parameter enum may be narrower than
   its runtime check. An unadvertised value is refused with the legal set named
   (ADR-0179); silent substitution of a default is prohibited.
7. **The published surface is budgeted.** The built-in tool surface is pinned by
   a token-budget test in the project's own accounting unit. Raising the budget
   is permitted only as a deliberate, same-change act that records the measured
   total and what was added.
8. **Truncation and unsupported analysis are visible.** Every bound
   (entries, bytes, file size, file count, symbol lines) discloses itself in the
   result. Silent clipping is prohibited.
9. **Read-only tools stay read-only.** All three `code_query` modes are `Read`.
   Structure retrieval is not authorization to overwrite: editing continues to
   use its own conflict and permission checks (ADR-0214).

### Positive Consequences

- A declaration can be fetched by name, so the model reads a function instead of
  a file — the dominant way context was being spent on navigation.
- Structure results are actionable, not merely informational: the version a
  query reports is accepted by a mutation precondition, closing the read→write
  loop that ADR-0214 specified but did not wire.
- A blind whole-file overwrite of content the model never saw is now refusable.
- Role dispatch cannot silently change a child's capability grant, and the
  advertised enum cannot drift from the enforced one.
- One index and one renderer mean outline, find, and symbol cannot disagree
  about what a file contains.
- Growth of the tool surface is a reviewable act with a recorded cost.

### Negative Consequences & Trade-offs

- The surface grew from 1579 to **1864 tokens/request** (+264 from this
  decision): `code_query` costs 274 where `get_outline` cost 88, and the two
  preconditions add 78. This was measured before and after, and the prose on all
  three tools was tightened first (recovering 77 tokens of the opening draft).
  The two capabilities bought are the reason it was accepted; anything further
  must justify itself the same way.
- `outline` mode now includes members, so its output is longer than
  `get_outline`'s for a file with bodies. That is the price of making methods
  first-class targets.
- The whole-file digest is coarser than a region anchor and costs one read per
  preconditioned write. It is also a weaker guarantee than a lock: two writers
  that both hold the current version can still race, and the last write wins.
- `find` scans and parses up to 2000 files; a scope-wide query on a large tree
  is materially more expensive than `search_text`'s line scan. The caps bound it
  but do not make it cheap.
- A syntactically valid but semantically meaningless match is still returned:
  extraction is not type-aware, so a same-named item in a dead configuration
  appears like any other.

## Alternatives considered

- **Ship the four proposed tools (`get_symbol_context`, `find_syntax`,
  `replace_function_body`, `check_changes`).** Rejected on three grounds.
  (a) Cost: measured at 406 tokens for the four, i.e. +318 over `get_outline`,
  and four more standing entries in every request; merging the two read-side
  tools into one `code_query` recovers ~155 of those tokens and removes a
  per-call decision (ADR-0215's stated purpose). (b) `replace_function_body`
  would be a **second mutation lifecycle** — a new write path with its own
  conflict semantics — which ADR-0214 explicitly forbids ("one production
  mutation lifecycle must own validation and invalidation"). Targeted
  replacement already exists as `edit_text` with a unique `old_string`; an
  AST-range-targeted mode belongs *inside* that tool if it is added at all. (c)
  `check_changes` is **strictly dominated**: it answers a boolean, after which
  the caller must re-query anyway to obtain current content. Re-querying with
  `code_query` dominates it in a single round trip, and the write-side
  `expected_version` covers the case where the answer matters (about to write).
  Adding it would be a tool that cannot pay for itself.
- **Make `find_syntax` expose tree-sitter queries.** Rejected: a malformed
  S-expression is a failure the caller cannot diagnose, the accepted node-kind
  vocabulary differs per grammar, and it would make a structural search
  name-dependent on the parser version. The closed vocabulary trades power for
  diagnosability, which is the right trade for a surface the model writes blind.
- **Keep `get_outline` and add `code_query` beside it.** Rejected: two tools on
  one resource with overlapping intent is exactly the decision burden ADR-0215
  removes. `outline` is a mode of `code_query`.
- **Put a freshness check tool in the read-only subagent surface.** Rejected as
  dominated (above). A read-only child that needs current structure re-queries.
- **Add `expected_version` only to `write_file`.** Rejected: `edit_text`
  genuinely needs whole-file drift detection too (its `old_string` cannot see a
  change elsewhere), and a precondition that exists on one writer and not its
  sibling invites the caller to route around the safe one.
- **Enforce roles by rejecting unknown names but keeping the silent default for
  absent roles** (i.e. change nothing else). This is what was chosen — but the
  rejected variant was *inferring* a role from the prompt text. Rejected:
  capabilities must not be granted by a heuristic over prose.
- **Delete the `skill` and `mcp` dispatch roles.** Deferred, not accepted. See
  the negative knowledge below; the `skill` role has no capability increment
  over `explore` (identical toolset) and `mcp` admits the parent's **full**
  toolset rather than dynamic tools only, which inverts the sandboxing intent it
  cites. Neither can be removed cleanly until skill discovery works, and that is
  a separate decision.
- **Build a persistent index first.** Rejected by ADR-0214 and still rejected:
  on-demand parsing is the correctness baseline, and the contract above (version
  provenance, fail-closed preconditions) holds without any cache.

## Negative Knowledge

- **A version with no consumer is not provenance.** `get_outline` emitted one
  for an entire release cycle and nothing read it. Any future "we should return
  a version" proposal must name its consumer in the same change.
- **`check_changes`-style freshness probes are dominated by re-querying.** A
  boolean send the caller back for content anyway. Do not re-propose it.
- **Skill discovery is currently unreachable from the model.** `use_skill` and
  `list_skills` exist in `muta-skills` but **no code path installs them**:
  `Agent::with_skills` attaches the registry only, and `SkillRegistry`'s
  metadata is not projected into the request either. The comment claiming
  "skills are progressively disclosed in the system prompt" describes an
  intention, not the implementation. Today the model reaches skill bodies only
  through `@name` / `skill://` mention injection, or through the `skill`
  subagent **role** — which is why that role was left in place despite having an
  identical toolset to `explore`. Fixing this (an index in the request, or
  installing the tools) is the prerequisite for removing the role, and it has
  not been decided.
- **`mcp_specialist`'s grant is wider than its description.** It declares
  `allowed_tools: None`, which admits the parent's entire toolset (every write
  and execute tool) with `unattended: true`, while its persona text promises "an
  isolated sandbox" with dynamic tools. It also cites ADR-0138, which ADR-0144
  superseded. The stale citation is corrected in code comments; the grant is
  recorded as an open question, not endorsed.
- **`delegate_code` / `delegate_mcp` are not shipped tools.**
  `SubagentTool::named` can construct them and the TUI renders those names, but
  only `spawn_agent` is registered. Their descriptions are referenced from no
  call site. The role enum is the supported path; treat any document claiming
  these are separate registered capabilities as stale.
- **Pattern-language ambition is a trap.** ADR-0143 already learned that
  "documentation alone cannot make an ambiguous tool boundary reliable". A
  structural search that accepts an open-ended pattern language will fail in
  ways the caller cannot diagnose. Closed vocabulary, actionable refusal.

## References

- [ADR-0214: On-demand code structure context and mutation freshness](0214-on-demand-code-structure-context-and-mutation-freshness.md) — amended here; its no-ambient-map decision stands.
- [ADR-0215: Tool surface consolidation](0215-tool-surface-consolidation.md) — the one-tool-per-lifecycle rule applied here.
- [ADR-0143: Filesystem search tools have one job each](0143-filesystem-search-tool-boundaries.md) — the boundary that keeps `search_text` (lines) and `code_query` (declarations) distinct.
- [ADR-0135: Retirement deletes](0135-retirement-deletes-no-teaching-errors.md) — `get_outline` leaves no alias.
- [ADR-0179: Tool modality orthogonality and parameter ergonomics](0179-tool-modality-orthogonality-pattern-consistency-and-parameter-ergonomics.md) — actionable refusals and action-discriminant naming.
- [ADR-0117: Native cl100k BPE tokenizer](0117-native-cl100k-bpe-tokenizer.md) and [ADR-0188: Runtime hot-path degradation elimination](0188-runtime-hot-path-degradation-elimination.md) — the accounting unit the budget test uses.
- [ADR-0011: Sub-agent profiles](0011-subagent-profiles.md) — the capability-axis admission the role enum must not violate.
- [ADR-0087: Code envoy runs autopilot](0087-code-envoy-runs-autopilot.md) — why the `code` role's writes are authorized by delegation.
