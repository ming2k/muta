# Head row

Single-row strip at the top of every view — the first row. It replaces the
old bottom status bar: ambient **session** state now lives at the top of the
view, not at the bottom of the footer.

Every view shares this chrome slot:

- **Session (Main):** `SESSION` identity, the persistent session-id tail
  (last 4 chars, dimmed), and the tilde-shortened workspace path on the
  left; the session mode (`autopilot`) on the right.
- **`/btw`:** `/btw` identity, "Side conversation", parent status, and
  `Esc back` on the right.
- **Envoy:** `Envoy` identity, the task label, `N of M` position, and
  navigation shortcuts on the right.
- **Dashboard:** `DASHBOARD` identity, "all projects", and a live
  session-count summary on the right.

## Session view appearance

Autopilot mode active:

```text
 SESSION b3c4 ~/projects/xx                                     autopilot
```

Ordinary session (no mode flag):

```text
 SESSION b3c4 ~/projects/xx
```

On narrow terminals the workspace path truncates from the left (keeping its
meaningful tail), and the mode flag drops before the workspace disappears.
The row never overflows.

| Attribute | Value |
|-----------|-------|
| Location | First row of the view |
| Height | `PAGE_HEADER_ROWS = 1` |
| `SESSION` title | BOLD, `text_primary` |
| Session-id tail | Dimmed (`text_dim`), last 4 chars of the persistent id |
| Workspace | `text_brand`, tilde-shortened |
| `autopilot` flag | Warning tone + BOLD, right-aligned, only while autopilot is on |
| Background | `body` |

## Visibility

| Condition | Visible? |
|-----------|----------|
| Always (every view) | Yes |
| Envoy zoom view | Yes (Envoy contextual header replaces Session head) |
| `/btw` side view | Yes (`/btw` contextual header replaces Session head) |

## Source

`draw_page_header` / `PageHeader::Session` / `SessionHead` in
`tui/page_header.rs`. The workspace path is tilde-shortened by `tilde_home`
(`tui/chrome.rs`) from `App::cwd`. The `autopilot` flag arrives through
`App::autopilot`.
