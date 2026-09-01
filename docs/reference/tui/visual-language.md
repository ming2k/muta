# Visual language: the join ladder

> **One rule governs every adjacent pair of tokens on a row:** the tighter the
> semantic relationship, the quieter the join. `·` is **not** a universal
> separator — it means exactly one thing, and everything looser than that is
> conveyed by whitespace.

## The problem

The middle dot was drifting into a catch-all: it joined same-rank peers
(`/skills · /repeat · /help`), container→member pairs at different levels
(`round 3 · turn 2`), and attribute→value pairs (`Thinking · 120 chars`) with
the same glyph. When every relationship renders identically, the dot carries
no information — and worse, it actively *misleads*: two peers look like one
modifies the other, and a round looks like a sibling of its own turn.

## The principle

Perception groups by proximity. So **relationship strength is encoded as
spacing**, and a glyph is used only where spacing alone is not enough — the
single attribute→value case. A row's joins should look like this:

```
Thinking · 120 chars · 0.5s        ↔↔   R1 modifier joins only
turn 2  sonnet                     ↔↔   R2 peers: whitespace, no dot
TODOS 1/8 · write the docs   Ctrl+T expand   ↔↔   R3 segment: wide whitespace
```

## The ladder

| Rung | Name | Relationship | Visual | Budget |
|------|------|--------------|--------|--------|
| **R0** | Atomic | Parts of one value | No symbol — 0–1 space | `24.1 KB`, `3 tool calls`, `round 1`, `Ctrl+P block` |
| **R1** | Modify | Trailing token is a state / measure / attribute of the leading one | ` · ` (space, middle dot, space) | `Thinking · 120 chars`, `↳ Completed · 3 calls · 1.2s`, `[Image #1 · 1.5 KB]`, `· blocked` |
| **R2** | Enumerate | Same-rank peers | Plain whitespace, no glyph | `turn 2  sonnet`, `/skills  /repeat  /help`, `Ctrl+P block  Ctrl+Q expand` |
| **R3** | Segment | Cross-group boundary (content vs keycap legend, panel identity vs preview) | Plain whitespace, wide | `BAR_LEGEND_GAP_MIN` (6 cols) |
| **↑** | Hierarchy | Container → member, tree parent → child | ` › ` inline breadcrumb; `↳` + indent for tree nesting | `round 3 › turn 2`, `Connections › keybindings` |

### R0 — Atomic (同一值)

The pieces of a single logical value touch with no symbol and at most one
space: a unit hugs its number (`1.5 KB`), a count follows its label
(`3 tool calls`), a keycap precedes its action (`Ctrl+P block`, `Ctrl+T expand`),
a counter sits one space off its label (`round 1`, `turn 2`).

### R1 — Modify (修饰) — the only sanctioned dot

`·` means *"the following is a state, measure, or attribute of the preceding"*.
The two tokens are the same rank and directly coupled.

- `Thinking · 120 chars · 0.5s` — chars and duration are properties of the thinking step.
- `↳ Running · 3 tool calls · Grep "foo"` — status and count are properties of the step (the `↳` already encodes the nesting).
- `[Image #1 · 24.1 KB]` — the size is a property of the staged image.
- `QUEUE 2 · blocked` — `blocked` is a state of the queue.
- `round 5 · 14:02` — the time is a property of the round.
- `◆ turn 2 · sonnet · 14:02` — model and time are properties of the turn anchor.
- `> turn 3 · glm-5.3 · high · 17:55` — model, reasoning effort, and time are
  all properties of the turn anchor (the effort appears only when the channel
  actually ran the turn with one).

Anything that is *not* "attribute of the preceding same-rank token" must **not**
use `·`.

### R2 — Enumerate (并列)

Same-rank peers — two things the row treats equally — are separated by
**two columns of plain whitespace** (`JOIN_ENUMERATE_COLS`). No glyph. The
equal gaps are what make the group read as a list.

- `turn 2  sonnet` (turn header: anchor  model)
- `Ctrl-M  /models` (carousel keycap + command peers)
- `Ctrl+P block  Ctrl+Q expand` (queue-bar legend: two independent affordances)
- `type filter  ↑↓ navigate  Enter activate` (modal footer hints)
- `v1.2.3  local  #rust #tui` (skill metadata: version, source, tags)

