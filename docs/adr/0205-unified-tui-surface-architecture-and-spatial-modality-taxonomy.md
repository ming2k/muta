# 0205. Radical Clean-Break: Stage-Scene-Overlay Architecture and Eradication of Modal Monolith

- Status: Accepted
- Date: 2026-09-12
- Scope: apps/tui/mutx
- Deciders: ming
- Consulted: —
- Informed: —
- Supersedes: [0139](0139-unified-tui-surface-router-and-view-lifecycle.md), [0141](0141-view-means-fullscreen-and-modal-means-modal.md)

---

## Context and Problem Statement

The TUI shell architecture has suffered from chronic conceptual and structural compromises. ADR-0139 and ADR-0141 attempted incremental taxonomy corrections but explicitly compromised on the underlying implementation ("Delete Modal ... Rejected for now"). 

This compromise entrenched architectural absurdities across the codebase:
1. **The Inverted Reality of `Modal::Host` & `Recess::Takeover`**: Full-screen destinations like `/dashboard` and `/config` are modeled as modals that "take over" the screen (`Recess::Takeover`), while the primary conversation view is reduced to `Modal::None`.
2. **The `Modal` God-Enum Anti-pattern**: `src/modal.rs` conflated full-screen scenes, floating browse panels, transient prompt sheets, multi-step configuration wizards, and quick-switchers into a single discriminant.
3. **Double-Identity Pathology**: Surfaces required dual representations—a routing identity (`View`/`PanelId`) and a presentation projection (`Modal`), glued together by fallible boilerplate mappings.
4. **Single-Slot Overlay Blocking**: The router supported at most one overlay over a view, causing nested interactions (e.g. an asynchronous permission sheet appearing while inside the model picker) to either overwrite state or bypass the router entirely via side-channels (`App::focus_stack`, `App::in_side_view`).
5. **Pollution by Alias Stubs**: Dead aliases (`ViewId = View`, `ModalId = PanelId`) remained active in production code.

We reject incremental patching. This decision executes a radical, uncompromised, clean-break overhaul of the entire UI architecture, eliminating every trace of the legacy modal monolith.

---

## Decision Drivers

- **First-Principles Correctness**: Concepts must match physical reality. The screen is a Stage; destinations are Scenes; floating components are Overlays. Modality is an input behavior, never an object.
- **Zero Legacy Baggage**: Delete `src/modal.rs`, `enum Modal`, and `enum Recess` completely. No backward-compatibility type aliases, no deprecated fallback branches, no bridge functions.
- **Native Multi-Layer Compositing**: The router must manage a true LIFO stack (`Vec<Box<dyn OverlaySurface>>`), enabling arbitrary clean nesting of sheets and popovers.
- **Strict Decoupling of Retention and Geometry**: State preservation (`RetentionPolicy`) is an orthogonal contract declared per component, not an accidental property of where it floats.
- **Trait-Driven Dispatch**: Replace monolithic multi-thousand-line `match` statements in `render.rs` and `input/router.rs` with component-level contracts.

---

## Decision Outcome

Execute a hard architectural clean break: replace the entire surface routing, presentation, and dispatch system with the **Stage-Scene-Overlay Architecture**.

```text
       ┌────────────────────────────────────────────────────────┐
       │                STAGE (Physical Terminal)               │
       └───────────────────────────┬────────────────────────────┘
                                   │ hosts
       ┌───────────────────────────▼────────────────────────────┐
       │            SCENE (Full-screen Root Canvas)             │
       │    [ ConversationScene | DashboardScene | ConfigScene ] │
       └───────────────────────────┬────────────────────────────┘
                                   │ mounts (0..N)
       ┌───────────────────────────▼────────────────────────────┐
       │             OVERLAY STACK (Composite Layers)           │
       │                                                        │
       │  Layer 2: [Sheet: PermissionRequest]  ◄── Active Focus │
       │  Layer 1: [Dialog: ModelPicker]      ◄── Suspended     │
       │  Layer 0: ──── DIMMER MASK ─────────────────────────── │
       └────────────────────────────────────────────────────────┘
```

### 1. Spatial Primitives

1. **`Stage`**: The physical terminal grid ($W \times H$ cells). Owns alternate screen buffers, raw SGR rendering, and terminal capability detection.
2. **`Scene`**: An independent, full-screen primary workspace. Exactly one Scene is mounted at any instant. Scenes are peers and do not have parents. Esc does not dismiss a Scene.
   - `ConversationScene`: Live transcript stream and composer.
   - `DashboardScene`: Session and daemon telemetry console (`/dashboard`).
   - `SettingsScene`: Interactive global configuration environment (`/config`).
   - `TaskInspectionScene`: Deep-dive inspection into subagent and envoy execution.
3. **`Overlay`**: Any bounded visual element floating above the active Scene:
   - **`Dialog`**: Centered, bounded floating workspace with structured tabs/lists (e.g. Tools, MCP, Models, Connections, Queue).
   - **`Sheet`**: Edge-anchored (bottom or lateral) prompt bound to a specific decision or asynchronous request (e.g. Permission approval, User Question, Action Confirm).
   - **`Popover`**: Coordinate-anchored floating flyout attached to an active cursor or key anchor (e.g. autocomplete menu, inline tooltip).

---

### 2. Core Type Architecture

`apps/tui/crates/mutx/src/surfaces/` is rewritten from scratch:

