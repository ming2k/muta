# Step state machine

Every collapsible transcript entry — a [tool step](tool-step.md), a
[thinking step](thinking-step.md), or an envoy task step — is presented
through one shared state model in `crates/neenee-tui/src/disclosure/`.
This page documents that model: its three orthogonal axes, the two
presentation channels they reduce to, and the transitions each axis allows.
Per-tool body content lives on the [tool step](tool-step.md) page; the shared
header shape lives on the [expandable step](expandable-step.md) page.

## Why three axes

Each step kind previously computed its summary-line color from a tangle of
ad-hoc flags (`expanded`, `focused`, `hovered`, `status`…) scattered across the
data, interaction, and render layers. That conflation caused bugs like "a
collapsed step stays highlighted because it still carries keyboard focus." The
fix is to model a step as **three orthogonal axes**, each with one reason to
change, and reduce the visible presentation to pure functions of them.

| Axis | Type | Owner | Persisted? | Drives |
|------|------|-------|------------|--------|
| **Lifecycle** | kind-specific (`ToolStatus`, or duration absent/present for reasoning) | model / harness | yes | accent or lifecycle text |
| **Disclosure** | `Disclosure` (Collapsed / Expanded) | model, user, or auto default | yes, with a `user_pinned` flag | weight + body visibility |
| **Interaction** | `Interaction` (Idle / Hovered / Focused) | pointer / keyboard hit-test | no — recomputed every frame | weight |

Lifecycle is **kind-specific** and therefore not unified here: tool steps carry
it through `ToolStatus` (5 states); reasoning traces derive streaming/finished
from whether a duration exists. Tool renderers resolve lifecycle to an
optional accent; reasoning carries no accent and expresses completion in its
summary text. The state module never asks “what kind of step is this?”.

## The two presentation channels

The summary line's color is the composition of two independent channels:

- **accent** (hue) — from Lifecycle. A non-`Ok` tool lifecycle stays visibly
  classified even when collapsed and idle. An `Ok` tool step and any reasoning
  trace yield `None`, handing control to the weight channel.
- **weight** (luminance) — from Disclosure × Interaction, via
  `summary_weight`. Decides how bright the summary reads (active vs. hover vs.
  muted), never which hue.

Keeping the channels separate is what makes behavior consistent across step
kinds. Keyboard focus is a separate concern from disclosure: on a collapsed
summary it uses the same transient lift as hover, while an expanded summary
stays pinned at full foreground weight.

## Disclosure FSM

Whether the step's body is shown. Two states, with a sticky `user_pinned`
flag on the message that gates automatic transitions:

```text
                ┌─────────────────────────────────────────────┐
                │  Auto default, re-evaluated on every         │
                │  lifecycle transition (start / finish /      │
                │  cancel). No-op once user_pinned == true.    │
                │  Writers: set_tool_step_expanded,            │
                │           set_thinking_expanded              │
                ▼                                             │
         ┌─────────────┐                                     │
         │  Collapsed  │                                     │
         │     (+)     │                                     │
         └─────────────┘                                     │
           │           ▲                                     │
   pin_*   │           │  pin_*_expanded(false)              │
 _expanded │           │  (sets user_pinned = true)          │
   (true)  │           │                                     │
           ▼           │                                     │
         ┌─────────────┐                                     │
         │  Expanded   │                                     │
         │     (-)     │                                     │
         └─────────────┘                                     │
                │                                             │
                 │  Body is painted; header may pin to the    │
                 │  top of the transcript area when scrolled  │
                 └─────────────────────────────────────────────┘
```

| State | Marker | Body | Summary weight (no accent) |
|-------|--------|------|-----------------------------|
| `Collapsed` | `▸` | hidden | `theme.muted()`, or `theme.hover()` under the pointer |
| `Expanded` | `▾` | visible | `theme.fg()` — expansion dominates every interaction |

### The `user_pinned` invariant

The single rule that prevents auto defaults from fighting the user:

| Writer | Used by | Effect |
|--------|---------|--------|
| `set_tool_step_expanded` / `set_thinking_expanded` | harness lifecycle transitions, step creation, scroll restore, selection-then-expand | no-op when `user_pinned == true` |
| `pin_tool_step_expanded` / `pin_thinking_expanded` | user toggle (click, `Enter`) | forces `expanded` and sets `user_pinned = true` |

Once the user has manually expanded or collapsed a step, later lifecycle
transitions leave it alone. There is no explicit "unpin"; a later manual
toggle just re-pins to the new value.

### Auto defaults

Default disclosure is a pure function of `(kind, lifecycle)`, evaluated by
`step_interaction::default_tool_expanded` (tools) and
`config::thinking_default_expanded` (reasoning):

