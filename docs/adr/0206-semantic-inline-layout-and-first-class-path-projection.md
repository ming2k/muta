# 0206. Semantic Inline Layout and First-Class Path Projection

- Status: Proposed
- Date: 2026-09-13
- Scope: apps/tui/mutx
- Deciders: ming
- Consulted: —
- Informed: —
- Amends: [0001](0001-tool-rendering-redesign.md), [0181](0181-capability-first-visual-archetypes-and-responsive-layout-pipeline.md)

---

## Context and Problem Statement

`mutx` features a sophisticated path formatting component (`apps/tui/crates/mutx/src/components/path.rs`) providing `PathView`, `PathFormatStrategy` (Adaptive, Fish, MiddleEllipsis, BasenameWithParent), and `PathStyle` (Semantic multi-tone styling). Despite this capability, tool invocations in the transcript frequently exhibit unreadable, excessively long paths that push vital execution metadata off-screen:

```text
+ Search "draw.rs" in apps/tui/crates/mutx/src/overlays/telemetry (3ms)
```

Investigation into the presentation and disclosure rendering pipeline (`apps/tui/crates/mutx/src/tools/` and `src/disclosure/renderers/payloads.rs`) reveals four fundamental architectural flaws:

1. **Premature Stringification & Semantic Erasure**: `ToolPresenter::summary(&self, view: &ToolView) -> String` requires presenters to format their collapsed invocation header as an opaque, flattened `String`. Structured metadata (the verb `Search`, query argument `"draw.rs"`, target directory path, and trailing execution duration `(3ms)`) is prematurely flattened into a single contiguous byte slice before the renderer ever knows the physical viewport width.
2. **Missing Constraint Inversion (The Broken Budget Pipeline)**: Presenters invoke `PathView::from_str(path).format_text()` without setting `max_width`. Under `format_path_str`, an unconstrained width (`max_width == None`) causes the `Adaptive` strategy to degenerate directly into `Full`, returning the unabbreviated path regardless of length.
3. **Right-Edge Tail Truncation Catastrophe**: Because `tool_summary_line` receives an opaque string, it applies naive horizontal clamping (`truncate_to_width(summary, summary_budget)`). On constrained viewports or nested split panes, this right-edge truncation erases the most critical trailing elements—the status/duration badge (`(3ms)`) and the filename leaf—while preserving the low-value ancestral directory prefixes (`apps/tui/crates/mutx/src/...`).
4. **Dormant Rich-Text Semantic Styling**: `PathView::to_spans` generates multi-tone Ratatui spans (dim ancestor directories, bold file stems, accented extensions, and highlighted `:line:col` indicators). Because `ToolPresenter::summary` returns a plain `String`, the entire header line is painted with a single uniform style (`summary_color`), rendering the semantic color system completely dormant.
5. **Cross-View Inconsistency**: Expanded results (`draw_matches_content` for search matches and `draw_listing_content` for directory trees) bypass `PathView` entirely, emitting raw path strings directly into terminal rows.

---

## Decision Drivers

- **Semantic Anchoring & Information Priority**: In developer workflows, the leaf filename (what), the immediate parent directory (subsystem context), and line/column numbers (precision) possess strictly higher information density than distant ancestor directories. Trailing telemetry (`(3ms)`, failure markers) must be protected against truncation.
- **Inversion of Control (Constraints Go Down, Content Adapts Up)**: Leaf formatting components must not guess screen dimensions, nor should callers hardcode arbitrary magic numbers (`max_width(36)`). The outer rendering container with viewport access must pass spatial constraints down to the components.
- **Dual-Mode Lossless Compatibility**: Introducing layout-aware inline rendering must not break headless testing, transcript snapshot verification (`insta`), clipboard extraction, or plain-text session serialization.
- **Cross-Cutting Orthogonality**: Path projection must be a shared primitive that flows seamlessly across Tool Step Headers, Search Match Groupings, Unified Diff Banners, Directory Listings, and Status Bars without duplicating layout math.

---

## Decision Outcome

We establish a first-class **Semantic Inline Layout** architecture in `mutx`, elevating paths from raw strings to layout-aware first-class citizens (`InlineSlot::Path`).

