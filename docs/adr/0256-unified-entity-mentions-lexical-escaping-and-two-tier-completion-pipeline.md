# 0256. Unified Entity Mentions, Lexical Escaping, and Two-Tier Completion Pipeline

- **Status:** Accepted
- **Date:** 2026-03-31

## Context

Prior to this decision, composer completions and prompt entity references suffered from severe architectural bifurcation and lexical leakage:

1. **Vocabulary Bifurcation and Semantic Mismatch**:
   - The harness agent context layer (`crates/muta-agent/src/conversation_context/files.rs`) strictly recognized `@file:{path}` and `@files:{path}` for implicit sandboxed file-content injection.
   - The TUI completion layer (`crates/muta-runtime/src/input_completion.rs` and `apps/terminal/crates/mutx/src/completion.rs`) treated `@path` purely as a path typing shortcut: upon completion acceptance, it stripped the `@` trigger and spliced a bare string (`src/main.rs`). The accepted path could never satisfy `files.rs`, failing to inject referenced context into subsequent rounds.
   - Skill references used `@skill:{name}` while files used bare paths, breaking orthgonality and symmetry across entity references.

2. **Absence of Lexical Escaping and Boundary Guards**:
   - `crates/muta-agent/src/conversation_context/files.rs` and `crates/muta-skills/src/render.rs` scanned prompt text for `@` without verifying word boundaries, backslash escaping, or markdown code-span boundaries.
   - Technical discussions quoting mentions inside inline code (`` `@file:...` ``), fenced code blocks, or emails (`user@file:...`) unconditionally invoked filesystem queries (`load_sandboxed`), polluting the conversation with repeated hard errors (`[File '...' not loaded: os error 2]`) across rounds.

3. **Duplicated TUI Presentation Routing**:
   - `mutx` hardcoded `CompletionKind` into `Slash` versus `Path`, maintaining disparate anchor calculations and ad-hoc continuation conditions (`!matches(comp.kind, PathDir)`).

## Decision

We establish a unified, uncompromised entity mention and completion architecture across contracts, agent context injection, runtime completion, and terminal TUI:

1. **Canonical Namespace Grammar**:
   - Every entity reference follows the strictly typed `@{namespace}:{target}` grammar:
     - `@file:{relative_path}` — Sandboxed workspace file content injection.
     - `@skill:{skill_name}` — Skill definition and instructions injection.
     - `@session:{id}` (or console `@N`) — Multi-session console target dispatch.
   - Bare `@path` references that strip the trigger on accept are abolished. File completion candidates accept directly as `@file:{path}` with a trailing space.

2. **Lexical Escaping and Three-Tier Boundary Guards**:
   - **Markdown Code Span Immunity**: Parsing masks all inline code (`` `...` ``) and fenced blocks (```` ```...``` ````) with byte-preserving spaces before reference extraction. Mentions within code are treated as literal discussion and never trigger filesystem or registry I/O.
   - **Backslash Escaping**: `\@file:...` and `\@skill:...` are explicitly skipped by mention extractors and unescaped to literal strings when building transcripts.
   - **Word Boundary Guards**: An `@` mention must begin at offset 0, follow whitespace, or follow open punctuation delimiters (`(`, `[`, `{`, `"`, `'`, `<`).
   - **Attempt Ledger Suppression**: Attempted file injections (both loaded and rejected) are recorded in the turn session state, preventing infinite error-injection loops across subsequent conversation turns.

3. **Two-Tier Completion Engine and Continuation Protocol**:
   - Synchronous Tier 1 (< 1µs zero-latency): slash command catalog, skill registry catalog, and local session routing.
   - Asynchronous Tier 2 (SWR filesystem cache): workspace file and directory queries.
   - Completion items explicitly declare their post-accept continuation:
     - Directory and namespace candidates retain completion mode (`DrillDown`).
     - Terminal files and commands close completion popups (`Terminal`).

## Alternatives considered

- **Retaining bare `@src/xxx` alongside `@file:xxx`**:
  Rejected. Maintaining two competing grammars creates irreconcilable lexical ambiguity (e.g. distinguishing a skill named `test` from a file named `test`), perpetuates non-standard desugaring heuristics, and confuses users about which form guarantees context injection.
- **Requiring manual typing of `file:`**:
  Rejected. User typing ergonomics are preserved via "fuzzy-at-mention": typing bare `@main` in the composer matches files and skills simultaneously with clear visual badges (`[File]`, `[Skill]`), while selection automatically commits the canonical `@file:src/main.rs` representation.

## Consequences

### Positive
- Unified mental model: all context attachments follow `@{namespace}:{target}`.
- Zero spurious file loads or `os error 2` notes during normal conversation and code discussion.
- Instant, non-flickering completion across commands, skills, and files.

### Negative
- Existing tests that asserted bare path insertion upon `@` file completion must be updated to expect `@file:{path}`.

## References

- ADR-0162: Zero-Latency Two-Tier Completion and Flicker-Free Composer
- ADR-0165: Reactive Skills Architecture and Decoupled Security
- `crates/muta-agent/src/conversation_context/files.rs`
- `crates/muta-runtime/src/input_completion.rs`
- `apps/terminal/crates/mutx/src/completion.rs`
