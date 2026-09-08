# 0195. Retained TUI runtime and component lifecycle

- Status: Accepted (partially superseded by ADR-0197 D7: the separate
  `render_tree` half of this decision — the Flutter-style retained box tree —
  never gained a production consumer and was deleted; the component/scene
  lifecycle this decision's other half specifies stands)
- Date: 2026-09-08
- Deciders: ming
- Consulted: —
- Informed: —

## Context and Problem Statement

The terminal engine retains cells, but the application independently
maintains surface navigation, screen rectangles, keyboard foreground,
pointer priority, and cursor ownership. Drawing and hit testing describe
the same interface through different structures. Modal-over-sheet input
regressions demonstrate the cost of that separation. Retaining cells
alone does not provide component identity or incremental layout.

The project requires a durable UI runtime, not another registry beside
the existing registries. Implementation size is not a design objective.
Complete core semantics and explicit ownership are.

## Decision Drivers

- Component changes must not require editing independent global routing
  and geometry cascades.
- Identity, unmount, focus restoration, clipping, and event boundaries
  must have engine-level contracts.
- Input geometry must correspond to the successfully presented frame;
  measurement passes and failed terminal writes must not publish it.
- Transcript virtualization and semantic text selection must remain
  independent of the number of mounted UI nodes.
- Existing terminal protocol, wide-character, and diff correctness must
  remain isolated from UI composition.

## Considered Options

1. Continue free-function drawing with application-owned geometry maps.
2. Add only a layout tree, leaving interaction and lifecycle in the app.
3. Introduce a retained UI runtime above the terminal rendering core.

## Decision Outcome

Choose option 3. `mutx-engine` contains two layers with one-way dependency:
the UI runtime depends on the terminal rendering core. The core does not
depend on UI nodes, components, or application concepts. `mutx` supplies
business components, composition, and action interpretation.

### Composition and identity

Components describe UI structure. Descriptions need not remain allocated
between updates. Mounted nodes have stable keys and generation-checked
identities. Reordering preserves identity; removing and recreating a node
does not resurrect old handles. State belongs to a mounted instance or an
explicit application model, never implicitly to a child index.

Unmounting removes descendants, invalidates handles, releases pointer
capture, and restores focus to a surviving eligible target. Hidden cached
application models are distinct from mounted interactive nodes.

### Layout, paint, and interaction

One node store represents the active UI. Layout ancestry supplies
constraints and inherited clipping. Paint order and input scopes are
explicit properties; logical ownership is not blindly equated with
visual ancestry. Overlays can escape an owner's clip through an explicit
viewport attachment, while retaining logical ownership.

Layout is a measurement and placement operation without terminal writes.
Layout invalidation and paint invalidation are distinct. Node movement,
removal, and changes in stacking invalidate both the former coverage and
the newly exposed coverage. Custom document painters are supported as
leaves; glyphs and every historical transcript row are not UI nodes.

Pointer dispatch uses the presented node geometry and reverse paint
order. Keyboard dispatch uses explicit input scopes and focused targets.
Blocking scopes consume unhandled input; input-transparent paint does not
acquire focus. Application components interpret routed events as domain
actions. The engine does not know about permissions, sessions, or models.

### Frame transaction

Updating, laying out, and painting produces a pending frame. Presenting
the terminal successfully commits its interaction snapshot. Staging a
bottom-follow measurement does not change the published hit targets.
Failed presentation leaves the last committed interaction snapshot
intact and schedules a redraw. A frame cannot mix pending hit regions
with previously presented cells.

### Application migration

Migrate from rendering primitives upward through reusable controls and
surface composition. The application scene includes the transcript,
footer, composer replacement sheets, completion, modal surfaces, nested
confirmations, and input-transparent notifications. The same composition
must determine paint order and event ownership.

Delete replaced application rectangle fields and parallel UI hit maps.
Retain semantic document mappings for selection and source coordinates;
these are document data, not an alternative surface router. Retain
business navigation history and drafts as application models, without
giving those models independent authority over visual hit testing.

There is no compatibility mode or permanent legacy/new runtime switch.
A migrated component uses the runtime for its complete structural
contract. Existing drawing primitives remain valid implementation
details of component painters, not a second UI lifecycle.

### Verification gates

Targeted tests cover stable identity under reorder, stale handles after
unmount, focus restoration, capture release, inherited clipping,
overlay input barriers, transparent notifications, layout invalidation,
old-coverage repaint, and failed/staged frame publication. Application
tests cover coexisting sheets and modals, composer scrolling, completion,
selection, and navigation. Use `cargo check` first and filtered
`cargo nextest run` for unit and integration verification.

### Positive Consequences

- Components share one structural authority for geometry and interaction.
- The terminal backend remains independently testable and reusable.
- Lifecycle and frame consistency become enforceable runtime behavior.

### Negative Consequences

- The engine acquires a substantial subsystem and its invariant tests.
- Explicit component identity and invalidation require disciplined APIs.
- Application migration changes internal APIs without compatibility
  aliases; implementation must remove the corresponding old authorities.

## Pros and Cons of the Options

The current free-function model minimizes infrastructure but multiplies
cross-cutting rules. A layout-only tree centralizes geometry but leaves
identity, input, and lifecycle defects unresolved. A retained runtime
requires more implementation work but gives those responsibilities one
owner. CSS parsing, general reactive dependency tracking, plugin ABIs,
and multithreaded layout are separate decisions, not prerequisites.

## Links

- [ADR-0038: Grid and diff rendering](0038-in-house-grid-diff-rendering-engine.md)
- [ADR-0114: Flex layout foundation](0114-identity-addressed-stream-appends-and-flex-layout-foundation.md)
  — revises the rejection of a retained UI tree based on shallow layout;
  the single-container flex solver remains a layout primitive.
- [ADR-0139: Surface navigation](0139-unified-tui-surface-router-and-view-lifecycle.md)
  — navigation history remains an application concern.
- [ADR-0173: Input ownership and sheets](0173-unbounded-session-keyboard-ownership-claims-and-interaction-sheets.md)
  — supersedes independently maintained foreground priority rules.