```text
┌──────────────────────────────────────────────────────────────────────────────────────────────────┐
│ SemanticLine DSL (Data / Presenter Layer)                                                        │
│ [ Fixed: "Search " ] ─ [ Flexible: "\"draw.rs\"" ] ─ [ Fixed: " in " ] ─ [ Path: PathView(...) ] │
└────────────────────────────────────────┬─────────────────────────────────────────────────────────┘
                                         │
                 ┌───────────────────────┴───────────────────────┐
                 ▼                                               ▼
┌──────────────────────────────────────┐     ┌─────────────────────────────────────────────────────┐
│ Headless / Plaintext Consumer        │     │ Terminal Render Pipeline (payloads.rs)              │
│ .to_plain_text()                     │     │ .resolve_to_line(available_width, theme, duration)  │
│                                      │     │                                                     │
│ Output:                              │     │ 1. Fixed Cost: measure "Search ", " in ", " (3ms)"  │
│ Search "draw.rs" in apps/.../telemetry│    │ 2. Available Slack = width - fixed - flex           │
│ (Uncorrupted, full fidelity)         │     │ 3. Inversion: PathView.max_width(slack).to_spans()  │
│                                      │     │                                                     │
│ - Snapshot tests pass                │     │ Output:                                             │
│ - Clipboard copy gets full path      │     │ + Search "draw.rs" in .../overlays/telemetry (3ms)  │
│ - Zero truncation artifacts          │     │ (Dim dirs, bold stem, duration preserved at edge)   │
└──────────────────────────────────────┘     └─────────────────────────────────────────────────────┘
```

### 1. The `SemanticLine` and `InlineSlot` Primitives

We introduce `crate::components::inline_layout` in `mutx`:

```rust
pub enum InlineSlot<'a> {
    /// Incompressible token with fixed visual width.
    /// E.g. "+ ", "Search ", " in ", " (3ms)", "(failed)".
    Fixed(Span<'a>),

    /// Elastic secondary text with optional min/max bounds and truncation priority.
    /// E.g. search query or regex pattern.
    Flexible {
        text: Cow<'a, str>,
        priority: u8,
        min_width: usize,
    },

    /// Adaptive filesystem path component.
    Path(PathView<'a>),
}

pub struct SemanticLine<'a> {
    slots: Vec<InlineSlot<'a>>,
}
```

### 2. Dual-Mode Evaluation (Lossless vs. Constrained)

`SemanticLine` supports two orthogonal evaluation pathways:

1. **`to_plain_text(&self) -> String` (Lossless Model String)**:
   Concatenates all slots without truncation, returning the uncorrupted string representation. Used by snapshot tests, log output, clipboard copy, and headless automation.
2. **`resolve(&self, width: usize, theme: &Theme, suffix: Option<Span<'static>>) -> Line<'static>` (1D Constraint Solver)**:
   Executes a deterministic three-pass layout solver at paint time:
   - **Pass 1 (Fixed Reservation)**: Calculates cumulative columns consumed by all `InlineSlot::Fixed` instances plus the trailing status/duration `suffix`.
   - **Pass 2 (Flexible Negotiation)**: Allocates width to `InlineSlot::Flexible` text according to priority and length.
   - **Pass 3 (Path Projection & Inversion)**: Computes remaining width budget (`slack`) and applies it to `InlineSlot::Path`:
     ```rust
     path_view
         .max_width(slack)
         .strategy(PathFormatStrategy::Adaptive)
         .style(PathStyle::Semantic)
         .to_spans(theme)
     ```
   - **Pass 4 (Assembly)**: Emits a styled Ratatui `Line<'static>` with two-tone semantic hierarchy and guaranteed suffix retention.

### 3. Progressive Presenter Contract Evolution

The `ToolPresenter` trait is enhanced with a non-breaking semantic presentation hook:

```rust
pub trait ToolPresenter {
    /// Legacy flat summary string for headless / test parity.
    fn summary(&self, view: &ToolView) -> String {
        self.render_summary(view).to_plain_text()
    }

    /// Semantic inline layout descriptor evaluated at render time.
    fn render_summary<'a>(&self, view: &'a ToolView) -> SemanticLine<'a> {
        SemanticLine::plain(self.summary(view))
    }

    fn result_kind(&self) -> ResultKind { ResultKind::Code }
    fn arg_layout(&self) -> ArgLayout { ArgLayout::None }
}
```

All standard file-touching presenters (`SearchTextPresenter`, `FindFilesPresenter`, `ListDirPresenter`, `ReadPresenter`, `EditTextPresenter`, `WriteFilePresenter`) are updated to implement `render_summary`.

### 4. Workspace Context Injection

`ToolView` is extended to carry the ambient session workspace root:

