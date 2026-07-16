//! Non-color design tokens: spacing, gutters, fixed row counts, and text
//! measurement limits shared by renderer components.

/// Uniform horizontal inset applied to transcript-area components so bands,
/// bars, and text do not touch the terminal frame.
pub(super) const TRANSCRIPT_H_INSET: u16 = 2;

/// Extra leading whitespace applied to prose after the transcript-area gutter.
/// Now that the horizontal gutter is applied once at the stream entry point,
/// this is the *only* indent prose-like content adds on top of the already-
/// inset rect.
pub(super) const TRANSCRIPT_BODY_LEADING_INDENT: u16 = 2;

/// Minimum readable width for compact expandable step header rows.
pub(super) const STEP_MIN_WIDTH: usize = 8;

/// Inner indent for expanded tool-step body content. The transcript band already
/// carries the outer gutter; this indent aligns label-free tool details with
/// prose-like body content inside that band.
pub(super) const TOOL_STEP_BODY_INDENT_COLS: usize = 2;

/// One blank row inserted at semantic transcript, round, or component-segment
/// boundaries. Consecutive tool-like components in one known round are the
/// compact exception and use zero rows.
pub(super) const MESSAGE_GAP_ROWS: usize = 1;

/// Separation between a round metadata header and its first component. The
/// header labels the group but is not part of the component stack, so one row
/// preserves the hierarchy without giving every child its own top margin.
pub(super) const ROUND_HEADER_BODY_GAP_ROWS: usize = 1;

/// Vertical chrome rows around a sent user message panel: one top transition
/// row and one bottom transition row.
pub(super) const USER_MESSAGE_TRANSITION_ROWS: usize = 1;

/// Vertical gap between an expanded tool step's header and its own body. Held
/// to **0**: a tool step is a flat *log entry* (no band, no Tool/Arguments/
/// Result labels — see `draw_tool_step`), so the body's grouping under its
/// header is carried by a **single** signal — the indent
/// (`TOOL_STEP_BODY_INDENT_COLS`). A blank row here would be a *panel/card*
/// affordance left over from the old banded shape; it competes with the indent
/// rather than reinforcing it (the row says "two separate blocks", the indent
/// says "this is the header's content"). Removing it lets the indent own the
/// grouping and keeps an expanded step as tight as a collapsed batch.
///
/// The token is kept at 0 rather than deleted so the decision is *visible* in
/// code: the absence of a top gap is a deliberate choice, not an oversight,
/// and the one place that would want to re-introduce it (`draw_tool_step`)
/// reads the named token.
///
/// The layout closes the body only when the next component crosses a semantic
/// round/message boundary. There is no dedicated bottom-gap token: an extra
/// one would break the flush same-round tool batch.
pub(super) const TOOL_STEP_BODY_TOP_GAP_ROWS: usize = 0;
pub(super) const TOOL_STEP_SECTION_GAP_ROWS: usize = 1;
pub(super) const TOOL_STEP_CHILDREN_GAP_ROWS: usize = TOOL_STEP_SECTION_GAP_ROWS;

/// Spacing inside expanded reasoning traces. The first body line sits directly
/// below the disclosure header; later reasoning blocks retain one row of
/// separation. These stay independent from tool-step spacing because reasoning
/// is prose-like, not a panel.
/// There is no bottom-gap token: the layout resolves the following semantic
/// component boundary.
pub(super) const REASONING_TRACE_BODY_TOP_GAP_ROWS: usize = 0;
pub(super) const REASONING_TRACE_BLOCK_GAP_ROWS: usize = 1;

/// Hint bar: a single-line status strip pinned directly below the input box
/// that surfaces the next input action plus ambient model/context info. Always
/// one row tall when visible (hidden only while an overlay modal replaces the
/// chrome).
pub(super) const HINT_BAR_ROWS: u16 = 1;
/// Internal left indent of hint-bar content, matching the composer's prompt
/// prefix feel.
pub(super) const HINT_BAR_INNER_PADDING: usize = 1;
/// Minimum gap between the left input-action cluster and the right-aligned
/// model/context cluster.
pub(super) const HINT_BAR_GAP_MIN: usize = 2;
/// Gap between adjacent right-aligned hint segments.
pub(super) const HINT_BAR_SEGMENT_GAP: usize = 2;

