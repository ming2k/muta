//! Semantic document model for the TUI.
//!
//! Unlike storing raw strings, this model preserves the structure of messages
//! so that selection and copy operate on semantic units (blocks) rather than
//! terminal grid characters.

use muta_contracts::{Role, RunnerEvent};

use crate::design::{COMMAND_CARD_LEAD_COLS, JOIN_ENUMERATE_COLS};
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
    /// work: the runner's partial transcript was preserved. Distinct from
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
        /// The bound runner profile name (`explore` / `plan` / `verify` / …)
        /// for an runner-spawning tool step, populated from the first
        /// `RunnerEvent::Started` and used to label the step by its role.
        /// `None` for non-runner steps, or until the `Started` event lands.
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
        /// its `Runner`/`Patch` variants) is large enough that an unboxed
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
        /// elapsed time while the call (or runner) is still running.
        /// `Instant` is cheap to capture at construction time and is not
        /// serialized — session restore reconstructs finished steps without it.
        started_at: Option<std::time::Instant>,
        /// Set when this runner surfaced a permission / user-input request that
        /// is still parked awaiting a human decision. The peek row reads it to
        /// show `awaiting approval` instead of the last tool activity, which
        /// would misleadingly suggest the runner is still making progress.
        /// Cleared by the next progress event from this runner (tool call,
        /// tool result, or streamed text) and on any terminal transition.
        awaiting: bool,
        /// Latest free-text activity line the runner reported via
        /// `RunnerEvent::Activity` (`waiting for model`, `waiting to retry
        /// (3s)`, …). The peek row prefers it over the derived
        /// `starting`/`thinking` fallbacks while no child event has landed
        /// yet, so a long model call reads as alive instead of stuck on
        /// `starting`. Not serialized — restored sessions render terminal
        /// steps, which never show a peek.
        activity: Option<String>,
        /// Child events emitted by an runner spawned from this tool step.
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
    ///
    /// [`NoticeParts`] carries the architecture-agreed split — *topic*
    /// (which subsystem is speaking, from the contract's
    /// `NoticeKind`/`NoticeSource` vocabulary) + *detail* (`title`/`body`) —
    /// so core notices render from structure instead of re-parsing `raw`.
    /// `None` for local/legacy notices; the renderer then falls back to its
    /// heuristic text parse.
    Notice {
        severity: NoticeSeverity,
        parts: Option<Box<NoticeParts>>,
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
    /// muted running tone (`⌘ /delegate`) — the input half of the component
    /// is already durable in the transcript, so a slow command never leaves
    /// the user wondering whether it ran.
    Pending,
    /// The typed result arrived (or is known not to exist — legacy folds,
    /// shell passthroughs): the row shows `invocation  reply` per its
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
    /// rendered inline on the same row as `invocation  reply`. No marker —
    /// there is no second view to disclose.
    Inline,
    /// A multi-line or long reply (`/search`, `/session status`, `/review`,
    /// …): the disclosure pattern is correct — a `+`/`-` header row that
    /// expands to the body.
    Disclose,
}

/// Width reserved for a trailing `HH:MM` timestamp on a command
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
    // The inline join is `invocation` + a two-column peer gap + reply; the row
    // must hold both without truncation for the reply to read as an
    // attribute, not a fragment. The card chrome (identity bar + marker slot
    // + glyph, ADR-0109) and the trailing timestamp eat into the same row, so
    // the budget subtracts them — but the time label is render-time state the
    // classifier cannot see, so the classifier subtracts only the fixed
    // chrome and the renderer's clamp guards the timestamp.
    let used = COMMAND_ROW_CHROME_COLS + invocation.width() + JOIN_ENUMERATE_COLS + text.width();
    if used <= available_width {
        CommandRowLayout::Inline
    } else {
        CommandRowLayout::Disclose
    }
}

