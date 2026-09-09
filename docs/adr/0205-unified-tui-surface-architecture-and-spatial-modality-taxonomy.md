# 0205. Unified TUI Surface Architecture and Spatial-Modality Taxonomy

- Status: Accepted
- Date: 2026-09-12
- Scope: tui/shell
- Deciders: ming
- Consulted: —
- Informed: —
- Supersedes: [0139](0139-unified-tui-surface-router-and-view-lifecycle.md), [0141](0141-view-means-fullscreen-and-modal-means-modal.md)

---

## Context and Problem Statement

ADR-0139 established unified surface routing and buffer-like state retention, and ADR-0141 attempted to correct the semantic confusion where centered overlays were called "views". However, ADR-0141 left significant structural ambiguities:

1. **Semantic Drift of `View`**: The term `View` remained overloaded. In general software engineering, `View` can mean any UI rendering unit or widget; using `View` exclusively for full-screen destinations (`Session`, `Dashboard`, `Settings`) caused cognitive friction and constant misattribution.
2. **The "Modal" As Noun Antipattern**: `Modal` was used as a concrete presentation noun rather than an interaction modifier (*modality*). This caused centered floating panels (`PanelId`), transient action sheets (`Permission`, `Question`), and context popovers to be crammed into a single flattened `Modal` enumeration.
3. **Flawed Single-Slot Overlay Router**: `SurfaceRouter` only tracked a single foreground surface over a base view (`active_view` + `Option<Overlay>`). It could not natively express layered interactions—such as an asynchronous permission request sheet popping up while the user was already inside the Model Picker dialog, or an autocomplete popover appearing over an input sheet.
4. **Coupled Retention and Geometry**: State retention (cursor, search queries, scroll offsets) was baked into the geometric identity of `PanelId` (managed by `PanelRegistry`), while transient sheets could not declare retention policies cleanly.
5. **Compromise Stubs and Alias Pollution**: The codebase accumulated residual compatibility aliases (`type ViewId = View;`, `type ModalId = PanelId;`, `active_modal`), obscuring boundaries for new contributors and AI assistants.

A clean break is required. This decision establishes an uncompromising, mathematically precise taxonomy and runtime type architecture across the entire TUI subsystem, covering all code, comments, tests, and documentation.

---

## Decision Drivers

- **Zero Semantic Ambiguity**: Terminology must directly mirror human-computer interaction (HCI) standards and physical geometry. A full-screen destination is a scene; a centered popup is a dialog; an edge-anchored action prompt is a sheet; an anchor-attached float is a popover.
- **True Stacking Modality**: The router must support a bounded, strictly-ordered overlay stack (`Vec<OverlaySurface>`) to handle multi-layer nested interruptions without losing background context.
- **Orthogonal Retention**: Geometry (Dialog vs. Sheet) must be completely decoupled from state lifetime (`RetentionPolicy`: Retained vs. Ephemeral vs. Transactional).
- **Uncompromising Replacement**: Hard break. Zero compatibility aliases (`ViewId`, `ModalId`, `PanelId`, `active_modal`). Stale types and documentation are deleted entirely.

---

## Considered Options

- **Option 1: Retain ADR-0141 and patch multi-layer overlays ad-hoc**. Add secondary stack channels on `App` for sheets while keeping `View` and `PanelId`.
- **Option 2: Fold everything back into a monolithic window tree**. Implement a heavy desktop-style Window/Widget manager.
- **Option 3: Unified Stage-Scene-Overlay Taxonomy with Orthogonal Modality & Retention**. Clean separation into Stage (physical screen), Scene (root destination), Overlay (Dialog / Sheet / Popover), and explicit Retention Policies.

---

## Decision Outcome

Chosen option: **Option 3**, because it provides unambiguous cognitive clarity, matches terminal spatial geometry, natively supports multi-layer overlay nesting, and eradicates historical legacy baggage without compromise.

---

### 1. The Three-Tier Spatial Hierarchy

```text
       ┌────────────────────────────────────────────────────────┐
       │                 Stage (Terminal Viewport)              │
       └───────────────────────────┬────────────────────────────┘
                                   │ hosts
       ┌───────────────────────────▼────────────────────────────┐
       │              Scene (Root Full-screen Canvas)           │
       │         [Conversation | Dashboard | Settings]          │
       └───────────────────────────┬────────────────────────────┘
                                   │ mounts
       ┌───────────────────────────▼────────────────────────────┐
       │            Overlay Surface (Stacked Over Scene)         │
       │  ┌──────────────────┬──────────────────┬────────────┐  │
       │  │  Dialog (Center) │   Sheet (Edge)   │  Popover   │  │
       │  │(Inspect/Manage)  │(Resolve/Decision)│(Completion)│  │
       │  └──────────────────┴──────────────────┴────────────┘  │
       └────────────────────────────────────────────────────────┘
```