pub(super) const STATUS_BAR_ROWS: u16 = 1;
/// State bar: a single-line strip for persistent session-state indicators
/// (unattended mode today; workspace and other ambient state later). Always
/// one row tall when visible; the caller allocates zero rows when no
/// indicator is active.
pub(super) const STATE_BAR_ROWS: u16 = 1;
/// Permanent breathing room between the transcript and footer chrome. Keeping
/// this row even while the activity bar is idle prevents the latest response
/// from visually running into the composer when the active row appears or
/// disappears.
pub(super) const FOOTER_TOP_GAP_ROWS: u16 = 1;
/// Height of the contextual header shown on every transcript page other than
/// Main (`/btw`, Envoy, and future focused pages).
pub(super) const PAGE_HEADER_ROWS: u16 = 1;

/// Horizontal inset applied to the footer area containing status/composer/hints.
pub(super) const FOOTER_H_INSET: u16 = TRANSCRIPT_H_INSET;

/// Composer chrome consists of one top and one bottom padding row.
pub(super) const COMPOSER_VERTICAL_CHROME_ROWS: u16 = 2;
pub(super) const COMPOSER_MIN_HEIGHT: u16 = 3;
pub(super) const COMPOSER_MAX_HEIGHT_DIVISOR: u16 = 2;
/// Columns reserved before the composer text: a `>` prompt glyph plus a space
/// on the first wrapped line, matched by a two-space indent on every wrapped
/// continuation line so the caret stays aligned.
pub(super) const COMPOSER_PROMPT_PREFIX_COLS: usize = 2;
pub(super) const COMPOSER_TEXT_ROW_OFFSET: u16 = 1;

/// User message panels mirror the composer: outer gutter, gap, text, then
/// User message panels used to reserve their own outer gutter matching
/// [`TRANSCRIPT_H_INSET`]. Now that the horizontal inset is applied once at
/// the stream entry point (`draw_transcript` → `band`), the outer gutter is
/// redundant: the `band` rect already excludes it. Set to 0 so the panel
/// background starts at the band edge, with only the inner text gap remaining.
pub(super) const USER_MESSAGE_OUTER_GUTTER_COLS: usize = 0;
/// Inner left padding (in `user_panel_bg`) between the outer gutter and the
/// text. Matches the composer's prompt prefix so sent messages and the input
/// box share the same left margin.
pub(super) const USER_MESSAGE_TEXT_GAP_COLS: usize = 2;
/// Inner right padding (in `user_panel_bg`) kept clear of wrapped text so a
/// sent message never runs its text into the panel's right edge.
pub(super) const USER_MESSAGE_RIGHT_PAD_COLS: usize = 2;

/// Inner right padding (in `input_bg`) kept clear of wrapped text inside the
/// composer, mirroring the left prompt prefix so the box reads as a balanced
/// panel.
pub(super) const COMPOSER_RIGHT_PAD_COLS: usize = 2;

// ── Modal overlays ───────────────────────────────────────────────────────
// Every centered modal (Activity, Sessions, Provider, Help, …) goes through
// `modal_frame`, which paints a borderless solid-bg panel and splits it into
// header / body / footer. These tokens are the single source of truth for
// spacing *inside* that panel, so every modal indents its content the same
// way instead of hard-coding whitespace per file.

/// Left/right padding between the panel edge and the header/body/footer.
/// Applied once by `modal_frame` via `Margin { horizontal, .. }`; section
/// content never adds its own outer gutter on top of this. Includes room for
/// the scrollbar track (1 col) plus `SCROLLBAR_GAP` (1 col) on the right.
pub(super) const MODAL_INNER_H_PADDING: u16 = 3;

/// Empty columns between the body text's right edge and the scrollbar track.
pub(super) const SCROLLBAR_GAP: u16 = 1;

/// Top/bottom padding between the panel edge and the header/body/footer.
/// Applied once by `modal_frame` via `Margin { vertical, .. }`.
pub(super) const MODAL_INNER_V_PADDING: u16 = 1;

/// Leading indent for body content (items, prose) under the header or a
/// section label, so all sections align across every modal regardless of
/// which overlay renders them. Added on top of `MODAL_INNER_H_PADDING`.
pub(super) const MODAL_BODY_LEADING_INDENT: usize = 2;

/// Columns between a header title and a trailing meta value shown beside it
/// (e.g. the Todos `done/total` counter), so title + meta read as one line.
pub(super) const MODAL_TITLE_META_GAP: usize = 2;