/// The headline/detail split of an ack reply, when the record carries detail
/// lines. The title is the part worth the reader's first glance; the detail
/// is the muted explanation beneath it (ADR-0106's two-tone ack scheme).
pub fn command_ack_split(
    result: Option<&muta_contracts::CommandResult>,
) -> Option<(&str, &[String])> {
    let muta_contracts::CommandResult::Ack { title, detail } = result? else {
        return None;
    };
    let detail = detail.as_deref().filter(|d| !d.is_empty())?;
    Some((title, detail))
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

/// The two-part, architecture-agreed content of a notice entry.
///
/// A notice is not an opaque string: it is *who is speaking* plus *what
/// happened*. The topic is a predictable label from the contract vocabulary
/// (see [`notice_topic_label`]), and the detail is the structured
/// `title`/`body` pair carried by `AgentNotice` across the wire. Populated by
/// [`TranscriptMessage::notice_from_core`]; local/legacy notices leave it
/// absent and the renderer falls back to its heuristic text parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeParts {
    /// Predictable subsystem label ("trust", "provider", "turn guard",
    /// "review", "command") identifying the notice's origin. Rendered as the
    /// entry head in place of the generic "notification" constant.
    pub topic: Option<String>,
    /// Summary line (`AgentNotice::title`), rendered as the bold body lead.
    pub title: String,
    /// Optional detail prose (`AgentNotice::body`), rendered muted below the
    /// title with blank-line paragraph separators preserved.
    pub detail: Option<String>,
}

