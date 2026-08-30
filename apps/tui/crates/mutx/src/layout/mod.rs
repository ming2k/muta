//! Pluggable transcript layout strategies.
//!
//! `draw_transcript` owns the *frame* — background, viewport carving, footer
//! chrome, sticky pinning — but the actual *arrangement* of messages is
//! delegated here. Each strategy implements [`TranscriptLayout`] and receives a
//! mutable [`Stream`] carrying every piece of shared render state.
//!
//! # The `Stream` contract
//! A layout walks `messages` in order and, for each message, calls the shared
//! helpers on `Stream`:
//!   - [`Stream::badge`]   — the model attribution badge above an assistant turn;
//!   - [`Stream::dispatch`] — the per-kind drawer (notice / tool step / reasoning
//!     trace / message body), including the height-cache fast path;
//!   - [`Stream::gap`]     — insert `n` blank rows of inter-message spacing.
//!
//! These three helpers are the *only* sanctioned mutations of `current_y` /
//! `skip_rows` / `content_lines`, so every layout agrees on scroll accounting
//! and height-cache semantics. A layout is free to add its own chrome (turn
//! headers, background bands, …) via the raw paint primitives, but the message
//! body itself always flows through `dispatch`.
//!
//! # Strategies
//! - [`turn_band::TurnBand`] — each tool-bearing ReAct turn is grouped
//!   under a labelled header (`◆ turn N · model`) and uses semantic boundary spacing. The
//!   default.
//!
//! New strategies are added by implementing the trait and wiring a match arm
//! in [`Strategy::build`].

pub mod turn_band;

use mutx_engine::flex::{Flex, FlexItem};
use mutx_engine::{Frame, Rect};

use crate::model::document::TranscriptMessage;
use crate::model::layout::{InteractiveTarget, LayoutMap};
use crate::model::selection::{CellDragInfo, SelectionState};

use super::HeightCache;
use super::disclosure::StickyStep;
use super::theme::Theme;
use crate::design::{
    MESSAGE_GAP_ROWS, STREAM_BOTTOM_GAP_ROWS, STREAM_TOP_GAP_ROWS, TURN_HEADER_BODY_GAP_ROWS,
};
use crate::disclosure::renderers::RenderCtx;

/// Which layout strategy to use for the transcript message stream.
///
/// Selectable via `[tui] transcript_layout` in `config.toml`; the default is
/// [`Strategy::TurnBand`], which groups stamped ReAct turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strategy {
    #[default]
    TurnBand,
}

impl Strategy {
    /// Parse a `config.toml` value into a strategy, case-insensitively.
    /// Unknown / empty values fall back to the default rather than
    /// erroring, so a typo never blocks startup.
    pub fn from_config(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "turn_band" | "turn-band" | "turnband" | "default" | "compact" | "flush" | "legacy"
            | "" => Self::TurnBand,
            _ => Self::TurnBand,
        }
    }

    /// The canonical configuration string for this strategy.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TurnBand => "turn_band",
        }
    }

    /// Construct the concrete layout for this strategy.
    pub fn build(self) -> Box<dyn TranscriptLayout> {
        match self {
            Self::TurnBand => Box::new(turn_band::TurnBand),
        }
    }
}

/// Cached line geometry for a settled transcript. It lets the renderer locate
/// the chunks intersecting a viewport with binary search, then asks the layout
/// strategy to draw only those chunks. The index is intentionally discarded on
/// any transcript/width change by [`super::HeightCache`], so it never guesses
/// about mutable live output.
#[derive(Clone)]
pub struct VirtualLayoutIndex {
    strategy: Strategy,
    source_ptr: usize,
    source_len: usize,
    chunks: Vec<VirtualChunk>,
    total_lines: usize,
}

#[derive(Clone)]
struct VirtualChunk {
    message_start: usize,
    message_end: usize,
    start_line: usize,
    end_line: usize,
}

#[derive(Clone, Copy)]
pub struct VirtualWindow {
    pub message_start: usize,
    pub message_end: usize,
    pub prefix_lines: usize,
    pub skip_rows: usize,
    pub total_lines: usize,
}

impl VirtualLayoutIndex {
    pub fn matches(&self, messages: &[TranscriptMessage], strategy: Strategy) -> bool {
        self.strategy == strategy
            && self.source_ptr == messages.as_ptr() as usize
            && self.source_len == messages.len()
    }

