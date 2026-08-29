# Command entry model

A command renders as an **Entry** that owns the command's
input and output: a clean header line starting with the `⌘` glyph,
typed invocation, timestamp, a 1-row gap, and — when completed — the result content rendered
directly as the entry's body ([ADR-0111](../../adr/0111-transcript-entry-unification-and-concurrent-rendering.md),
revising [ADR-0109](../../adr/0109-command-card-and-triangle-disclosure.md),
[ADR-0108](../../adr/0108-one-command-component-input-output-lifecycle.md),
and [ADR-0106](../../adr/0106-command-row-interaction-and-projection.md)).

Commands are first-class inputs with the exact same granularity and projection
lifecycle as conversational turns. In accordance with ADR-0111, collapsible
folding (`▸`/`▾`) is overturned in favor of flat, direct body rendering with
standard header-body spacing.

## Shapes

One span grammar for the Entry header, followed by a 1-row gap, the concrete invocation, and the unfolded body:

```text
⌘ command · 21:39                     pending header
                                      ← 1-row blank gap
  /delegate on                        concrete command invocation

⌘ command · 21:39                     completed header
                                      ← 1-row blank gap
  /permissions                        concrete command invocation
                                      ← 1-row blank gap
  Always-allowed tools: …             …result body through the shared block renderer
```

| Attribute | Value |
|-----------|-------|
| Header Tag | `⌘ command`, BOLD — info tone |
| Trailing meta | ` · HH:MM` muted, when `sent_at_ms` is present |
| Gap | 1 blank row (`TURN_HEADER_BODY_GAP_ROWS = 1`) between header and body |
| Invocation | Concrete command text (`/name args`), BOLD, indented by `TRANSCRIPT_BODY_LEADING_INDENT` so it lines up with the body it introduces |
| Body | Directly rendered beneath the gap (ADR-0111) in the muted `Role::Tool` prose tone — one step quieter than the bold invocation, so input and output are distinguishable by weight/color at a glance; collapsible folding is eliminated |

## Lifecycle

| Phase | Render |
|-------|--------|
| `Pending` | `⌘ command · HH:MM` header, followed by 1-row gap and invocation in muted running tone |
| `Completed` | `⌘ command · HH:MM` header, 1-row gap, invocation in active tone, and result body blocks |
| `Cancelled` | Settled with no reply (e.g. a modal command); reads like a plain entry |

## Direct body rendering

The command result body renders directly through the shared block renderer
(`draw_message_body`), so lists, code blocks, tables, and prose render
identically to standard messages without requiring manual disclosure clicks.

## Concurrent rendering

Different entry types (e.g. an active streaming Turn Entry A and a Command
Entry B) render concurrently. As Entry A streams and expands in height, Entry B's
vertical position naturally shifts downward according to Entry A's actual height.

## Source

`draw_command_result` / `command_summary_line` in
`apps/tui/crates/mutx/src/disclosure/renderers.rs`; band tokens in
`apps/tui/crates/mutx/src/theme.rs` (`command_surface`,
`command_surface_hover`).