```rust
pub struct ToolView<'a> {
    pub name: &'a str,
    pub args: &'a serde_json::Map<String, Value>,
    pub profile: Option<&'a str>,
    pub workspace_root: Option<&'a Path>,
}
```

Presenters construct `PathView` using `.maybe_base_dir(view.workspace_root)`, ensuring absolute paths automatically canonicalize to clean workspace-relative paths before display.

### 5. Cross-View Rendering Alignment

The `PathView` projection pipeline is systematically applied across all result body renderers in `src/disclosure/renderers/payloads.rs`:
- **`draw_matches_content`**: Search match file-group banners construct a `PathView` with `PathStyle::Semantic`, replacing raw string output with two-tone directory/stem highlighting.
- **`draw_listing_content`**: Directory and file listings use `PathView` normalization.
- **Unified Diff Headers**: Diff hunk headers (`--- a/path`, `+++ b/path`) render through `PathView` with extension protection.

---

## Invariants & Behavioral Boundaries

- **`[INV-TUI-PATH-01]` No Premature Stringification**: Presenters and model projections must never flatten filesystem paths, queries, and status badges into an opaque `String` prior to render-time layout resolution.
- **`[INV-TUI-PATH-02]` Metric and Leaf Protection**: Line truncation must never drop trailing duration badges (`(3ms)`), execution statuses (`(failed)`), or filename extensions before exhausting ancestral directory contraction via `PathFormatStrategy::Adaptive`.
- **`[INV-TUI-PATH-03]` Lossless Plaintext Parity**: `SemanticLine::to_plain_text()` must produce an exact, uncorrupted, un-truncated string identical to full path expansion for all non-interactive, clipboard, and test consumers.
- **`[INV-TUI-PATH-04]` Relative Workspace Canonicalization**: When `workspace_root` is present, paths inside the admitted workspace must be projected relative to the workspace root without leaking absolute host filesystem hierarchies.

---

## Alternatives Considered

### 1. Hardcoded Ad-Hoc Character Limits in Presenters

- **Approach**: Set a fixed budget (e.g. `PathView::from_str(path).max_width(36).format_text()`) inside `search.rs` and `read_text.rs`.
- **Rejection Reason**: Violates responsive layout principles. On wide terminal viewports (e.g. 180 columns), it leaves 120 columns completely blank while unnecessarily abbreviating paths. On narrow viewports (e.g. 60 columns or vertical splits), a 36-column path still overflows and pushes the duration badge off-screen.

### 2. Querying Terminal Dimensions Inside Leaf Components

- **Approach**: Have `PathView` directly query `crossterm::terminal::size()` or global layout state.
- **Rejection Reason**: Breaks component isolation and testability. Fails catastrophically when components render inside floating modals, sidebars, or subagent popovers whose allocated widths are a fraction of the terminal window.

### 3. Immediate Deletion of `ToolPresenter::summary(&self) -> String`

- **Approach**: Force all presenters to return `Line<'static>` directly, deleting `summary() -> String`.
- **Rejection Reason**: Breaks hundreds of unit tests, transcript snapshot assertions, and headless automation that verify exact step summary strings without instantiating a Ratatui terminal backend.

---

## Consequences

### Positive
- **Guaranteed Metric Visibility**: The status and duration badge `(3ms)` remains permanently anchored at the visible edge regardless of terminal resize.
- **High Information Density**: The `Adaptive` path solver contracts directories intelligently (`.../overlays/telemetry`) instead of chopping off the filename leaf.
- **Visual Elegance**: Wakes up `PathView`'s two-tone semantic styling (muted directories, prominent stems, highlighted line/column numbers) across both headers and expanded bodies.
- **Universal Reusability**: Provides a standardized 1D inline flex layout mechanism for future components.

### Negative / Neutral
- Presenters declare their summary via `render_summary` using the `SemanticLine` builder.
- Adds an inline layout resolution step in `draw_step_summary`.

---

## References

- ADR-0001: Tool Rendering Redesign (`docs/adr/0001-tool-rendering-redesign.md`)
- ADR-0181: Capability-First Visual Archetypes and Responsive Layout Pipeline (`docs/adr/0181-capability-first-visual-archetypes-and-responsive-layout-pipeline.md`)
- ADR-0205: Stage-Scene-Overlay Architecture (`docs/adr/0205-unified-tui-surface-architecture-and-spatial-modality-taxonomy.md`)
- Path Component Implementation: `apps/tui/crates/mutx/src/components/path.rs`
- Disclosure Payloads Renderer: `apps/tui/crates/mutx/src/disclosure/renderers/payloads.rs`