    pub fn window(&self, scroll: usize, view_height: u16) -> Option<VirtualWindow> {
        if self.chunks.is_empty() {
            return None;
        }
        let start = self
            .chunks
            .partition_point(|chunk| chunk.end_line <= scroll);
        let start = start.min(self.chunks.len().saturating_sub(1));
        let viewport_end = scroll.saturating_add(view_height as usize).max(scroll + 1);
        let mut end = self
            .chunks
            .partition_point(|chunk| chunk.start_line < viewport_end);
        end = end.max(start + 1).min(self.chunks.len());
        let first = &self.chunks[start];
        let last = &self.chunks[end - 1];
        Some(VirtualWindow {
            message_start: first.message_start,
            message_end: last.message_end,
            prefix_lines: first.start_line,
            skip_rows: scroll.saturating_sub(first.start_line),
            total_lines: self.total_lines,
        })
    }
}

/// Build an exact index only once every message body has a stable cached
/// height. While a live tail is still streaming the caller naturally falls
/// back to the cache-only path; once that tail settles, the next draw upgrades
/// to a fully virtualized transcript.
///
/// Geometry comes from the engine's flex solver (ADR-0114): the chunks are
/// declared as a single vertical flex pass — `FlexItem::fixed(chunk_height)`
/// per chunk, `Flex::column()` with no gap (inter-chunk spacing is already
/// part of each chunk's height via `default_gap_before`) — and the solver
/// yields every chunk's exact main-axis offset. The hand-rolled accumulation
/// loop this replaces could drift from the painting path's cursor arithmetic;
/// sharing one solver keeps the virtual index and the paint pass in lockstep
/// by construction.
pub fn build_virtual_index(
    messages: &[TranscriptMessage],
    cache: &HeightCache,
    strategy: Strategy,
) -> Option<VirtualLayoutIndex> {
    if messages.is_empty() {
        return None;
    }
    // Chunk planning is strategy-specific; heights resolve through the cache.
    let plans: Vec<VirtualChunkPlan> = match strategy {
        Strategy::TurnBand => plan_turn_band(messages, cache)?,
    };
    if plans.is_empty() {
        return None;
    }

    // Single flex solve for the whole transcript's chunk geometry. The
    // container's main axis is intentionally unbounded (usize::MAX): the
    // transcript can exceed any u16 row count, so there must be no shrinking
    // and no clamping — every chunk keeps its exact planned height.
    let items: Vec<FlexItem> = plans
        .iter()
        .map(|p| {
            // A single chunk taller than 65535 rows is pathological (a whole
            // turn group spanning 65k+ lines); clamp rather than silently
            // truncate. Totals stay usize-exact via `used_main`.
            FlexItem::fixed(u16::try_from(p.height).unwrap_or(u16::MAX))
        })
        .collect();
    let solved = Flex::column().solve_with(Rect::new(0, 0, 0, u16::MAX), &items, &|_, _| 0);

    let chunks: Vec<VirtualChunk> = plans
        .iter()
        .enumerate()
        .map(|(i, p)| VirtualChunk {
            message_start: p.message_start,
            message_end: p.message_end,
            start_line: solved.main_offset(i),
            end_line: solved.main_offset(i) + solved.main_exact(i),
        })
        .collect();
    let total_lines = solved.used_main;
    debug_assert_eq!(
        total_lines,
        chunks.last().map(|c| c.end_line).unwrap_or(0),
        "flex solve must reproduce the exact chunk extents"
    );

    Some(VirtualLayoutIndex {
        strategy,
        source_ptr: messages.as_ptr() as usize,
        source_len: messages.len(),
        chunks,
        total_lines,
    })
}

/// A planned chunk before geometry: message range + resolved height. Heights
/// resolve to `None` (aborting the whole index) whenever any message lacks a
/// cached height, preserving the "virtualize only fully settled transcripts"
/// invariant.
struct VirtualChunkPlan {
    message_start: usize,
    message_end: usize,
    height: usize,
}

fn plan_turn_band(
    messages: &[TranscriptMessage],
    cache: &HeightCache,
) -> Option<Vec<VirtualChunkPlan>> {
    let mut plans = Vec::new();
    let mut index = 0usize;
    while index < messages.len() {
        let start = index;
        let mut height = default_gap_before(messages, index);
        if let Some(end) = default_group_end(messages, index) {
            height += 1 + TURN_HEADER_BODY_GAP_ROWS;
            for (offset, message) in messages[index..end].iter().enumerate() {
                if offset > 0 {
                    height += default_boundary_gap(&messages[index + offset - 1], message);
                }
                height += cached_height(cache, message)?;
            }
            index = end;
        } else {
            height += cached_height(cache, &messages[index])?;
            index += 1;
        }
        if index == messages.len() {
            height += STREAM_BOTTOM_GAP_ROWS;
        }
        plans.push(VirtualChunkPlan {
            message_start: start,
            message_end: index,
            height,
        });
    }
    Some(plans)
}

