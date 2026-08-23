# 0135. Retirement deletes: no teaching errors, no compatibility shims

- **Status:** Accepted
- **Date:** 2026-08-23
- **Builds on:** ADR-0119 (CLI verb vocabulary, lifecycle budgets; the
  "retirement teaches" half is superseded on this one point)

## Context

ADR-0119 retired five top-level spellings (`serve`, `stop`, `status`,
`resume`, `exec`) with a dedicated match arm each, returning an error
that named the canonical form ("'neenee stop' is now 'neenee daemon
stop'"). `session ls` got the same treatment. The intent was
pedagogical: one rerun per user, then the new spelling sticks.

Two years of upkeep say the cost never ended:

- The teaching strings were a second grammar to maintain. Every error
  message elsewhere that named a fix had to be audited against them —
  and drifted anyway: for several releases the dev-drift and
  version-skew errors in `client.rs` told the operator to run
  `neenee stop`, which the teaching arm itself refused. A fix message
  that names a command the same binary refuses is the sharpest form
  this drift takes.
- Five arms plus the `session ls` probe survived two vocabulary
  revisions, each time re-checked, each time re-tested
  (`retired_spellings_teach_the_canonical_form` pinned the strings).
- The shim is invisible in `--help`, completions, and docs — three
  surfaces users actually consult — so its teaching value was bounded
  to the single error moment it existed for.

## Decision

1. **Retirement deletes.** A retired spelling has no parse-tree
   presence at all: no match arm, no alias entry in the `Spec` table,
   no probe. `neenee stop` falls through to the generic
   unrecognized-command error (exit 2) that any typo gets.

2. **Multi-word retired phrases are positional prompts.** `neenee
   serve --fg` parses like any other unknown multi-word phrase: a
   prompt handed to the agent. This is the pre-existing generic
   behavior, not a new rule; deleting the match arm simply lets
   retired words enter the same pool as every other unknown phrase.

3. **Error messages name only commands that exist.** Any hint, fix, or
   pointer in a user-facing message must name a canonical spelling the
   same binary accepts. (The `client.rs`/`serve.rs`/`host.rs`/CLI
   messages now say `neenee daemon stop` etc., enforced by the pinned
   tests.)

4. **The generic paths stay generic.** No per-word special cases are
   reintroduced for retired spellings — not for friendliness, not for
   disambiguation. If a retired word deserves a tip, the existing
   edit-distance `suggest_command` either produces it from the live
   command table or it does not; nothing is hand-wired.

## Alternatives considered

- **Keep the teaching errors (ADR-0119's posture).** Rejected: the
  upkeep is permanent and the strings already drifted into naming
  refused commands. The one-time teaching value does not pay for a
  second grammar.

- **Hidden aliases (parse as the canonical form, silent).** Rejected:
  aliases never die — docs, scripts, and muscle memory keep them alive
  forever, and `--help`/completions must either list them (the
  ambiguity ADR-0119 removed returns) or hide them (undocumented
  behavior, the worst of both).

- **A retirement whitelist that errors while everything else parses as
  a prompt.** Rejected: one more per-word table, exactly the machinery
  this ADR deletes, reintroduced for a distinction (typo vs. retired
  word) no consumer can act on differently.

## Consequences

- `serve`/`stop`/`status`/`resume`/`exec`/`session ls` are gone:
  single words error as unrecognized commands; multi-word phrases
  become prompts. Scripts using the retired forms break loudly at
  parse time with exit 2 (or, for multi-word forms, open a session —
  visible immediately).
- The parser has one grammar; retirement touches only the `Spec`
  tables. Future retirements delete a `Spec` entry and its dispatch
  arm — nothing else.
- Error-message hygiene has a new invariant (fixes name live
  commands); the pinned tests in `cli.rs` and `serve_integration.rs`
  assert canonical spellings.
- ADR-0119's "retirement teaches" row is superseded by this record;
  its lifecycle-budget half is untouched.

## References

- [ADR-0119](0119-cli-verb-vocabulary-and-lifecycle-budgets.md) — the
  vocabulary convergence and spec-table parser the retired arms lived
  in; the teaching posture this record supersedes
- `crates/neenee-cli/src/cli.rs` — the deleted match arm and `session ls`
  probe
