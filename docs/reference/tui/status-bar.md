# Head band

Strip at the top of every view — the first rows. It replaces the
old bottom status bar: ambient **session** state now lives at the top of the
view, not at the bottom of the footer.

Row 1 carries identity and status for the current page. A second row — the
view-level affordance legend with the keycap shortcuts that apply to *this*
view (ADR-0103 §3) — is **demand-driven** (ADR-0104): it renders only while
the view has page-specific affordances that no other surface already
carries, so the common cases show a single-row band and the transcript
reclaims the line. Navigation shortcuts live on row 2 (or the Envoy
footer), never on row 1.

Every view shares this chrome slot:

- **Session (Main):** `SESSION` identity, the persistent session-id tail
  (last 4 chars, dimmed), and the tilde-shortened workspace path on the
  left; the session mode (`autopilot`) on the right. Row 2 appears only
  while asides are live: the aside chip (`btw 2 · 1 running`) and
  `F5 asides`. No interrupt pair — the activity bar's `Esc Esc interrupt`
  hint (which spells the real double-Esc arming) is the authoritative copy.
- **`/btw`:** `/btw` identity, "Side conversation", parent status. Row 2
  always shows `Ctrl-C back`, `F5 asides`, and `Esc interrupt aside`
  (while the aside's round runs) — the exit pair exists on no other
  surface.
- **Envoy:** `Envoy` identity, the task label, `N of M` position. No row 2
  at all — the page's own permanent footer already carries the legend.
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
| Location | First rows of the view |
| Height | 1 row always; row 2 (`PAGE_HEADER_ROWS = 2` ceiling) only while the view has page-specific affordances — main view: while asides are live; `/btw`: always; Envoy: never (ADR-0104) |
| Band width | Full terminal width — the `body` background owns every cell of the row (the head is top-level chrome, the counterpart of the Envoy key-legend band at the bottom edge), with no `app_bg` gap at either edge |
| Text inset | `TRANSCRIPT_H_INSET = 2` cols on each side, rendered as pad spans inside `draw_page_header`, so the text stays aligned with the transcript band below |
| `SESSION` title | BOLD, `text_primary` |
| Session-id tail | Dimmed (`text_dim`), last 4 chars of the persistent id |
| Workspace | `text_brand`, tilde-shortened |
| `autopilot` flag | Warning tone + BOLD, right-aligned (before the trailing pad), only while autopilot is on |
| Background | `body` |

## Visibility

| Condition | Visible? |
|-----------|----------|
| Always (every view) | Yes |
| Envoy zoom view | Yes (Envoy contextual header replaces Session head) |
| `/btw` aside view | Yes (`/btw` contextual header replaces Session head) |

## Source

`draw_page_header` / `PageHeader::Session` / `SessionHead` in
`tui/page_header.rs`. The workspace path is tilde-shortened by `tilde_home`
(`tui/chrome.rs`) from `App::cwd`. The `autopilot` flag arrives through
`App::autopilot`.