fn cached_height(cache: &HeightCache, message: &TranscriptMessage) -> Option<usize> {
    cache.get(message.id).map(usize::from)
}

/// Whether a message participates in an assistant model-request group. A group
/// is only promoted to a visible turn band when its run contains a tool-like
/// step; final prose-only responses retain the ordinary transcript shape.
fn is_turn_component(message: &TranscriptMessage) -> bool {
    message.is_tool_step()
        || message.is_runner_task()
        || message.is_thinking()
        || message.role == muta_contracts::Role::Assistant
}

fn is_tool_like(message: &TranscriptMessage) -> bool {
    message.is_tool_step() || message.is_runner_task()
}

fn default_group_start(messages: &[TranscriptMessage], index: usize) -> bool {
    let message = &messages[index];
    if message.turn.is_none() || !is_turn_component(message) {
        return false;
    }
    if index == 0 {
        return true;
    }
    let previous = &messages[index - 1];
    !is_turn_component(previous) || previous.round != message.round || previous.turn != message.turn
}

/// Return the exclusive end of the turn group beginning at `start`.
///
/// Thinking can be the first component in a tool-producing model request, so
/// group discovery starts from any stamped assistant component and looks
/// forward for a tool-like step. This makes the presence or absence of optional
/// thinking content irrelevant to the group's outer geometry.
pub(super) fn default_group_end(messages: &[TranscriptMessage], start: usize) -> Option<usize> {
    if !default_group_start(messages, start) {
        return None;
    }
    let position = (messages[start].round, messages[start].turn);
    let mut end = start;
    while end < messages.len() {
        let message = &messages[end];
        if (message.round, message.turn) != position || !is_turn_component(message) {
            break;
        }
        end += 1;
    }
    Some(end)
}

/// Resolve exactly one blank-row decision for a pair of adjacent transcript
/// components. A same-turn tool batch is the only zero-gap relationship;
/// thinking, prose, and tool batches remain distinct visual segments. Tool
/// disclosure state never changes the boundary. Unknown legacy tool steps
/// retain the former collapsed-stack fallback because old sessions have no
/// structural stamp.
pub(super) fn default_boundary_gap(
    previous: &TranscriptMessage,
    next: &TranscriptMessage,
) -> usize {
    let known_same_tool_batch = is_tool_like(previous)
        && is_tool_like(next)
        && previous.turn.is_some()
        && previous.round == next.round
        && previous.turn == next.turn;
    let legacy_collapsed_tool_batch = previous.turn.is_none()
        && next.turn.is_none()
        && previous.is_tool_step()
        && previous.tool_step_expanded() == Some(false)
        && is_tool_like(next);

    if known_same_tool_batch || legacy_collapsed_tool_batch {
        0
    } else {
        MESSAGE_GAP_ROWS
    }
}

/// Boundary space before an item/chunk. The first message (index 0) carries the
/// stream top scroll padding ([`STREAM_TOP_GAP_ROWS`]); subsequent messages consume
/// the boundary gap rule between consecutive messages.
pub(super) fn default_gap_before(messages: &[TranscriptMessage], index: usize) -> usize {
    if index == 0 {
        STREAM_TOP_GAP_ROWS
    } else {
        default_boundary_gap(&messages[index - 1], &messages[index])
    }
}

/// The shared render context handed to a layout. Owns the mutable scroll/Y
/// state and the references a layout needs to paint.
///
/// Field visibility is `(pub)` to layouts in this module. `draw_transcript`
/// constructs this once and hands it to `layout.run(&mut stream)`; layouts do
/// not construct it themselves.
///
/// Two lifetime parameters keep variance sane: `'a` is the borrow lifetime of
/// every shared reference (`messages`, `theme`, `layout_map`, …); `'f` is the
/// independent lifetime of the `Frame`'s internal buffer. `Frame` is invariant
/// over its parameter, so unifying `'a` with the frame's lifetime would infect
/// every other field with invariance and trap short-lived locals (like the
/// fallback height cache) in `draw_transcript`.
pub struct Stream<'a, 'f> {
    pub frame: &'a mut Frame<'f>,
    /// The already-inset transcript band every message body renders into.
    pub band: Rect,
    pub messages: &'a [TranscriptMessage],
    pub theme: &'a Theme,
    pub layout_map: &'a mut LayoutMap,
    pub height_cache: &'a mut HeightCache,
    pub selection: &'a SelectionState,
    pub cell_selection: Option<&'a CellDragInfo>,
    pub hovered_step: Option<usize>,
    pub focused_target: Option<InteractiveTarget>,
    /// First / exclusive-last message selected by a [`VirtualLayoutIndex`].
    /// The normal path covers the full slice.
    pub message_start: usize,
    pub message_end: usize,
    /// Exact total stream height from the virtual index. Layout strategies set
    /// this after painting the selected window, avoiding a trailing walk just
    /// to rediscover the scroll extent.
    pub virtual_total_lines: Option<usize>,

