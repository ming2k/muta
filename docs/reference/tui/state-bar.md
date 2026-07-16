# State bar

Persistent session-state indicators on one dedicated row between the
[activity bar](status-bar.md) and the [input box](input-box.md). Neither
the activity bar above nor the [hint bar](hint-line.md) below carries
long-lived session state: this row is its designated home, so both bars
stay uncluttered and there is room for more indicators later (current
workspace and other ambient state).

The row is conditional: it occupies zero rows when no indicator is
active, so an ordinary session pays no vertical space for it.

## Appearance

Unattended mode active:

```text
 UNATTENDED
```

Flags are left-aligned with a one-space indent and joined by ` · ` when
more than one is on.

| Attribute | Value |
|-----------|-------|
| Location | 1 row between the activity bar and the input box |
| Height | `STATE_BAR_ROWS = 1` while any flag is active, 0 otherwise |
| `UNATTENDED` flag | warning tone + BOLD, only while unattended mode is on |
| Flag separator | ` · ` in `text_muted` |
| Indent | 1 space |

## Unattended mode

When unattended mode is active (`--unattended` / `/unattended on`), the
agent runs without human intervention — no confirmations, no questions.
The state bar shows a flat `UNATTENDED` flag in the warning tone, bold and
uppercase: the one flag that bypasses human oversight gets the strongest
treatment on the row. Plain text rather than a bracketed pill: it reads
as a persistent state flag (always-on while the session is elevated)
rather than a momentary input mode, so it carries its meaning without any
chrome.

## Visibility

| Condition | Visible? |
|-----------|----------|
| No indicator active | No — the row collapses to 0 height |
| Unattended mode on | Yes |
| Overlay modal open | No (chrome hidden) |
| Envoy zoom view | No (footer hidden) |

## Source

`draw_state_bar` in `render/chrome.rs`. The row's height and placement
are resolved in `draw_transcript` (`render/mod.rs`) from
`STATE_BAR_ROWS` (`render/design.rs`); the `unattended` flag arrives
through `TranscriptView::unattended`.
