//! Semantic document model for the TUI.
//!
//! Unlike storing raw strings, this model preserves the structure of messages
//! so that selection and copy operate on semantic units (blocks) rather than
//! terminal grid characters.

use muta_contracts::{EnvoyEvent, Role};

use crate::design::{COMMAND_CARD_LEAD_COLS, JOIN_MODIFY};
use unicode_width::UnicodeWidthStr;

/// Lifecycle of a tool step, stored explicitly (not inferred from `output`)
/// so an aborted call has its own terminal state instead of being stuck in
/// "no output yet". This is the single source of truth for tool-step state —
/// the renderer classifies it into a [`crate::tools::ToolStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolStepStatus {
    /// Still in flight (no terminal event observed yet).
    #[default]
    Running,
    /// Finished with a non-error output.
    Ok,
    /// Finished with an explicit error output.
    Failed,
    /// Aborted because the user denied permission for the call.
    Denied,
    /// Aborted mid-flight (e.g. the user interrupted the turn). Terminal, just
    /// like `Ok`/`Failed`: a later result or cancel event is ignored.
    Cancelled,
    /// Stopped by the user (the turn was interrupted) *after* producing real
    /// work: the envoy's partial transcript was preserved. Distinct from
    /// [`ToolStepStatus::Cancelled`] (nothing recovered) and
    /// [`ToolStepStatus::Failed`] (the sub-task errored on its own): this is
    /// resumable work the user deliberately cut short.
    Interrupted,
}

impl ToolStepStatus {
    /// Whether this state can still transition (i.e. the step is in flight).
    pub fn is_running(self) -> bool {
        matches!(self, ToolStepStatus::Running)
    }
}

#[derive(Debug, Clone)]
pub enum MessageKind {
    Text,
    ToolStep {
        id: String,
        name: String,
        /// The bound envoy profile name (`explore` / `plan` / `verify` / …)
        /// for an envoy-spawning tool step, populated from the first
        /// `EnvoyEvent::Started` and used to label the step by its role.
        /// `None` for non-envoy steps, or until the `Started` event lands.
        profile: Option<String>,
        arguments: String,
        output: Option<String>,
        /// Typed result (ADR-0001). `None` until the result lands, then a
        /// [`muta_contracts::ToolOutput`] carrying structured data (e.g. a shell
        /// exit code) alongside the legacy `output` text. Consumed by the
        /// renderer for data-level classification — `finish_tool_step` derives
        /// [`ToolStepStatus`] from `ToolOutput::is_error()` instead of
        /// string-sniffing the output, and `bash_command_for` reads the typed
        /// `Shell` command. The legacy `output`/`arguments` strings remain the
        /// fallback for restored sessions that predate the typed payload.
        ///
        /// Boxed to keep this enum variant small: `ToolOutput` (and especially
        /// its `Envoy`/`Patch` variants) is large enough that an unboxed
        /// `Option<ToolOutput>` would dominate the `MessageKind` enum size
        /// (clippy::large_enum_variant). The indirection is transparent to
        /// callers — the surrounding accessors deref it as needed.
        structured: Option<Box<muta_contracts::ToolOutput>>,
        /// Explicit lifecycle. Kept in sync with `output` by the
        /// `finish_tool_step` / `cancel_tool_step` transitions below.
        status: ToolStepStatus,
        expanded: bool,
        /// Whether the user has manually pinned `expanded`. While true, the
        /// auto/system setter (`set_tool_step_expanded`) is a no-op so
        /// lifecycle transitions can't override a deliberate user choice.
        user_pinned: bool,
        duration_ms: Option<u64>,
        /// Wall-clock instant the step started, so the UI can show a live
        /// elapsed time while the call (or envoy) is still running.
        /// `Instant` is cheap to capture at construction time and is not
        /// serialized — session restore reconstructs finished steps without it.
        started_at: Option<std::time::Instant>,
        /// Set when this envoy surfaced a permission / user-input request that
        /// is still parked awaiting a human decision. The peek row reads it to
        /// show `awaiting approval` instead of the last tool activity, which
        /// would misleadingly suggest the envoy is still making progress.
        /// Cleared by the next progress event from this envoy (tool call,
        /// tool result, or streamed text) and on any terminal transition.
        awaiting: bool,
        /// Latest free-text activity line the envoy reported via
        /// `EnvoyEvent::Activity` (`waiting for model`, `waiting to retry
        /// (3s)`, …). The peek row prefers it over the derived
        /// `starting`/`thinking` fallbacks while no child event has landed
        /// yet, so a long model call reads as alive instead of stuck on
        /// `starting`. Not serialized — restored sessions render terminal
        /// steps, which never show a peek.
        activity: Option<String>,
        /// Child events emitted by an envoy spawned from this tool step.
        children: Vec<TranscriptMessage>,
    },
    Thinking {
        content: String,
        duration_ms: Option<u64>,
        expanded: bool,
        /// User-pinned flag — see [`MessageKind::ToolStep::user_pinned`].
        user_pinned: bool,
    },
    /// Transient provider-retry state rendered inline in the transcript.
    ///
    /// Unlike a notice, this message is updated in place for every failed
    /// attempt and removed when the request succeeds or terminates. Keeping
    /// the timing data structured lets the renderer derive a live countdown
    /// (and then the current attempt's elapsed time) on every frame without
    /// appending one line per retry.
    ProviderRetry {
        /// Upcoming provider attempt, including the initial request
        /// (`2` means the first retry after attempt `1` failed).
        attempt: usize,
        /// Maximum provider attempts, including the initial request.
        max_attempts: usize,
        /// Most recent retryable provider error.
        failure: String,
        /// When the backoff countdown finishes and this attempt begins.
        retry_at: std::time::Instant,
        expanded: bool,
        user_pinned: bool,
    },
    /// A harness-level notice — errors, turn-pause signals, compaction
    /// summaries, provider switches, and other status lines that previously
    /// were smuggled through `Role::System` with hand-rolled `"Error: "`
    /// / `"System: "` text prefixes. Carrying an explicit [`NoticeSeverity`]
    /// lets one renderer (the `paint::notice` module) own the
    /// severity→color/icon mapping and lets callers stop string-sniffing.
    Notice {
        severity: NoticeSeverity,
        expanded: bool,
        user_pinned: bool,
    },
    /// A slash-command invocation as **one component that owns both its input
    /// and its output** (ADR-0108, revising ADR-0091/0106): the row is created
    /// optimistically when the user dispatches the command and settles when
    /// the typed result (or its terminal state) arrives — the same
    /// running→completed lifecycle a tool step has. `raw` holds the invocation
    /// text (`/search foo`, `!ls -la`); `blocks` hold the parsed result text
    /// (`CommandResult::to_text()`), empty while pending and when the record
    /// carried no result (legacy folds and shell passthroughs). Never rendered
    /// as a separate user bubble: the `⌘`/`❯` row *is* the input echo.
    CommandResult {
        /// The typed result (ADR-0091). `None` while the command is still
        /// running, and when the invocation was recorded but the reply was
        /// never persisted (legacy echo folds, `!command` passthroughs). Boxed
        /// to keep this enum variant small (`CommandResult` carries
        /// `Vec<SearchHit>` / `Vec<ReviewVerdict>`).
        result: Option<Box<muta_contracts::CommandResult>>,
        /// Lifecycle of the invocation (ADR-0108) — see [`CommandPhase`].
        phase: CommandPhase,
        expanded: bool,
        /// User-pinned flag — see [`MessageKind::ToolStep::user_pinned`].
        user_pinned: bool,
    },
    /// A round-interrupt marker (C11): the round stopped before completing —
    /// user interrupt (Esc Esc), superseded by newer input, or killed with
    /// the process. The durable record rides the session store (never the
    /// model window); this row is its transcript projection, created live
    /// from `RoundEvent::RoundInterrupted` and re-created on resume by
    /// timestamp seam. Renders as its own warning entry with the reason and
    /// the stop time, so a resumed session answers "should I continue?"
    /// at a glance.
    RoundInterrupt {
        /// The durable record — reason, `at_ms`, and (usually) the round.
        record: muta_contracts::RoundInterrupt,
        user_pinned: bool,
    },
}

/// Lifecycle of a command component (ADR-0108). Commands are synchronous
/// control-plane operations, so the lifecycle has exactly two live states plus
/// the cancel mark — unlike a tool step there is no permission-denied or
/// interrupted state to represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPhase {
    /// Dispatched, no result yet. The row shows the invocation alone in the
    /// muted running tone (`⌘ /autopilot`) — the input half of the component
    /// is already durable in the transcript, so a slow command never leaves
    /// the user wondering whether it ran.
    Pending,
    /// The typed result arrived (or is known not to exist — legacy folds,
    /// shell passthroughs): the row shows `invocation · reply` per its
    /// [`CommandRowLayout`].
    Completed,
    /// No result will ever arrive (the session view moved on before the reply
    /// landed, or the runtime errored out of band). Reads as a settled row
    /// with no reply, never as a promise.
    Cancelled,
}

/// How a command row presents its result — derived at render time from the
/// result's shape, not stored. Commands are operations, not conversation:
/// most replies are one short line that should simply *be* the row, with no
/// disclosure marker at all. Only a genuinely long reply earns the `+`/`-`
/// affordance. See ADR-0106.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRowLayout {
    /// No result at all (shell passthroughs, legacy folds) — the row is just
    /// the invocation, dimmed. Nothing to expand.
    Plain,
    /// A single-line, short reply (acks, `/new`'s confirmation, `/schedule`):
    /// rendered inline on the same row as `invocation · reply`. No marker —
    /// there is no second view to disclose.
    Inline,
    /// A multi-line or long reply (`/search`, `/session status`, `/review`,
    /// …): the disclosure pattern is correct — a `+`/`-` header row that
    /// expands to the body.
    Disclose,
}

/// Width of the trailing sent-time label ` · HH:MM` appended to a command
/// card when `sent_at_ms` is present — reserved by
/// [`TranscriptMessage::command_row_layout`] before the inline/Disclose
/// classification so a timestamped row never flips to Disclose at render
/// time.
pub const SENT_TIME_LABEL_COLS: usize = 8;

/// The single classifier for [`CommandRowLayout`]: a reply joins inline when
/// it is exactly one line and fits beside the invocation; otherwise it
/// discloses. `available_width` is the row's usable columns (the terminal
/// band minus gutters), not the full terminal width.
///
/// The classifier subtracts the fixed command-card chrome (identity bar +
/// marker slot + family glyph, ADR-0109) from the budget: the inline join
/// has to fit *inside the card*, not merely inside the terminal.
pub const COMMAND_ROW_CHROME_COLS: usize = COMMAND_CARD_LEAD_COLS + 2 /* marker slot */ + 2 /* glyph */;
pub fn command_row_layout(
    result: Option<&muta_contracts::CommandResult>,
    invocation: &str,
    available_width: usize,
) -> CommandRowLayout {
    let Some(result) = result else {
        return CommandRowLayout::Plain;
    };
    let text = result.to_text();
    if text.contains('\n') {
        return CommandRowLayout::Disclose;
    }
    // The inline join is `invocation` + JOIN_MODIFY (` · `) + reply; the row
    // must hold both without truncation for the reply to read as an
    // attribute, not a fragment. The card chrome (identity bar + marker slot
    // + glyph, ADR-0109) and the trailing timestamp eat into the same row, so
    // the budget subtracts them — but the time label is render-time state the
    // classifier cannot see, so the classifier subtracts only the fixed
    // chrome and the renderer's clamp guards the timestamp.
    let used = COMMAND_ROW_CHROME_COLS + invocation.width() + JOIN_MODIFY.width() + text.width();
    if used <= available_width {
        CommandRowLayout::Inline
    } else {
        CommandRowLayout::Disclose
    }
}

/// Severity of a [`MessageKind::Notice`]. Drives the color and the leading
/// icon through the central severity→presentation map in
/// `render/notice.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeSeverity {
    /// Neutral status (compaction summary, provider switch, …). Replaces the
    /// old `Role::System` + `system_text()` rendering.
    Info,
    /// A non-terminal condition that needs attention.
    Warning,
    /// A terminal failure surfaced from the harness or a tool.
    Error,
}

pub fn notice_severity_from_core(severity: muta_contracts::NoticeSeverity) -> NoticeSeverity {
    match severity {
        muta_contracts::NoticeSeverity::Info => NoticeSeverity::Info,
        muta_contracts::NoticeSeverity::Warning => NoticeSeverity::Warning,
        muta_contracts::NoticeSeverity::Error => NoticeSeverity::Error,
    }
}

/// Table column text alignment for GFM tables parsed by the in-house parser,
/// kept as a separate type so the `Block` definition stays dependency-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableAlignment {
    None,
    Left,
    Center,
    Right,
}

/// A byte range `[start, end)` within a prose block's `content` that should be
/// rendered as inline code. The in-house parser keeps the backtick delimiters
/// in the flattened `content` and records the range here so the renderer can
/// paint it on the code surface without disturbing the byte-addressable
/// copy/selection model (which still sees plain text).
///
/// Ranges always cover the full `` `…` `` span including both backticks, and
/// are clamped to `content.len()`. An empty vector means "no inline code".
pub type CodeRange = (usize, usize);

/// A byte range `[start, end)` within a prose block's `content` that should be
/// rendered as inline math. The source delimiters stay in `content` for exact
/// copy/selection; renderers may elide them visually.
pub type MathRange = (usize, usize);

type ParsedLink = ((usize, usize), (usize, usize), String);

/// A byte range for a recognized hyperlink inside prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRange {
    /// Full source range, including markdown / TeX link delimiters when present.
    pub range: (usize, usize),
    /// The visible label range inside `range`. For bare URLs this equals `range`.
    pub label_range: (usize, usize),
    /// Normalized URL target. First-pass support intentionally records only
    /// browser-safe `http://` and `https://` targets.
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InlineScan {
    pub code_ranges: Vec<CodeRange>,
    pub bold_ranges: Vec<CodeRange>,
    pub math_ranges: Vec<MathRange>,
    pub link_ranges: Vec<LinkRange>,
}

/// Inline-prose payload shared by the prose block variants: the flattened
/// text plus the byte ranges of its inline markup.
///
/// The ranges are produced by [`scan_inline`] at parse time, address bytes in
/// `content`, and are clamped to `content.len()`. `content` keeps the original
/// delimiters (backticks, `**`, link syntax) so copy/selection yields exact
/// source while renderers may elide them visually.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inline {
    pub content: String,
    /// Byte ranges of inline-code runs within `content` (see [`CodeRange`]).
    pub code_ranges: Vec<CodeRange>,
    /// Byte ranges of strong/bold text runs within `content`.
    pub bold_ranges: Vec<CodeRange>,
    /// Byte ranges of inline math runs within `content`.
    pub math_ranges: Vec<MathRange>,
    /// Hyperlink ranges within `content`.
    pub link_ranges: Vec<LinkRange>,
}

impl Inline {
    /// Verbatim text with no inline markup.
    pub fn plain(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ..Self::default()
        }
    }

    /// Scan `content` for inline markup, trimming trailing whitespace and
    /// clamping every range to the trimmed length.
    fn scanned(content: &str) -> Self {
        let scan = scan_inline(content);
        let trimmed_len = content.trim_end().len();
        Self {
            content: content[..trimmed_len].to_string(),
            code_ranges: clamp_ranges(&scan.code_ranges, trimmed_len),
            bold_ranges: clamp_ranges(&scan.bold_ranges, trimmed_len),
            math_ranges: clamp_ranges(&scan.math_ranges, trimmed_len),
            link_ranges: clamp_link_ranges(&scan.link_ranges, trimmed_len),
        }
    }
}

