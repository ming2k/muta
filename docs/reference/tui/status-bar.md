# Head band

Strip at the top of every view — the first rows. It replaces the
old bottom status bar: ambient **session** state now lives at the top of the
view, not at the bottom of the footer.

Row 1 carries identity and status for the current page. A second row — the
view-level affordance legend and view stack breadcrumbs — is **demand-driven** (ADR-0104):
it renders only while the view has page-specific affordances or stack depth that no
other surface already carries, so the common single-view case shows a strict single-row
band and the transcript reclaims the line. Navigation shortcuts and stack breadcrumbs
live on row 2 (or the Envoy footer), never on row 1. Two-stroke leader chords (`Ctrl+X`,
`Ctrl+C`) render as a decoupled floating Which-Key overlay card at the application
shell level, without shifting the header or transcript.

Every view shares this chrome slot:

- **Session (Main):** `SESSION` identity, the persistent session-id tail
  (last 4 chars, dimmed), and the tilde-shortened workspace path on the
  left; the delegated-autonomous flag (`DELEGATED`) on the right. Row 2
  appears when view stack depth > 1 (rendering the breadcrumb trail `Main › Runner[...]`
  plus `[C-x b: view  C-g: back]`), or while asides are live (the aside chip
  `btw 2 · 1 running` and `F5 asides`). No interrupt pair — the activity bar's
  `Esc Esc interrupt` hint is the authoritative copy.
- **`/btw`:** `/btw` identity, "Side conversation", parent status. Row 2
  shows the view breadcrumbs (`Main › Aside`), `Ctrl-C back`, `F5 asides`, and
  `Esc interrupt aside` (while the aside's round runs).
- **Envoy:** `Envoy` identity, the task label, `N of M` position. Row 2 is omitted
  when single-depth (its permanent footer already carries the legend); when nested,
  it displays the runner breadcrumb hierarchy.
- **Dashboard:** `DASHBOARD` identity, "all projects", and a live
  session-count summary on the right.

## Session view appearance

Delegated autonomous mode active:

```text
 SESSION b3c4 ~/projects/xx                                    DELEGATED
```

Ordinary session (single view, row 2 collapsed):

```text
 SESSION b3c4 ~/projects/xx
```

Nested view stack (row 2 expanded with breadcrumbs):

```text
 SESSION b3c4 ~/projects/xx                                    DELEGATED
   Main › Runner[explore: repo scan]                C-x b view  C-g back
```

On narrow terminals the workspace path truncates from the left (keeping its
meaningful tail), and the mode flag drops before the workspace disappears.
The row never overflows.

| Attribute | Value |
|-----------|-------|
| Location | First rows of the view (flush top, `y = 0`) |
| Height | 1 row always when depth == 1; row 2 (`PAGE_HEADER_ROWS = 2` ceiling) only while view stack depth > 1 or view has page-specific affordances (main view: with breadcrumbs or live asides; `/btw`: always; Envoy: demand-driven) |
| Band width | Full terminal width — the `body` background owns every cell of the row, with no `app_bg` gap at either edge |
| Text inset | `TRANSCRIPT_H_INSET = 2` cols on each side, rendered as pad spans inside `draw_page_header`, so the text stays aligned with the transcript band below |
| `SESSION` title | BOLD, `text_primary` |
| Session-id tail | Dimmed (`text_dim`), last 4 chars of the persistent id |
| Workspace | `text_brand`, tilde-shortened |
| `DELEGATED` flag | Warning tone + BOLD, right-aligned (before the trailing pad), only while delegated autonomous mode is on |
| Breadcrumbs | `text_primary` for breadcrumb trail, `text_brand` + bold for keycaps (`C-x b`, `C-g`), `text_muted` for labels |
| Background | `body` |

## Source

`draw_page_header` / `draw_page_header_hints` / `PageHeader::Session` / `SessionHead` in
`page_header.rs`. The workspace path is tilde-shortened by `tilde_home`
(`chrome.rs`) from `App::cwd`. The `DELEGATED` flag arrives through
`App::delegated` (the harness snapshot's delegated-autonomous bit).
Floating leader chords are rendered by `components::which_key::draw_which_key_overlay`.
