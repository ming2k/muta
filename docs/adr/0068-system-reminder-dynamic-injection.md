# ADR-0068: System-reminder dynamic injection layer

- Status: Accepted
- Date: 2026-07-17

## Context

A stable head system prompt (ADR-0056) cannot carry *event-driven,
situation-specific* instructions: "you entered read-only mode", "budget at 80%,
converge", "the tool you keep calling is disabled". Those belong in a separate,
append-only channel that lands mid-conversation as hidden user messages,
distinct from the rebuilt head.

The sibling `kimi-code` project solves this with a two-tier XML trust model:
`<system-reminder>` blocks are **authoritative** directives the model must
follow (may override normal behaviour), while `<untrusted_…>` blocks wrap
**data** (pasted content, an objective string) that must never override system
messages, tool schemas, or permission rules. Foreign text is XML-escaped so it
cannot smuggle tags.

neenee had no equivalent: every mid-turn injection was an ad-hoc hidden user
message with bespoke framing, and the pursuit prompt composed its own
`<objective>` tag inline with its own escaping.

## Decision

Add a `conversation_context::system_reminder` module that owns the reminder
channel with the two-tier trust model.

1. **`authoritative(body)`** wraps content in `<system-reminder>` and stamps
   `InjectionKind::SystemReminder`. The prelude teaches the model these are
   harness directives it MUST follow and that may override normal behaviour.

2. **`untrusted(body)` / `untrusted_with_tag(tag, body)`** XML-escape the content
   and wrap it in `<untrusted_data>` (or a caller-named, identifier-safe tag),
   stamping `InjectionKind::UntrustedDirective`. The prelude labels it as data,
   not instructions.

3. **`ReminderSink` + `inject`** — a small accumulator (`remind` / `data` /
   `data_as`) flushed into the message window in one pass, mirroring the
   `inject_mentioned_skills` pattern.

4. **Two new `InjectionKind` variants** (`SystemReminder`, `UntrustedDirective`)
   so a persisted transcript discriminates authoritative directives from
   untrusted data without string-sniffing, and round-trip distinctly (added to
   the exhaustiveness test).

The tag is restricted to `[A-Za-z0-9_]` so a caller can never smuggle a closing
tag or attribute; anything unsafe falls back to the default. Empty bodies
collapse to a no-op (no instruction, no bloat).

## Consequences

- One canonical path for every mid-turn reminder, with consistent escaping and
  trust labelling — replacing ad-hoc hidden-user constructions.
- The pursuit convergence nudge (ADR-0069) rides the authoritative channel so
  "converge now" is a directive, not a suggestion.
- The untrusted API is a forward-looking primitive reserved for the
  pursuit-objective / pasted-content path (the current objective prompt still
  composes its own tag); it is `#[allow(dead_code)]` until that migration.
- New `InjectionKind` variants require a serialization-distinctness test entry,
  preserving the exhaustiveness-as-traceability design lever.