    // ── mutable scroll / Y accounting ──────────────────────────────────────
    pub current_y: u16,
    pub skip_rows: usize,
    /// Total stream height (un-clipped by the viewport).
    pub content_lines: usize,

    // ── accumulators consumed by `draw_transcript` after the layout returns ─
    pub sticky_steps: Vec<StickyStep>,
}

impl<'a, 'f> Stream<'a, 'f> {
    /// No-op. The per-turn model attribution badge (`provider · model`) was
    /// removed — the turn-band header already labels the producing model and
    /// the compact layout needs no per-turn heading. Layouts still call this
    /// unconditionally at the top of each message; keeping the call site means
    /// a future per-turn label can be reintroduced in one place.
    pub fn badge(&mut self, _mi: usize) {}

    /// Dispatch a single message to its per-kind drawer, honoring the
    /// height-cache fast path for every settled message. Running tool/runner/
    /// reasoning steps retain their live renderer because their visible height
    /// can still change; completed expanded steps are safe to cache and can be
    /// skipped wholesale when fully off-screen.
    /// `content_lines` is advanced by the message's true height; `current_y`
    /// stops advancing once it reaches the viewport bottom.
    pub fn dispatch(&mut self, mi: usize) {
        let msg = &self.messages[mi];
        let viewport_bottom = self.band.y + self.band.height;

        // Snapshot the interaction state before the cursor borrow: once the
        // RenderCtx holds &mut frame/layout_map/counters, only ctx fields move.
        let hovered = self.hovered_step == Some(mi);
        let focused_tool = self.focused_target == Some(InteractiveTarget::tool_step(mi));
        let focused_thinking = self.focused_target == Some(InteractiveTarget::thinking(mi));
        let focused_command = self.focused_target == Some(InteractiveTarget::command_result(mi));
        let _focused_notice = self.focused_target == Some(InteractiveTarget::notice(mi));

        let body_before = self.content_lines;
        let skippable = (msg.is_notice() && !msg.is_provider_retry())
            || (!msg.is_runner_task()
                && if msg.is_tool_step() {
                    !msg.tool_step_status()
                        .is_some_and(|status| status.is_running())
                } else if msg.is_thinking() {
                    !msg.is_thinking_streaming()
                } else if msg.is_provider_retry() {
                    false
                } else {
                    true
                });
        let cached_height = if skippable {
            self.height_cache.get_with_rev(msg.id, msg.rev)
        } else {
            None
        };
        let fully_above = cached_height.is_some_and(|h| (h as usize) <= self.skip_rows);
        let fully_below = self.current_y >= viewport_bottom;

        if let Some(h) = cached_height.filter(|_| fully_above || fully_below) {
            // Reproduce exactly the counter mutations a fully-clipped body draw
            // would make, minus the wrapping work.
            self.content_lines += h as usize;
            if fully_above {
                self.skip_rows -= h as usize;
            }
        } else if msg.is_notice() {
            super::draw_notice(
                self.frame,
                self.band,
                msg,
                mi,
                self.layout_map,
                &mut self.skip_rows,
                &mut self.current_y,
                &mut self.content_lines,
                self.theme,
                self.hovered_step == Some(mi),
                self.focused_target == Some(InteractiveTarget::notice(mi)),
            );
        } else if msg.is_runner_task() {
            let mut ctx = RenderCtx::from_cursor(
                self.frame,
                self.band,
                self.band.width as usize,
                self.theme,
                self.layout_map,
                &mut self.skip_rows,
                &mut self.current_y,
                &mut self.content_lines,
            );
            super::disclosure::draw_runner_inline_step(&mut ctx, msg, mi, hovered, focused_tool);
        } else if msg.is_tool_step() {
            let mut ctx = RenderCtx::from_cursor(
                self.frame,
                self.band,
                self.band.width as usize,
                self.theme,
                self.layout_map,
                &mut self.skip_rows,
                &mut self.current_y,
                &mut self.content_lines,
            );
            super::disclosure::draw_tool_step(
                &mut ctx,
                msg,
                mi,
                self.selection,
                self.cell_selection,
                &mut self.height_cache.diff_cache,
                &mut self.sticky_steps,
                hovered,
                focused_tool,
            );
        } else if msg.is_thinking() {
            let mut ctx = RenderCtx::from_cursor(
                self.frame,
                self.band,
                self.band.width as usize,
                self.theme,
                self.layout_map,
                &mut self.skip_rows,
                &mut self.current_y,
                &mut self.content_lines,
            );
            super::disclosure::draw_reasoning_trace(
                &mut ctx,
                msg,
                mi,
                self.selection,
                self.cell_selection,
                &mut self.sticky_steps,
                hovered,
                focused_thinking,
            );
        } else if msg.is_command_result() {
            let mut ctx = RenderCtx::from_cursor(
                self.frame,
                self.band,
                self.band.width as usize,
                self.theme,
                self.layout_map,
                &mut self.skip_rows,
                &mut self.current_y,
                &mut self.content_lines,
            );
            super::disclosure::draw_command_result(
                &mut ctx,
                msg,
                mi,
                self.selection,
                self.cell_selection,
                hovered,
                focused_command,
            );
        } else {
            super::draw_message_body(
                self.frame,
                self.band,
                msg,
                mi,
                self.selection,
                self.cell_selection,
                self.theme,
                self.layout_map,
                &mut self.skip_rows,
                &mut self.current_y,
                &mut self.content_lines,
                true,
            );
        }

        // Cache the freshly-measured height for skippable kinds only.
        if skippable && cached_height.is_none() {
            self.height_cache
                .set_with_rev(msg.id, msg.rev, (self.content_lines - body_before) as u16);
        }
    }