/// Map a contract notice kind to its user-facing topic label — the
/// predictable, architecture-agreed vocabulary for "what is speaking".
/// Each kind maps 1:1 to its topic; frontends may localize these, the
/// *kind* stays stable on the wire.
pub fn notice_topic_label(kind: muta_contracts::NoticeKind) -> &'static str {
    match kind {
        muta_contracts::NoticeKind::ProviderRetry => "provider",
        muta_contracts::NoticeKind::NudgeInjected => "turn guard",
        muta_contracts::NoticeKind::ReviewAlert => "review",
        muta_contracts::NoticeKind::TrustChanged => "trust",
        muta_contracts::NoticeKind::CommandAck => "command",
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
    pub(crate) fn scanned(content: &str) -> Self {
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
    /// A busy-Enter steer whose round ended — naturally or by an
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
    /// Steering input admitted at an inner turn boundary of a running round.
    Steer,
    /// Follow-up input executed after the current round completes.
    FollowUp,
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
    /// Correlation id for a busy-Enter steer (`AgentRequest::Steer`). Set when
    /// the entry is staged into the
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
    /// Durable provenance / injection origin stamped by the harness (ADR-0050).
    pub injection_origin: Option<muta_contracts::InjectionOrigin>,
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
            injection_origin: None,
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

    /// Correlate this message with a busy-Enter steer by its
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
            injection_origin: None,
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
        // An ack's newlines are its chosen structure (headline + muted detail
        // lines, ADR-0106), so it parses plain — the markdown parser's
        // soft-break rule would squeeze those lines onto one row. Every other
        // result keeps the markdown block renderer (lists, tables, code).
        let is_ack = matches!(result, Some(muta_contracts::CommandResult::Ack { .. }));
        Self {
            id: next_message_id(),
            // A harness artifact, not user or model prose — the renderer gives
            // it its own dimmed command-row treatment.
            role: Role::Tool,
            blocks: if is_ack {
                parse_blocks_plain(&result_text)
            } else {
                parse_blocks(&result_text)
            },
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
            injection_origin: None,
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
        // way the constructor derives it (acks parse plain — their newlines
        // are structure, not markdown soft breaks).
        let result_text = result.to_text();
        let is_ack = matches!(result, muta_contracts::CommandResult::Ack { .. });
        self.blocks = if is_ack {
            parse_blocks_plain(&result_text)
        } else {
            parse_blocks(&result_text)
        };
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
        // exit, an explicit `ToolOutput::Error`, a `failed` runner). The
        // legacy `starts_with("Error")` text fallback was removed once tool
        // error sites migrated to `ToolOutput::Error` and runners carried
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
            muta_contracts::ToolOutput::Runner {
                interrupted: true,
                ..
            }
        ) {
            // A cooperatively-drained runner: the user interrupted the turn,
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
            arguments,
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
            let cmd = parse_arguments_kv(arguments)
                .into_iter()
                .find(|(k, _)| k == "command")
                .map(|(_, v)| v)
                .unwrap_or_default();
            *structured = Some(Box::new(muta_contracts::ToolOutput::Shell {
                command: cmd,
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
    /// untouched and returns `false`. When the step is a `task` (runner),
    /// its still-running nested tool children are cancelled too, so an aborted
    /// runner never leaves a "running" child step behind.
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
    /// (used for runner children and as a defensive sweep). Returns `true`
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

    /// Append an runner event as a nested child of this tool step.
    ///
    /// Returns `true` if this message is a tool step and the event was stored.
    pub fn push_runner_event(&mut self, event: &RunnerEvent) -> bool {
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
            RunnerEvent::PermissionRequest(_)
            | RunnerEvent::UserQuestionRequest(_)
            | RunnerEvent::StdinRequest(_) => *awaiting = true,
            RunnerEvent::ToolCall { .. }
            | RunnerEvent::ToolResult { .. }
            | RunnerEvent::StreamStart { .. }
            | RunnerEvent::StreamDelta(_)
            | RunnerEvent::StreamEnd(_)
            | RunnerEvent::StreamReasoningStart { .. }
            | RunnerEvent::StreamReasoningDelta(_)
            | RunnerEvent::StreamReasoningEnd(_) => *awaiting = false,
            _ => {}
        }
        match event {
            // The runner announced its role — stamp it on the step so the
            // renderer can draw an `[RUNNER_EXPLORE]` / `[PLAN]` role badge in front
            // of the summary instead of a generic `[ENVOY]`.
            // No child message is produced.
            RunnerEvent::Started { profile: name } => {
                *profile = Some(name.clone());
            }
            RunnerEvent::StreamStart { round, turn } => {
                children.push(
                    TranscriptMessage::new(Role::Assistant, "")
                        .with_round(*round)
                        // `turn` is the runner's 0-indexed model-request
                        // position; the transcript's `turn` is 1-indexed.
                        .with_turn((*turn as u64) + 1),
                );
            }
            RunnerEvent::StreamDelta(delta) => {
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
            RunnerEvent::StreamEnd(content) => {
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
            // The runner's live reasoning chain, folded into the same
            // `MessageKind::Thinking` message a resumed session restores from
            // `reasoning_content` — so a live drill-in and a reloaded one show
            // the same children. Placement mirrors the wire order the child
            // emits (reasoning precedes its turn's assistant text and tool
            // calls), so the trace lands in the right turn band. Disclosed
            // chains only: the sender gates hidden-chain models out at the
            // source, so no phantom summary trace can appear here.
            RunnerEvent::StreamReasoningStart { round, turn } => {
                children.push(
                    TranscriptMessage::thinking("")
                        .with_round(*round)
                        .with_turn((*turn as u64) + 1),
                );
            }
            RunnerEvent::StreamReasoningDelta(delta) => {
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
            RunnerEvent::StreamReasoningEnd(content) => {
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
            RunnerEvent::ToolCall {
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
            RunnerEvent::ToolResult {
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
            RunnerEvent::Notice(notice) => {
                children.push(TranscriptMessage::notice_from_core(notice));
            }
            // The runner reported a free-text activity line (`waiting for
            // model`, `waiting to retry (3s)`). Stored for the peek row so a
            // stretch with no child events still reads as alive. No child
            // message is produced.
            RunnerEvent::Activity(text) => *activity = Some(text.clone()),
            // Full-duplex (ADR-0029): an runner surfaced a permission /
            // ask_user request up through the runner tool. The down-direction
            // reply (registry → handle → reply_permission / reply_user_question)
            // is wired at the agent layer; rendering the nested prompt in the
            // TUI and routing the user's answer back down is the harness↔TUI
            // integration step that follows. Until then these are observed but
            // not rendered as a nested child step (the request still reaches
            // the harness via the `RoundEvent::Runner` envelope, so a future
            // handler can attach without changing the event shape).
            RunnerEvent::PermissionRequest(_)
            | RunnerEvent::UserQuestionRequest(_)
            | RunnerEvent::StdinRequest(_) => {}
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
    /// A present `sent_at_ms` renders a trailing `HH:MM` timestamp,
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

    /// The typed result itself, when this row carries one.
    pub fn command_result_payload(&self) -> Option<&muta_contracts::CommandResult> {
        match &self.kind {
            MessageKind::CommandResult { result, .. } => result.as_deref(),
            _ => None,
        }
    }

    /// The headline/detail split for an ack reply (ADR-0106 two-tone ack):
    /// the title alone when there is no detail, `None` for non-acks.
    pub fn command_ack_split(&self) -> Option<(&str, &[String])> {
        command_ack_split(self.command_result_payload())
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

    /// A tool step that spawns an runner — the read-only `runner` tool or the
    /// write-capable `runner_code` tool. Such steps render as a compact,
    /// non-expandable line that navigates into a dedicated runner view on
    /// activation (see the TUI focus stack) rather than expanding inline.
    pub fn is_runner_task(&self) -> bool {
        matches!(
            &self.kind,
            MessageKind::ToolStep { name, .. }
                if matches!(
                    name.as_str(),
                    "spawn_runner" | "runner" | "runner_code" | "runner_mcp"
                )
        )
    }

    /// The bound runner profile name (`explore` / `plan` / `verify` / …), used
    /// by the inline step's role badge. `None` until the `Started` event lands
    /// (or for non-runner steps); the renderer falls back to a generic
    /// `[RUNNER]` badge then.
    pub fn runner_profile(&self) -> Option<&str> {
        match &self.kind {
            MessageKind::ToolStep { profile, .. } => profile.as_deref(),
            _ => None,
        }
    }

    /// The call id of a tool step, used as the addressable identity of a
    /// runner task for the focus stack.
    pub fn tool_step_call_id(&self) -> Option<&str> {
        match &self.kind {
            MessageKind::ToolStep { id, .. } => Some(id),
            _ => None,
        }
    }

    /// The nested child messages emitted by an runner task. Returns `None`
    /// for non-tool-step messages.
    pub fn runner_children(&self) -> Option<&[TranscriptMessage]> {
        match &self.kind {
            MessageKind::ToolStep { children, .. } => Some(children),
            _ => None,
        }
    }

    /// Mutable access to a tool step's child messages (used when the view is
    /// zoomed into an runner and its children are the active message stream).
    pub fn runner_children_mut(&mut self) -> Option<&mut Vec<TranscriptMessage>> {
        match &mut self.kind {
            MessageKind::ToolStep { children, .. } => Some(children),
            _ => None,
        }
    }

    /// The runner's role (`explore` / `plan` / `verify` / …), identified by
    /// the `Started` event. `None` for non-task steps and before the role is
    /// known. The Runner page header renders this as the `[ROLE]` tag between
    /// the `ENVOY` identity and the task title.
    pub fn runner_role(&self) -> Option<String> {
        match &self.kind {
            MessageKind::ToolStep { profile, .. } => profile.clone(),
            _ => None,
        }
    }

    /// The runner's task description (the `description` argument), truncated
    /// for display. Shown as the title of the Runner page header.
    pub fn runner_description(&self) -> String {
        let MessageKind::ToolStep { arguments, .. } = &self.kind else {
            return "Runner".to_string();
        };
        let label = parse_arguments_kv(arguments)
            .into_iter()
            .find(|(k, _)| k == "description")
            .map(|(_, v)| v)
            .unwrap_or_else(|| "Runner".to_string());
        truncate(&label, 48)
    }

    /// One-line live "peek" at the runner's current activity, e.g.
    /// `running Grep "foo"  12s` or `running thinking  8s`. Shown as the
    /// step's second row while the runner runs and replaced in place by
    /// [`Self::runner_outcome_line`] when the step terminates. Returns `None`
    /// for non-task steps and for terminal steps (the outcome row owns the
    /// second row then). The elapsed timer is derived from `started_at` at
    /// render time, so the line stays fresh on every animation tick without
    /// storing any ticking state.
    pub fn runner_status_line(&self) -> Option<String> {
        if !self.is_runner_task() {
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
        // activity: the runner is blocked on the user, not making progress.
        // It keeps the bare phrase — no `running` prefix — because nothing
        // is moving while the runner waits.
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
                // the runner is composing between tools. A bare `starting`
                // here read as "possibly stuck" during long model calls,
                // which is exactly what the `running` prefix disambiguates.
                Some(child) if child.role == Role::Assistant && !child.raw.is_empty() => {
                    Some("thinking".to_string())
                }
                // Nothing observable has landed yet. Prefer the runner's own
                // reported activity (`waiting for model`, …) over the
                // generic `starting`: it proves the runner is alive during
                // the model call that precedes the first child event.
                _ => activity.clone(),
            };
            // A transport wait (provider backoff) is a pause, not progress:
            // bare phrase, same rule as `awaiting approval` above — `running`
            // would falsely claim forward motion.
            match current {
                Some(current) if current.starts_with("waiting to retry") => current,
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

    /// One-line outcome replacing the peek row once the runner terminates: the
    /// first non-empty line of its conclusion (`ToolOutput::Runner.summary`,
    /// falling back to the legacy `output` text for restored sessions).
    /// Returns `None` for non-task steps, running steps, and terminal steps
    /// with no conclusion text.
    pub fn runner_outcome_line(&self) -> Option<String> {
        if !self.is_runner_task() {
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
            Some(muta_contracts::ToolOutput::Runner { summary, .. }) => summary,
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
            injection_origin: None,
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
            injection_origin: None,
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
    /// renders from `sent_at_ms` as right-aligned `HH:MM` metadata.
    pub fn round_interrupted(record: muta_contracts::RoundInterrupt) -> Self {
        let raw = match record.round {
            Some(round) => match record.reason {
                muta_contracts::RoundInterruptReason::User => {
                    format!("Round {round} — cancelled via [Esc Esc]")
                }
                muta_contracts::RoundInterruptReason::Superseded => {
                    format!("Round {round} — superseded by new message")
                }
                muta_contracts::RoundInterruptReason::Terminated => {
                    format!("Round {round} — process exited")
                }
            },
            None => match record.reason {
                muta_contracts::RoundInterruptReason::User => "Cancelled via [Esc Esc]".to_string(),
                muta_contracts::RoundInterruptReason::Superseded => {
                    "Superseded by new message".to_string()
                }
                muta_contracts::RoundInterruptReason::Terminated => "Process exited".to_string(),
            },
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
            injection_origin: None,
        }
    }

    /// Construct a notice message. Replaces the ad-hoc
    /// `TranscriptMessage::new(Role::System, format!("Error: …"))` pattern with
    /// a typed severity so the renderer can pick color/icon from one place.
    /// Leaves the structured parts unset — the renderer falls back to its
    /// heuristic parse of `raw`. Core (`AgentNotice`) notices should use
    /// [`Self::notice_from_core`] instead so the topic/title/detail split
    /// survives the boundary instead of being re-derived from text.
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
                parts: None,
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
            injection_origin: None,
        }
    }

    /// Construct a notice from its contract form, keeping the
    /// architecture-agreed two-part split intact: the topic label from
    /// [`notice_topic_label`] and the `title`/`body` detail pair. `raw` still
    /// carries the flattened `render_text()` form for copy fidelity and as
    /// the renderer's fallback, but the renderer never has to guess the
    /// split back out of it.
    pub fn notice_from_core(notice: &muta_contracts::AgentNotice) -> Self {
        // Same sanitized boundary as `raw`: provider HTTP bodies commonly
        // carry CRLF, and a raw `\r` reaching the grid moves the physical
        // cursor while the retained grid believes it advanced.
        let parts = NoticeParts {
            topic: Some(notice_topic_label(notice.kind).to_string()),
            title: sanitize_text(&notice.title).into_owned(),
            detail: notice
                .body
                .as_deref()
                .filter(|body| !body.trim().is_empty())
                .map(|body| sanitize_text(body).into_owned()),
        };
        Self::notice(
            notice_severity_from_core(notice.severity),
            notice.render_text(),
        )
        .with_notice_parts(parts)
    }

    /// Attach the structured topic/detail split to a notice. No-op on
    /// non-notice messages.
    pub fn with_notice_parts(mut self, parts: NoticeParts) -> Self {
        if let MessageKind::Notice { parts: slot, .. } = &mut self.kind {
            *slot = Some(Box::new(parts));
        }
        self
    }

    /// The structured topic/detail split, when this notice carries one.
    pub fn notice_parts(&self) -> Option<&NoticeParts> {
        match &self.kind {
            MessageKind::Notice { parts, .. } => parts.as_deref(),
            _ => None,
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
    /// Spray style: while the trace streams, the line leads with a live
    /// glyph (`✦`) and counts tokens up as they arrive (`✦ Thinking · 148
    /// tokens`), reading like a filling meter rather than an estimate.
    /// A finished trace settles into its final form and appends the
    /// duration (`Thinking · 1318 tokens · 2.4s`).
    ///
    /// Above [`Self::STREAM_COUNT_QUANTUM`] tokens the streamed count is
    /// floored to a multiple of that quantum rather than reported exactly:
    /// the streaming summary repaints on every render heartbeat, and a
    /// per-token count would dirty the row for nearly every delta — the
    /// per-frame redraw churn the middle-component flicker is made of.
    /// The floor keeps the label changes O(n ÷ quantum) while the number
    /// still climbs monotonically like a real count. A finished trace
    /// reports the exact count.
    pub fn thinking_summary(&self) -> Option<String> {
        /// Live-count floor applied while streaming (see method doc).
        const STREAM_COUNT_QUANTUM: usize = 25;
        /// Below this many tokens even the live count updates per token —
        /// the trace is short enough that per-token increments are rare
        /// relative to the render heartbeat.
        const STREAM_EXACT_UNDER: usize = 100;

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
                // Floor to the quantum once the count grows past the
                // per-token regime so the number climbs in visible steps
                // instead of strobing digit-by-digit every heartbeat.
                let shown = if tokens < STREAM_EXACT_UNDER {
                    tokens
                } else {
                    tokens - tokens % STREAM_COUNT_QUANTUM
                };
                format!("✦ Thinking · {shown} tokens")
            }
            Some(ms) => format!("Thinking · {tokens} tokens · {}", duration_text(Some(*ms))),
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
            ToolStepStatus::Ok => format!("{} ({})", summary, duration_text(*duration_ms)),
            ToolStepStatus::Failed => {
                format!("{} (failed {})", summary, duration_text(*duration_ms))
            }
            ToolStepStatus::Denied => {
                format!("{} (denied {})", summary, duration_text(*duration_ms))
            }
            ToolStepStatus::Cancelled => {
                format!("{} (cancelled {})", summary, duration_text(*duration_ms))
            }
            ToolStepStatus::Interrupted => {
                format!("{} (interrupted {})", summary, duration_text(*duration_ms))
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
                ToolStepStatus::Ok => format!(" ({})", duration_text(*duration_ms)),
                ToolStepStatus::Failed => format!(" (failed {})", duration_text(*duration_ms)),
                ToolStepStatus::Denied => format!(" (denied {})", duration_text(*duration_ms)),
                ToolStepStatus::Cancelled => {
                    format!(" (cancelled {})", duration_text(*duration_ms))
                }
                ToolStepStatus::Interrupted => {
                    format!(" (interrupted {})", duration_text(*duration_ms))
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
pub(crate) use super::markdown::{clamp_link_ranges, clamp_ranges, scan_inline};
pub use super::markdown::{parse_blocks, parse_blocks_plain};

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;
