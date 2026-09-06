# 0181. Capability-First Visual Archetypes, Structural Elevation, and Responsive Layout Pipeline

- **Status:** Accepted
- **Date:** 2026-04-18

## Context

Following the establishment of formal standards-based terminal capability profiles in `mutx-engine` (ADR-0180), the rendering stack cleanly decouples protocol escapes (ITU-T T.416 DirectColor, ECMA-48 Ansi16, and DEC VT100 Monochrome) at the engine driver boundary.

However, the layout and presentation pipeline in `mutx` still suffers from a flawed structural assumption: **it treats responsive layout as purely geometric and orthogonal to terminal capability**.

Currently:
1. **Geometry-First Breakpoint Fallacy:** `LayoutTier` calculates breakpoints (`Wide >= 90`, `Compact < 90`) directly from raw terminal viewport dimensions (`frame.area().width`), entirely oblivious to how components will be visually represented.
2. **Chromatic Elevation Fragility:** Modern terminal UI styling relies heavily on chromatic luminance deltas—subtle background tints such as `theme.surface()`, `theme.panel_bg()`, and `theme.code_bg()`, plus transparent floating overlays. Because background tint carries the semantic boundary, panels consume zero margin, zero padding, and zero border cells.
3. **The Linux VT / Monochrome Collapse:** On constrained display engines—specifically the Linux Virtual Terminal (`/dev/tty1..6`, `TERM=linux`, VGA 16-color console) and DEC VT100 physical/serial terminals (`/dev/ttyS*`, `TERM=vt100` / `dumb` / rescue shells)—background tinting is non-existent or clamped to coarse 8-color solid blocks. In this regime, chromatic elevation fails catastrophically:
   - Panel boundaries disappear; cards and message blocks blend seamlessly into the screen canvas.
   - Contrast between foreground text and background colors collapses, producing unreadable low-contrast or black-on-black text.
   - Floating modals and dialogs lose their z-order hierarchy, rendering the interface an ambiguous, flat plane of text.
4. **The Structural Cost Paradox:** To render hierarchy without background colors, a constrained terminal requires **structural boundaries**: explicit bounding boxes (ASCII `+--+` or CP437 box-drawing lines), visual indentation prefixes (`> `, `[x]`), and DEC VT100 standard reverse video (`SGR 7`). Crucially, **structural boundaries consume physical grid space** (e.g., an explicit panel border consumes 2 columns and 2 rows). If responsive layout runs *before* capability-driven structural scoping, a terminal with 90 physical columns will select a dual-pane `Wide` layout, but the addition of structural borders will compress the inner viewports below usable minimums, causing catastrophic text truncation and layout overflow.

Attempting to fix this via ad-hoc `if is_monochrome()` or `if term == "linux"` checks scattered across dozens of individual view components is an unacceptable anti-pattern that creates spaghetti maintenance, fragile edge cases, and visual regressions.

## Decision Drivers

- **Zero Compromise & Zero Workarounds:** A clean, principled architecture where capability discovery, visual structure, and spatial geometry form a deterministic, unidirectional pipeline.
- **Standards-Anchored Representation:** Full compliance with DEC VT100 / ECMA-48 conventions (reverse video as canonical active focus, ASCII structural scaffolding).
- **Correct Spatial Geometry:** Viewport and responsive layout calculations must account for the spatial footprint required by the active visual archetype.
- **Declarative View Layer:** Individual UI components must never branch on raw terminal types or capability flags; they consume high-level semantic elevation and archetype tokens.

## Decision

We establish an authoritative, **Capability-First Presentation Pipeline** in `mutx` that binds `mutx-engine` terminal profiles directly to visual elevation archetypes and effective layout geometry.

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ 1. Engine Capability Profile (Hardware & Protocol Standard)              │
│    mutx-engine::TerminalProfile { ColorStandard, CharsetStandard, ... }  │
└────────────────────────────────────┬─────────────────────────────────────┘
                                     │
                                     ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ 2. Visual Elevation Archetype & Spatial Cost                             │
│    Archetype: Chromatic (TrueColor) | Structured (Mono/VT) | Hybrid      │
│    SpatialCost: Border (X, Y) + Margin (X, Y) + Indentation              │
└────────────────────────────────────┬─────────────────────────────────────┘
                                     │
                                     ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ 3. Effective Viewport & Responsive Layout Calculation                    │
