# Head band

Strip at the top of every view — the first rows. It replaces the
old bottom status bar: ambient **session** state now lives at the top of the
view, not at the bottom of the footer.

Row 1 carries identity and status for the current page. A second row — the
view-level affordance legend and view stack breadcrumbs — is **demand-driven** (ADR-0104):
it renders only while the view has page-specific affordances or stack depth that no
other surface already carries, so the common single-view case shows a strict single-row
band and the transcript reclaims the line. Navigation shortcuts and stack breadcrumbs
live on row 2 (or the Subagent footer), never on row 1. The `Ctrl-X` scene
namespace renders as a decoupled floating Which-Key overlay card at the
application shell level, without shifting the header or transcript. **Esc never
closes a Scene** (ADR-0205 `[INV-TUI-CLEAN-02]`): it dismisses overlays, unwinds
scene-local sub-layers, clears focus, and (armed twice) interrupts a round.

Every view shares this chrome slot:

- **Session (Main):** `SESSION` identity, the persistent session-id tail
  (last 4 chars, dimmed), and the tilde-shortened workspace path on the
  left; the delegated-autonomous flag (`DELEGATED`) on the right. Row 2
  appears while asides are live (the chip `btw: 2 total (1 active)` plus
  `F5 asides`); with no live asides the band collapses to the identity row and
  the transcript reclaims the line. No interrupt pair — the activity bar's
  `Esc Esc interrupt` hint is the authoritative copy.
- **`/btw`:** `/btw` identity, "Side conversation", the parent's status. Row 2
  shows the view breadcrumb `Main › Aside` and the `Ctrl-x scene` namespace.
- **Subagent:** `Subagent` identity, the task label, `N of M` position. Row 2
  shows the breadcrumb `Main › Subagent[role]` and the `Ctrl-x scene` namespace;
  the rest of the scene's keyboard surface (the sibling walk) is discoverable
  through the Command Palette rather than advertised here.
- **Dashboard:** `DASHBOARD` identity, "all projects", and a live fleet
  summary on the right (escalated to the warning tone while any session needs
  attention). Row 2 carries the `Ctrl-x scene` namespace, exactly like
  Settings.

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
   Main › Subagent[explore: repo scan]                         Ctrl-x scene
```

On narrow terminals the workspace path truncates from the left (keeping its
meaningful tail), and the mode flag drops before the workspace disappears.
The row never overflows.

| Attribute | Value |
|-----------|-------|
| Location | First rows of the view (flush top, `y = 0`) |
| Height | 1 row always when depth == 1; row 2 (`PAGE_HEADER_ROWS = 2` ceiling) only while view stack depth > 1 or view has page-specific affordances (main view: with breadcrumbs or live asides; `/btw`: always; Subagent: demand-driven) |
| Band width | Full terminal width — the `body` background owns every cell of the row, with no `app_bg` gap at either edge |
| Text inset | `TRANSCRIPT_H_INSET = 2` cols on each side, rendered as pad spans inside `draw_page_header`, so the text stays aligned with the transcript band below |
| `SESSION` title | BOLD, `text_primary` |
| Session-id tail | Dimmed (`text_dim`), last 4 chars of the persistent id |
| Workspace | `text_brand`, tilde-shortened |
| `DELEGATED` flag | Warning tone + BOLD, right-aligned (before the trailing pad), only while delegated autonomous mode is on |
| Breadcrumbs | `text_primary` for breadcrumb trail, `text_brand` + bold for keycaps (`Ctrl-x`), `text_muted` for labels (`scene`) |
| Background | `body` |

## Source

`draw_view_header` / `draw_view_header_hints` / `ViewHeader` / `ViewHints` in
`view_header.rs`. The workspace path is tilde-shortened by `tilde_home`
(`chrome.rs`) from `App::cwd`. The `DELEGATED` flag arrives through
`App::delegated` (the harness snapshot's delegated-autonomous bit). The
`Ctrl-x` scene namespace is rendered by
`components::which_key::draw_which_key_overlay`.

## Scene exit

Esc never closes a Scene (ADR-0205 `[INV-TUI-CLEAN-02]`, enforced by
ADR-0298). Row 2 therefore advertises the `Ctrl-x` **scene namespace** rather
than a back pair, and the exit itself is a deliberate verb:

| Chord | Effect |
|-------|--------|
| `C-x w` / `C-x k` | Dismiss a foreground dialog if one is up; otherwise leave the current Scene (detach from an aside, pop one subagent focus level, or return from the Dashboard / Settings). At the home Conversation scene it is a spent gesture |
| `C-x b` / `C-x p` | Open (or close) the Command Palette / surface switcher |
| `C-x C-c` | Quit muta (the same armed double-press as the global `Ctrl+C`) |
| `C-x C-g` / `C-x Esc` | Cancel the namespace without acting |
| `C-x C-x` | Re-arm (idempotent) |

A verb is dispatched **and** advertised from one table
(`keymap::scene_namespace::SceneVerb`), so the card cannot print a stroke the
dispatcher does not honour, nor can a verb become dispatchable without
appearing on the card (ADR-0238). Second strokes are case-insensitive — a
leader chord is typed without looking. No scene carries an exit chord of its
own: `q` is an ordinary printable everywhere (it seeds the Dashboard console
composer like any other unclaimed letter).

Esc's complete remit on a Scene is a *dismiss*: dismiss the top overlay, unwind
a scene-local sub-layer (a config dropdown, a drill-in pane, the dashboard
preview or inline prompt), clear step/browse focus, dismiss a completion, or
(armed twice) interrupt the running round.

The namespace is drawn as a floating card by
`components::which_key::draw_which_key_overlay` while the chord is armed. The
card is *generated* from the verb table (`SceneVerb::ALL`) — one row per verb,
keyed by the stroke that verb advertises — so a verb cannot be dispatchable
without appearing on the card, nor can the card print a stroke the dispatcher
does not honour. The leave row names the *resolved* action for the current
surface stack (`close overlay` / `leave scene` / `home already`) via
`close_label_for` (ADR-0238/0298).
