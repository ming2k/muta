---
id: ADR-0298
title: "One Scene-Exit Mechanism: the C-x Namespace Owns Scene Lifecycle, Esc Never Closes a Scene"
status: accepted
date: 2026-09-29
scope: mutx/tui (input router, scene schemes, head band chrome, which-key overlay, chord registry)
superseded_by: null
negative_knowledge: true
---

# 0298. One Scene-Exit Mechanism: the `C-x` Namespace Owns Scene Lifecycle, Esc Never Closes a Scene

- **Status:** Accepted
- **Date:** 2026-09-29
- **Scope:** `mutx` (`input/router.rs`, `session.rs`, `modal_keys.rs`,
  `app/surfaces.rs`, `event_loop/actions{,/modals}.rs`, `view_header.rs`,
  `components/which_key.rs`, `overlays/dashboard.rs`, `views/settings/`,
  `keymap.rs`, `render/mod.rs`)
- **Deciders:** Muta Architecture Team
- **Amends:** [ADR-0205](0205-unified-tui-surface-architecture-and-spatial-modality-taxonomy.md) (enforces its `[INV-TUI-CLEAN-02]`, which the code had drifted from), [ADR-0103](0103-btw-background-asides.md) §2 (restores `Ctrl+C` as the aside's sole leave-key)
- **Related:** [ADR-0104](0104-demand-driven-hints.md) (row-2 demand gating), [ADR-0238](0238-footer-chrome-advertises-only-bindings-that-fire.md) (chrome only advertises what fires)

---

## Context and Problem Statement

The question *"how do you leave a full-screen Scene?"* had grown **three**
competing answers, each internally consistent and mutually incompatible. All
three are instances of the same failure: a second mechanism accreted beside the
first, and nothing forced the two to agree.

**1. The `C-x` namespace existed in the dispatcher but in no other layer.**
`input/router.rs` armed a two-stroke leader chord on `Ctrl+X` and resolved
`w`/`k` to `InputAction::CloseScene`, `b`/`p` to the view switcher, `C-c` to
quit, and `C-g`/`Esc` to a cancel — as a `match` on raw `KeyCode`s. Its verb
list was therefore written **twice**: once for dispatch (the router's `match`)
and once for advertisement (`which_key.rs`'s `vec![...]`). That is precisely the
defect class ADR-0238 exists to kill — chrome and dispatcher as independent
copies of one intent. (ADR-0238 documented the queue bar rendering a `Ctrl-q`
that resolved to nothing; here the same shape survived in the namespace.) The
`LeaderChord` type was a two-state enum with a single non-`None` variant. And
the only chrome that mentioned the namespace at all was the floating card, which
appears *after* the chord is armed — nothing advertised it where a user looks
first, the head band's row 2.

**2. Esc was still a Scene exit, in five separate places.** ADR-0205 states the
rule plainly — *"Scenes are peers and do not have parents. Esc does not dismiss
a Scene"*, codified as `[INV-TUI-CLEAN-02]` ("A Scene cannot be pushed into
`overlay_stack` or closed via `Esc`"). The shipped code contradicted it:

| Path | Code | Effect |
| --- | --- | --- |
| TaskInspection zoom | `session.rs` `resolve_subagent_key` | `Esc` (after focus/browse/completion clear) → `ExitSubagent` |
| `/btw` aside | `session.rs` `resolve_side_key` | `Esc` when idle → `ExitSideView` |
| Dashboard | `input/router.rs` Esc arm | → `CloseModal` |
| Settings | `input/router.rs` Esc arm | → `ConfigBack` |
| any scene | `app/surfaces.rs` `dismiss_active_dialog` | → `back_scene()` |

The fourth and fifth rows compounded the error: `handle_close_modal` ended with
a `reset_to_conversation()` for any non-Models/Connections dialog, so
*Esc-dismissing any overlay* while standing on the Dashboard or Settings also
demoted the Scene underneath. Esc had become a second, undocumented scene
router that fired as a side effect of dismissing a dialog.

Two concrete harms followed from that state:

- **The subagent zoom needed two Escs to leave**, but the second Esc's meaning
  depended on transient focus state (was a step focused? was a completion
  dismissed?). A user who pressed Esc "again" as a *dismiss-alike* would find
  themselves ejected from the scene instead, with no way to tell in advance
  which press was which. The 0.47.0 notes had already tried to tame this with a
  documented unwind cascade, which is a symptom: a chord whose meaning depends
  on how many unpublished sub-states happen to be clear cannot be reasoned
  about.
- **Esc's meanings contradicted each other across surfaces.** ADR-0103 §2
  assigns `Ctrl+C` the leave-the-aside role and Esc the interrupt role; the
  aside's `resolve_side_key` nonetheless exited the view on an idle Esc, so the
  same key was "leave" in one scene and "interrupt" in another depending on
  whether a round happened to be running.

Meanwhile the `C-x` namespace — the mechanism that *is* a deliberate, uniform,
cross-scene exit — stayed undiscoverable. The system had two exits, one of
which lied about being an exit.

**3. A third exit: `q` on three surfaces, and a fourth copy of the head band.**
Independently of both, `q` exited the Dashboard console, the Settings categories
pane, and the TaskInspection zoom. Each was hand-rolled in a different resolver
(`resolve_host_key`, `resolve_config_key`, `resolve_subagent_key`) — three more
spellings of "leave this scene", none of them advertised in a consistent place,
and two of them (`q` on the Dashboard) coexisting with a footer legend that read
`q/Esc back` while Esc in fact did something else entirely. The TaskInspection
zoom's `q` was additionally gated on four transient conditions (no completion,
no focused step, no browse focus, empty input), reproducing inside the `q`
mechanism the same state-dependence that made the Esc cascade unteachable.

The same divergence ran through the *chrome*. The Dashboard did not use the
shared head band at all: it hand-rolled `draw_header`, its own one-row layout
with a gap row, and its own inline prompt styling — so it could not carry a
row-2 namespace legend even in principle, and its footer advertised a
`q/Esc back` pair whose Esc half had already stopped being true. Settings,
meanwhile, called the shared band but had to pass a **dummy
`GlobalOverrides::default()`** purely to satisfy the signature: the parameter
existed for one variant (Session) and every other caller stubbed it. Two
`ViewHints` fields — `interruptible` and `parent_note` — were written at every
call site and **read by nothing**: dead weight carried by eight constructors.
And `render_footer` took a `keymap_page` flag whose only caller passed `false`,
guarding a `Esc close / ? close` pair left over from the retired in-dialog
keymap sub-page (ADR-0172).

## Decision

**A Scene has exactly one exit mechanism, and it is not `Esc`. One namespace,
one table, one head band.**

### 1. Scene exit is a deliberate verb; `Esc` is a dismiss, never a navigation

A new `InputAction::SceneBack` carries the *only* thing `Esc` may do to a Scene:
step back one level **inside** it. It is produced by the router's Esc arm for
the Dashboard and Settings scenes, and unwinds exactly what
`App::pop_sublayer` does — a config dropdown, a drill-in pane (with the theme
preview revert), the dashboard's preview, or its inline prompt. It has no arm
that leaves the scene, by construction: `App::scene_back` cannot change
`current_scene()`.

`App::dismiss_active_dialog` becomes strictly overlay-scoped and returns `false`
on a bare scene. The trailing `reset_to_conversation()` in
`handle_close_modal` is deleted. And `App::close_scene` is introduced as the
single scene-leaving verb:

| Scene | What `close_scene` does |
| --- | --- |
| `Aside` | Detach to the primary transcript (ADR-0103: non-destructive, the aside keeps running), clear the Esc interrupt arm, `ExitSideView` |
| `TaskInspection` | Pop one focus level (`exit_subagent`); at the root, `leave_scene` |
| `Dashboard` / `Settings` | `leave_scene` — return to the origin scene |
| `Conversation` | Nothing (nowhere to go); reports `false` |

**No scene carries an exit chord of its own.** The `q` exits are deleted from
all three surfaces. `q` becomes an ordinary printable everywhere — on the
Dashboard it seeds the console composer (`HostPromptSeed('q')`) exactly like
every other unclaimed letter, which is what "typing is opening" already
promised. The consequences of deleting it are two dead action variants
(`ExitSubagent`, `ExitSideView`) that `close_scene` had already absorbed, and a
`resolve_subagent_key` that reduces to the shared chat core plus its two
remappable sibling verbs.

### 2. The namespace is one table, consumed by both dispatch and advertisement

`keymap::scene_namespace` owns the namespace:

```rust
pub enum SceneVerb { Leave, Switcher, Quit }
impl SceneVerb {
    pub const ALL: &'static [SceneVerb];          // advertisement order
    pub const fn strokes(self) -> &'static [Key]; // dispatch table (+ spellings)
    pub const fn advertised_stroke(self) -> Key;  // what the card prints
    pub fn from_second_stroke(key: Key) -> Option<Self>; // case-folding resolve
}
```

The router resolves second strokes through `from_second_stroke`; the which-key
card renders one row per `SceneVerb::ALL` entry. A verb therefore cannot be
dispatchable without being advertised, nor advertised without being
dispatchable — and the router no longer matches raw `KeyCode`s, so the two-case
`C-x W`/`C-x w` duplication disappears into one case-folding normalizer.

`LeaderChord` is deleted. It had exactly one non-`None` variant, so the state it
modelled is one bool: `App::scene_namespace_armed`. The two `InputAction`
namespace variants (`SetLeaderChord`/`CancelLeaderChord`) become
`SetSceneNamespaceArmed(bool)`/`CancelSceneNamespace`.

`C-x C-x` re-arms (idempotent); `C-x Esc`, `C-x C-g`, and any unrecognized
second stroke cancel. A bare `C-x c` cancels rather than quitting — only the
declared `C-x C-c` spelling reaches the quit verdict, so a mistyped chord cannot
exit the app.

### 3. Row 2 advertises the namespace, and never an Esc exit

`ViewHints` loses its `back_key` field entirely — a parameter whose only job was
to render `Esc back`. The crumb line (`Main › Aside`, `Main › Subagent[role]`)
and the Settings/Dashboard legends now render one pair: `Ctrl-x scene`, sourced
from the new `Key::CTRL_X` constant and a shared `SCENE_NAMESPACE_LABEL`. The
label names the *namespace*, not one of its verbs, because the same row is
shared by pages already at their home scene where `w`/`k` have nothing to close.

`draw_which_key_overlay` takes a resolved `close_label` (`close overlay` /
`leave scene` / `home already`) through `close_label_for(has_dialog,
leaves_a_scene)` instead of a boolean, so the leave row describes what the
dispatcher will actually do at that instant (ADR-0238).

### 4. There is one head band, and every scene uses it

The Dashboard's homegrown `draw_header` is deleted. `ViewHeader` gains a
`Dashboard(DashboardHead)` variant carrying the pre-aggregated fleet summary and
its attention flag, and the dashboard lays out the same two rows as every other
scene — row 1 identity + status, row 2 the namespace — through the shared
`draw_view_header` / `draw_view_header_hints`. Its footer's `q/Esc back` legend
becomes `C-x scene`.

Three pieces of chrome debt are removed rather than documented:

- `draw_view_header`'s `key_overrides` parameter is gone. It existed for one
  variant (Session) and every other caller — Settings, the dashboard, tests —
  passed a dummy `GlobalOverrides::default()`. `SessionHead` now carries its own
  `palette_key: Option<Key>`, resolved by the caller that actually has the
  override table. `TranscriptProps::key_overrides` (already unused) goes with it.
- `ViewHints::interruptible` and `ViewHints::parent_note` are deleted: written
  by eight constructors, read by nothing.
- `render_footer`'s `keymap_page` parameter is deleted: its only caller passed
  `false`, and it guarded a dead `Esc close / ? close` pair from the retired
  in-dialog keymap sub-page (ADR-0172).

### 5. The standalone-startup carve-out moves off the dismiss path

A scene opened **standalone** at startup (`mutx dashboard`, `mutx settings`)
still quits the program instead of dropping the user into a carrier
conversation they never asked for. That carve-out moved out of the Esc path
(where it had lived as a branch of the overlay-dismiss handler) into
`quit_standalone_scene_at_startup`, called only from the scene-exit verb. It is
a program exit, not a scene transition, and is now unreachable from Esc.

A foreground *dialog* is dismissed before the scene is left: the overlay is the
visual foreground, so one press spends itself on it. A decision-bearing *sheet*
is transparent to the namespace — it owns its own keys (ADR-0173 §3) — so the
chord leaves the scene beneath it rather than silently discarding a pending
question.

### Invariants & Behavioral Boundaries

- **`[INV-SCENE-EXIT-01]` Esc never changes `SceneKind`.** No Esc chord, and no
  handler reachable from one, may call `back_scene`, `leave_scene`,
  `close_scene`, `exit_subagent`, or `exit_side_view`. Esc's complete remit is:
  dismiss a top overlay, unwind a scene-local sub-layer, clear step/browse
  focus, dismiss a completion, cancel the namespace or a sheet, and (armed
  twice) interrupt a round.
- **`[INV-SCENE-EXIT-02]` Scene exit is one verb, one producer.** Every
  scene-leaving path funnels through `App::close_scene`, and the `C-x`
  namespace's leave verb is its *only* producer. No scene may add an exit chord
  of its own.
- **`[INV-SCENE-EXIT-03]` Chrome never advertises an Esc scene exit.**
  `ViewHints` has no back-key field to render one with.
- **`[INV-SCENE-EXIT-04]` A dismiss never navigates.** `dismiss_active_dialog`
  is overlay-scoped and returns `false` on a bare scene; no caller may treat its
  `false` as "nothing left, so leave the scene".
- **`[INV-SCENE-EXIT-05]` The namespace's verbs are declared once.** Dispatch
  and advertisement both read `scene_namespace::SceneVerb`; neither may hold a
  parallel list. A stroke the table does not declare must cancel, never fall
  through to a global.
- **`[INV-SCENE-EXIT-06]` One head band.** Every scene renders its head through
  `draw_view_header`; no scene may hand-roll one.

### Positive Consequences

- The subagent zoom and the aside now have an unambiguous exit (`q` or
  `C-x w`), and Esc's meaning no longer depends on unpublished focus state.
  The 0.47.0 unwind cascade is reduced to its honest core: Esc clears focus and
  stops.
- ADR-0205 `[INV-TUI-CLEAN-02]` is true in the code again, and is pinned by
  `esc_never_leaves_a_scene_anywhere_in_the_dispatch`, which sweeps all four
  non-home scenes through the close-modal handler, the step-back verb, and the
  dismiss verb.
- The `C-x` namespace — already the only chord that works over every overlay —
  is now discoverable from the head band, which is where a user looking for
  "how do I leave this page" actually looks.
- The Dashboards' and Settings' overlay dismissals no longer demote the scene as
  a side effect, which was an undocumented fourth exit nobody had designed.
- **There is now one head band.** The Dashboard renders through the same
  `draw_view_header` as every other scene, so a future chrome change lands in
  one place instead of two. Four pieces of dead surface went with it: a
  duplicated verb list, a two-state enum, two never-read `ViewHints` fields, and
  a dead `keymap_page` branch guarded by a parameter no caller ever varied.
- **The `C-x W` / `C-x w` duplication collapsed.** The router previously matched
  four `KeyCode` patterns per verb to work around terminal case-folding; there
  is now one case-folding normalizer in `SceneVerb::from_second_stroke`.

### Negative Consequences & Trade-offs

- **Subagent, aside, Dashboard and Settings users all lose a habit.** Two Escs
  no longer leaves the zoom, and `q` no longer exits anything. Every scene's
  exit is now the same two-stroke chord, which is a single re-learned gesture
  rather than four surface-specific ones — but it *is* a re-learn, and `q` in
  particular was a strongly habitual TUI convention.
- **A `q` typo on the Dashboard now types a `q`** into the console composer
  rather than leaving. That is the intended unification (typing is opening), but
  a user reaching for "quit this page" will briefly be confused.
- **`ui.stage_document` and the retained-state lifecycle are untouched, but
  one behaviour is gone**: previously, dismissing a dialog on the Dashboard
  cleaned up by returning to Conversation. A user who dismisses a dialog on a
  scene now stays on that scene, which is the intent — but it does mean a
  dialog's own "I'm done" gesture no longer doubles as navigation.
- The which-key card grew a third label (`home already`) to stay honest. A
  shorter card would have required lying about a chord that resolves but has
  nothing to act on.
- The namespace's verbs are now a `const` table rather than an inline `match`,
  which is slightly more indirection at the dispatch site in exchange for the
  card and the dispatcher being unable to disagree.

## Alternatives considered

### Delete the `q` exits but keep the `Ctrl+X` verb list inline (rejected)

This was the minimal diff for the `q` half of the decision. Rejected because it
leaves the *larger* defect standing: the verb list would still exist twice (a
`match` in the router, a `vec!` in the card), which is the exact shape ADR-0238
was written to eliminate. Fixing `q` without extracting the table would have
reproduced the bug with a smaller blast radius.

### Keep `q`, documented as "the scene's own exit, the same verb as `C-x w`" (rejected)

This was the first iteration of this change, and it is defensible: `q` resolved
to the *same* `close_scene` verb, so it was not a divergent mechanism, only a
divergent *spelling*. Rejected on your direction and on the architecture's own
terms: a second spelling of one verb is a second mechanism for the user to
learn and for chrome to advertise, and each of the three `q` exits carried its
own bespoke guard conditions (the zoom's was gated on four transient states).
Two of them also coexisted with a legend that advertised `q/Esc back` while Esc
did something else — a second promise that could drift. One chord, one place to
look, is the property worth protecting.

### Keep the Dashboard's own `draw_header` and add a row-2 legend beside it (rejected)

Rejected because it makes the head band two implementations with one contract —
the same divergence, just made explicit. The Dashboard needed `ViewHeader`
coverage anyway to carry the namespace row, and the shared band already had a
demand-driven row-2 mechanism (`has_content`) built for exactly this.

### Keep `key_overrides` on `draw_view_header` and pass real overrides everywhere (rejected)

Considered plumbing the real `GlobalOverrides` through to Settings and the
Dashboard instead of deleting the parameter. Rejected: neither scene has a
palette keycap to render, so the plumbing would exist only to keep a signature
uniform — and the dummy default it currently received is precisely the kind of
"satisfy the compiler" call site that hides a dead parameter. `SessionHead`
carrying its own `palette_key` puts the value where it is used.

### Keep `ViewHints::interruptible` / `parent_note` as documented reserved fields (rejected)

Both were written by every constructor and read by nothing. A reserved field
that no reader consumes is indistinguishable from a mistake, and the two
constructors that filled them with plausible values (`"main running"`,
`viewed_running`) made them look load-bearing. Deleted; if the legend ever needs
them, the caller that reads them can add them back with the reader in the same
commit.

### Keep Esc as the scene exit and merely document the cascade (rejected)

The 0.47.0 notes took this route: keep `Esc` exiting the zoom, and document
that it unwinds completion → focus → exit. Rejected because it does not fix the
underlying defect — a chord whose *reachability* depends on transient,
unpublished sub-state cannot be reasoned about by the user, and the document
cannot be read at the moment of the keypress. It also leaves ADR-0205's written
invariant false, which is worse than having no invariant: a document that
contradicts the code trains contributors to distrust the documents.

### Leave `dismiss_active_dialog` scene-aware and only remove the Esc *verbs* (rejected)

This is the minimal diff: delete the `ExitSubagent`/`ExitSideView` arms from
`resolve_subagent_key`/`resolve_side_key` and stop there. Rejected because the
Dashboard and Settings paths would still reach `back_scene()` through
`CloseModal`/`ConfigBack` → `dismiss_active_dialog`, and through the
`reset_to_conversation()` tail of `handle_close_modal`. Esc would still close
Scenes; only the two most visible instances would be gone. The invariant needs
to hold at the *verb*, not at each call site.

### Make Esc context-sensitive: exit only when the scene is "root-most" (rejected)

Considered making Esc exit a scene only when no sub-layer is open — i.e.
"Esc backs out one level, and the scene is a level". Rejected: it makes the
Scene a parent of its own overlays, which is exactly the parent/child model
ADR-0205 abolishes ("Scenes are peers and do not have parents"). It also
resurrects the same unpredictability in a different shape — whether the final
Esc ejects you would still depend on whether a dropdown happened to be open.

### Absorb printables in the read-only zoom instead of letting them edit (rejected)

The zoom draws no composer, so a printable key there edits a buffer with no
visible representation. Absorbing them looked like a strict improvement, but it
changes behaviour beyond this decision's scope (bracket keys, history recall and
the shared editing layer all have pinned contracts), and the pre-existing
behaviour is not what this ADR is about. Left for its own decision.

### Bind the namespace to `C-x C-c` (Emacs-faithful) (rejected)

`C-x C-c` is Emacs' `save-buffers-kill-emacs`. Rejected: in `mutx`, `Ctrl+C`
already means interrupt/quit with an armed double-press confirmation, so
`C-x C-c` inside an already-armed namespace would create a two-stroke chord
whose second stroke is itself a two-stroke gesture. `C-x w` / `C-x k` (already
shipped and already resolving) is unambiguous.

### Show the full namespace in row 2 (`C-x w close  C-x b switch`) (rejected)

Rejected on ADR-0238 grounds plus width: the row is shared by home-scene pages
where `w` has nothing to close, so the legend would advertise verbs that do
nothing there; and a three-pair legend does not fit the band at typical widths
(the existing pair-fitting loop would silently drop pairs, making the legend's
content terminal-width-dependent — a worse promise than a shorter one).

### Keep `InputAction::ConfigBack` as a separate verb (rejected)

`ConfigBack` existed only to carry the Settings scene's Esc exit. Its
scene-local unwind is now `SceneBack`, and its scene exit is `CloseScene`, so
the variant had no producers left. Deleted rather than kept as a deprecated
alias — a dead variant is exactly the shape of the `InputAction::RecallQueued`
defect ADR-0238 removed.

## Links

- Supersedes in substance (not in status): the `Esc`-as-scene-exit behaviour
  documented in the 0.47.0 changelog entry and in the retired `draw_subagent_bar`
  legend.
- Enforces: [ADR-0205](0205-unified-tui-surface-architecture-and-spatial-modality-taxonomy.md) `[INV-TUI-CLEAN-02]`.
- Restores: [ADR-0103](0103-btw-background-asides.md) §2 (`Ctrl+C` leaves an aside; `Esc` interrupts it).
- Conforms to: [ADR-0238](0238-footer-chrome-advertises-only-bindings-that-fire.md) (the which-key card's resolved label), [ADR-0104](0104-demand-driven-hints.md) (row-2 demand gating).
- Related: [ADR-0169](archive/0169-session-view-dual-mode-and-unified-leader-keyboard-architecture.md) (archived) and [ADR-0170](0170-composer-first-command-palette-and-action-registry-architecture.md), which retired the first `Ctrl+X` leader chord. That retirement targeted the *dual-mode confinement and nested keymap pages* it shipped with; the `Ctrl+X` namespace has since returned (0.37.5) scoped to scene lifecycle alone, which is what this record ratifies.
