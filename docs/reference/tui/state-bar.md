# State bar

Persistent session-state indicators on one dedicated row directly below the
[input box](input-box.md) and above the [hint bar](hint-line.md). Neither the
[activity bar](status-bar.md) above the input nor the hint bar below it
carries long-lived session state: this row is its designated home, so both
bars stay uncluttered and there is room for more indicators later (current
workspace and other ambient state).

Sitting just under the input box makes `unattended` read as an attribute of
the composer area — the place the user is acting — rather than as a transient
status line above the prompt.

The row is conditional: it occupies zero rows when no indicator is
active, so an ordinary session pays no vertical space for it.

## Appearance

Unattended mode active:

```text
 unattended
```

Flags are left-aligned with a one-space indent and joined by ` · ` when
more than one is on.

| Attribute | Value |
|-----------|-------|
| Location | 1 row directly below the input box, above the hint bar |
| Height | `STATE_BAR_ROWS = 1` while any flag is active, 0 otherwise |
| `unattended` flag | lowercase, warning tone + BOLD, only while unattended mode is on |
| Flag separator | ` · ` in `text_muted` |
| Indent | 1 space |

## Unattended mode

When unattended mode is active (`--unattended` / `/unattended on`), the
agent runs without human intervention — no confirmations, no questions.
The state bar shows a lowercase `unattended` flag in the warning tone, bold.
Plain text rather than a bracketed pill: it reads as a persistent state flag
(always-on while the session is elevated) rather than a momentary input mode,
so it carries its meaning without any chrome.

## Visibility

| Condition | Visible? |
|-----------|----------|
| No indicator active | No — the row collapses to 0 height |
| Unattended mode on | Yes |
| Overlay modal open | No (chrome hidden) |
| Envoy zoom view | No (footer hidden) |

## Source

`draw_state_bar` in `render/chrome.rs`. The row's height and placement
are resolved in `draw_transcript` (`view.rs`) from
`STATE_BAR_ROWS` (`render/design.rs`); the `unattended` flag arrives
through `TranscriptView::unattended`.
