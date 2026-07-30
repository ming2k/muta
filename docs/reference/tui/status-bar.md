# Status bar

Single-row strip pinned at the bottom of the footer, directly below the
[hint bar](hint-line.md). It is the dedicated home for ambient **session**
state — state that describes the whole session rather than the current input.

- **Left:** persistent session status flags, currently just `autopilot`. The
  safety flag leads the row because a silent agent running is the most
  glance-worthy session state.
- **Right:** the workspace path the session is rooted at, tilde-shortened to its
  `~/...` form (e.g. `~/projects/xx`).

Neither the [activity bar](activity-bar.md) above the input nor the hint bar
below the input carries this kind of long-lived state: this row is its
designated home, so both of those bars stay uncluttered and focused on their
own concerns (transient liveness, and the next input action respectively).

Unlike the older conditional state row this bar used to be, it is now
**always present** whenever the footer chrome is visible — the workspace path
is always glanceable, which is its reason to exist. The left cluster is built
only from whatever flags are active.

## Appearance

Autopilot mode active:

```text
 autopilot                                                    ~/projects/xx
```

Ordinary session (no flag):

```text
                                                              ~/projects/xx
```

On narrow terminals the workspace path is truncated from the left, keeping a
`…`-style prefix plus its tail (the project directory is the meaningful part,
pinned to the right edge), and the left flag cluster drops before the workspace
disappears. The row never overflows.

| Attribute | Value |
|-----------|-------|
| Location | 1 row directly below the hint bar (bottom of the footer stack) |
| Height | `STATUS_BAR_ROWS = 1` whenever chrome is visible (never conditionally hidden) |
| Workspace | `text_muted`, tilde-shortened, truncated from the left (`…tail`) when it would collide with the left cluster |
| `autopilot` flag | lowercase, warning tone + BOLD, left-aligned (leads the row), only while autopilot mode is on |
| Indent | 1 space |
| Background | `surface` |

## Autopilot mode

When autopilot mode is active (`--autopilot` / `/autopilot on`), the
agent runs without human intervention — no confirmations, no questions.
The status bar shows a lowercase `autopilot` flag in the warning tone, bold,
leading on the left. Plain text rather than a bracketed pill: it reads as a
persistent session flag (always-on while the session is elevated) rather than
a momentary input mode, so it carries its meaning without any chrome.

## Visibility

| Condition | Visible? |
|-----------|----------|
| Chrome visible (no overlay modal) | Yes (always) |
| Overlay modal open | No (chrome hidden) |
| Envoy zoom view | No (footer hidden) |
| Permission sheet open | No — the sheet takes over the input-box, hint, and status rows |

## Source

`draw_status_bar` / `StatusBarView` in `render/chrome.rs`. The workspace path
is tilde-shortened by `tilde_home` (same module) from `App::cwd`, captured at
startup. The row's height and placement are resolved in `draw_transcript`
(`view.rs`) from `STATUS_BAR_ROWS` (`render/design.rs`); the `autopilot` flag
arrives through `App::autopilot`.