/// A single semantic block within a message.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Plain text paragraph.
    Text(Inline),
    /// Display math block (`$$…$$` or `\[…\]`).
    Math { content: String },
    /// Inline or fenced code.
    Code {
        language: Option<String>,
        content: String,
    },
    /// A heading.
    Heading { level: u8, inline: Inline },
    /// A list item, preserving its marker and nesting level.
    ListItem {
        inline: Inline,
        ordered: Option<u64>,
        depth: usize,
        checked: Option<bool>,
    },
    /// A blockquote.
    Quote(Inline),
    /// A GFM-style table, kept as a semantic unit so columns stay aligned and
    /// copy yields the rendered grid rather than re-wrapped prose.
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        aligns: Vec<TableAlignment>,
        /// Pre-rendered aligned grid (what is drawn and what copy returns).
        rendered: String,
    },
    /// A horizontal rule.
    Rule,
    /// Soft / hard line break marker.
    Break,
}

impl Block {
    /// The inline-prose payload of the prose variants (`Text`, `Heading`,
    /// `ListItem`, `Quote`); `None` for the structural blocks.
    #[allow(dead_code)]
    pub fn inline(&self) -> Option<&Inline> {
        match self {
            Block::Text(inline) | Block::Quote(inline) => Some(inline),
            Block::Heading { inline, .. } | Block::ListItem { inline, .. } => Some(inline),
            _ => None,
        }
    }

    /// Returns the raw text content of this block (without formatting).
    pub fn raw_text(&self) -> &str {
        match self {
            Block::Text(inline) | Block::Quote(inline) => &inline.content,
            Block::Math { content } => content,
            Block::Code { content, .. } => content,
            Block::Heading { inline, .. } => &inline.content,
            Block::ListItem { inline, .. } => &inline.content,
            Block::Table { rendered, .. } => rendered,
            Block::Rule => "",
            Block::Break => "\n",
        }
    }

    /// Returns true if this block is empty.
    pub fn is_empty(&self) -> bool {
        self.raw_text().is_empty()
    }
}

/// Lifecycle of a user-authored message from the user's point of view.
///
/// All other roles are inherently "delivered" (the harness only renders them
/// once they exist), so this only matters on `Role::User` messages. The TUI
/// uses it to draw a distinct "⏸ Queued" panel while a message is waiting for
/// the in-flight turn to finish, and the event loop flips it back to
/// [`DeliveryStatus::Delivered`] once the queued message is actually shipped
/// to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeliveryStatus {
    /// The message has been handed off to the agent (or is an assistant /
    /// tool / system message that doesn't go through the queue).
    #[default]
    Delivered,
    /// The user pressed Enter while a turn was still running, so the message
    /// is staged in the TUI's send queue and will be dispatched automatically
    /// when the harness returns to idle.
    Queued,
    /// A mid-round insert (`Ctrl+O`) whose round ended — naturally or by an
    /// interrupt (Esc Esc) — before it could be admitted at a turn boundary.
    /// The entry stays in the transcript (it never leaves the conversation)
    /// but is re-queued as the **next round's** prompt: it renders with the
    /// same pending treatment as [`DeliveryStatus::Queued`] and flips to
    /// delivered when that round starts.
    HeldNextRound,
}

/// A structured transcript message.
/// What kind of user message this `Role::User` message originates from. Only
/// meaningful for user messages; the other roles carry the default
/// ([`UserMessageOrigin::Chat`]) and it is never consulted for them.
///
/// The Activity modal uses this to decide whether a `Role::User` message is
/// the genuine prompt that drove the current round: slash commands
/// (`/review …`) and shell passthroughs (`!ls`) are surfaced as user messages
/// in the transcript but are *not* the LLM prompt, so they must not be shown
/// as the round's "Prompt".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UserMessageOrigin {
    /// A normal chat prompt the user composed and sent to the model. This is
    /// the only origin the Activity modal treats as the round's prompt.
    #[default]
    Chat,
    /// Human input admitted at an inner boundary of an already-running round.
    Insert,
    /// A slash command (`/review`, `/pursue …`, …). The harness handles these
    /// directly; the model never sees them as a prompt.
    Slash,
    /// A `!command` shell passthrough run directly through the bash tool,
    /// bypassing the model entirely.
    Shell,
}