│    Effective Rect = Raw Viewport Rect - Archetype Spatial Overhead        │
│    LayoutTier::from_effective_width(effective_width) -> Wide | Compact    │
└────────────────────────────────────┬─────────────────────────────────────┘
                                     │
                                     ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ 4. Semantic Container Painting                                           │
│    Draw via ElevationContainer: Tinted Fill OR Explicit Border + Reverse  │
│    Zero ad-hoc capability branches in application view components        │
└────────────────────────────────────┬─────────────────────────────────────┘
                                     │
                                     ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ 5. Grid Diff & Driver Emission                                           │
│    mutx-engine Driver emits sanitized ANSI / VT100 / DEC 2026 sequences  │
└──────────────────────────────────────────────────────────────────────────┘
```

---

### 1. The Three Visual Elevation Archetypes

Visual representation is decoupled into three formal archetypes derived directly from `TerminalProfile`:

1. **`ElevationArchetype::Chromatic` (DirectColor / ITU-T T.416):**
   - **Target:** Modern GUI terminal emulators with 24-bit TrueColor and full UTF-8 support.
   - **Elevation Mechanism:** Continuous luminance delta (`app_bg` -> `surface` -> `panel_bg` -> `code_bg`) and smooth alpha dimming.
   - **Spatial Cost:** `SpatialCost::ZERO` (no forced borders or padding needed to separate surfaces).
   - **Focus Indicator:** Subtle accent hue and continuous cosine luminance breathing.

2. **`ElevationArchetype::Structured` (Monochrome / DEC VT100 & Linux VT / Console):**
   - **Target:** Physical serial consoles (`ttyS*`), recovery environments, Linux virtual consoles (`TERM=linux`), and environments with `NO_COLOR` set.
   - **Elevation Mechanism:** **Pure Structural & Attribute Differentiation**. Background tints are forbidden. Boundaries are established exclusively through:
     - Explicit bounding borders (using `GlyphSet::ascii()` or CP437 single-line box characters).
     - Structural indentation and glyph markers (`>`, `*`, `[-]`).
     - **DEC VT100 Reverse Video (`SGR 7`):** The universal standard for interaction focus, selected list rows, active tabs, and primary buttons.
   - **Spatial Cost:** Consumes 2 columns (left/right borders) and 2 rows (top/bottom borders) per elevated container (`SpatialCost { width: 2, height: 2 }`).
   - **Focus Indicator:** SGR 7 (Reverse Video) and bold styling (`SGR 1`). Spinners use rotating ASCII rods (`|/-\`).

3. **`ElevationArchetype::Hybrid` (ECMA-48 Ansi16):**
   - **Target:** 16-color ANSI terminals with distinct palette contrast.
   - **Elevation Mechanism:** High-contrast 16-color foregrounds combined with single-character line dividers. Background tinting is restricted to high-contrast standard ANSI backgrounds (e.g., dark blue or gray) when supported, falling back to thin borders.
   - **Spatial Cost:** Low-overhead structural separators (`SpatialCost { width: 0, height: 1 }` or full framed borders depending on container density).

---

### 2. Spatial Cost & The Responsive Layout Pipeline

Responsive layout decisions **must never operate on raw terminal viewport geometry**. The pipeline strictly enforces:

```rust
pub struct SpatialCost {
    pub horizontal: u16,
    pub vertical: u16,
}

impl SpatialCost {
    pub const ZERO: Self = Self { horizontal: 0, vertical: 0 };

    pub fn framed() -> Self {
        Self { horizontal: 2, vertical: 2 }
    }
}
```

#### Layout Execution Sequence:

1. **Profile to Archetype Resolution:**
   ```rust
   let profile = TerminalProfile::detect();
   let archetype = ElevationArchetype::for_profile(&profile);
   ```
2. **Container Spatial Deduction:**
   Before computing chunk distributions or responsive breakpoints, any container requiring structural elevation reserves its `SpatialCost`:
   ```rust
   let content_rect = archetype.inner_bounds(viewport_rect);
   ```
3. **Responsive Breakpoint Evaluation:**
   `LayoutTier` is evaluated against `content_rect.width`:
   ```rust
   // Evaluated against effective usable columns, NOT physical terminal columns:
   let tier = LayoutTier::from_effective_width(content_rect.width);
   ```
   *Consequence:* On an 90-column terminal under `ElevationArchetype::Structured`, the 2-column border overhead drops effective width to 88 columns, cleanly triggering `LayoutTier::Compact` (vertical stack). This guarantees that dual-pane master-detail views never suffer clipping on edge-case terminal dimensions.

---

### 3. Declarative `ElevationContainer` Widget

To eliminate ad-hoc capability branching, all elevated panels (Composer, Tool Cards, Command Disclosures, Modals, Overlays) render through a unified declarative primitive:

```rust
pub struct ElevationContainer<'a> {
    title: Option<&'a str>,
    focused: bool,
    level: ElevationLevel,
}