| Step kind | Lifecycle | Default disclosure | Reason |
|-----------|-----------|--------------------|--------|
| Tool | `Running` | Collapsed | no result yet; live-streaming tools still accumulate output the user can expand manually |
| Tool | `Failed` | Expanded | the error is the whole point |
| Tool | `Denied` | Expanded | the denial message must be visible without an extra click |
| Tool | `Cancelled` | Collapsed | an aborted call reads as inert |
| Tool | `Ok` | per-tool `[tui.default_expanded]` entry, or `true` under Comfortable density | `edit_file` shows its diff; `bash` / `read_file` stay collapsed |
| Thinking | streaming | Collapsed (or `[tui.default_expanded] thinking`) | reasoning defaults to collapsed; opt in to auto-expand via config |
| Thinking | finished | Unchanged | no auto-collapse — do not yank away content the user was reading |

## Interaction FSM

Transient pointer/keyboard state for the summary line, recomputed every frame
from the layout-map hit-test. Never persisted.

```text
         pointer leaves summary
   ┌───────────┐ ◄────────────────── ┌───────────┐
   │   Idle    │                     │  Hovered  │
   └───────────┘ ──────────────────► └───────────┘
                  pointer enters summary
```

`Interaction::Focused` shares the hover rung. A collapsed focused or hovered
summary resolves to `theme.hover()`; a collapsed idle summary resolves to
`theme.muted()`. Expanded summaries resolve to `theme.fg()` regardless of
interaction, so pointing at or focusing an open body never makes it recede.

## Lifecycle accent

The accent color a renderer passes to `summary_text_color`, by source:

| Step kind | Lifecycle | Accent | Source |
|-----------|-----------|--------|--------|
| Tool | `Running` | `Some(theme.muted)` — neutral pending state | `draw_tool_step` |
| Tool | `Failed` | `Some(theme.err)` | `draw_tool_step`, `draw_envoy_bar` |
| Tool | `Denied` | `Some(theme.warn)` — distinct from a runtime failure | `draw_tool_step`, `draw_envoy_bar` |
| Tool | `Cancelled` | `Some(theme.dim)` — reads as inert, not as a fresh failure | `draw_tool_step`, `draw_envoy_bar` |
| Tool | `Ok` | `None` — hands control to the weight channel | `draw_tool_step`, `draw_envoy_bar` |
| Reasoning | streaming / finished | `None` — lifecycle reads from the summary text (duration omitted while streaming); the marker is always `▸`/`▾` | `draw_reasoning_trace` |

A `Some(accent)` supplies the dominant hue, then blends toward the
disclosure/interaction weight. Collapsed idle leaves the accent unchanged;
collapsed hover/focus blends 35% toward `theme.hover()`; expanded blends 60%
toward `theme.fg()`. `None` falls through to `summary_weight`.

## Color resolution table

The full `summary_text_color(accent, disclosure, interaction)` resolution:

| Disclosure | Interaction | `accent` | Summary color |
|------------|-------------|----------|---------------|
| Collapsed | Idle | `Some(c)` | `c` |
| Collapsed | Hovered or Focused | `Some(c)` | `c` blended 35% toward `theme.hover()` |
| Expanded | any | `Some(c)` | `c` blended 60% toward `theme.fg()` |
| Expanded | any | `None` | `theme.fg()` |
| Collapsed | Hovered or Focused | `None` | `theme.hover()` |
| Collapsed | Idle | `None` | `theme.muted()` |

## Invariants worth keeping

These are the load-bearing contracts. Breaking any of them tends to regress
one of the historical bugs the state machine was introduced to fix:

- **One reason to change per axis.** Lifecycle changes do not write
  `user_pinned`; user toggles do not mutate Lifecycle. The only thing that
  crosses the seam is the auto-default re-evaluation, and it goes through the
  pinned-gated setter.
- **Accent owns hue; weight modulates it.** Lifecycle classification remains
  dominant while hover/focus and disclosure still have visible affordances.
- **Focus equals hover.** Both lift a collapsed summary to the same rung and
  introduce no fourth luminance state.
- **Expansion dominates interaction.** An open step is always the primary
  foreground; the pointer state cannot dim it.
- **Reasoning never carries a text accent.** Its lifecycle is expressed in
  summary text, so the weight ladder stays meaningful.

## Source

| File | Responsibility |
|------|----------------|
| `disclosure/state.rs` | `Disclosure`, `Interaction`, `summary_weight`, `summary_text_color`. Pure functions, unit-tested in isolation from rendering |
| `disclosure/mod.rs` | The three-axes architectural overview and the public re-exports |
| `disclosure/renderers.rs` | Concrete step renderers that feed the axes in: `draw_tool_step`, `draw_reasoning_trace`, `draw_envoy_bar`, `draw_envoy_inline_step` |
| `tools/mod.rs` | `ToolStatus` (5 states), `ToolStatus::color` |
| `step_interaction.rs` | `default_tool_expanded`, summary-at-pointer classification (`summary_at`, `hovered_summary`) |
| `document.rs` | `set_*_expanded` (auto, no-op if pinned) and `pin_*_expanded` (user, sets `user_pinned`); the `user_pinned` field on `MessageKind::ToolStep` / `MessageKind::Thinking` |
| `app.rs` | `toggle_step_pinned` — wires the user toggle to the pin setter and the sticky-scroll keep-anchored behavior |