// ── Block-level code/text surfaces ────────────────────────────────────────
// Every block-level content surface — the markdown `Block::Code` band and the
// tool-step result blocks (read / bash / listing / grep / diff) — shares ONE
// design contract so a code block looks the same whether it sits in assistant
// prose or inside an expanded tool step. These tokens are that contract: the
// geometry every code band agrees on. Colors live in `theme` (incl. the diff
// tokens); spacing/gutter live here.

/// Horizontal inset of block-level solid surfaces from the already-inset
/// transcript band. This keeps markdown code/math bands visually panel-like
/// without re-applying the global transcript gutter.
pub(super) const BLOCK_SURFACE_H_INSET: u16 = 2;

/// Columns between the math marker glyph and the rendered math text.
pub(super) const MATH_MARKER_GAP_COLS: usize = 2;

/// Columns of left padding inside a code band before the line-number gutter.
/// Markdown and tool-step bands use the same inner left margin so the code
/// text column lines up across block origins.
pub(super) const CODE_BAND_LEFT_INDENT: usize = 2;

/// Empty columns between the line-number gutter and the code text. Wide enough
/// to read the gutter as a distinct column, narrow enough not to waste width.
pub(super) const CODE_BAND_GUTTER_GAP: usize = 1;

/// Minimum width of the line-number column so single-digit files align
/// cleanly. Grows to fit the highest displayed line number in either band.
pub(super) const CODE_BAND_GUTTER_MIN_WIDTH: usize = 2;

// ── Bash output middle-folding ───────────────────────────────────────────
// An expanded bash step can emit hundreds of stdout/stderr lines, burying the
// trailing "events" — the `exit N` line, the `[output truncated]` marker, and
// the termination footer (timeout / blocked / cancelled) — far below the fold.
// Folding collapses the verbose middle into a single `⋯ N lines hidden` row,
// keeping a head of leading context and a tail of trailing context plus every
// event footer always visible. Short output (≤ HEAD + TAIL + 1 logical lines)
// renders verbatim, so folding only kicks in when it actually saves a row.
// This is a pure rendering convenience — the binary Disclosure
// (Collapsed/Expanded) and the persisted `expanded` field are untouched, so
// tool-batch spacing and the `user_pinned` invariant are unaffected.

/// Leading output lines kept visible above a folded bash middle.
pub(super) const BASH_FOLD_HEAD_ROWS: usize = 3;
/// Trailing output lines kept visible below a folded bash middle.
pub(super) const BASH_FOLD_TAIL_ROWS: usize = 3;

// ── Left-bar panels (panel_block family) ─────────────────────────────────
// `panel_block` is a borderless solid-bg panel with a single thick colored
// left `┃` bar — the severity/identity cue shared by the tool-step detail
// overlay and the permission sheet. These tokens size the content rect
// inside it, the left-bar-panel family's counterpart to `modal_frame`'s
// `MODAL_INNER_H_PADDING` (which insets the borderless modal family).

/// Per-side horizontal inset of `panel_block` content: the thick left `┃`
/// bar occupies 1 column, and a matching 1-column gutter is reserved on the
/// right so the panel's content is symmetric and a long line never runs
/// into either edge.
#[cfg(test)]
pub(super) const PANEL_BAR_INSET: u16 = 1;

// ── Minimum terminal size ────────────────────────────────────────────────
// Below this geometry the layout math (footer split, composer height, code
// band gutters) would underflow or produce an unusable UI. Instead of drawing
// garbage — or panicking deep in a subtraction chain — `draw_transcript`
// short-circuits and shows a single centered notice telling the user how large
// the terminal must be, hiding everything else. The values are the smallest
// width/height at which the normal footer (status bar + composer + hint bar)
// plus a one-line transcript row all fit.

/// Minimum terminal width (columns) for a usable layout. The footer needs the
/// horizontal inset on both sides plus the composer prompt prefix and right
/// pad; a narrower terminal cannot render even the input box legibly.
pub(super) const MIN_TERMINAL_COLS: u16 = 40;

/// Minimum terminal height (rows) for a usable layout. Accounts for the
/// viewport's top/bottom margin (2 rows), the footer chrome (gap + status bar
/// + composer minimum + hint bar = 6 rows), and at least one transcript row.
pub(super) const MIN_TERMINAL_ROWS: u16 = 12;
