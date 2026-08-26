# 0141. View means full-screen destination; a modal is a modal

- **Status:** Accepted
- **Date:** 2026-08-25

## Context

ADR-0139 unified surface routing and gave every browse surface one
lifecycle, but it kept the word **view** for the wrong thing. Two facts
about the codebase make the current taxonomy dishonest:

1. **Most "views" are centered panels.** Of the 18 `ViewId` variants, 16
   (Help, Activity, Todos, Tools, Mcp, Skills, Permissions, UsageStats,
   TokenReport, Btw, Models, Connections, HistorySearch, Queue, Sessions,
   Tree) render through `modal_area` / `content_modal_area`
   (`src/primitives.rs:357`, `:377`) — a centered sub-rect floating over a
   recessed live surface. They are modals in every observable way
   (geometry, recess policy, Esc semantics, outside-click dismissal)
   except their type name.

2. **The real full-screen destinations are modeled five different ways.**
   The surfaces that actually own the whole viewport are modeled
   inconsistently:

   | Surface | Modeling today |
   |---|---|
   | Session (live conversation) | `Surface::Chat` — a special-cased router variant, not a `ViewId` |
   | Dashboard (`/dashboard`) | `ViewId::Host` projecting to `Modal::Host`, `Recess::Takeover` |
   | Settings (`/config`) | `ViewId::Config` projecting to `Modal::Config`, full-screen canvas |
   | Envoy zoom | `App::focus_stack: Vec<ZoomFrame>` + `in_envoy_view()` — **outside** the router and registry entirely (62 call sites) |
   | Side view (aside transcript) | `App::in_side_view: bool` + `side_session_id` — also outside the router |

   So "view" currently means five different things, and the two surfaces
   that behave most like views (envoy zoom, side view) are not views at
   all, while sixteen modal panels are.

This is the confusion this ADR resolves. A word that means five things
means none of them: contributors cannot tell whether a new surface should
be a `ViewId`, a `Modal`, a stack on `App`, or a bare `bool`, and the
answer has historically been "all of the above".

## Decision

**A *view* is an independent, full-screen destination.** The user stands
in a view; the terminal is the view. The set is closed and small:

```rust
enum View {
    Session,   // the live conversation (composer + transcript)
    Dashboard, // /dashboard — session dashboard (was ViewId::Host)
    Settings,  // /config — full-screen settings center (was ViewId::Config)
    Envoy,     // zoomed into an envoy task's transcript (was focus_stack)
    Side,      // an aside's transcript (was in_side_view)
  }
```

**A *modal* is an overlay.** It floats over whatever view is active,
recesses it (`Modal::recess()`), and dismisses on Esc / outside click. It
is never full-screen and never a navigation destination.

**A *panel* is a retained modal.** The sixteen browse overlays keep
ADR-0139's buffer-like contract (retained cursor/scroll/drafts, MRU,
registry) — that machinery was correct — but they are honestly named
`PanelId` (was `ViewId` minus `Host`/`Config`), live in a `PanelRegistry`
(was `ViewRegistry`), and open with `open_panel` / hide with
`hide_active_panel`. Retention is orthogonal to geometry: a panel is
still a modal while being retained.

**The router owns all navigation.** `SurfaceRouter` (in `surfaces.rs`,
renamed from `views.rs`) is the single source of truth:

```rust
enum Surface {
    View(View),        // full-screen destination (default: View::Session)
    Panel(PanelId),    // retained browse modal over the active view
    Transient(Modal),  // request sheets, editors, quick switcher
}
```

Invariants:

- `Surface::View(View::Session)` replaces the `Surface::Chat` special
  case. The session is a view like any other, not an "absence of view".
- Envoy zoom keeps `App::focus_stack` as *frame data* (call ids + saved
  scrolls — nested zoom is a stack), but entering/leaving zoom routes
  through the router: `in_envoy_view()` is derived from
  `active_view() == Some(View::Envoy)`, never from stack emptiness alone.
- The side view likewise: `App::in_side_view()` reads
  `active_view() == Some(View::Side)`; `side_session_id` remains payload.
- A panel may be open over any view (e.g. Ctrl+M while zoomed into an
  envoy task); Esc pops panel → view, consistent with today's behavior.
- `Modal` remains the presentation discriminant for render/input dispatch.
  Every surface projects to one: `View::Dashboard → Modal::Host`,
  `View::Settings → Modal::Config`, `Panel(id) → id.modal()`. The
  projection is total and one-way; identity always comes from the router.

## Alternatives considered

- **Keep ADR-0139's taxonomy, rename nothing.** Rejected: the five-way
  meaning of "view" is the reported confusion; naming is the fix.
- **Make every current `ViewId` a full-screen view** (promote panels to
  views instead of demoting them to panels). Rejected: sixteen full-screen
  takeovers for glanceable reference data (help keys, todos, queue) would
  destroy the "live surface stays visible" property the recess system was
  built for, and would break the outside-click/Esc modal contract users
  already know.
- **Delete `Modal` and dispatch on `Surface` directly.** Rejected for
  now: `Modal` is the render-layer seam (geometry, recess, per-modal
  renderers, input dispatch) and Activity/Todos legitimately share one
  presentation. The router→Modal projection keeps that layer untouched.
- **Model envoy zoom as a stack of surfaces in the router.** Rejected:
  the router's return stack is for navigation; zoom frames carry
  per-frame transcript scroll snapshots that belong with the transcript
  state on `App`. The *surface* is one (`View::Envoy`); the *frames* are
  data.

## Consequences

- The vocabulary matches observation: `View` = full-screen, `Panel` =
  retained modal, `Modal` = presentation. New surfaces have one obvious
  home.
- `in_envoy_view()` / `in_side_view()` stay as derived methods, so their
  ~70 call sites (including input tests) survive unchanged; only their
  bodies change to consult the router.
- `ViewId::Host`/`ViewId::Config` identity sites move to
  `View::Dashboard`/`View::Settings`; their render paths
  (`Modal::Host`, `Modal::Config`) and all dashboard/settings state
  fields are untouched.
- The quick switcher lists both views and panels (views first), keeping
  its discovery role.
- Cost: a broad mechanical rename (`ViewId`→`PanelId`,
  `crate::views`→`crate::surfaces`, `open_view`→`open_panel`,
  `show_chat`→`show_session_view`, …). Behavior-preserving; verified by
  the existing unit, input, and snapshot suites.
- ADR-0139's lifecycle contract (retention, MRU, unified
  open/hide/close, per-panel drafts) is restated for panels and carries
  over unchanged. This ADR supersedes only its taxonomy.

## References

- ADR-0139 (unified surface router and view lifecycle) — superseded in
  taxonomy, retained in lifecycle machinery.
- ADR-0133 (views as retained, buffer-like surfaces) — the retained-state
  contract this builds on.
- User report prompting this ADR: "modal 并不能被人定位 view，view 必须是
  独立全屏铺开的 … 而 modal 就是 modal，不是 view。"