1. **`Stage`**: The physical terminal grid allocated to `mutx` ($W \times H$ cells). Owns raw terminal double-buffering, mouse capture, and color profile capabilities.
2. **`Surface`**: The overarching domain primitive. Any interactive or visual UI layer that can receive events or present layout is a `Surface`.
3. **`Scene`**: An independent, full-screen root workspace. A Scene owns the entire Stage viewport. The set is closed:
   - `Conversation` (live transcript + composer, the default root scene);
   - `Dashboard` (`/dashboard`, session and cluster management overview);
   - `Settings` (`/config`, full-screen preferences center);
   - `TaskInspection` (deep dive into a subagent/envoy task transcript).
4. **`OverlaySurface`**: Any bounded surface rendered above the active Scene. Overlays recess (dim) underlying content and trap or arbitrate input:
   - **`Dialog`**: Centered, bounded modal panel (e.g. Tools, MCP, Models, Connections, History, Queue, Tree, Help).
   - **`Sheet`**: Edge-anchored (bottom or side) action-oriented surface driven by reactive requests or workflows (e.g. Permission approval, User Question prompt, OAuth pending).
   - **`Popover`**: Coordinate-anchored floating micro-surface attached to a cursor or visual element (e.g. slash command completion, mention picker).

---

### 2. Rust Type Architecture

The core routing and identity model in `apps/tui/crates/mutx/src/surfaces/mod.rs` is restructured as follows:

```rust
/// Unified authority over active TUI navigation and overlay stacking.
#[derive(Debug, Default)]
pub struct SurfaceRouter {
    /// The currently active root scene.
    active_scene: SceneId,

    /// Stack of active overlays in bottom-to-top paint order.
    /// The top of the stack holds primary input focus.
    overlay_stack: Vec<OverlaySurface>,

    /// Bounded history of visited scenes for bidirectional scene navigation.
    scene_history: Vec<SceneId>,
}

/// Root full-screen scene identifier (closed set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SceneId {
    #[default]
    Conversation,
    Dashboard,
    Settings,
    TaskInspection,
}

/// Category of overlay floating above the active scene.
#[derive(Debug, Clone, PartialEq)]
pub enum OverlaySurface {
    Dialog(DialogId),
    Sheet(SheetId),
    Popover(PopoverKind),
}

/// Identity of centered, reference and management dialogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DialogId {
    Help,
    Tools,
    Mcp,
    Skills,
    PermissionsManager,
    UsageStats,
    Telemetry,
    Asides,
    Models,
    Connections,
    HistorySearch,
    Queue,
    Sessions,
    SessionTree,
}

/// Identity of edge-anchored, task-driven action sheets.
#[derive(Debug, Clone, PartialEq)]
pub enum SheetId {
    PermissionApproval(PermissionRequestPayload),
    UserQuestion(QuestionRequestPayload),
    OAuthPending(ProviderId),
    ActionConfirm(ConfirmActionPayload),
    ModelEditor(ModelEditorState),
}
```

---

### 3. Orthogonal State Retention Policies