impl<'a> ElevationContainer<'a> {
    pub fn render(self, area: Rect, frame: &mut Frame, theme: &Theme, archetype: ElevationArchetype) -> Rect {
        match archetype {
            ElevationArchetype::Chromatic => {
                // Paint background fill without border overhead
                frame.render_widget(Block::default().style(theme.elevation_bg(self.level)), area);
                area
            }
            ElevationArchetype::Structured => {
                // Paint explicit ASCII / CP437 border + SGR 7 focus
                let mut block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(theme.glyphs.border_type());
                
                if self.focused {
                    block = block.style(Style::default().add_modifier(Modifier::REVERSED));
                }
                
                let inner = block.inner(area);
                frame.render_widget(block, area);
                inner
            }
            ElevationArchetype::Hybrid => {
                // Paint high-contrast divider / thin border
                ...
            }
        }
    }
}
```

Application views simply invoke `container.render(...)` and receive the exact, guaranteed-safe inner content area to populate with sub-components.

---

### 4. Semantic Focus & Interaction Redirection

Under `ElevationArchetype::Structured` and `ElevationArchetype::Hybrid`:
1. **Modal Backdrops:** Traditional TrueColor backdrop dimming (`alpha_dim`) is disabled. Backdrops on monochrome/VT terminals are rendered as an ASCII stipple pattern (e.g., alternating `.` and ` `) or cleared to base cells, surrounded by a heavy double-struck border to assert window hierarchy.
2. **Interactive Items & Buttons:** Instead of subtle hover background fills, interactive focus transitions into **Reverse Video (`SGR 7`)** with bracket delimiters (`[ OK ]` vs `  Cancel  `).

---

## Considered Options

### Option 1: Capability-First Pipeline with Strict Spatial Insets (Chosen)
Tie terminal capability directly to visual elevation archetypes, subtract structural spatial costs prior to responsive layout calculation, and encapsulate rendering in declarative elevation primitives.
- **Result:** Complete architectural purity, zero clipping or overflow in constrained environments, and zero ad-hoc capability branching in view code.

### Option 2: Post-Layout Degradation in Engine Driver
Run existing layout assuming TrueColor, and have `mutx-engine` drivers inject borders or draw outlines during rasterization.
- **Rejected:** The engine rasterizer has no semantic understanding of UI containers or cards. Injecting borders at the diff engine level would corrupt coordinates and destroy text wrapping.

### Option 3: Ad-Hoc Widget-Level Branching
Allow individual widgets to inspect `profile` and conditionally draw borders or alternate backgrounds.
- **Rejected:** Creates rampant code duplication across hundreds of views, inconsistent spatial calculations, and inevitable clipping regressions on 80/90-column boundaries.

---

## Consequences

### Positive
- **Rock-Solid Visual Hierarchy in Linux VT & Serial Consoles:** Panels, modals, and tool steps in emergency recovery shells and Linux virtual consoles gain crisp, unambiguous visual separation via explicit borders and DEC VT100 reverse video.
- **Mathematically Immune to Breakpoint Clipping:** Deducting spatial framing costs before evaluating `LayoutTier` ensures that dual-pane and compact transitions are physically accurate across all capability profiles.
- **Clean Component Code:** View code remains purely declarative; individual components never contain terminal type heuristics or escape sequence hacks.
- **Full Backward and Forward Compatibility:** Seamlessly scales from physical VT100 hardware terminals to high-DPI modern graphical emulators.

### Negative / Trade-Offs
- On constrained terminals, elevated panels lose 2 columns and 2 rows to structural borders, slightly reducing total transcript line capacity. This is an intentional, strictly necessary trade-off for visual legibility.

---

## References

- ADR-0180: *Standards-Based Terminal Capability Profiles and Adaptive Degradation*
- ADR-0097: *Session Addressing and Orchestrator Console (2-Tier Responsive Layout)*
- ADR-0038: *In-House Grid Diff Rendering Engine*
- DEC VT100 User Guide (EK-VT100-UG-003): Reverse Video and Character Display Standards
- ECMA-48: *Control Functions for Coded Character Sets*
