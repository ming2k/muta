//! The original (legacy) transcript layout: messages flush against each other
//! with single-row gaps, and adjacent collapsed tool steps stack with no gap at
//! all.
//!
//! This is a verbatim extraction of the message loop that lived in
//! `draw_transcript` before the `layout` split. Behavior is byte-for-byte
//! identical to the pre-refactor renderer.

use super::{Stream, TranscriptLayout};

/// Original flush-stack layout. See the module docs.
pub struct Legacy;

impl TranscriptLayout for Legacy {
    fn run(&mut self, stream: &mut Stream<'_, '_>) {
        let messages_len = stream.message_end;
        for mi in stream.message_start..messages_len {
            let msg = &stream.messages[mi];

            // Per-turn label hook (currently a no-op — the model attribution
            // badge was removed). Kept so a future per-turn label lands here.
            stream.badge(mi);

            // Per-kind drawer (height-cache fast path included).
            stream.dispatch(mi);

            // ── Inter-message spacing ───────────────────────────────────────
            // A user message's panel ends with a full panel-bg padding row.
            // That row reads as part of the panel, not as open space, so the
            // message still needs one blank row of `surface` before the next
            // component to keep the same visual separation the old half-block
            // `▀` transition provided (its bottom half was app_bg). The
            // exception is when the next message is a step (thinking or tool
            // step): a blank row between the user panel's edge and the step
            // header keeps the two visually distinct.
            //
            // Collapsed tool steps stack flush: a batch of parallel/sequential
            // collapsed tool-call headers forms a compact log block with no
            // blank rows between them. The separating row is supplied *only*
            // by an expanded step's body.
            let next = stream.messages.get(mi + 1);
            let next_is_tool_step = next.is_some_and(|n| n.is_tool_step() || n.is_envoy_task());
            let collapsed_tool_into_tool_step =
                msg.is_tool_step() && msg.tool_step_expanded() == Some(false) && next_is_tool_step;
            if collapsed_tool_into_tool_step {
                // Flush stack: no separating row.
            } else if msg.role == neenee_core::Role::User || next.is_some() {
                stream.message_gap();
            }
        }
        stream.finish_virtual();
    }
}