Whether a surface retains its state across dismissal is an orthogonal attribute (`RetentionPolicy`), decoupled from its visual shape:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionPolicy {
    /// State is preserved in `SurfaceStore` across dismissal (scroll offset, search filter, cursor).
    Retained,
    /// State is discarded immediately upon dismissal or pop.
    Ephemeral,
    /// State is scoped to the active session id and purged on session switch.
    SessionScoped,
}
```

- Most `DialogId` variants declare `RetentionPolicy::Retained`.
- `SheetId` action prompts declare `RetentionPolicy::Ephemeral` or rely on backend task channels.

---

### 4. Lifecycle Verbs and Modality Contract

The navigation verbs are strictly standardized:

| Verb | Target | Operational Semantics |
| :--- | :--- | :--- |
| **`switch_scene(target)`** | `SceneId` | Suspends active scene, clears non-sticky overlays, switches root rendering to `target`, records origin in `scene_history`. |
| **`present(overlay)`** | `OverlaySurface` | Pushes overlay to top of `overlay_stack`. Background dims; input focus shifts to top overlay. |
| **`dismiss()`** | `OverlaySurface` | Pops the top overlay. If `Retained`, writes state snapshot to `SurfaceStore`. Input focus restores to previous layer. |
| **`resolve(payload)`** | `SheetId` | Specialization of `dismiss()` for sheets: returns decision payload to caller channel and closes the sheet. |

#### Input Arbitration (Reverse Stack Traversal)
Keyboard and mouse events hit the top of `overlay_stack` first:
1. If the top overlay consumes the event, processing stops.
2. If `Esc` or an outside mouse click occurs, the top overlay is dismissed.
3. If no overlay is present, events are delivered directly to the active `SceneId`.

---

### 5. Breaking Replacements and Deprecation Matrix

No transitional shims or backward-compatibility aliases are permitted:

| Obsolete Identifier | Replacement Identifier | Rationale |
| :--- | :--- | :--- |
| `View` (enum) | **`SceneId`** | Clarifies that it is a root, full-screen canvas. |
| `ViewId` (type alias) | **DELETED** | Eradicated. |
| `PanelId` (enum) | **`DialogId`** | A centered floating box is a Dialog, not a VS Code-style panel. |
| `PanelRegistry` | **`SurfaceStore`** | Manages retention across all surfaces uniformly. |
| `Modal` (presentation enum) | **`OverlayPresentation` / `DialogKind`** | `Modal` is an adjective describing input blocking, not an object. |
| `active_modal` | **DELETED** | Router's `overlay_stack` is the sole source of truth. |
| `Surface::Chat` | **`SceneId::Conversation`** | The main session is a first-class scene. |
| `Surface::Transient` | **`OverlaySurface::Sheet` / `Popover`** | Replaced with explicit geometric overlay types. |

---

## Invariants & Behavioral Boundaries

- **`[INV-TUI-SURF-01]` Zero Alias Tolerance**: The identifiers `ViewId`, `PanelId`, and `active_modal` must not appear anywhere in source code, type definitions, or active documentation.
- **`[INV-TUI-SURF-02]` Sole Navigation Authority**: All spatial transitions must execute through `SurfaceRouter`. No component, command, or background handler may manipulate modal visibility via raw booleans or sidecar stacks on `App`.
- **`[INV-TUI-SURF-03]` Scene Invariance**: Exactly one `SceneId` must be active at any given instant. A Scene cannot be dismissed via `Esc`; `Esc` on a root scene without overlays executes the designated root action (e.g. defocus composer / clear selection).
- **`[INV-TUI-SURF-04]` Strict Stacking Order**: New overlays must push to the top of `overlay_stack`. Event routing is strictly top-down; rendering composition is strictly bottom-up (`Scene` $\to$ `Dimmer` $\to$ `Overlays` $\to$ `Popovers` $\to$ `Toasts`).

---

## Positive Consequences

- **Cognitive Clarity**: Developers and AI agents have unambiguous terms: `Scene` for destinations, `Dialog` for browse panels, `Sheet` for action prompts, `Popover` for anchored flyouts.
- **Support for Nested Overlays**: An urgent permission or question sheet can preemptively appear over an open model dialog without corrupting the dialog's state or losing navigation history.
- **Deterministic Testing**: Testing overlay sequences becomes pure state-machine assertions on `SurfaceRouter::overlay_stack`.

---

## Negative Consequences & Trade-offs

- **Extensive Refactoring Blast Radius**: Requires touch points across `apps/tui/crates/mutx/src/surfaces/`, `app/`, `render/`, `input/`, `chrome/`, tests, and documentation.
- **Mitigation**: Execute refactoring in phased, compile-gated milestones: Core Types $\to$ Router State Machine $\to$ Input & Render Dispatch $\to$ Test Suite & Snapshots $\to$ Documentation.

---

## Rejected Alternatives & Negative Knowledge

### Option 1: Retain `View` and `PanelId` and patch multi-layer overlays
- *Why considered*: Minimal immediate code churn.
- *Why rejected*: Perpetuated the five-way overload of `View` and the misnomer of `Panel`. Technical debt would continue compounding with each new overlay requirement.

### Option 2: Monolithic Desktop-style Window Manager
- *Why considered*: Maximally generic abstraction supporting arbitrary windows, docks, and tiling.
- *Why rejected*: Extreme over-engineering for a terminal interface. Terminal UX relies on predictable, focused modal sheets and scenes, not floating overlapping movable desktop windows.

---

## Links

- Supersedes: [ADR-0139](0139-unified-tui-surface-router-and-view-lifecycle.md)
- Supersedes: [ADR-0141](0141-view-means-fullscreen-and-modal-means-modal.md)
- Living Blueprint: [`docs/explanation/tui.md`](../explanation/tui.md)