```rust
/// The single source of navigation truth.
pub struct SurfaceRouter {
    /// Active root scene.
    active_scene: Box<dyn SceneSurface>,
    /// Active overlay stack (bottom to top). Top owns input.
    overlay_stack: Vec<Box<dyn OverlaySurface>>,
    /// Bounded historical trace of scenes for explicit back-navigation.
    scene_history: Vec<SceneKind>,
}

/// Identifiers for closed root scenes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SceneKind {
    Conversation,
    Dashboard,
    Settings,
    TaskInspection,
}

/// Identifiers for centered dialogs (retrievable via Quick Switcher).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DialogKind {
    Help,
    Tools,
    Mcp,
    Skills,
    Permissions,
    UsageStats,
    Telemetry,
    Asides,
    Models,
    Connections,
    History,
    Queue,
    Sessions,
    SessionTree,
}

/// Identifiers for edge action sheets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SheetKind {
    PermissionApproval,
    UserQuestion,
    OAuthWait,
    ActionConfirm,
    ModelConfigEditor,
}
```

---

### 3. Component Contracts and Input Arbitration

Instead of central match statements, surfaces implement cohesive traits:

```rust
pub trait Surface: Send {
    fn render(&self, frame: &mut Frame, area: Rect);
    fn handle_key(&mut self, key: KeyEvent) -> EventOutcome;
    fn handle_mouse(&mut self, mouse: MouseEvent) -> EventOutcome;
}

pub trait SceneSurface: Surface {
    fn kind(&self) -> SceneKind;
    fn on_enter(&mut self);
    fn on_leave(&mut self);
}

pub trait OverlaySurface: Surface {
    fn geometry(&self, stage_bounds: Rect) -> Rect;
    fn retention_policy(&self) -> RetentionPolicy;
    fn blocks_input(&self) -> bool { true }
    fn dismisses_on_outside_click(&self) -> bool { true }
    fn on_dismiss(&mut self);
}
```

#### Event Dispatch Loop:
1. Deliver events to `overlay_stack.last_mut()`.
2. If unhandled and event is `Esc` or outside-bounds click: invoke `dismiss()` on the top overlay.
3. If unhandled and top overlay has `blocks_input() == false`: bubble event to next overlay down, down to `active_scene`.
4. If unhandled by `active_scene`: dispatch to global application shortcuts.

---

### 4. Zero-Tolerance Eradication Matrix (Files & Types to Delete)

The following files, types, and fields are slated for **unconditional deletion with zero backward compatibility**:

| Target to Delete | Replacement | Rationale |
| :--- | :--- | :--- |
| `apps/tui/crates/mutx/src/modal.rs` | **FILE DELETED** | The core monolith is eradicated. |
| `enum Modal` | **DELETED** | Conflated scenes, dialogs, and sheets. |
| `enum Recess` | **`DimmerPass` (Render pipeline)** | Rendering detail, not a surface property. |
| `type ViewId` & `type ModalId` | **DELETED** | Misleading aliases removed. |
| `enum View` | **`enum SceneKind`** | "View" is permanently decommissioned. |
| `enum PanelId` | **`enum DialogKind`** | Centered boxes are dialogs, not panels. |
| `struct PanelRegistry` | **`struct SurfaceStore`** | Manages `RetentionPolicy` across all surfaces. |
| `App::active_modal` | **DELETED** | Router's `overlay_stack` is sole authority. |
| `App::focus_stack` | **`TaskInspectionScene`** | Integrated as a first-class Scene. |
| `App::in_side_view` | **`SceneKind::Conversation` aside context** | Side channels eliminated. |

---

## Invariants & Behavioral Boundaries

- **`[INV-TUI-CLEAN-01]` Zero Mention of Modal as a Type**: The word `Modal` may only appear in documentation as an adjective describing input blocking behavior. It must never be the name of a `struct`, `enum`, or module.
- **`[INV-TUI-CLEAN-02]` Strict Scene Cardinality**: Exactly one `Scene` exists in the active slot at all times. A Scene cannot be pushed into `overlay_stack` or closed via `Esc`.
- **`[INV-TUI-CLEAN-03]` No Sidecar Visibility Channels**: No boolean flags (`is_open`, `in_view`) or parallel stacks (`focus_stack`) may dictate visibility on `App`.
- **`[INV-TUI-CLEAN-04]` LIFO Overlay Invariance**: `overlay_stack` enforces strict LIFO order. An overlay dismissal must restore input focus to the immediately preceding layer without side effects.
- **`[INV-TUI-CLEAN-05]` No Transitional Compatibility Layers**: Re-exporting deleted types under old names or introducing deprecation shims is an automatic build failure.

---

## Negative Consequences & Migration Strategy

- **Large Refactoring Surface**: High blast radius across the TUI binary crate.
- **Execution Strategy**:
  1. Create new primitives in `surfaces/` alongside clean contracts.
  2. Implement composite rendering pipeline in `render/` with explicit `DimmerPass`.
  3. Migrate scene implementations (`Conversation`, `Dashboard`, `Settings`).
  4. Migrate dialogs and sheets into self-contained `OverlaySurface` components.
  5. Delete `src/modal.rs` and all historical references.
  6. Fix tests and refresh golden snapshots.

---

## Rejected Alternatives & Negative Knowledge

### Retaining `Modal` for rendering dispatch
- *Why considered*: Avoided rewriting the `match` arms in `render.rs`.
- *Why rejected*: Preserving `Modal` would perpetuate the entire root cause of structural confusion and prevent true multi-layer overlay nesting.

### Keeping `View` for full-screen destinations
- *Why considered*: Familiarity from ADR-0141.
- *Why rejected*: `View` is the most overloaded term in GUI/TUI programming. `Scene` is crisp, cinematic, distinct, and carries no ambiguous historical baggage in terminal programming.

---

## Links

- Supersedes: [ADR-0139](0139-unified-tui-surface-router-and-view-lifecycle.md)
- Supersedes: [ADR-0141](0141-view-means-fullscreen-and-modal-means-modal.md)
- Living Blueprint: [`docs/explanation/tui.md`](../explanation/tui.md)