/// Monotonic source of per-message identities. A message keeps its `id` across
/// the per-frame clone into `App::messages`, so the renderer
/// can use it as a stable cache key for the message's laid-out height (see the
/// height cache in `render`). Ids are process-unique; cloning a message copies
/// its id (a clone represents the same logical message), which is exactly what
/// the height cache wants.
static NEXT_MESSAGE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_message_id() -> u64 {
    NEXT_MESSAGE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub struct TranscriptMessage {
    /// Stable, process-unique identity used as the renderer's height-cache key.
    /// Assigned at construction and preserved across clones.
    pub id: u64,
    pub role: Role,
    pub blocks: Vec<Block>,
    /// The original raw markdown/text, preserved for exact copy.
    pub raw: String,
    pub kind: MessageKind,
    /// What kind of user message this `Role::User` message is. Defaults to
    /// [`UserMessageOrigin::Chat`]; slash commands and shell passthroughs mark
    /// themselves so they are not mistaken for the round's driving prompt.
    pub origin: UserMessageOrigin,
    /// Lifecycle of this message from the send queue's point of view. Only
    /// `Role::User` messages ever carry [`DeliveryStatus::Queued`]; everything
    /// else stays at the default [`DeliveryStatus::Delivered`]. The renderer
    /// and the queue dispatch/recall paths key off this.
    pub delivery: DeliveryStatus,
    /// Correlation id for a mid-round insert entry (`Ctrl+O`,
    /// `AgentRequest::InsertUserInput`). Set when the entry is staged into the
    /// transcript as [`DeliveryStatus::Queued`]; the response listener uses it
    /// to find and settle the entry when the harness reports
    /// `UserInputInserted` / `NextRoundStarted` / `UserInputUnavailable`.
    /// `None` for every non-insert message.
    pub insert_id: Option<String>,
    /// Provider/solution id that produced this message, mirrored from the
    /// core [`muta_contracts::Message`] so the transcript stays traceable across
    /// model switches. `None` for messages that don't carry attribution.
    pub provider: Option<String>,
    /// Model id that produced this message, companion to [`TranscriptMessage::provider`].
    pub model: Option<String>,
    /// The reasoning effort (depth) this message's model request ran with
    /// (`"high"`, `"max"`, …), when the active channel exposes one. Stamped at
    /// the same point as [`TranscriptMessage::model`] so the turn header can
    /// show the depth a given turn actually ran at. `None` for non-reasoning
    /// channels and messages that carry no attribution.
    pub effort: Option<String>,
    /// The user-visible round this message belongs to (1-indexed). Driving
    /// user messages open a round; assistant-side messages inherit it.
    pub round: Option<u64>,
    /// The ReAct turn this assistant-side message belongs to within its round
    /// (1-indexed, stamped from `TurnStarted`). The renderer uses the
    /// `(round, turn)` position to identify compact tool batches. `None`
    /// means the position is unknown; legacy tool batches retain a compatible
    /// flush-stack fallback.
    pub turn: Option<u64>,
    /// Wall-clock send time for transcript headers, in Unix epoch milliseconds.
    /// Restored messages use the persisted millisecond value when available and
    /// fall back to the durable core-message timestamp for legacy sessions.
    pub sent_at_ms: Option<u64>,
}

impl TranscriptMessage {
    pub fn new(role: Role, raw: impl Into<String>) -> Self {
        let raw = sanitize_text(&raw.into()).into_owned();
        // User messages are rendered verbatim as plain text — no markdown
        // interpretation — so pasted text containing markdown-like syntax
        // does not get mangled into headings/code fences/lists and the
        // transcript stays readable. The raw text becomes a single `Text`
        // block; `wrap_text` preserves intra-block line breaks.
        let blocks = if role == Role::User {
            parse_blocks_plain(&raw)
        } else {
            parse_blocks(&raw)
        };
        Self {
            id: next_message_id(),
            role,
            blocks,
            raw,
            kind: MessageKind::Text,
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
        }
    }

    /// Label this `Role::User` message with its turn origin (slash command /
    /// shell passthrough). No-op for non-user messages, which never surface
    /// an origin. Builder-style, used alongside [`Self::queued`].
    pub fn with_origin(mut self, origin: UserMessageOrigin) -> Self {
        self.origin = origin;
        self
    }

    /// Mark this message as queued in the send queue (waiting for the
    /// in-flight turn to finish before it is dispatched). Only meaningful on
    /// `Role::User` messages; the renderer and dispatch logic key off this.
    #[allow(dead_code)]
    pub fn queued(mut self) -> Self {
        self.delivery = DeliveryStatus::Queued;
        self
    }

    /// Correlate this message with a mid-round insert (`Ctrl+O`) by its
    /// harness-side input id, so the response listener can settle the entry
    /// when the insert is admitted or handed back. Builder-style companion of
    /// [`Self::queued`].
    pub fn with_insert_id(mut self, insert_id: impl Into<String>) -> Self {
        self.insert_id = Some(insert_id.into());
        self
    }

    /// Mark this insert entry as waiting for the **next** round: the round it
    /// was steered into ended (naturally or interrupted) before admission, so
    /// the content ships as a fresh round's prompt instead. Idempotent on
    /// already-delivered messages — a late race can never un-deliver one.
    pub fn hold_pending_round(&mut self) {
        if self.delivery == DeliveryStatus::Queued {
            self.delivery = DeliveryStatus::HeldNextRound;
        }
    }

    /// Stamp the provider/solution id and model that produced this message.
    pub fn with_attribution(
        mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        self.provider = Some(provider.into());
        self.model = Some(model.into());
        self
    }

    /// Stamp the reasoning effort (depth) this message's model request ran
    /// with. Kept separate from [`Self::with_attribution`] so existing call
    /// sites (and their tests) keep their arity; pass `None` (or skip the
    /// call) when the channel exposes no effort.
    pub fn with_effort(mut self, effort: Option<impl Into<String>>) -> Self {
        self.effort = effort.map(Into::into);
        self
    }

    /// Stamp the enclosing user round.
    pub fn with_round(mut self, round: u64) -> Self {
        self.round = Some(round);
        self
    }

    /// Stamp the ReAct turn within the enclosing round.
    pub fn with_turn(mut self, turn: u64) -> Self {
        self.turn = Some(turn);
        self
    }

    /// Stamp the visible send time for a user-authored message.
    pub fn with_sent_at_ms(mut self, sent_at_ms: u64) -> Self {
        self.sent_at_ms = Some(sent_at_ms);
        self
    }

    /// Whether this message is a notice with `Error` severity (indicating an
    /// unrecovered turn error, provider failure, or error notice).
    pub fn is_error_notice(&self) -> bool {
        matches!(
            self.kind,
            MessageKind::Notice {
                severity: NoticeSeverity::Error,
                ..
            }
        )
    }

    /// The `(provider, model)` pair to show as an attribution badge, when this
    /// message carries at least a model. Used by the renderer to label which
    /// model produced a turn; `None` when the message has no attribution
    /// (user/system messages, or untagged history).
    #[allow(dead_code)]
    pub fn attribution_label(&self) -> Option<(String, String)> {
        let model = self.model.clone()?;
        let provider = self.provider.clone().unwrap_or_default();
        Some((provider, model))
    }

    pub fn tool_step(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        let mut message = Self {
            id: next_message_id(),
            role: Role::Tool,
            blocks: Vec::new(),
            raw: String::new(),
            kind: MessageKind::ToolStep {
                id: id.into(),
                name: name.into(),
                profile: None,
                arguments: arguments.into(),
                output: None,
                structured: None,
                status: ToolStepStatus::Running,
                expanded: false,
                user_pinned: false,
                duration_ms: None,
                started_at: Some(std::time::Instant::now()),
                awaiting: false,
                activity: None,
                children: Vec::new(),
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
        };
        message.refresh_tool_step();
        message
    }

    /// A slash-command invocation with its typed result (ADR-0091). `name` is
    /// the command word without the leading slash (`"search"`), `"shell"` for
    /// a `!command` passthrough. The collapsed row shows the invocation; the
    /// expandable body shows `result.to_text()`. The row starts `Completed`.
    pub fn command_result(
        name: impl Into<String>,
        args: impl Into<String>,
        result: Option<muta_contracts::CommandResult>,
    ) -> Self {
        Self::command_result_in_phase(name, args, result, CommandPhase::Completed)
    }

    /// The optimistic dispatch row (ADR-0108): the user just sent the command
    /// and no result exists yet. Renders as the pending input half of the
    /// command component; the `RoundEvent::CommandResult` handler settles it
    /// in place via [`Self::settle_command_result`].
    pub fn pending_command(name: impl Into<String>, args: impl Into<String>) -> Self {
        Self::command_result_in_phase(name, args, None, CommandPhase::Pending)
    }

    fn command_result_in_phase(
        name: impl Into<String>,
        args: impl Into<String>,
        result: Option<muta_contracts::CommandResult>,
        phase: CommandPhase,
    ) -> Self {
        let name = name.into();
        let args = args.into();
        // The invocation as displayed: `!cmd` for shell passthroughs (their
        // args carry the literal `!cmd` text), `/name args` for slash
        // commands.
        let invocation = if name == "shell" {
            args.clone()
        } else {
            let full = format!("/{} {}", name, args);
            full.trim_end().to_string()
        };
        let result_text = result
            .as_ref()
            .map(|result| result.to_text())
            .unwrap_or_default();
        Self {
            id: next_message_id(),
            // A harness artifact, not user or model prose — the renderer gives
            // it its own dimmed command-row treatment.
            role: Role::Tool,
            blocks: parse_blocks(&result_text),
            raw: sanitize_text(&invocation).into_owned(),
            kind: MessageKind::CommandResult {
                result: result.map(Box::new),
                phase,
                expanded: false,
                user_pinned: false,
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
        }
    }

    /// Settle a pending command component with its typed result (ADR-0108):
    /// this is the *only* live path that turns [`CommandPhase::Pending`] into
    /// [`CommandPhase::Completed`], and it reuses the existing message id so
    /// the row is updated in place — one component, input and output, no
    /// second row and no seam in the transcript. Returns `false` when the
    /// message is not a pending command (an id mismatch — the pending row was
    /// dropped by a transcript rebuild — so the caller may push a fresh
    /// completed row instead).
    pub fn settle_command_result(&mut self, result: muta_contracts::CommandResult) -> bool {
        let MessageKind::CommandResult {
            result: slot,
            phase,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if *phase != CommandPhase::Pending {
            return false;
        }
        // Keep the parsed body in sync with the stored typed result, the same
        // way the constructor derives it.
        let result_text = result.to_text();
        self.blocks = parse_blocks(&result_text);
        *slot = Some(Box::new(result));
        *phase = CommandPhase::Completed;
        true
    }

    /// Mark a pending command component as never receiving a reply
    /// (ADR-0108) — the input half stays readable but stops promising an
    /// output. Returns whether the transition applied.
    pub fn cancel_pending_command(&mut self) -> bool {
        if let MessageKind::CommandResult { phase, .. } = &mut self.kind
            && *phase == CommandPhase::Pending
        {
            *phase = CommandPhase::Cancelled;
            true
        } else {
            false
        }
    }

    pub fn finish_tool_step(
        &mut self,
        id: &str,
        output: impl Into<String>,
        structured: muta_contracts::ToolOutput,
        duration_ms: u64,
    ) -> bool {
        let MessageKind::ToolStep {
            id: step_id,
            output: step_output,
            structured: step_structured,
            status,
            duration_ms: step_duration,
            awaiting,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if step_id != id || !status.is_running() {
            return false;
        }
        *awaiting = false;
        let output = output.into();
        // Classify from the structured result (data-level: a non-zero shell
        // exit, an explicit `ToolOutput::Error`, a `failed` envoy). The
        // legacy `starts_with("Error")` text fallback was removed once tool
        // error sites migrated to `ToolOutput::Error` and envoys carried
        // an explicit `failed` flag — classification is now fully data-driven.
        // Permission denial gets its own status so the UI shows it distinctly
        // from a runtime error.
        *status = if matches!(
            structured,
            muta_contracts::ToolOutput::PermissionDenied { .. }
        ) {
            ToolStepStatus::Denied
        } else if matches!(
            &structured,
            muta_contracts::ToolOutput::Envoy {
                interrupted: true,
                ..
            }
        ) {
            // A cooperatively-drained envoy: the user interrupted the turn,
            // but the partial transcript was preserved. Classified before
            // `is_error()` because interruption is not a failure.
            ToolStepStatus::Interrupted
        } else if structured.is_error() {
            ToolStepStatus::Failed
        } else {
            ToolStepStatus::Ok
        };
        *step_output = Some(output);
        *step_structured = Some(Box::new(structured));
        *step_duration = Some(duration_ms);
        self.refresh_tool_step();
        true
    }

    /// Accumulate an incremental stream chunk into a still-running tool step,
    /// so the UI can render partial output (e.g. bash stdout) live. The first
    /// chunk initializes a partial [`muta_contracts::ToolOutput::Shell`]; the
    /// terminal `finish_tool_step` later overwrites it with the final result.
    /// Returns `false` if this isn't a matching running step.
    pub fn push_tool_stream(&mut self, id: &str, stream: &muta_contracts::ToolStream) -> bool {
        let MessageKind::ToolStep {
            id: step_id,
            structured,
            status,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if step_id != id || !status.is_running() {
            return false;
        }
        if !matches!(
            structured.as_deref(),
            Some(muta_contracts::ToolOutput::Shell { .. })
        ) {
            *structured = Some(Box::new(muta_contracts::ToolOutput::Shell {
                command: String::new(),
                stdout: String::new(),
                stderr: String::new(),
                lines: Vec::new(),
                exit: None,
                truncated: false,
                // Still-streaming seed: the real termination lands with the
                // final result (`finish_tool_step`). Default until then.
                termination: muta_contracts::tool_output::ShellTermination::default(),
            }));
        }
        if let Some(muta_contracts::ToolOutput::Shell {
            stdout,
            stderr,
            lines,
            ..
        }) = structured.as_deref_mut()
        {
            // Build the TUI-authoritative `lines` view alongside the flat
            // strings so the streaming view matches the final result: stderr
            // stays red-tinted and stdout/stderr keep their true arrival
            // interleaving, instead of the all-stdout-then-all-stderr
            // degraded band the empty-`lines` fallback used to force.
            //
            // Each stream chunk is one complete `\n`-terminated line (bash's
            // capture is line-buffered and emits `format!("{text}\n")`), so
            // split on `\n` and tag each non-empty piece with its source
            // stream. Trailing empties (from the terminal `\n`) are dropped so
            // they don't paint phantom blank rows.
            let stream_tag = match stream {
                muta_contracts::ToolStream::Stdout(_) => {
                    muta_contracts::tool_output::ShellStream::Out
                }
                muta_contracts::ToolStream::Stderr(_) => {
                    muta_contracts::tool_output::ShellStream::Err
                }
            };
            let text = match stream {
                muta_contracts::ToolStream::Stdout(s) | muta_contracts::ToolStream::Stderr(s) => s,
            };
            for piece in text.split('\n') {
                if !piece.is_empty() {
                    lines.push(muta_contracts::tool_output::ShellLine {
                        stream: stream_tag,
                        text: piece.to_string(),
                    });
                }
            }
            match stream {
                muta_contracts::ToolStream::Stdout(s) => stdout.push_str(s),
                muta_contracts::ToolStream::Stderr(s) => stderr.push_str(s),
            }
        }
        self.refresh_tool_step();
        true
    }

    /// Mark a still-running tool step as cancelled. Idempotent: a step that
    /// already reached a terminal state (`Ok` / `Failed` / `Cancelled`) is left
    /// untouched and returns `false`. When the step is a `task` (envoy),
    /// its still-running nested tool children are cancelled too, so an aborted
    /// envoy never leaves a "running" child step behind.
    pub fn cancel_tool_step(&mut self, id: &str) -> bool {
        let MessageKind::ToolStep {
            id: step_id,
            status,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        if step_id != id || !status.is_running() {
            return false;
        }
        // Apply the transition through `cancel_all_running`, which also handles
        // the nested-children sweep and refreshes the rendered view in one
        // place.
        self.cancel_all_running()
    }

    /// Recursively cancel every still-running tool step within this message
    /// (used for envoy children and as a defensive sweep). Returns `true`
    /// if anything transitioned.
    pub fn cancel_all_running(&mut self) -> bool {
        let (step_running, child_changed) = {
            let MessageKind::ToolStep {
                status,
                started_at,
                duration_ms,
                awaiting,
                children,
                ..
            } = &mut self.kind
            else {
                return false;
            };
            let mut changed = false;
            if status.is_running() {
                *status = ToolStepStatus::Cancelled;
                *awaiting = false;
                // Freeze the elapsed time at the moment of cancellation so the
                // step stops showing a live-running timer.
                if duration_ms.is_none() {
                    *duration_ms = started_at
                        .map(|started| started.elapsed().as_millis() as u64)
                        .or(Some(0));
                }
                changed = true;
            }
            let mut child_changed = changed;
            for child in children.iter_mut() {
                child_changed |= child.cancel_all_running();
            }
            (changed, child_changed)
        };
        if step_running || child_changed {
            self.refresh_tool_step();
        }
        step_running || child_changed
    }

    /// The explicit lifecycle of a tool step, or `None` for non-tool messages.
    pub fn tool_step_status(&self) -> Option<ToolStepStatus> {
        match &self.kind {
            MessageKind::ToolStep { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Append an envoy event as a nested child of this tool step.
    ///
    /// Returns `true` if this message is a tool step and the event was stored.
    pub fn push_envoy_event(&mut self, event: &EnvoyEvent) -> bool {
        let MessageKind::ToolStep {
            children,
            profile,
            awaiting,
            activity,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        // Progress events clear a parked human-decision wait; request events
        // (permission / ask-user / input) park it, so the peek row can say
        // `awaiting approval` instead of replaying the last tool activity.
        match event {
            EnvoyEvent::PermissionRequest(_)
            | EnvoyEvent::UserQuestionRequest(_)
            | EnvoyEvent::InputRequest(_) => *awaiting = true,
            EnvoyEvent::ToolCall { .. }
            | EnvoyEvent::ToolResult { .. }
            | EnvoyEvent::StreamStart { .. }
            | EnvoyEvent::StreamDelta(_)
            | EnvoyEvent::StreamEnd(_)
            | EnvoyEvent::StreamReasoningStart { .. }
            | EnvoyEvent::StreamReasoningDelta(_)
            | EnvoyEvent::StreamReasoningEnd(_) => *awaiting = false,
            _ => {}
        }
        match event {
            // The envoy announced its role — stamp it on the step so the
            // renderer can draw an `[EXPLORE]` / `[PLAN]` role badge in front
            // of the summary instead of a generic `[ENVOY]`.
            // No child message is produced.
            EnvoyEvent::Started { profile: name } => {
                *profile = Some(name.clone());
            }
            EnvoyEvent::StreamStart { round, turn } => {
                children.push(
                    TranscriptMessage::new(Role::Assistant, "")
                        .with_round(*round)
                        // `turn` is the envoy's 0-indexed model-request
                        // position; the transcript's `turn` is 1-indexed.
                        .with_turn((*turn as u64) + 1),
                );
            }
            EnvoyEvent::StreamDelta(delta) => {
                // Identity-addressed (ADR-0114): fold the delta into the
                // latest assistant-text child of the *same* stream turn, not
                // merely the last child — a tool-call/result child can be
                // appended between two deltas and would otherwise fork the
                // text into a second entry.
                let target = children
                    .iter_mut()
                    .rfind(|m| m.role == Role::Assistant && matches!(m.kind, MessageKind::Text));
                if let Some(last) = target {
                    last.push_stream(&sanitize_text(delta));
                } else {
                    let mut msg = TranscriptMessage::new(Role::Assistant, "");
                    msg.push_stream(&sanitize_text(delta));
                    children.push(msg);
                }
            }
            EnvoyEvent::StreamEnd(content) => {
                if let Some(last) = children
                    .iter_mut()
                    .rfind(|m| m.role == Role::Assistant && matches!(m.kind, MessageKind::Text))
                {
                    last.raw = content.clone();
                    last.reparse();
                } else {
                    children.push(TranscriptMessage::new(Role::Assistant, content.clone()));
                }
            }
            // The envoy's live reasoning chain, folded into the same
            // `MessageKind::Thinking` message a resumed session restores from
            // `reasoning_content` — so a live drill-in and a reloaded one show
            // the same children. Placement mirrors the wire order the child
            // emits (reasoning precedes its turn's assistant text and tool
            // calls), so the trace lands in the right turn band. Disclosed
            // chains only: the sender gates hidden-chain models out at the
            // source, so no phantom summary trace can appear here.
            EnvoyEvent::StreamReasoningStart { round, turn } => {
                children.push(
                    TranscriptMessage::thinking("")
                        .with_round(*round)
                        .with_turn((*turn as u64) + 1),
                );
            }
            EnvoyEvent::StreamReasoningDelta(delta) => {
                // Identity-addressed (ADR-0114): fold into the latest still-
                // streaming thinking child. `StreamReasoningStart` pushes a
                // stamped Thinking child; a tool-call child landing between
                // two deltas must not fork the trace into a second entry.
                if let Some(last) = children
                    .iter_mut()
                    .rfind(|m| m.is_thinking() && m.is_thinking_streaming())
                {
                    if let MessageKind::Thinking { content, .. } = &mut last.kind {
                        content.push_str(&sanitize_text(delta));
                    }
                    last.raw.push_str(&sanitize_text(delta));
                } else {
                    children.push(TranscriptMessage::thinking(delta));
                }
            }
            EnvoyEvent::StreamReasoningEnd(content) => {
                if let Some(last) = children
                    .iter_mut()
                    .rfind(|m| m.is_thinking() && m.is_thinking_streaming())
                {
                    last.raw = sanitize_text(&content.clone()).into_owned();
                    last.reparse();
                    if let MessageKind::Thinking {
                        content: current, ..
                    } = &mut last.kind
                    {
                        *current = sanitize_text(content).into_owned();
                    }
                    // No wall clock is available on the folding path; 0 is
                    // the same terminal stamp a resumed session applies, and
                    // what matters is that the trace stops "streaming" so the
                    // spinner freezes.
                    last.set_thinking_duration(0);
                } else if !content.is_empty() {
                    children.push(TranscriptMessage::thinking(content));
                }
            }
            EnvoyEvent::ToolCall {
                id,
                name,
                arguments,
                round,
                turn,
            } => {
                children.push(
                    TranscriptMessage::tool_step(id.clone(), name.clone(), arguments.clone())
                        .with_round(*round)
                        .with_turn((*turn as u64) + 1),
                );
            }
            EnvoyEvent::ToolResult {
                id,
                output,
                duration_ms,
                ..
            } => {
                if let Some(child) = children.iter_mut().find(|m| {
                    m.is_tool_step()
                        && if let MessageKind::ToolStep {
                            id: step_id,
                            output: None,
                            ..
                        } = &m.kind
                        {
                            step_id == id
                        } else {
                            false
                        }
                }) {
                    child.finish_tool_step(
                        id,
                        output.clone(),
                        muta_contracts::ToolOutput::text(output.clone()),
                        *duration_ms,
                    );
                } else {
                    let mut msg = TranscriptMessage::tool_step(id.clone(), "tool", "{}");
                    msg.finish_tool_step(
                        id,
                        output.clone(),
                        muta_contracts::ToolOutput::text(output.clone()),
                        *duration_ms,
                    );
                    children.push(msg);
                }
            }
            EnvoyEvent::Notice(notice) => {
                children.push(TranscriptMessage::notice(
                    notice_severity_from_core(notice.severity),
                    notice.render_text(),
                ));
            }
            // The envoy reported a free-text activity line (`waiting for
            // model`, `waiting to retry (3s)`). Stored for the peek row so a
            // stretch with no child events still reads as alive. No child
            // message is produced.
            EnvoyEvent::Activity(text) => *activity = Some(text.clone()),
            // Full-duplex (ADR-0029): an envoy surfaced a permission /
            // ask_user request up through the envoy tool. The down-direction
            // reply (registry → handle → reply_permission / reply_user_question)
            // is wired at the agent layer; rendering the nested prompt in the
            // TUI and routing the user's answer back down is the harness↔TUI
            // integration step that follows. Until then these are observed but
            // not rendered as a nested child step (the request still reaches
            // the harness via the `RoundEvent::Envoy` envelope, so a future
            // handler can attach without changing the event shape).
            EnvoyEvent::PermissionRequest(_)
            | EnvoyEvent::UserQuestionRequest(_)
            | EnvoyEvent::InputRequest(_) => {}
        }
        true
    }

    pub fn is_tool_step(&self) -> bool {
        matches!(self.kind, MessageKind::ToolStep { .. })
    }

    pub fn is_command_result(&self) -> bool {
        matches!(self.kind, MessageKind::CommandResult { .. })
    }

    /// Whether this row is a round-interrupt marker (C11).
    pub fn is_round_interrupt(&self) -> bool {
        matches!(self.kind, MessageKind::RoundInterrupt { .. })
    }

    pub fn command_result_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::CommandResult { expanded, .. } => Some(*expanded),
            _ => None,
        }
    }

    /// The lifecycle phase of a command component (ADR-0108).
    pub fn command_result_phase(&self) -> Option<CommandPhase> {
        match &self.kind {
            MessageKind::CommandResult { phase, .. } => Some(*phase),
            _ => None,
        }
    }

    /// The render layout for this command row (ADR-0106): `Plain` when there
    /// is no result, `Inline` when a single-line reply fits beside the
    /// invocation, `Disclose` otherwise. A `Pending` row has no result yet and
    /// always classifies `Plain` — the phase owns its presentation until the
    /// reply settles. `available_width` is the row's usable columns.
    ///
    /// A present `sent_at_ms` renders a trailing `· HH:MM` (always 8 columns),
    /// so the method reserves that span before classifying — the free
    /// classifier stays purely shape-based and timestamp-blind.
    pub fn command_row_layout(&self, available_width: usize) -> Option<CommandRowLayout> {
        match &self.kind {
            MessageKind::CommandResult { result, phase, .. } => {
                if *phase == CommandPhase::Pending {
                    return Some(CommandRowLayout::Plain);
                }
                let usable = if self.sent_at_ms.is_some() {
                    available_width.saturating_sub(SENT_TIME_LABEL_COLS)
                } else {
                    available_width
                };
                Some(command_row_layout(result.as_deref(), &self.raw, usable))
            }
            _ => None,
        }
    }

    /// User-driven disclosure change: force `expanded` and mark it pinned so
    /// later transitions leave it alone.
    pub fn pin_command_result_expanded(&mut self, expanded: bool) {
        if let MessageKind::CommandResult {
            expanded: current,
            user_pinned,
            ..
        } = &mut self.kind
        {
            *current = expanded;
            *user_pinned = true;
        }
    }

    /// The invocation text shown on the collapsed command row (`/search foo`,
    /// `!ls -la`), from the message `raw`.
    pub fn command_result_summary(&self) -> Option<String> {
        if self.is_command_result() {
            Some(self.raw.clone())
        } else {
            None
        }
    }

    /// The typed result body text (ADR-0091 `to_text`), when the record
    /// carries a result.
    pub fn command_result_text(&self) -> Option<String> {
        match &self.kind {
            MessageKind::CommandResult {
                result: Some(result),
                ..
            } => Some(result.to_text()),
            _ => None,
        }
    }

    pub fn tool_step_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::ToolStep { expanded, .. } => Some(*expanded),
            _ => None,
        }
    }

    /// Auto/system disclosure setter: sets `expanded` **unless** the user has
    /// pinned the step (in which case it's a no-op). This is what lifecycle
    /// transitions (start / finish / cancel) and step creation call, so the
    /// derived default never fights a manual choice. User-driven toggles go
    /// through [`Self::pin_tool_step_expanded`].
    pub fn set_tool_step_expanded(&mut self, expanded: bool) {
        if let MessageKind::ToolStep {
            expanded: current,
            user_pinned,
            ..
        } = &mut self.kind
        {
            if *user_pinned {
                return;
            }
            *current = expanded;
            self.refresh_tool_step();
        }
    }

    /// User-driven disclosure change: force `expanded` and mark it pinned so
    /// later lifecycle transitions leave it alone.
    pub fn pin_tool_step_expanded(&mut self, expanded: bool) {
        if let MessageKind::ToolStep {
            expanded: current,
            user_pinned,
            ..
        } = &mut self.kind
        {
            *current = expanded;
            *user_pinned = true;
            self.refresh_tool_step();
        }
    }

    /// A tool step that spawns an envoy — the read-only `envoy` tool or the
    /// write-capable `envoy_code` tool. Such steps render as a compact,
    /// non-expandable line that navigates into a dedicated envoy view on
    /// activation (see the TUI focus stack) rather than expanding inline.
    pub fn is_envoy_task(&self) -> bool {
        matches!(
            &self.kind,
            MessageKind::ToolStep { name, .. } if name == "envoy" || name == "envoy_code"
        )
    }

    /// The bound envoy profile name (`explore` / `plan` / `verify` / …), used
    /// by the inline step's role badge. `None` until the `Started` event lands
    /// (or for non-envoy steps); the renderer falls back to a generic
    /// `[ENVOY]` badge then.
    pub fn envoy_profile(&self) -> Option<&str> {
        match &self.kind {
            MessageKind::ToolStep { profile, .. } => profile.as_deref(),
            _ => None,
        }
    }

    /// The call id of a tool step, used as the addressable identity of a
    /// envoy task for the focus stack.
    pub fn tool_step_call_id(&self) -> Option<&str> {
        match &self.kind {
            MessageKind::ToolStep { id, .. } => Some(id),
            _ => None,
        }
    }

    /// The nested child messages emitted by an envoy task. Returns `None`
    /// for non-tool-step messages.
    pub fn envoy_children(&self) -> Option<&[TranscriptMessage]> {
        match &self.kind {
            MessageKind::ToolStep { children, .. } => Some(children),
            _ => None,
        }
    }

    /// Mutable access to a tool step's child messages (used when the view is
    /// zoomed into an envoy and its children are the active message stream).
    pub fn envoy_children_mut(&mut self) -> Option<&mut Vec<TranscriptMessage>> {
        match &mut self.kind {
            MessageKind::ToolStep { children, .. } => Some(children),
            _ => None,
        }
    }

    /// The envoy's role (`explore` / `plan` / `verify` / …), identified by
    /// the `Started` event. `None` for non-task steps and before the role is
    /// known. The Envoy page header renders this as the `[ROLE]` tag between
    /// the `ENVOY` identity and the task title.
    pub fn envoy_role(&self) -> Option<String> {
        match &self.kind {
            MessageKind::ToolStep { profile, .. } => profile.clone(),
            _ => None,
        }
    }

    /// The envoy's task description (the `description` argument), truncated
    /// for display. Shown as the title of the Envoy page header.
    pub fn envoy_description(&self) -> String {
        let MessageKind::ToolStep { arguments, .. } = &self.kind else {
            return "Envoy".to_string();
        };
        let label = parse_arguments_kv(arguments)
            .into_iter()
            .find(|(k, _)| k == "description")
            .map(|(_, v)| v)
            .unwrap_or_else(|| "Envoy".to_string());
        truncate(&label, 48)
    }

    /// One-line live "peek" at the envoy's current activity, e.g.
    /// `running Grep "foo"  12s` or `running thinking  8s`. Shown as the
    /// step's second row while the envoy runs and replaced in place by
    /// [`Self::envoy_outcome_line`] when the step terminates. Returns `None`
    /// for non-task steps and for terminal steps (the outcome row owns the
    /// second row then). The elapsed timer is derived from `started_at` at
    /// render time, so the line stays fresh on every animation tick without
    /// storing any ticking state.
    pub fn envoy_status_line(&self) -> Option<String> {
        if !self.is_envoy_task() {
            return None;
        }
        let MessageKind::ToolStep {
            status,
            started_at,
            awaiting,
            activity,
            children,
            ..
        } = &self.kind
        else {
            return None;
        };
        if !status.is_running() {
            return None;
        }
        let elapsed = started_at.map(|started| {
            let ms = started.elapsed().as_millis() as u64;
            if ms < 1000 {
                format!("{}ms", ms)
            } else if ms < 60_000 {
                format!("{}s", ms / 1000)
            } else {
                duration_text(Some(ms))
            }
        });
        // A parked human-decision wait outranks replaying the last tool
        // activity: the envoy is blocked on the user, not making progress.
        // It keeps the bare phrase — no `running` prefix — because nothing
        // is moving while the envoy waits.
        let activity = if *awaiting {
            "awaiting approval".to_string()
        } else {
            let current = match children.last() {
                Some(child)
                    if child.is_tool_step()
                        && child.tool_step_status() == Some(ToolStepStatus::Running) =>
                {
                    // A tool step still in flight — name the tool so the
                    // row says *what* is being done, not just that the
                    // parent is busy.
                    Some(
                        child
                            .tool_step_summary()
                            .unwrap_or_else(|| "tool".to_string()),
                    )
                }
                // Assistant text has streamed but no tool call followed it:
                // the envoy is composing between tools. A bare `starting`
                // here read as "possibly stuck" during long model calls,
                // which is exactly what the `running` prefix disambiguates.
                Some(child) if child.role == Role::Assistant && !child.raw.is_empty() => {
                    Some("thinking".to_string())
                }
                // Nothing observable has landed yet. Prefer the envoy's own
                // reported activity (`waiting for model`, …) over the
                // generic `starting`: it proves the envoy is alive during
                // the model call that precedes the first child event.
                _ => activity.clone(),
            };
            match current {
                Some(current) => format!("running {current}"),
                None => "running".to_string(),
            }
        };
        // The activity and its elapsed time are same-rank metadata — plain
        // whitespace (R2 on the join ladder), never a `·` glyph.
        Some(match elapsed {
            Some(elapsed) => format!("{activity}  {elapsed}"),
            None => activity,
        })
    }

    /// One-line outcome replacing the peek row once the envoy terminates: the
    /// first non-empty line of its conclusion (`ToolOutput::Envoy.summary`,
    /// falling back to the legacy `output` text for restored sessions).
    /// Returns `None` for non-task steps, running steps, and terminal steps
    /// with no conclusion text.
    pub fn envoy_outcome_line(&self) -> Option<String> {
        if !self.is_envoy_task() {
            return None;
        }
        let MessageKind::ToolStep {
            status,
            output,
            structured,
            ..
        } = &self.kind
        else {
            return None;
        };
        if status.is_running() {
            return None;
        }
        let source: &str = match structured.as_deref() {
            Some(muta_contracts::ToolOutput::Envoy { summary, .. }) => summary,
            _ => output.as_deref()?,
        };
        source
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string)
    }

    pub fn thinking(content: impl Into<String>) -> Self {
        let content = sanitize_text(&content.into()).into_owned();
        let mut message = Self {
            id: next_message_id(),
            role: Role::Assistant,
            blocks: Vec::new(),
            raw: String::new(),
            kind: MessageKind::Thinking {
                content: content.clone(),
                duration_ms: None,
                expanded: false,
                user_pinned: false,
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
        };
        message.raw = content;
        message.blocks = parse_blocks(&message.raw);
        message
    }

    pub fn is_thinking(&self) -> bool {
        matches!(self.kind, MessageKind::Thinking { .. })
    }

    /// Whether this is the one live provider-retry disclosure.
    pub fn is_provider_retry(&self) -> bool {
        matches!(self.kind, MessageKind::ProviderRetry { .. })
    }

    /// Construct transient provider-retry state. Callers should update this
    /// message in place via [`Self::update_provider_retry`] rather than append
    /// another one for the next failed attempt.
    pub fn provider_retry(
        attempt: usize,
        max_attempts: usize,
        delay: std::time::Duration,
        failure: impl Into<String>,
    ) -> Self {
        let failure = sanitize_text(&failure.into()).into_owned();
        Self {
            id: next_message_id(),
            role: Role::System,
            blocks: Vec::new(),
            raw: failure.clone(),
            kind: MessageKind::ProviderRetry {
                attempt,
                max_attempts,
                failure,
                retry_at: std::time::Instant::now() + delay,
                expanded: false,
                user_pinned: false,
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
        }
    }

    /// Refresh the existing retry disclosure for a later attempt while
    /// preserving the user's expanded/collapsed choice.
    pub fn update_provider_retry(
        &mut self,
        attempt: usize,
        max_attempts: usize,
        delay: std::time::Duration,
        failure: impl Into<String>,
    ) -> bool {
        let failure = sanitize_text(&failure.into()).into_owned();
        let MessageKind::ProviderRetry {
            attempt: current_attempt,
            max_attempts: current_max,
            failure: current_failure,
            retry_at,
            ..
        } = &mut self.kind
        else {
            return false;
        };
        *current_attempt = attempt;
        *current_max = max_attempts;
        *current_failure = failure.clone();
        *retry_at = std::time::Instant::now() + delay;
        self.raw = failure;
        true
    }

    pub fn provider_retry_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::ProviderRetry { expanded, .. } => Some(*expanded),
            _ => None,
        }
    }

    /// User-driven retry-detail disclosure change.
    pub fn pin_provider_retry_expanded(&mut self, expanded: bool) {
        if let MessageKind::ProviderRetry {
            expanded: current,
            user_pinned,
            ..
        } = &mut self.kind
        {
            *current = expanded;
            *user_pinned = true;
        }
    }

    /// Whether this message is a harness notice (error / turn-pause / status).
    pub fn is_notice(&self) -> bool {
        matches!(self.kind, MessageKind::Notice { .. })
    }

    pub fn notice_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::Notice { expanded, .. } => Some(*expanded),
            _ => None,
        }
    }

    pub fn pin_notice_expanded(&mut self, expanded: bool) {
        if let MessageKind::Notice {
            expanded: current,
            user_pinned,
            ..
        } = &mut self.kind
        {
            *current = expanded;
            *user_pinned = true;
        }
    }

    /// Construct a round-interrupt marker row (C11) from its durable record.
    /// The body is composed here (and re-composed identically on resume) so
    /// the live row and the restored row render byte-identically: the round
    /// number when known, the reason label, and nothing else — the timestamp
    /// renders from `sent_at_ms` as the trailing ` · HH:MM` chip.
    pub fn round_interrupted(record: muta_contracts::RoundInterrupt) -> Self {
        let raw = match record.round {
            Some(round) => format!("Interrupted · round {} · {}", round, record.reason.label()),
            None => format!("Interrupted · {}", record.reason.label()),
        };
        Self {
            id: next_message_id(),
            role: Role::System,
            blocks: parse_blocks(&raw),
            raw,
            kind: MessageKind::RoundInterrupt {
                record,
                user_pinned: false,
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
        }
    }

    /// Construct a notice message. Replaces the ad-hoc
    /// `TranscriptMessage::new(Role::System, format!("Error: …"))` pattern with
    /// a typed severity so the renderer can pick color/icon from one place.
    pub fn notice(severity: NoticeSeverity, raw: impl Into<String>) -> Self {
        // Notices receive transport and harness errors directly. HTTP proxy
        // bodies commonly use CRLF, and allowing the raw `\r` through to the
        // terminal moves its physical cursor back to column zero while the
        // retained grid still believes it advanced normally. The next diff
        // then paints over unrelated transcript cells. Keep this constructor
        // on the same sanitized boundary as ordinary and retry messages.
        let raw = sanitize_text(&raw.into()).into_owned();
        let blocks = parse_blocks(&raw);
        Self {
            id: next_message_id(),
            role: Role::System,
            blocks,
            raw,
            kind: MessageKind::Notice {
                severity,
                expanded: false,
                user_pinned: false,
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
        }
    }

    /// A reasoning trace that has not yet been stamped with a duration — i.e.
    /// its stream is still open. The renderer treats this as the "spinner
    /// should keep breathing" state, and `finalize_streaming_reasoning` uses
    /// it to find orphaned traces to freeze after an interrupt.
    pub fn is_thinking_streaming(&self) -> bool {
        matches!(
            self.kind,
            MessageKind::Thinking {
                duration_ms: None,
                ..
            }
        )
    }

    pub fn thinking_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::Thinking { expanded, .. } => Some(*expanded),
            _ => None,
        }
    }

    /// Auto/system disclosure setter — respects a user pin. See
    /// [`Self::set_tool_step_expanded`] for the rationale.
    pub fn set_thinking_expanded(&mut self, expanded: bool) {
        if let MessageKind::Thinking {
            expanded: current,
            user_pinned,
            ..
        } = &mut self.kind
        {
            if *user_pinned {
                return;
            }
            *current = expanded;
        }
    }

    /// User-driven disclosure change: force `expanded` and pin it.
    pub fn pin_thinking_expanded(&mut self, expanded: bool) {
        if let MessageKind::Thinking {
            expanded: current,
            user_pinned,
            ..
        } = &mut self.kind
        {
            *current = expanded;
            *user_pinned = true;
        }
    }

    pub fn set_thinking_duration(&mut self, duration_ms: u64) {
        if let MessageKind::Thinking { duration_ms: d, .. } = &mut self.kind {
            *d = Some(duration_ms);
        }
    }

    /// Human-readable summary for the reasoning trace (always one line).
    /// Reports **tokens** (ADR-0120) — the unit of what this thinking block
    /// costs against the context window, not a scalar count of the text.
    ///
    /// While the trace is still streaming (`duration_ms: None`) the token
    /// count is quantized to a bucket (`~`-prefixed) rather than exact: the
    /// streaming summary repaints on every render heartbeat, and an exact
    /// count would dirty the row for nearly every delta — the per-frame
    /// redraw churn the middle-component flicker is made of. A finished
    /// trace reports the exact count.
    pub fn thinking_summary(&self) -> Option<String> {
        let MessageKind::Thinking {
            content,
            duration_ms,
            ..
        } = &self.kind
        else {
            return None;
        };
        let tokens = muta_contracts::tokenizer::count_tokens(content);
        Some(match duration_ms {
            None => {
                // Bucket to steps that grow geometrically-ish: the label
                // changes O(log n) times over a trace instead of O(tokens).
                const BUCKETS: &[usize] = &[0, 25, 50, 100, 200, 350, 500, 750, 1000, 1500, 2000];
                let bucket = BUCKETS
                    .iter()
                    .rev()
                    .find(|&&edge| tokens >= edge)
                    .copied()
                    .unwrap_or(0);
                if bucket == 0 {
                    "Thinking · …".to_string()
                } else {
                    format!("Thinking · ~{bucket} tokens")
                }
            }
            Some(_) => format!(
                "Thinking · {tokens} tokens · {}",
                duration_text(*duration_ms)
            ),
        })
    }

    /// Human-readable header for the tool step (always one line).
    ///
    /// Shows only what the tool did and a duration suffix for finished
    /// states — the technical tool name lives inside the expanded body to
    /// reduce cognitive load.
    pub fn tool_step_summary(&self) -> Option<String> {
        let MessageKind::ToolStep {
            name,
            profile,
            arguments,
            status,
            duration_ms,
            ..
        } = &self.kind
        else {
            return None;
        };
        let summary = crate::tools::summary_for(name, arguments, profile.as_deref());
        Some(match status {
            ToolStepStatus::Running => summary,
            ToolStepStatus::Ok => format!("{} · {}", summary, duration_text(*duration_ms)),
            ToolStepStatus::Failed => {
                format!("{} · failed {}", summary, duration_text(*duration_ms))
            }
            ToolStepStatus::Denied => {
                format!("{} · denied {}", summary, duration_text(*duration_ms))
            }
            ToolStepStatus::Cancelled => {
                format!("{} · cancelled {}", summary, duration_text(*duration_ms))
            }
            ToolStepStatus::Interrupted => {
                format!("{} · interrupted {}", summary, duration_text(*duration_ms))
            }
        })
    }

    fn refresh_tool_step(&mut self) {
        let MessageKind::ToolStep {
            id: _,
            name,
            profile,
            arguments,
            output,
            structured: _,
            status,
            expanded,
            user_pinned: _,
            duration_ms,
            started_at: _,
            awaiting: _,
            activity: _,
            children: _,
        } = &self.kind
        else {
            return;
        };
        if *expanded {
            // Expanded tool-step bodies are rendered directly from the
            // structured data (see draw_tool_step), not from parsed
            // markdown. We still populate `blocks` so semantic selection and
            // copy work: block 0 = display arguments, block 1 = output.
            let kv = parse_arguments_kv(arguments);
            let display_args: String = kv
                .iter()
                .map(|(k, v)| format!("{}: {}", k, v))
                .collect::<Vec<_>>()
                .join("\n");
            self.raw = display_args.clone();
            let mut blocks = vec![Block::Text(Inline::plain(display_args))];
            if let Some(out) = output {
                self.raw.push_str("\n\n");
                self.raw.push_str(out);
                blocks.push(Block::Text(Inline::plain(out.clone())));
            }
            self.blocks = blocks;
        } else {
            let summary = crate::tools::summary_for(name, arguments, profile.as_deref());
            let suffix = match status {
                ToolStepStatus::Running => String::new(),
                ToolStepStatus::Ok => format!(" · {}", duration_text(*duration_ms)),
                ToolStepStatus::Failed => format!(" · failed {}", duration_text(*duration_ms)),
                ToolStepStatus::Denied => format!(" · denied {}", duration_text(*duration_ms)),
                ToolStepStatus::Cancelled => {
                    format!(" · cancelled {}", duration_text(*duration_ms))
                }
                ToolStepStatus::Interrupted => {
                    format!(" · interrupted {}", duration_text(*duration_ms))
                }
            };
            self.raw = format!("{}{}", summary, suffix);
            self.blocks = parse_blocks(&self.raw);
        }
    }

    /// Re-parse blocks from raw text (e.g. after streaming append).
    pub fn reparse(&mut self) {
        self.blocks = parse_blocks(&self.raw);
    }

    /// Append streaming text and re-parse.
    ///
    /// Parsing every accumulated chunk keeps the live layout structurally
    /// consistent with the final layout. The previous append-only Text block
    /// path delayed all Markdown structure until StreamEnd, causing the whole
    /// response to jump when headings, lists, and code fences were discovered.
    pub fn push_stream(&mut self, delta: &str) {
        self.raw.push_str(&sanitize_text(delta));
        self.reparse();
    }
}

/// Strip control characters (except \n, \t) to prevent Ratatui from rendering
/// them as block characters (█).
fn sanitize_text(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains(|c: char| c.is_control() && c != '\n' && c != '\t') {
        std::borrow::Cow::Owned(
            text.replace(|c: char| c.is_control() && c != '\n' && c != '\t', ""),
        )
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// Parse a JSON arguments string into ordered `(key, display_value)` pairs
/// suitable for compact rendering in the tool step body.
///
/// String values are shown unquoted; other JSON types keep their native
/// representation. Non-JSON input falls back to a single pair.
pub fn parse_arguments_kv(arguments: &str) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return vec![("raw".to_string(), arguments.trim().to_string())];
    };
    let Some(object) = value.as_object() else {
        return vec![("value".to_string(), arguments.trim().to_string())];
    };
    object
        .iter()
        .map(|(key, val)| {
            let display = match val {
                serde_json::Value::String(s) => s.clone(),
                _ => val.to_string(),
            };
            (key.clone(), display)
        })
        .collect()
}

fn duration_text(duration_ms: Option<u64>) -> String {
    match duration_ms {
        None => "...".to_string(),
        Some(ms) if ms < 1000 => format!("{}ms", ms),
        Some(ms) if ms < 60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        Some(ms) => {
            let total_secs = ms / 1000;
            let h = total_secs / 3600;
            let m = (total_secs % 3600) / 60;
            let s = total_secs % 60;
            if h > 0 {
                format!("{}h {}m", h, m)
            } else {
                format!("{}m {}s", m, s)
            }
        }
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{}...", prefix)
    } else {
        prefix
    }
}

/// Parse raw markdown-like text into semantic blocks.
///
/// This is intentionally lightweight — it splits on major block boundaries
/// (code fences, headings, rules, blockquotes) while preserving the original
/// text so copying yields exact source.
pub fn parse_blocks(text: &str) -> Vec<Block> {
    parse_blocks_markdown(text)
}

/// Parse plain-text input (user messages) into blocks without any markdown
/// interpretation. The entire text becomes a single [`Block::Text`] so it
/// renders as one continuous verbatim panel; line breaks are preserved by the
/// renderer's wrapper rather than being collapsed by a markdown parser.
fn parse_blocks_plain(text: &str) -> Vec<Block> {
    if text.is_empty() {
        return Vec::new();
    }
    vec![Block::Text(Inline::plain(text.to_string()))]
}

fn parse_blocks_markdown(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0;

    // Accumulator for a paragraph: the prose lines (already stripped of their
    // block-prefix), joined with soft-break→space / hard-break→`\n` rules.
    // Once a paragraph is flushed we scan the resulting string for inline
    // `code` / `**bold**` runs and record their byte ranges.
    let mut para: Vec<String> = Vec::new();
    let mut para_hard: Vec<bool> = Vec::new(); // hard-break before this line?

    // (List items are pushed directly during the list run — adjacent items
    // share no Break thanks to push_block's ListItem-pair rule.)

    let flush_para =
        |para: &mut Vec<String>, para_hard: &mut Vec<bool>, blocks: &mut Vec<Block>| {
            if para.is_empty() {
                return;
            }
            // Join lines: a soft break inserts a space; a hard break (the *previous*
            // line ended with a two-space marker) inserts a literal "\n".
            let mut content = String::new();
            for (idx, line) in para.iter().enumerate() {
                if idx > 0 {
                    content.push(if para_hard[idx - 1] { '\n' } else { ' ' });
                }
                content.push_str(line);
            }
            push_block(blocks, Block::Text(Inline::scanned(&content)));
            para.clear();
            para_hard.clear();
        };

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        // --- Fenced code block ------------------------------------------------
        if let Some(rest) = trimmed.strip_prefix("```") {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            let lang = rest.trim().to_string();
            let language = if lang.is_empty() { None } else { Some(lang) };
            let mut content = String::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(lines[i]);
                i += 1;
            }
            // skip closing fence (if present)
            if i < lines.len() {
                i += 1;
            }
            push_block(&mut blocks, Block::Code { language, content });
            continue;
        }

        // --- Display math block -----------------------------------------------
        if trimmed == "$$" || trimmed.starts_with("$$") || trimmed == "\\[" {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            let closing = if trimmed.starts_with("$$") {
                "$$"
            } else {
                "\\]"
            };
            let mut content = String::new();
            if let Some(rest) = trimmed.strip_prefix("$$") {
                if let Some(end) = rest.find("$$") {
                    content.push_str(rest[..end].trim());
                    i += 1;
                    push_block(&mut blocks, Block::Math { content });
                    continue;
                }
                let rest = rest.trim();
                if !rest.is_empty() {
                    content.push_str(rest);
                }
            }
            i += 1;
            while i < lines.len() {
                let candidate = lines[i].trim();
                if candidate == closing {
                    i += 1;
                    break;
                }
                if closing == "$$"
                    && let Some(end) = candidate.find("$$")
                {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(candidate[..end].trim_end());
                    i += 1;
                    break;
                }
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(lines[i].trim_end());
                i += 1;
            }
            push_block(&mut blocks, Block::Math { content });
            continue;
        }

        // --- Horizontal rule --------------------------------------------------
        if is_rule(trimmed) {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            push_block(&mut blocks, Block::Rule);
            i += 1;
            continue;
        }

        // --- Heading ----------------------------------------------------------
        if let Some((level, content_line)) = parse_heading(trimmed) {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            push_block(
                &mut blocks,
                Block::Heading {
                    level,
                    inline: Inline::scanned(content_line),
                },
            );
            i += 1;
            continue;
        }

        // --- Blockquote -------------------------------------------------------
        if let Some(content_line) = parse_quote(trimmed) {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            // Collect consecutive quote lines.
            let mut q_lines: Vec<String> = Vec::new();
            let mut q_hard: Vec<bool> = Vec::new();
            q_lines.push(content_line.to_string());
            q_hard.push(false);
            i += 1;
            while i < lines.len() {
                let t = lines[i].trim_start();
                if let Some(c) = parse_quote(t) {
                    let hard = q_lines.last().is_some_and(|line| line_ends_hard(line));
                    q_hard.push(hard);
                    q_lines.push(c.to_string());
                    i += 1;
                } else {
                    break;
                }
            }
            let mut content = String::new();
            for (idx, l) in q_lines.iter().enumerate() {
                if idx > 0 {
                    content.push(if q_hard[idx] { '\n' } else { ' ' });
                }
                content.push_str(l);
            }
            push_block(&mut blocks, Block::Quote(Inline::scanned(&content)));
            continue;
        }

        // --- List item --------------------------------------------------------
        if parse_list_item(trimmed).is_some() {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            // Collect consecutive list items as a group; push_block's
            // ListItem↔ListItem rule keeps them tight (no Break between).
            while i < lines.len() {
                let t = lines[i].trim_start();
                if let Some((m, c, ch)) = parse_list_item(t) {
                    push_block(
                        &mut blocks,
                        Block::ListItem {
                            inline: Inline::scanned(c),
                            ordered: m,
                            depth: 0,
                            checked: ch,
                        },
                    );
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }

        // --- Table (GFM: | ... | lines with a separator row) ------------------
        if trimmed.starts_with('|')
            && i + 1 < lines.len()
            && is_table_separator(lines[i + 1].trim())
        {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            let mut table = TableAccumulator::default();
            // Header row
            let header_cells = split_table_row(trimmed);
            table.header = header_cells.clone();
            // Alignment from separator
            table.aligns = parse_table_aligns(lines[i + 1].trim());
            i += 2;
            // Body rows
            while i < lines.len() {
                let t = lines[i].trim();
                if t.starts_with('|') && !is_table_separator(t) {
                    let cells = split_table_row(t);
                    table.rows.push(cells);
                    i += 1;
                } else {
                    break;
                }
            }
            // GFM tables define the column count from the header: a body row
            // with fewer cells is padded with empty cells, and a row with more
            // is truncated. Normalizing here establishes the invariant that
            // every row in `Block::Table` has exactly `headers.len()` cells, so
            // every consumer (live renderer, selection copy, hit-testing) can
            // index a row by column without per-access bounds checks. Without
            // this, a ragged body row panicked the adaptive renderer (index out
            // of bounds in `build_table_render`).
            normalize_table_rows(&table.header, &mut table.rows);
            let rendered = table.render();
            if !rendered.is_empty() {
                push_block(
                    &mut blocks,
                    Block::Table {
                        headers: table.header,
                        rows: table.rows,
                        aligns: table.aligns,
                        rendered,
                    },
                );
            }
            continue;
        }

        // --- Blank line: paragraph break -------------------------------------
        if trimmed.is_empty() {
            flush_para(&mut para, &mut para_hard, &mut blocks);
            i += 1;
            continue;
        }

        // --- Ordinary prose line ---------------------------------------------
        // A trailing two-space (or tab) marker is a hard line break. Strip it
        // from the stored text; the `para_hard` flag records that this line
        // ends in a hard break so the join inserts a literal "\n" before the
        // *next* line.
        let hard = line_ends_hard(line);
        let stored = trimmed.trim_end_matches([' ', '\t']);
        para.push(stored.to_string());
        para_hard.push(hard);
        i += 1;
    }

    flush_para(&mut para, &mut para_hard, &mut blocks);

    // Strip trailing Breaks (a trailing blank line should not produce one).
    while matches!(blocks.last(), Some(Block::Break)) {
        blocks.pop();
    }
    blocks
}

/// Whether a line is a thematic break (`---`, `***`, `___` with ≥3 same chars).
fn is_rule(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 3 {
        return false;
    }
    let Some(c) = s.chars().next() else {
        return false;
    };
    if c != '-' && c != '*' && c != '_' {
        return false;
    }
    s.chars().all(|ch| ch == c) && s.chars().count() >= 3
}

/// Parse a heading line `# title` … `###### title`. Returns `(level, content)`
/// where `content` still carries any inline formatting markers.
fn parse_heading(s: &str) -> Option<(u8, &str)> {
    let hashes = s.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &s[hashes..];
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    if rest.is_empty() && !s[..hashes].chars().all(|c| c == '#') {
        return None;
    }
    Some((hashes as u8, rest))
}

/// Parse a blockquote line `> text`. Supports `> text` and `>text`.
fn parse_quote(s: &str) -> Option<&str> {
    s.strip_prefix('>')
        .map(|rest| rest.strip_prefix(' ').unwrap_or(rest))
}

/// Parse a list-item line. Returns `(ordered_marker, content, checked)`.
/// `ordered_marker` is `Some(n)` for `N. `, `None` for bullet (`-`/`*`/`+ `).
/// `checked` is `Some(bool)` for task-list items `- [x]`/`- [ ]`.
fn parse_list_item(s: &str) -> Option<(Option<u64>, &str, Option<bool>)> {
    // Task list: - [x] / - [ ] / * [x] / + [ ]
    if let Some(after_bullet) = strip_bullet(s) {
        let after = after_bullet.trim_start_matches(' ');
        if let Some(rest) = after.strip_prefix("[") {
            let rest_first = rest.chars().next();
            let checked = match rest_first {
                Some('x') | Some('X') => Some(true),
                Some(' ') => Some(false),
                _ => None,
            };
            if checked.is_some()
                && let Some(content) = rest[1..].strip_prefix("]")
            {
                return Some((None, content.trim_start(), checked));
            }
        }
        return Some((None, after, None));
    }
    // Ordered list: 1. / 2. …
    if let Some((num, rest)) = parse_ordered(s) {
        let rest = rest.trim_start_matches(' ');
        // Ordered task list: 1. [x] (rare, but handle it)
        if let Some(r) = rest.strip_prefix("[") {
            let checked = match r.chars().next() {
                Some('x') | Some('X') => Some(true),
                Some(' ') => Some(false),
                _ => None,
            };
            if checked.is_some()
                && let Some(content) = r[1..].strip_prefix("]")
            {
                return Some((Some(num), content.trim_start(), checked));
            }
        }
        return Some((Some(num), rest, None));
    }
    None
}

/// Strip a bullet prefix (`-`/`*`/`+`), returning the remainder.
fn strip_bullet(s: &str) -> Option<&str> {
    if let Some(rest) = s.strip_prefix("- ") {
        Some(rest)
    } else if let Some(rest) = s.strip_prefix("* ") {
        Some(rest)
    } else if let Some(rest) = s.strip_prefix("+ ") {
        Some(rest)
    } else if let Some(rest) = s.strip_prefix("-\t") {
        Some(rest)
    } else {
        None
    }
}

/// Parse an ordered-list marker `N. ` or `N) `, returning `(N, remainder)`.
fn parse_ordered(s: &str) -> Option<(u64, &str)> {
    let digits_end = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits_end == 0 {
        return None;
    }
    let rest = &s[digits_end..];
    if let Some(after) = rest.strip_prefix(". ") {
        let n: u64 = s[..digits_end].parse().ok()?;
        return Some((n, after));
    }
    if let Some(after) = rest.strip_prefix(") ") {
        let n: u64 = s[..digits_end].parse().ok()?;
        return Some((n, after));
    }
    None
}

/// Whether a line ends with a hard break (≥2 trailing spaces). The two-space
/// marker is stripped from the content before this is called on the stored
/// string, so we check the *original* line; callers pass the raw line.
fn line_ends_hard(line: &str) -> bool {
    line.ends_with("  ") || line.ends_with("\t")
}

/// Is this line a GFM table separator (`| --- | :--: | ---: |`)?
fn is_table_separator(s: &str) -> bool {
    if !s.contains('-') {
        return false;
    }
    let stripped = s.trim_matches('|').trim();
    if stripped.is_empty() {
        return false;
    }
    // Each cell must contain at least one `-`, only `-`,`:`,and spaces.
    stripped.split('|').all(|cell| {
        let c = cell.trim();
        !c.is_empty() && c.contains('-') && c.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ')
    })
}

/// Parse alignment markers from a separator row into `TableAlignment`s.
fn parse_table_aligns(sep: &str) -> Vec<TableAlignment> {
    sep.trim_matches('|')
        .split('|')
        .map(|cell| {
            let c = cell.trim();
            let left = c.starts_with(':');
            let right = c.ends_with(':');
            match (left, right) {
                (true, true) => TableAlignment::Center,
                (true, false) => TableAlignment::Left,
                (false, true) => TableAlignment::Right,
                (false, false) => TableAlignment::None,
            }
        })
        .collect()
}

/// Split a `| a | b | c |` row into trimmed cell strings.
fn split_table_row(line: &str) -> Vec<String> {
    let line = line.trim();
    // Strip leading/trailing `|`.
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').unwrap_or(line);
    line.split('|').map(|c| c.trim().to_string()).collect()
}

/// Scan a prose string for inline code, bold, math, and links. Delimiters are
/// kept in `content`; renderers decide which marker bytes are visually elided.
pub fn scan_inline(content: &str) -> InlineScan {
    let bytes = content.as_bytes();
    let mut out = InlineScan::default();
    let mut i = 0usize;

    while i < bytes.len() {
        // Inline code: a run of backticks, closed by the same number. Nothing
        // inside code is scanned for math/links.
        if bytes[i] == b'`' {
            let tick_count = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            let close_start = i + tick_count;
            if let Some(rel) = find_backtick_run(&content[close_start..], tick_count) {
                let end = close_start + rel + tick_count;
                out.code_ranges.push((i, end));
                i = end;
                continue;
            }
        }

        if let Some((range, label_range, url)) = parse_markdown_link(content, i) {
            out.link_ranges.push(LinkRange {
                range,
                label_range,
                url,
            });
            i = range.1;
            continue;
        }
        if let Some((range, label_range, url)) = parse_tex_link(content, i) {
            out.link_ranges.push(LinkRange {
                range,
                label_range,
                url,
            });
            i = range.1;
            continue;
        }
        if let Some((start, end, url)) = parse_bare_url(content, i) {
            out.link_ranges.push(LinkRange {
                range: (start, end),
                label_range: (start, end),
                url,
            });
            i = end;
            continue;
        }

        // Inline math: `$…$` or `\(…\)`. Keep this after links so URLs with `$`
        // query fragments are not split before link detection gets a chance.
        if bytes[i] == b'$'
            && !starts_with_at(content, i, "$$")
            && let Some(rel) = content[i + 1..].find('$')
        {
            let end = i + 1 + rel + 1;
            if end > i + 2 {
                out.math_ranges.push((i, end));
                i = end;
                continue;
            }
        }
        if starts_with_at(content, i, "\\(")
            && let Some(rel) = content[i + 2..].find("\\)")
        {
            let end = i + 2 + rel + 2;
            if end > i + 4 {
                out.math_ranges.push((i, end));
                i = end;
                continue;
            }
        }

        // Bold: `**…**`.
        if bytes[i] == b'*'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'*'
            && let Some(rel) = content[i + 2..].find("**")
        {
            let end = i + 2 + rel + 2;
            out.bold_ranges.push((i, end));
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

fn starts_with_at(s: &str, i: usize, needle: &str) -> bool {
    s.as_bytes().get(i..i + needle.len()) == Some(needle.as_bytes())
}

fn parse_markdown_link(content: &str, i: usize) -> Option<ParsedLink> {
    if content.as_bytes().get(i) != Some(&b'[') {
        return None;
    }
    let label_end = i + 1 + content[i + 1..].find(']')?;
    let url_start = label_end + 1;
    if content.as_bytes().get(url_start) != Some(&b'(') {
        return None;
    }
    let url_end = url_start + 1 + content[url_start + 1..].find(')')?;
    let raw_url = content[url_start + 1..url_end].trim();
    let url = normalize_http_url(raw_url)?;
    Some(((i, url_end + 1), (i + 1, label_end), url))
}

fn parse_tex_link(content: &str, i: usize) -> Option<ParsedLink> {
    if starts_with_at(content, i, "\\url{") {
        let url_start = i + "\\url{".len();
        let url_end = url_start + content[url_start..].find('}')?;
        let url = normalize_http_url(content[url_start..url_end].trim())?;
        return Some(((i, url_end + 1), (url_start, url_end), url));
    }
    if starts_with_at(content, i, "\\href{") {
        let url_start = i + "\\href{".len();
        let url_end = url_start + content[url_start..].find('}')?;
        let after_url = url_end + 1;
        if content.as_bytes().get(after_url) != Some(&b'{') {
            return None;
        }
        let label_start = after_url + 1;
        let label_end = label_start + content[label_start..].find('}')?;
        let url = normalize_http_url(content[url_start..url_end].trim())?;
        return Some(((i, label_end + 1), (label_start, label_end), url));
    }
    None
}

fn parse_bare_url(content: &str, i: usize) -> Option<(usize, usize, String)> {
    if !(starts_with_at(content, i, "https://") || starts_with_at(content, i, "http://")) {
        return None;
    }
    let mut end = i;
    for (offset, ch) in content[i..].char_indices() {
        if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'') {
            break;
        }
        end = i + offset + ch.len_utf8();
    }
    while end > i
        && matches!(
            content.as_bytes()[end - 1],
            b'.' | b',' | b';' | b':' | b'!' | b'?' | b')' | b']'
        )
    {
        end -= 1;
    }
    let url = normalize_http_url(&content[i..end])?;
    Some((i, end, url))
}

fn normalize_http_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.starts_with("https://") || raw.starts_with("http://") {
        Some(raw.to_string())
    } else {
        None
    }
}

/// Find the byte offset of a run of exactly `n` backticks within `s`.
fn find_backtick_run(s: &str, n: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + n <= bytes.len() {
        if bytes[i..i + n].iter().all(|&b| b == b'`') {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Enforce the GFM table column-count invariant: the number of columns is
/// fixed by the header row, so every body row is normalized to exactly that
/// width — short rows are padded with empty cells, over-wide rows truncated.
/// Establishing this once at parse time lets every consumer index rows by
/// column without per-access bounds checks.
fn normalize_table_rows(header: &[String], rows: &mut [Vec<String>]) {
    let ncols = header.len();
    if ncols == 0 {
        // Degenerate: no columns to normalize against. Such a table yields an
        // empty render and is dropped by the caller, so the rows are unused.
        return;
    }
    for row in rows {
        if row.len() > ncols {
            row.truncate(ncols);
        } else if row.len() < ncols {
            row.resize(ncols, String::new());
        }
    }
}

#[derive(Default)]
struct TableAccumulator {
    aligns: Vec<TableAlignment>,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl TableAccumulator {
    /// Render the table as a GFM-style aligned grid using box-drawing borders.
    ///
    /// Columns are sized to their widest cell (intrinsic width) so vertical
    /// separators line up across all rows. The header is followed by a
    /// separator rule. Wide tables that exceed the viewport are handed to the
    /// renderer's normal line wrapping rather than being truncated.
    fn render(&self) -> String {
        if self.header.is_empty() {
            return String::new();
        }
        let ncols = self.header.len();
        let width = |cell: &str| display_width(cell);

        // Per-column intrinsic width: max of header and every body cell.
        // Rows are pre-normalized to `ncols` cells by `normalize_table_rows`,
        // so iterating in full here touches exactly one cell per column.
        let mut widths = vec![0usize; ncols];
        for (i, h) in self.header.iter().enumerate() {
            widths[i] = widths[i].max(width(h));
        }
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(width(cell));
            }
        }

        let join_borders = |sep: &str| -> String {
            widths
                .iter()
                .map(|w| "─".repeat(w + 2))
                .collect::<Vec<_>>()
                .join(sep)
        };

        let mut out = String::new();
        out.push_str(&format!("┌{}┐\n", join_borders("┬")));
        out.push_str(&format_row(&self.header, &widths, &self.aligns));
        out.push('\n');
        out.push_str(&format!("├{}┤\n", join_borders("┼")));
        for row in &self.rows {
            out.push_str(&format_row(row, &widths, &self.aligns));
            out.push('\n');
        }
        out.push_str(&format!("└{}┘", join_borders("┴")));
        out
    }
}

/// Format one table row as `│ cell │ cell │`, honoring per-column alignment.
fn format_row(cells: &[String], widths: &[usize], aligns: &[TableAlignment]) -> String {
    let ncols = widths.len();
    let parts: Vec<String> = (0..ncols)
        .map(|i| {
            let cell = cells.get(i).map(String::as_str).unwrap_or("");
            let align = aligns.get(i).copied().unwrap_or(TableAlignment::None);
            pad_cell(cell, widths[i], align)
        })
        .collect();
    format!("│ {} │", parts.join(" │ "))
}

fn pad_cell(cell: &str, width: usize, align: TableAlignment) -> String {
    let cell_w = display_width(cell);
    let pad = width.saturating_sub(cell_w);
    match align {
        TableAlignment::Right => format!("{}{}", " ".repeat(pad), cell),
        TableAlignment::Center => {
            let left = pad / 2;
            let right = pad - left;
            format!("{}{}{}", " ".repeat(left), cell, " ".repeat(right))
        }
        TableAlignment::None | TableAlignment::Left => format!("{}{}", cell, " ".repeat(pad)),
    }
}

fn display_width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// Drop ranges that fall entirely past `len` and clamp the end of any range
/// that straddles it (trim_end can only shrink trailing whitespace, so in
/// practice this is a no-op for interior code runs, but it keeps the invariant
/// `end <= content.len()` airtight).
fn clamp_ranges(ranges: &[CodeRange], len: usize) -> Vec<CodeRange> {
    ranges
        .iter()
        .map(|&(s, e)| (s.min(len), e.min(len)))
        .filter(|&(s, e)| s < e)
        .collect()
}

fn clamp_link_ranges(ranges: &[LinkRange], len: usize) -> Vec<LinkRange> {
    ranges
        .iter()
        .filter_map(|link| {
            let range = (link.range.0.min(len), link.range.1.min(len));
            let label_range = (link.label_range.0.min(len), link.label_range.1.min(len));
            (range.0 < range.1 && label_range.0 < label_range.1).then(|| LinkRange {
                range,
                label_range,
                url: link.url.clone(),
            })
        })
        .collect()
}

fn push_block(blocks: &mut Vec<Block>, block: Block) {
    if block.is_empty() && !matches!(block, Block::Rule | Block::Break) {
        return;
    }
    let needs_gap = blocks.last().is_some_and(|previous| {
        !matches!(
            (previous, &block),
            (Block::Break, _) | (Block::ListItem { .. }, Block::ListItem { .. })
        )
    });
    if needs_gap {
        blocks.push(Block::Break);
    }
    blocks.push(block);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_text() {
        let blocks = parse_blocks("Hello world");
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], Block::Text(inline) if inline.content == "Hello world"));
    }

    #[test]
    fn test_parse_code_block() {
        let text = "Some text\n\n```rust\nfn main() {}\n```\n\nMore text";
        let blocks = parse_blocks(text);
        assert_eq!(blocks.len(), 5);
        assert!(matches!(&blocks[0], Block::Text(inline) if inline.content == "Some text"));
        assert!(
            matches!(&blocks[2], Block::Code { language, content } if language.as_deref() == Some("rust") && content == "fn main() {}")
        );
        assert!(matches!(&blocks[4], Block::Text(inline) if inline.content == "More text"));
    }

    #[test]
    fn inline_code_keeps_its_backtick_quotes_in_prose() {
        // Inline code keeps its backtick delimiters in the flattened content
        // so the rendered/copied paragraph still shows the quotes, and the
        // renderer can paint the span on the code surface. This holds across
        // paragraph / heading / list item / quote contexts.
        let blocks = parse_blocks("Call the `read_text` tool.");
        assert!(matches!(
            &blocks[0],
            Block::Text(inline) if inline.content == "Call the `read_text` tool."
        ));

        // Heading.
        let blocks = parse_blocks("# Use `list_dir` for directories");
        assert!(matches!(
            &blocks[0],
            Block::Heading { level: 1, inline } if inline.content == "Use `list_dir` for directories"
        ));

        // List item.
        let blocks = parse_blocks("- item with `code` inside");
        assert!(matches!(
            &blocks[0],
            Block::ListItem { inline, .. } if inline.content == "item with `code` inside"
        ));

        // Blockquote.
        let blocks = parse_blocks("> quoted `code` span");
        assert!(matches!(
            &blocks[0],
            Block::Quote(inline) if inline.content == "quoted `code` span"
        ));

        // Multiple inline spans in one paragraph, mixed with emphasis.
        let blocks = parse_blocks("Mix `a` and `b` and plain.");
        assert!(matches!(
            &blocks[0],
            Block::Text(inline) if inline.content == "Mix `a` and `b` and plain."
        ));
    }

    /// Helper: find the byte range of the first `` `…` `` run in `s`, matching
    /// what the parser records, so the `code_ranges` assertions below can be
    /// written against the literal content rather than hand-counted offsets.
    fn code_ranges_of(s: &str) -> Vec<CodeRange> {
        let mut ranges = Vec::new();
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'`' {
                // find the closing backtick
                if let Some(rel) = s[i + 1..].find('`') {
                    ranges.push((i, i + 1 + rel + 1));
                    i = i + 1 + rel + 1;
                    continue;
                }
            }
            i += 1;
        }
        ranges
    }

    #[test]
    fn parses_inline_math_and_http_links_outside_code() {
        let text = "Use $x^2$ and [Rust](https://www.rust-lang.org), not `https://ignored.test`.";
        let blocks = parse_blocks(text);
        let Block::Text(inline) = &blocks[0] else {
            panic!("expected text block");
        };
        assert_eq!(inline.math_ranges, vec![(4, 9)]);
        assert_eq!(inline.code_ranges.len(), 1);
        assert_eq!(inline.link_ranges.len(), 1);
        assert_eq!(inline.link_ranges[0].label_range, (15, 19));
        assert_eq!(inline.link_ranges[0].url, "https://www.rust-lang.org");
    }

    #[test]
    fn parses_display_math_blocks() {
        let blocks = parse_blocks("Before\n\n$$\n\\int_0^\\infty e^{-x} dx = 1\n$$\n\nAfter");
        assert!(matches!(&blocks[0], Block::Text(inline) if inline.content == "Before"));
        assert!(
            matches!(&blocks[2], Block::Math { content } if content.contains("\\int_0^\\infty"))
        );
        assert!(matches!(&blocks[4], Block::Text(inline) if inline.content == "After"));
    }

    #[test]
    fn inline_code_records_byte_ranges_for_every_prose_context() {
        // Paragraph: the run is `read_text` including both backticks.
        let text = "Call the `read_text` tool.";
        let expected = code_ranges_of(text);
        let blocks = parse_blocks(text);
        let Block::Text(inline) = &blocks[0] else {
            panic!("expected Text block, got {:?}", blocks[0]);
        };
        assert_eq!(inline.content, text);
        assert_eq!(inline.code_ranges, expected);

        // Heading.
        let text = "Use `list_dir` for directories";
        let expected = code_ranges_of(text);
        let blocks = parse_blocks(&format!("# {text}"));
        let Block::Heading { inline, .. } = &blocks[0] else {
            panic!("expected Heading block, got {:?}", blocks[0]);
        };
        assert_eq!(inline.content, text);
        assert_eq!(inline.code_ranges, expected);

        // List item.
        let text = "item with `code` inside";
        let expected = code_ranges_of(text);
        let blocks = parse_blocks(&format!("- {text}"));
        let Block::ListItem { inline, .. } = &blocks[0] else {
            panic!("expected ListItem block, got {:?}", blocks[0]);
        };
        assert_eq!(inline.content, text);
        assert_eq!(inline.code_ranges, expected);

        // Blockquote.
        let text = "quoted `code` span";
        let expected = code_ranges_of(text);
        let blocks = parse_blocks(&format!("> {text}"));
        let Block::Quote(inline) = &blocks[0] else {
            panic!("expected Quote block, got {:?}", blocks[0]);
        };
        assert_eq!(inline.content, text);
        assert_eq!(inline.code_ranges, expected);

        // Multiple spans → multiple, non-overlapping, ordered ranges.
        let text = "Mix `a` and `b` and plain.";
        let expected = code_ranges_of(text);
        let blocks = parse_blocks(text);
        let Block::Text(inline) = &blocks[0] else {
            panic!("expected Text block");
        };
        assert_eq!(inline.code_ranges, expected);
    }

    #[test]
    fn test_push_stream() {
        let mut streamed = TranscriptMessage::new(Role::Assistant, "");
        for chunk in [
            "# Result\n\n",
            "First paragraph.\n\n",
            "- one\n",
            "- two\n\n",
            "```rust\nfn main() {}\n```",
        ] {
            streamed.push_stream(chunk);
        }

        let completed = TranscriptMessage::new(Role::Assistant, streamed.raw.clone());
        assert_eq!(streamed.blocks, completed.blocks);
    }

    #[test]
    fn parses_block_boundaries_without_collapsing_the_document() {
        let blocks = parse_blocks(
            "# Result\n\nFirst paragraph.\n\nSecond paragraph.\n\n1. one\n2. two\n\n> quoted",
        );

        assert!(matches!(
            &blocks[0],
            Block::Heading { level: 1, inline } if inline.content == "Result"
        ));
        assert!(blocks.iter().any(|block| matches!(block, Block::Break)));
        assert!(blocks.iter().any(
            |block| matches!(block, Block::Text(inline) if inline.content == "First paragraph.")
        ));
        assert!(blocks.iter().any(
            |block| matches!(block, Block::Text(inline) if inline.content == "Second paragraph.")
        ));
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::ListItem {
                inline,
                ordered: Some(1),
                ..
            } if inline.content == "one"
        )));
        assert!(
            blocks
                .iter()
                .any(|block| matches!(block, Block::Quote(inline) if inline.content == "quoted"))
        );
    }

    #[test]
    fn headings_are_visually_separated_from_following_body_text() {
        let blocks = parse_blocks("# Result\nFirst paragraph.");

        assert!(matches!(&blocks[0], Block::Heading { inline, .. } if inline.content == "Result"));
        assert!(
            matches!(&blocks[1], Block::Break),
            "heading-to-text boundaries should render with a blank row"
        );
        assert!(matches!(&blocks[2], Block::Text(inline) if inline.content == "First paragraph."));
    }

    #[test]
    fn markdown_soft_breaks_flow_but_hard_breaks_are_preserved() {
        let soft = parse_blocks("alpha bravo\ncharlie delta");
        assert!(matches!(
            &soft[0],
            Block::Text(inline) if inline.content == "alpha bravo charlie delta"
        ));

        let hard = parse_blocks("alpha bravo  \ncharlie delta");
        assert!(matches!(
            &hard[0],
            Block::Text(inline) if inline.content == "alpha bravo\ncharlie delta"
        ));
    }

    #[test]
    fn parses_task_lists_and_tables() {
        let blocks = parse_blocks(
            "- [x] done\n- [ ] next\n\n| Name | State |\n| --- | --- |\n| muta | ready |",
        );

        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::ListItem {
                checked: Some(true),
                inline,
                ..
            } if inline.content == "done"
        )));
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::ListItem {
                checked: Some(false),
                inline,
                ..
            } if inline.content == "next"
        )));
        let table = blocks.iter().find_map(|block| match block {
            Block::Table { headers, rows, .. } => Some((headers, rows)),
            _ => None,
        });
        let (headers, rows) = table.expect("table block present");
        assert_eq!(headers, &["Name".to_string(), "State".to_string()]);
        assert_eq!(rows, &[vec!["muta".to_string(), "ready".to_string()]]);

        // The rendered grid must align columns and separate the header from
        // the body, the regression that motivated reintroducing Block::Table.
        let rendered = blocks
            .iter()
            .find_map(|block| match block {
                Block::Table { rendered, .. } => Some(rendered.as_str()),
                _ => None,
            })
            .expect("rendered table text");
        assert!(rendered.contains("┌"), "missing top border: {rendered}");
        assert!(
            rendered.contains("├"),
            "missing header/body separator: {rendered}"
        );
        // Pipes must line up: the header and data rows share the same `│`
        // positions, so splitting on `│` yields the same number of pieces.
        let pipes = |line: &str| line.matches('│').count();
        let header_line = rendered.lines().nth(1).unwrap();
        let data_line = rendered.lines().nth(3).unwrap();
        assert_eq!(
            pipes(header_line),
            pipes(data_line),
            "header and body rows must align: {rendered}"
        );
    }

    #[test]
    fn table_alignment_and_uneven_cells_line_up() {
        let blocks =
            parse_blocks("| Tool | Count |\n| :--- | ---: |\n| read | 1 |\n| webfetch | 250 |");
        let rendered = blocks
            .iter()
            .find_map(|block| match block {
                Block::Table {
                    rendered, aligns, ..
                } => Some((rendered.as_str(), aligns.clone())),
                _ => None,
            })
            .expect("table block");
        let (rendered, aligns) = rendered;
        assert_eq!(
            aligns,
            vec![TableAlignment::Left, TableAlignment::Right],
            "alignment must be captured: {rendered}"
        );
        // Right-aligned numeric column: digits hug the right border, so the
        // single-digit "1" gets more left padding than "250" does.
        let data_lines: Vec<&str> = rendered.lines().skip(3).take(2).collect();
        assert!(
            data_lines[0].ends_with("│     1 │"),
            "got: {}",
            data_lines[0]
        );
        assert!(
            data_lines[1].ends_with("│   250 │"),
            "got: {}",
            data_lines[1]
        );
    }

    /// GFM fixes the table column count from the header, so every body row in
    /// a `Block::Table` must be normalized to exactly `headers.len()` cells:
    /// short rows padded with empty strings, over-wide rows truncated. This is
    /// the invariant the live renderer indexes against; a ragged row used to
    /// panic `build_table_render` with an out-of-bounds index.
    #[test]
    fn table_normalizes_ragged_body_rows_to_header_width() {
        // 2-column header; body rows have 2, 1, and 3 cells respectively.
        let blocks = parse_blocks("| A | B |\n|---|---|\n| 1 | 2 |\n| 3 |\n| 4 | 5 | 6 |");
        let (headers, rows) = blocks
            .iter()
            .find_map(|block| match block {
                Block::Table { headers, rows, .. } => Some((headers.clone(), rows.clone())),
                _ => None,
            })
            .expect("table block present");
        let ncols = headers.len();
        assert_eq!(ncols, 2, "header defines 2 columns");
        assert!(
            rows.iter().all(|row| row.len() == ncols),
            "every body row must be normalized to {ncols} cells, got {rows:?}"
        );
        // Short rows are padded with empty cells, the over-wide row truncated.
        assert_eq!(rows[0], vec!["1".to_string(), "2".to_string()]);
        assert_eq!(rows[1], vec!["3".to_string(), String::new()]);
        assert_eq!(rows[2], vec!["4".to_string(), "5".to_string()]);
    }

    #[test]
    fn tool_step_collapses_and_restores_full_semantic_detail() {
        let mut message =
            TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"README.md"}"#);
        // Collapsed running: human-readable summary only — no tool name.
        assert!(message.raw.contains("Read README.md"));
        assert!(!message.raw.contains("read_text"));

        assert!(message.finish_tool_step(
            "call_1",
            "contents",
            muta_contracts::ToolOutput::text("contents"),
            1234
        ));
        // Collapsed completed: summary + duration suffix.
        assert!(message.raw.contains("Read README.md"));
        assert!(message.raw.contains("1.2s"));
        message.set_tool_step_expanded(true);

        // Expanded: arguments as compact key-value text + output verbatim.
        assert!(message.raw.contains("path: README.md"));
        assert!(message.raw.contains("contents"));
    }

    #[test]
    fn envoy_task_is_detected_and_addressable() {
        let task = TranscriptMessage::tool_step(
            "call_42",
            "envoy",
            r#"{"description":"explore src","prompt":"..."}"#,
        );
        assert!(task.is_envoy_task());
        assert_eq!(task.tool_step_call_id(), Some("call_42"));
        assert_eq!(task.envoy_children().map(|c| c.len()), Some(0));
        assert_eq!(task.envoy_description(), "explore src");
        assert_eq!(task.envoy_role(), None);

        // A regular tool step is not an envoy task.
        let read = TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"a"}"#);
        assert!(!read.is_envoy_task());
        assert!(read.envoy_status_line().is_none());
    }

    #[test]
    fn envoy_started_event_labels_step_by_role() {
        // A `Started` event stamps the bound profile name on the step so the
        // page header can read the role out as its `[ROLE]` tag.
        let mut task = TranscriptMessage::tool_step(
            "call_7",
            "envoy",
            r#"{"description":"write the plan","prompt":"..."}"#,
        );
        assert_eq!(task.envoy_description(), "write the plan");
        assert_eq!(task.envoy_role(), None);
        assert!(task.push_envoy_event(&muta_contracts::EnvoyEvent::Started {
            profile: "explore".to_string()
        }));
        assert_eq!(task.envoy_role().as_deref(), Some("explore"));
        assert_eq!(task.envoy_description(), "write the plan");
        // The collapsed header carries only the description — the role is
        // shown by the renderer's `[PROFILE]` badge in front of it.
        let header = task.tool_step_summary().expect("summary");
        assert_eq!(header, "write the plan");
    }

    #[test]
    fn envoy_status_reflects_children_and_completion() {
        let mut task =
            TranscriptMessage::tool_step("call_9", "envoy", r#"{"description":"d","prompt":"p"}"#);

        // No children yet, still running — the peek row opens with the
        // generic `running` state until the envoy reports more.
        let running = task.envoy_status_line().expect("running status");
        assert!(running.starts_with("running"), "got: {running}");

        // A reported activity line (e.g. during the first model call) is
        // surfaced so the row reads as alive, not stuck on a bare state.
        task.push_envoy_event(&EnvoyEvent::Activity("waiting for model".into()));
        let waiting = task.envoy_status_line().expect("waiting status");
        assert!(
            waiting.starts_with("running waiting for model"),
            "got: {waiting}"
        );

        // Streaming assistant text => the peek row reports `thinking`.
        task.push_envoy_event(&EnvoyEvent::StreamStart { round: 1, turn: 0 });
        task.push_envoy_event(&EnvoyEvent::StreamDelta("partial".into()));
        let thinking = task.envoy_status_line().expect("thinking status");
        assert!(thinking.starts_with("running thinking"), "got: {thinking}");

        // An in-flight child tool call surfaces the tool's header.
        task.push_envoy_event(&EnvoyEvent::ToolCall {
            id: "inner".into(),
            name: "grep".into(),
            arguments: r#"{"pattern":"foo"}"#.into(),
            round: 1,
            turn: 0,
        });
        let running = task.envoy_status_line().expect("running status");
        assert!(running.contains("Grep"), "got: {running}");

        // Completing the parent hides the peek row; the outcome row takes over
        // with the envoy's one-line conclusion.
        assert!(task.finish_tool_step(
            "call_9",
            "final answer",
            muta_contracts::ToolOutput::text("final answer"),
            1500
        ));
        assert!(
            task.envoy_status_line().is_none(),
            "the peek row must disappear once the envoy terminates"
        );
        assert_eq!(
            task.envoy_outcome_line().as_deref(),
            Some("final answer"),
            "the outcome row carries the envoy's conclusion"
        );

        // Children are accessible for the dedicated envoy view.
        assert_eq!(task.envoy_children().map(|c| c.len()), Some(2));
    }

    #[test]
    fn envoy_failed_status_reports_failure() {
        let mut task =
            TranscriptMessage::tool_step("c", "envoy", r#"{"description":"d","prompt":"p"}"#);
        task.push_envoy_event(&EnvoyEvent::ToolCall {
            id: "i".into(),
            name: "bash".into(),
            arguments: "{}".into(),
            round: 1,
            turn: 0,
        });
        // The envoy failure is now signalled by the structured `failed`
        // flag on `ToolOutput::Envoy`, not by an "Error:" text prefix.
        let structured = muta_contracts::ToolOutput::Envoy {
            summary: "Error: boom".into(),
            messages: Vec::new(),
            usage: muta_contracts::TokenUsage::default(),
            generation_ms: 0,
            failed: true,
            interrupted: false,
        };
        assert!(task.finish_tool_step("c", structured.to_text(), structured, 100));
        assert!(
            task.envoy_status_line().is_none(),
            "a terminal envoy hides the peek row"
        );
        // The outcome row surfaces the error summary's first line.
        assert_eq!(task.envoy_outcome_line().as_deref(), Some("Error: boom"));
    }

    #[test]
    fn envoy_peek_reports_awaiting_approval_while_parked() {
        let mut task =
            TranscriptMessage::tool_step("c", "envoy", r#"{"description":"d","prompt":"p"}"#);
        task.push_envoy_event(&EnvoyEvent::ToolCall {
            id: "i".into(),
            name: "bash".into(),
            arguments: r#"{"command":"rm -rf x"}"#.into(),
            round: 1,
            turn: 0,
        });
        // The in-flight tool normally drives the peek row…
        let peek = task.envoy_status_line().unwrap();
        assert!(peek.starts_with("running Run rm"), "got: {peek}");

        // …but a parked permission request takes over the row: the envoy is
        // blocked on a human, not making progress.
        task.push_envoy_event(&EnvoyEvent::PermissionRequest(
            muta_contracts::PermissionRequest {
                id: "p1".into(),
                tool: "bash".into(),
                label: "Run rm".into(),
                description: String::new(),
                arguments: "{}".into(),
                scope: "workspace".into(),
                elevation: false,
                one_off: false,
            },
        ));
        let peek = task.envoy_status_line().unwrap();
        assert!(peek.starts_with("awaiting approval"), "got: {peek}");

        // The next progress event from the envoy clears the parked wait.
        task.push_envoy_event(&EnvoyEvent::ToolResult {
            id: "i".into(),
            name: "bash".into(),
            output: "done".into(),
            duration_ms: 3,
        });
        task.push_envoy_event(&EnvoyEvent::StreamStart { round: 1, turn: 0 });
        task.push_envoy_event(&EnvoyEvent::StreamDelta("…".into()));
        let peek = task.envoy_status_line().unwrap();
        assert!(peek.starts_with("running thinking"), "got: {peek}");
    }

    #[test]
    fn interrupted_envoy_status_reports_interrupted_not_failed() {
        let mut task =
            TranscriptMessage::tool_step("c", "envoy", r#"{"description":"d","prompt":"p"}"#);
        task.push_envoy_event(&EnvoyEvent::ToolCall {
            id: "i".into(),
            name: "read_text".into(),
            arguments: "{}".into(),
            round: 1,
            turn: 0,
        });
        task.push_envoy_event(&EnvoyEvent::ToolResult {
            id: "i".into(),
            name: "read_text".into(),
            output: "found 1 of 3 handlers".into(),
            duration_ms: 5,
        });
        // An interrupted envoy carries `interrupted: true, failed: false`:
        // the partial work was preserved, so it must classify as Interrupted
        // — never as Failed (it did not error) and never as Ok (it did not
        // finish).
        let structured = muta_contracts::ToolOutput::Envoy {
            summary: "Interrupted: stopped by the user".into(),
            messages: Vec::new(),
            usage: muta_contracts::TokenUsage::default(),
            generation_ms: 0,
            failed: false,
            interrupted: true,
        };
        assert!(task.finish_tool_step("c", structured.to_text(), structured, 100));
        assert_eq!(
            task.tool_step_status(),
            Some(ToolStepStatus::Interrupted),
            "an interrupted envoy classifies as Interrupted"
        );
        assert!(
            task.envoy_status_line().is_none(),
            "a terminal envoy hides the peek row"
        );
        assert_eq!(
            task.envoy_outcome_line().as_deref(),
            Some("Interrupted: stopped by the user"),
            "the outcome row carries the interruption summary"
        );
    }

    #[test]
    fn bash_failure_is_classified_failed_from_structured_exit_code() {
        // Regression: a bash failure emits `Exit N …` which does NOT start with
        // "Error", so the legacy text sniff misclassified it as `Ok`. With
        // structured `ToolOutput::Shell { exit: Some(1) }`, `is_error()` now
        // drives the classification and the step correctly reads `Failed`.
        let mut step = TranscriptMessage::tool_step("c", "bash", r#"{"command":"false"}"#);
        let structured = muta_contracts::ToolOutput::Shell {
            command: "false".into(),
            stdout: String::new(),
            stderr: "boom".into(),
            lines: Vec::new(),
            exit: Some(1),
            truncated: false,
            termination: muta_contracts::tool_output::ShellTermination::Exited,
        };
        let text = structured.to_text();
        assert!(
            !text.starts_with("Error"),
            "precondition: text is not Error-prefixed"
        );
        assert!(step.finish_tool_step("c", text, structured, 50));
        assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Failed));
    }

    #[test]
    fn bash_success_is_classified_ok() {
        let mut step = TranscriptMessage::tool_step("c", "bash", r#"{"command":"true"}"#);
        let structured = muta_contracts::ToolOutput::Shell {
            command: "true".into(),
            stdout: "ok\n".into(),
            stderr: String::new(),
            lines: Vec::new(),
            exit: Some(0),
            truncated: false,
            termination: muta_contracts::tool_output::ShellTermination::Exited,
        };
        let text = structured.to_text();
        assert!(step.finish_tool_step("c", text, structured, 5));
        assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Ok));
    }

    #[test]
    fn push_tool_stream_builds_interleaved_lines_for_live_view() {
        // L5: the streaming seed must populate `lines` (with the right stream
        // tag each) so the live view renders arrival-ordered, stderr-tinted,
        // interleaved output — not the all-stdout-then-all-stderr degraded
        // band the empty-`lines` fallback forced.
        use muta_contracts::{ToolStream, tool_output::ShellStream};
        let mut step = TranscriptMessage::tool_step("c", "bash", r#"{"command":"x"}"#);
        assert!(step.push_tool_stream("c", &ToolStream::Stdout("Compiling a\n".into())));
        assert!(step.push_tool_stream("c", &ToolStream::Stderr("warning: b\n".into())));
        assert!(step.push_tool_stream("c", &ToolStream::Stdout("Compiling c\n".into())));

        let lines = match &step.kind {
            MessageKind::ToolStep {
                structured: Some(b),
                ..
            } => match b.as_ref() {
                muta_contracts::ToolOutput::Shell { lines, .. } => lines,
                _ => panic!("expected Shell"),
            },
            _ => panic!("expected ToolStep"),
        };
        assert_eq!(
            lines
                .iter()
                .map(|l| (l.stream, l.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (ShellStream::Out, "Compiling a"),
                (ShellStream::Err, "warning: b"),
                (ShellStream::Out, "Compiling c"),
            ],
            "streaming seed must preserve arrival order + stream tags"
        );
        // The flat strings stay populated too (model-facing path).
        match step.kind {
            MessageKind::ToolStep {
                structured: Some(b),
                ..
            } => match b.as_ref() {
                muta_contracts::ToolOutput::Shell { stdout, stderr, .. } => {
                    assert!(stdout.contains("Compiling a"));
                    assert!(stdout.contains("Compiling c"));
                    assert!(stderr.contains("warning: b"));
                }
                _ => unreachable!(),
            },
            _ => unreachable!(),
        }
    }

    #[test]
    fn cancel_tool_step_transitions_to_a_terminal_state() {
        let mut step = TranscriptMessage::tool_step("call_1", "websearch", r#"{"query":"rust"}"#);
        // Running -> Cancelled is a real terminal transition.
        assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Running));
        assert!(step.cancel_tool_step("call_1"));
        assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Cancelled));

        // The summary advertises the cancelled state instead of staying blank.
        let summary = step.tool_step_summary().expect("summary");
        assert!(summary.contains("cancelled"), "got: {summary}");
        // The raw (collapsed) transcript line mirrors the summary.
        assert!(step.raw.contains("cancelled"), "got: {}", step.raw);

        // Cancelled is terminal: a late result or another cancel is ignored.
        assert!(!step.finish_tool_step(
            "call_1",
            "late result",
            muta_contracts::ToolOutput::text("late result"),
            10
        ));
        assert!(!step.cancel_tool_step("call_1"));
        assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Cancelled));
    }

    #[test]
    fn cancel_only_acts_on_the_matching_call_id() {
        let mut step = TranscriptMessage::tool_step("call_1", "websearch", "{}");
        // A different id does nothing and leaves the step running.
        assert!(!step.cancel_tool_step("call_9"));
        assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Running));
    }

    #[test]
    fn cancelling_a_envoy_also_cancels_its_running_children() {
        let mut task =
            TranscriptMessage::tool_step("task_1", "envoy", r#"{"description":"d","prompt":"p"}"#);
        // A nested tool call still in flight.
        task.push_envoy_event(&EnvoyEvent::ToolCall {
            id: "inner".into(),
            name: "grep".into(),
            arguments: r#"{"pattern":"foo"}"#.into(),
            round: 1,
            turn: 0,
        });
        let children = task.envoy_children().expect("has children");
        assert_eq!(
            children[0].tool_step_status(),
            Some(ToolStepStatus::Running)
        );

        // Interrupting the parent task cancels it AND the nested running child,
        // so the envoy view never shows a stuck "running" step.
        assert!(task.cancel_tool_step("task_1"));
        assert_eq!(task.tool_step_status(), Some(ToolStepStatus::Cancelled));
        let children = task.envoy_children().expect("has children");
        assert_eq!(
            children[0].tool_step_status(),
            Some(ToolStepStatus::Cancelled),
            "nested child must converge with the parent"
        );

        // A cancelled envoy is terminal: the peek row disappears and the
        // outcome row falls back to the legacy output text (none was recorded
        // here, so the row hides entirely).
        assert!(task.envoy_status_line().is_none());
        assert!(task.envoy_outcome_line().is_none());
    }

    #[test]
    fn cancel_all_running_is_a_defensive_sweep_that_skips_terminal_steps() {
        let mut a = TranscriptMessage::tool_step("a", "read_text", "{}");
        let mut b = TranscriptMessage::tool_step("b", "read_text", "{}");
        // `b` already finished successfully; the sweep must not clobber it.
        assert!(b.finish_tool_step(
            "b",
            "contents",
            muta_contracts::ToolOutput::text("contents"),
            5
        ));
        assert_eq!(b.tool_step_status(), Some(ToolStepStatus::Ok));

        // The sweep cancels a running step and is then a no-op on it.
        assert!(a.cancel_all_running());
        assert!(!a.cancel_all_running());
        assert_eq!(a.tool_step_status(), Some(ToolStepStatus::Cancelled));
        // A finished step is untouched by the sweep.
        assert!(!b.cancel_all_running());
        assert_eq!(b.tool_step_status(), Some(ToolStepStatus::Ok));
    }

    #[test]
    fn notice_carries_severity_and_is_classified_as_notice() {
        let n = TranscriptMessage::notice(NoticeSeverity::Error, "boom");
        assert!(n.is_notice());
        assert!(matches!(
            n.kind,
            MessageKind::Notice {
                severity: NoticeSeverity::Error,
                ..
            }
        ));
        // The raw text is preserved verbatim for the renderer (no "Error: "
        // prefix injection — the glyph is the renderer's job).
        assert_eq!(n.raw, "boom");
        assert_eq!(n.notice_expanded(), Some(false));

        let mut mut_n = n.clone();
        mut_n.pin_notice_expanded(true);
        assert_eq!(mut_n.notice_expanded(), Some(true));

        // A text message is not a notice.
        let plain = TranscriptMessage::new(Role::Assistant, "hi");
        assert!(!plain.is_notice());
    }

    #[test]
    fn user_message_origin_defaults_to_chat_and_can_be_overridden() {
        // A plain user message is a genuine chat prompt by default.
        let chat = TranscriptMessage::new(Role::User, "fix the bug");
        assert_eq!(chat.origin, UserMessageOrigin::Chat);

        // Slash commands and shell passthroughs tag themselves so the
        // Activity modal does not mistake them for the driving prompt.
        let slash = TranscriptMessage::new(Role::User, "/review working-tree")
            .with_origin(UserMessageOrigin::Slash);
        assert_eq!(slash.origin, UserMessageOrigin::Slash);

        let shell =
            TranscriptMessage::new(Role::User, "!ls -la").with_origin(UserMessageOrigin::Shell);
        assert_eq!(shell.origin, UserMessageOrigin::Shell);

        // with_origin is idempotent and does not depend on the text: a
        // genuine chat prompt that happens to start with '/' stays Slash only
        // when explicitly tagged, never inferred from text here.
        let explicit_chat = TranscriptMessage::new(Role::User, "/etc is a path")
            .with_origin(UserMessageOrigin::Chat);
        assert_eq!(explicit_chat.origin, UserMessageOrigin::Chat);
    }

    #[test]
    fn provider_retry_updates_in_place_and_preserves_disclosure() {
        let mut retry = TranscriptMessage::provider_retry(
            2,
            4,
            std::time::Duration::from_secs(3),
            "first failure",
        );
        let id = retry.id;
        retry.pin_provider_retry_expanded(true);

        assert!(retry.update_provider_retry(
            3,
            4,
            std::time::Duration::from_secs(1),
            "second failure",
        ));
        assert_eq!(retry.id, id, "an update must retain transcript identity");
        assert_eq!(retry.provider_retry_expanded(), Some(true));
        assert_eq!(retry.raw, "second failure");
        assert!(matches!(
            retry.kind,
            MessageKind::ProviderRetry {
                attempt: 3,
                max_attempts: 4,
                ref failure,
                ..
            } if failure == "second failure"
        ));
    }

    #[test]
    fn notice_strips_terminal_controls_from_crlf_http_errors() {
        let n = TranscriptMessage::notice(
            NoticeSeverity::Error,
            "OpenAI HTTP 504: <html>\r\n<head>timeout</head>\x1b[2J\r\n</html>",
        );

        assert_eq!(
            n.raw,
            "OpenAI HTTP 504: <html>\n<head>timeout</head>[2J\n</html>"
        );
        assert!(!n.raw.chars().any(|c| c.is_control() && c != '\n'));
    }
}