### R3 — Segment (断隔)

A wide whitespace budget (`BAR_LEGEND_GAP_MIN`, 6 cols) marks a **boundary
between groups** — most often the content of a bar and its right-pinned
keycap legend, so a truncated preview's `…` never butts against a key.

```
TODOS 1/8 · write the docs   Ctrl+T expand
QUEUE 2  fix the flaky test   Ctrl+P block  Ctrl+Q expand
```

### Hierarchy — never `·`

A different *level* is never joined with `·`, which would falsely imply the
two are siblings. Inline containment uses the ` › ` breadcrumb
(`JOIN_BREADCRUMB`): `round 3 › turn 2`, `Connections › keybindings`. Tree
nesting keeps its existing `↳` + indentation.

## Choosing a join (rules of thumb)

Ask, in order:

1. **Are these parts of one value?** → R0, no symbol.
2. **Does the trailing token modify the leading one?** (state, measure,
   attribute, unit) → R1, ` · `.
3. **Are they same-rank peers?** → R2, two spaces.
4. **Do they belong to different groups?** → R3, six spaces (or a modal row).
5. **Is one the container of the other?** → ` › ` breadcrumb (inline) or
   `↳` + indent (tree).

If in doubt between `·` and whitespace, **use whitespace**. The dot is the
loudest join in the language and must earn its place.

## Code vocabulary

The ladder is a single source of truth in `apps/tui/crates/mutx/src/design.rs`:

| Constant | Rung | Value |
|----------|------|-------|
| `JOIN_MODIFY` | R1 | `" · "` |
| `JOIN_ENUMERATE_COLS` | R2 | `2` |
| `JOIN_BREADCRUMB` | hierarchy | `" › "` |
| `BAR_LEGEND_GAP_MIN` | R3 | `6` |

New renderers must reference these constants instead of inlining literals, so
a later tweak to the ladder propagates everywhere.

## Anti-patterns

- ❌ `A · B` for same-rank peers — reads as "B modifies A".
- ❌ `round 3 · turn 2` — different levels joined like siblings.
- ❌ `·` inside a keycap unit — a key and its action are one value (`Ctrl+P block`,
  never `Ctrl+P · block`).
- ❌ `·` as a generic list bullet in prose and comments — it is a render token,
  not a punctuation mark (the UI previews follow the same rule).

## Conformance

Conforming surfaces (R1 modifiers, kept):

- Transcript metadata headers — `MetaStrip` (`round N · HH:MM`,
  `◆ turn N · sonnet · 14:02`), the component's default separator is
  `JOIN_MODIFY`.
- Tool-step summaries — `Thinking · 120 chars · 0.5s`,
  `↳ Completed · 3 calls · 1.2s`.
- Todo bar identity — `TODOS 1/8 · write the docs`.
- Activity bar interrupt hint — `(23s · Esc Esc interrupt)`.
- Attachment chips — `[Image #1 · 24.1 KB]`.
- Page header — `Side conversation · main needs approval`.
- History modal title and state tags. The Queue modal title stays plain
  (`Queue`) — count and blocked state already live in the queue bar.

Migrated to whitespace / breadcrumb:

- Queue-bar legend: `Ctrl+P block  Ctrl+Q expand` (R2).
- Modal footer hints: `type filter  ↑↓ navigate  Enter activate` (R2).
- Empty-state suggestions: `/skills  /repeat  /help` (R2).
- Help copy: `copy  clear input  quit (×2)` (R2).
- Skill metadata: `v1.2.3  local  #rust #tui` (R2).
- Activity-modal detail: `round 3 › turn 2 · sonnet · 12s` (breadcrumb + R1).
- Modal keybindings page: `Connections › keybindings` (breadcrumb).

## Source

`design.rs` (constants), `chrome.rs` (todo/queue/activity bars), `footer.rs`
(modal hint joins), `meta_strip.rs` (header chips), `overlays/activity.rs`
(round › turn breadcrumb). The ladder supersedes the ad-hoc ` · ` guidance in
`components.md` and ADR-0049's "anchor · detail · time" phrasing, which is now
one sanctioned instance of R1.