    /// Insert `n` blank rows of inter-message spacing. Consumes `skip_rows`
    /// while still above the viewport, and stops advancing `current_y` at the
    /// viewport bottom. `content_lines` always counts the full height.
    pub fn gap(&mut self, n: usize) {
        self.content_lines += n;
        if self.skip_rows > 0 {
            self.skip_rows = self.skip_rows.saturating_sub(n);
        } else if self.current_y < self.band.y + self.band.height {
            self.current_y = self.current_y.saturating_add(n as u16);
        }
    }

    /// Convenience: one standard inter-message blank row (`MESSAGE_GAP_ROWS`).
    pub fn message_gap(&mut self) {
        self.gap(MESSAGE_GAP_ROWS);
    }

    /// The viewport's bottom y (exclusive). Layouts use it to decide whether a
    /// chrome row (turn header) is on-screen before painting it.
    pub fn viewport_bottom(&self) -> u16 {
        self.band.y + self.band.height
    }

    /// Complete a virtualized pass after the selected chunks have been drawn.
    pub fn finish_virtual(&mut self) {
        if let Some(total) = self.virtual_total_lines {
            self.content_lines = total;
        }
    }
}

/// A transcript layout strategy. Implementations walk `messages` via the
/// [`Stream`] helpers and return, leaving `content_lines` / `sticky_steps` /
/// `last_shown_attribution` populated for `draw_transcript`'s post-processing.
pub trait TranscriptLayout {
    fn run(&mut self, stream: &mut Stream<'_, '_>);
}

#[cfg(test)]
mod tests {
    use muta_contracts::Role;

    use super::*;

    #[test]
    fn default_spacing_compacts_only_same_turn_tool_batches() {
        let thinking = TranscriptMessage::thinking("reasoning").with_turn(4);
        let mut tool = TranscriptMessage::tool_step("call", "read_text", "{}").with_turn(4);
        tool.set_tool_step_expanded(true);
        let next_tool = TranscriptMessage::tool_step("next", "search_text", "{}").with_turn(4);
        let text = TranscriptMessage::new(Role::Assistant, "answer").with_turn(4);
        let next_round = TranscriptMessage::new(Role::Assistant, "next").with_turn(5);

        assert_eq!(default_boundary_gap(&thinking, &tool), MESSAGE_GAP_ROWS);
        assert_eq!(default_boundary_gap(&tool, &next_tool), 0);
        assert_eq!(default_boundary_gap(&tool, &text), MESSAGE_GAP_ROWS);
        assert_eq!(default_boundary_gap(&text, &next_round), MESSAGE_GAP_ROWS);
    }
}
