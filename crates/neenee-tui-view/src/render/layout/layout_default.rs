//! The default transcript layout: each tool round is grouped into a labelled
//! band with a header row (`round N · model · K calls`), so the history reads
//! as discrete model-request chunks instead of one flush stream.
//!
//! ## Grouping model
//! A "round group" is a maximal run of consecutive assistant-side messages
//! (tool steps, reasoning traces, envoy tasks, assistant text) that share the
//! same `round` stamp and contain at least one tool-like step. User messages
//! and notices are *not* grouped and act as group terminators.
//!
//! Assistant-side components carry a `round: Option<u64>` (1-indexed, stamped
//! from the harness). When a message's round is `None` (legacy sessions
//! predating the stamp), it falls back to ordinary legacy-compatible flow,
//! without a band, so old transcripts stay readable.
//!
//! ## Visual form
//! Each group with a *known* round gets, immediately before its first message,
//! a single-line header:
//!
//! ```text
//! ◆ round 2 · sonnet
//! ```
//!
//! rendered in an info-tone bold for the `◆ round N` anchor and muted for the
//! rest, using foreground color only — no background band. The header is
//! composed from the shared `MetaStrip` component
//! (`render/components/meta_strip.rs`), so this two-tone "anchor · detail"
//! treatment is the same one the sent user-message header uses. This keeps the
//! layout cheap (no per-cell background fill across the group's body, which
//! would require repaint coordination with every drawer) while giving each
//! round a clear, labelled anchor.

use neenee_tui::Rect;

use crate::document::TranscriptMessage;
use crate::render::components::meta_strip::{MetaStrip, MetaTone};
use crate::render::time::sent_time_label;

use super::{
    Stream, TranscriptLayout, default_boundary_gap, default_gap_before, default_group_end,
};

/// Round-banded default layout. See module docs.
pub struct Default;

impl TranscriptLayout for Default {
    fn run(&mut self, stream: &mut Stream<'_, '_>) {
        let messages_len = stream.message_end;
        let mut mi = stream.message_start;

        while mi < messages_len {
            let msg = &stream.messages[mi];

            // ── Detect the start of a round group ───────────────────────────
            // The group planner can start at optional thinking/assistant text,
            // then look forward for the tool step that makes this a tool round.
            if let Some(group_end) = default_group_end(stream.messages, mi) {
                stream.gap(default_gap_before(stream.messages, mi));
                draw_round_header(stream, msg.turn.expect("group has a known turn"), msg);

                for gj in mi..group_end {
                    if gj > mi {
                        stream.gap(default_boundary_gap(
                            &stream.messages[gj - 1],
                            &stream.messages[gj],
                        ));
                    }
                    stream.badge(gj);
                    stream.dispatch(gj);
                }

                mi = group_end;
                continue;
            }

            // ── Non-grouped message: legacy behavior ────────────────────────
            stream.gap(default_gap_before(stream.messages, mi));
            stream.badge(mi);
            stream.dispatch(mi);

            mi += 1;
        }
        stream.finish_virtual();
    }
}

/// Paint the round header row: `◆ round N · model · HH:MM`, info-tone bold
/// anchor with muted metadata, no background band. The header itself is the
/// visual separator and therefore sits flush with the group's first component.
fn draw_round_header(stream: &mut Stream<'_, '_>, round: u64, msg: &TranscriptMessage) {
    // Always account for one content line even when scrolled out of view, so
    // scroll height stays faithful to what a user scrolling back would see.
    stream.content_lines += 1;
    if stream.skip_rows > 0 {
        stream.skip_rows -= 1;
        return;
    }
    if stream.current_y >= stream.viewport_bottom() {
        return;
    }

    let band = stream.band;

    // Two-tone label, no background band: `◆ round N` is the info-tone
    // anchor, the rest (model, send time) reads as muted metadata on the
    // same line. The strip component keeps this treatment shared with sent
    // user-message headers.
    let mut strip = MetaStrip::new()
        .lead("◆ ", MetaTone::Accent)
        .anchor(format!("round {}", round));

    if let Some(name) = msg
        .model
        .as_deref()
        .filter(|m| !m.is_empty())
        .map(crate::providers::model_display_name)
    {
        strip = strip.detail(name);
    }
    if let Some(sent_at_ms) = msg.sent_at_ms {
        strip = strip.detail(sent_time_label(sent_at_ms));
    }

    let rect = Rect::new(band.x, stream.current_y, band.width, 1);
    strip.render(stream.frame, rect, stream.theme);
    stream.current_y += 1;
}
