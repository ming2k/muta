//! Context-pressure accounting and relief.
//!
//! Cheap character/token estimates over message lists (used when a provider
//! does not report real usage) plus the policy that clears old `Tool`-role
//! results to relieve pressure while keeping the OpenAI `tool_call_id` chain
//! intact. Compaction thresholds are derived from the active model's context
//! window via [`CompactionPolicy`] / [`ContextBudget`].

use crate::tokenizer;
use crate::{Message, Role};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Prefix of the placeholder a *fully cleared* tool result is replaced with.
/// The full form carries a breadcrumb — `[cleared tool result: read foo.rs (42
/// lines, 1500 chars)]` — so the model can decide whether to re-fetch instead of
/// guessing, and so later passes recognise an already-cleared result.
pub const CLEARED_TOOL_PREFIX: &str = "[cleared tool result:";

/// Marker embedded in a *truncated* tool result (head + tail kept, middle
/// elided). Distinguishes the intermediate "truncated" tier from a full clear so
/// a later, higher-pressure pass can escalate truncation to a clear.
/// Reports **tokens** (ADR-0120).
const ELIDED_MARKER: &str = " tokens elided to relieve context ...]";

/// Token size of a tool result above which a prune candidate is first
/// *truncated* (a gentler tier that keeps the shape of the output) rather
/// than cleared outright. Below it, truncation would not save enough to be
/// worth the lost signal, so the candidate is cleared directly.
/// ≈ the old 2 000-byte gate for ASCII, 3× tighter for CJK-heavy output —
/// which is the point: CJK results were wrongly entering the gentle tier.
const TRUNCATE_MIN_TOKENS: usize = 512;

/// Tokens of head and of tail preserved when truncating a candidate.
const TRUNCATE_KEEP_EACH_SIDE_TOKENS: usize = 128;

/// Declarative context-compaction policy expressed as fractions of the active
/// model's full context window, plus a fallback window for models whose size
/// the registry does not know. Pure data; resolved into absolute token
/// thresholds for one concrete model by [`CompactionPolicy::resolve`].
///
/// Request pressure is estimated in tokens from prepared messages and tool
/// schemas, so these thresholds compare directly against a model's
/// token-denominated context window.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionPolicy {
    /// Trigger a full summarizing compaction once pressure reaches this fraction
    /// of the window. The remaining headroom absorbs finishing the current round
    /// and the summarization call itself, so a value near `1.0` risks overflow.
    pub utilization: f64,
    /// After a full compaction, compress the model window down to this fraction
    /// of the window. Lower values compact less often but deeper — the right
    /// tradeoff for an agentic loop that may run hundreds of rounds.
    pub target_utilization: f64,
    /// Trigger cheap tool-result pruning once pressure reaches this fraction,
    /// below `utilization`. Pruning keeps the tool-call id chain intact, so it
    /// is safe to run earlier and more often than a full compaction.
    pub prune_utilization: f64,
    /// Assumed window (tokens) when the active model's context window is unknown
    /// (the registry resolves to `0`). Conservative, so unknown / local models
    /// still relieve pressure instead of overflowing.
    pub fallback_window_tokens: usize,
    /// Number of recent complete user rounds preserved verbatim by full compaction.
    #[serde(alias = "compaction_preserve_rounds")]
    pub preserve_rounds: usize,
    /// Use the active model to produce an anchored, structured summary when
    /// compacting. When `false` (or when summarization fails) compaction falls
    /// back to deterministic message excerpts.
    #[serde(alias = "compaction_summarize")]
    pub summarize: bool,
    /// Enable cheap tool-result pruning (pre-turn and mid-turn) that clears old
    /// tool outputs in place to relieve context pressure before a full compaction.
    #[serde(alias = "compaction_prune")]
    pub prune: bool,
    /// Token budget of the most recent tool results protected from pruning.
    #[serde(alias = "compaction_prune_protect_tokens")]
    pub prune_protect_tokens: usize,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            utilization: 0.85,
            target_utilization: 0.25,
            prune_utilization: 0.65,
            fallback_window_tokens: 32_000,
            preserve_rounds: 6,
            summarize: true,
            prune: true,
            prune_protect_tokens: 6_000,
        }
    }
}

impl CompactionPolicy {
    /// Resolve into absolute token thresholds for a concrete model window.
    /// `window_tokens == 0` (unknown model) substitutes the fallback window so
    /// compaction still engages.
    pub fn resolve(&self, window_tokens: usize) -> ContextBudget {
        let window = if window_tokens == 0 {
            self.fallback_window_tokens
        } else {
            window_tokens
        }
        .max(1);
        let threshold = |fraction: f64| (window as f64 * fraction) as usize;
        ContextBudget {
            window_tokens: window,
            prune_threshold_tokens: threshold(self.prune_utilization),
            compaction_threshold_tokens: threshold(self.utilization),
            target_tokens: threshold(self.target_utilization),
        }
    }
}

/// Resolved, model-specific token thresholds — the runtime form of a
/// [`CompactionPolicy`] against one active model. Projected request pressure is
/// compared against these; content-level sizing (summary budgets, pruning
/// protect budgets) is derived from them in characters.
#[derive(Debug, Clone, Copy)]
pub struct ContextBudget {
    /// Full provider context window used to derive the thresholds (or the
    /// fallback value when the model's window is unknown).
    pub window_tokens: usize,
    /// Cheap tool-result pruning fires above this many tokens.
    pub prune_threshold_tokens: usize,
    /// A full summarizing compaction fires above this many tokens.
    pub compaction_threshold_tokens: usize,
    /// Post-compaction active-window target, in tokens.
    pub target_tokens: usize,
}

/// Wire-size (bytes) of a message list: byte length of `content` +
/// tool-call `name`+`arguments`. **Diagnostic only** (ADR-0120): context
/// pressure and every budget are token-denominated; bytes answer a transport
/// question ("how big is the payload"), not a context question.
/// `reasoning_content` is excluded (never sent to providers).
pub fn estimate_bytes(messages: &[Message]) -> usize {
    messages.iter().map(message_bytes).sum()
}

/// Token estimate of a message list using the native BPE tokenizer
/// (ADR-0117) with per-message chat framing overhead. This is the pressure
/// predictor: exact for `cl100k_base`-family models, a close approximation
/// for sibling encodings.
///
/// `reasoning_content` is excluded (never sent to providers), mirroring
/// `message_bytes`.
pub fn estimate_tokens(messages: &[Message]) -> usize {
    let mut tokens: i64 = 0;
    for m in messages {
        tokens += estimate_message_tokens(m);
        // Nested envoy transcripts are real session weight (persisted, replayed
        // on resume) so they count, just like `message_bytes` does.
        if let Some(children) = m.children.as_ref() {
            tokens += estimate_tokens(children) as i64;
        }
    }
    tokens.max(1) as usize
}

// NOTE: the provider-reported-usage path (ADR-0019/0023 "layered token
// accounting", `effective_pressure_tokens` / `USAGE_TRUST_FLOOR`) was removed
// as dead code: the `Provider` trait never surfaces usage, so the function had
// no production caller and only advertised a capability that does not exist.
// Pressure is computed purely from `estimate_tokens`. Revive a usage-preferring
// policy here once a provider actually reports `prompt_tokens`. See the deferral
// note in docs/adr/0023-relevance-aware-tiered-pruning-and-layered-token-accounting.md.

pub(crate) fn message_bytes(message: &Message) -> usize {
    let own = message.content.len()
        + message
            .tool_calls
            .as_ref()
            .map(|calls| {
                calls
                    .iter()
                    .map(|c| c.name.len() + c.arguments.len())
                    .sum::<usize>()
            })
            .unwrap_or(0);
    // Recursively count nested envoy transcripts. A `task` tool result
    // carries the envoy's full conversation as `children`, and that
    // conversation is real context weight the parent model is effectively
    // paying for (it sees the summary, but the children live in the same
    // session.json and survive resume — both the context-pressure meter and
    // the compaction budget must see them).
    let nested = message
        .children
        .as_ref()
        .map(|children| children.iter().map(message_bytes).sum::<usize>())
        .unwrap_or(0);
    own + nested
}

#[derive(Debug, Clone, Default)]
pub struct PruneOutcome {
    /// Number of tool-result messages whose content was cleared.
    pub cleared_count: usize,
    /// **Tokens** reclaimed by clearing (old content minus new content,
    /// measured with the exact BPE tokenizer).
    pub reclaimed_tokens: usize,
    /// Original (pre-clear) tool messages, oldest-first, for durable archival.
    pub originals: Vec<Message>,
}

/// Relieve context pressure by degrading older `Tool`-role results in place,
/// keeping the OpenAI `tool_call_id` chain intact. Mutates `messages`; returns
/// `Some(PruneOutcome)` only when at least `min_reclaim_tokens` would be
/// reclaimed, else `None` with `messages` untouched (atomic: nothing is mutated
/// unless the gate passes).
///
/// All budgets are **tokens** (ADR-0120): the recency-protection window and
/// the reclaim gate compare against exactly what the provider's context
/// window charges.
///
/// This is more than FIFO-by-age. For each candidate the policy chooses *what*
/// to prune and *how hard*:
///
/// - **Recency protection** keeps the most recent `protect_recent_tokens` of
///   tool output verbatim — that is what is usually still relevant.
/// - **Keep-alive** spares a fresh result whose file target is mentioned in the
///   last few non-tool messages (likely still in play).
/// - **Staleness / dedup** clears a result outright when a *later* tool
///   supersedes it on the same file: a mutation (`write`/`edit`), or a `read`
///   that fully re-covers its line range. Reads of *different* pages of one file
///   are complementary, not superseding, so paging never self-evicts — keeping
///   genuinely stale content is worse than clearing it, but evicting a live page
///   just makes the model re-read it.
/// - **Tiered degradation** truncates a large, fresh result to head + tail first
///   (a gentler tier that keeps its shape) and only fully clears it on a later,
///   higher-pressure pass — or immediately when it is already small.
/// - **Informative clears** replace content with `[cleared tool result: <label>
///   (<n> lines, <m> tokens)]` so the model can decide whether to re-fetch.
///
/// Idempotent: already-cleared results are skipped; a truncated result escalates
/// to a clear on a subsequent pass, so repeated calls converge.
pub fn prune_tool_results(
    messages: &mut [Message],
    protect_recent_tokens: usize,
    min_reclaim_tokens: usize,
) -> Option<PruneOutcome> {
    let plan = plan_prune(messages, protect_recent_tokens);
    let reclaimable: usize = plan.iter().map(|c| c.reclaim).sum();
    if plan.is_empty() || reclaimable < min_reclaim_tokens {
        return None;
    }
    Some(apply_prune(messages, plan, protect_recent_tokens))
}

/// One planned degradation: replace `messages[index].content` with
/// `new_content`, reclaiming `reclaim` chars of own content.
struct PrunePlan {
    index: usize,
    new_content: String,
    reclaim: usize,
}

/// Owned (mutation-safe) summary of the tool call that produced a result: a
/// short human label and the file path it targeted, correlated via
/// `tool_call_id`.
#[derive(Clone, Default)]
struct ToolMeta {
    label: String,
    file_key: Option<String>,
    /// For a *read*, the 1-based line range `[start, end)` it covered. `end` is
    /// `usize::MAX` for an open-ended read (no `limit`, i.e. to EOF). `None` for
    /// non-read file touches (write/edit). This is what makes staleness
    /// range-aware: two reads of *different pages* of one file no longer evict
    /// each other — only a later read that fully re-covers an earlier one (or a
    /// mutation) supersedes it.
    read_range: Option<(usize, usize)>,
    /// True when the call mutated the file (write/edit). A mutation invalidates
    /// every prior read of the same path regardless of range.
    mutates: bool,
}

impl ToolMeta {
    /// Does this (later) same-file result supersede an `earlier` one, making the
    /// earlier one stale? The caller guarantees both touched the same file.
    ///
    /// - A mutation supersedes any prior read (its content is now outdated).
    /// - A read supersedes an earlier read only when it fully **covers** the
    ///   earlier read's line range (a strict re-read / superset), so paging
    ///   through complementary regions of one file never self-evicts.
    /// - A non-read earlier result (`read_range == None`, e.g. a write
    ///   confirmation) keeps the legacy "any later same-file touch supersedes
    ///   it" behaviour — such results are tiny and outdated once re-touched.
    fn supersedes(&self, earlier: &ToolMeta) -> bool {
        match earlier.read_range {
            None => true,
            Some(earlier_range) => {
                self.mutates
                    || self
                        .read_range
                        .is_some_and(|later| range_covers(later, earlier_range))
            }
        }
    }
}

/// Whether `outer` fully contains `inner` (`outer.start <= inner.start` and
/// `inner.end <= outer.end`). Used to decide when a later read makes an earlier
/// read redundant.
fn range_covers(outer: (usize, usize), inner: (usize, usize)) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

/// Plan (without mutating) which tool results to degrade and how. Returns an
/// empty vec when there is nothing to do.
fn plan_prune(messages: &[Message], protect_recent_tokens: usize) -> Vec<PrunePlan> {
    let meta = collect_tool_meta(messages);
    let tools: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::Tool && !is_cleared(&m.content))
        .map(|(i, _)| i)
        .collect();
    if tools.is_empty() {
        return Vec::new();
    }

    // Recency protection: protect newest results until the token budget is
    // met. Token-denominated (ADR-0120) — the byte accumulator this replaced
    // under-protected CJK-heavy sessions by 3–4×.
    let mut protected: HashSet<usize> = HashSet::new();
    let mut protected_tokens = 0usize;
    for &i in tools.iter().rev() {
        if protected_tokens >= protect_recent_tokens {
            break;
        }
        protected_tokens += estimate_message_tokens(&messages[i]).max(0) as usize;
        protected.insert(i);
    }

    // Staleness: a read is stale only when a *later* same-file result supersedes
    // it — a mutation of the file, or a read that fully re-covers its line range
    // (see `ToolMeta::supersedes`). Reads of different pages are complementary,
    // not superseding, so paging through one large file never self-evicts —
    // closing the read/re-read oscillation that file-level (path-only) staleness
    // caused once the prune gate engaged.
    let mut plan = Vec::new();
    for (pos, &i) in tools.iter().enumerate() {
        if protected.contains(&i) {
            continue;
        }
        let meta_i = meta.get(&i).cloned().unwrap_or_default();
        let stale = meta_i.file_key.as_deref().is_some_and(|fk| {
            tools[pos + 1..].iter().any(|j| {
                meta.get(j).is_some_and(|meta_j| {
                    meta_j.file_key.as_deref() == Some(fk) && meta_j.supersedes(&meta_i)
                })
            })
        });
        // Keep-alive spares a *fresh* result whose file target is still in play.
        // A stale result is cleared even if mentioned, because its content is
        // outdated. "In play" means referenced *after* this result was produced
        // — by later natural language or a later tool call on the same file.
        // Looking forward from `i` (not at a global recent window) is what stops
        // a result's own originating call from self-referencing and sparing it.
        if !stale && mentioned_after(messages, i, meta_i.file_key.as_deref()) {
            continue;
        }
        let content = &messages[i].content;
        let new_content = degrade(content, &meta_i, stale);
        let content_tokens = tokenizer::count_tokens(content);
        let new_tokens = tokenizer::count_tokens(&new_content);
        if new_tokens >= content_tokens {
            continue; // no real gain
        }
        let reclaim = content_tokens - new_tokens;
        plan.push(PrunePlan {
            index: i,
            new_content,
            reclaim,
        });
    }
    plan
}

/// Apply a plan, recording originals for archival and recursing into any nested
/// envoy transcript on the messages it touches.
fn apply_prune(
    messages: &mut [Message],
    plan: Vec<PrunePlan>,
    protect_recent_tokens: usize,
) -> PruneOutcome {
    let mut outcome = PruneOutcome::default();
    for item in plan {
        outcome.originals.push(messages[item.index].clone());
        outcome.reclaimed_tokens += item.reclaim;
        outcome.cleared_count += 1;
        messages[item.index].content = item.new_content;
        messages[item.index].reasoning_content = None;
        // A `task` result carries the envoy's whole transcript as
        // `children`; its old `Tool` results are the same kind of bulky weight,
        // so prune them too (ungated — durability already happened when the
        // envoy finished; here we relieve in-memory pressure).
        if let Some(children) = messages[item.index].children.as_mut() {
            let child_plan = plan_prune(children, protect_recent_tokens);
            if !child_plan.is_empty() {
                let nested = apply_prune(children, child_plan, protect_recent_tokens);
                outcome.reclaimed_tokens += nested.reclaimed_tokens;
            }
        }
    }
    outcome
}

/// Choose the degraded form of a tool result's content.
fn degrade(content: &str, meta: &ToolMeta, stale: bool) -> String {
    // Stale (superseded on the same file) or already truncated -> clear fully.
    if stale || is_truncated(content) {
        return cleared_placeholder(meta, content);
    }
    // Large and fresh -> truncate (gentler). Small -> clear directly.
    if tokenizer::count_tokens(content) >= TRUNCATE_MIN_TOKENS {
        truncate_middle(content)
    } else {
        cleared_placeholder(meta, content)
    }
}

/// Correlate each `Tool` result with the assistant `tool_call` that produced it,
/// returning owned per-message-index metadata so later mutation is borrow-safe.
fn collect_tool_meta(messages: &[Message]) -> HashMap<usize, ToolMeta> {
    let mut by_id: HashMap<&str, (&str, &str)> = HashMap::new();
    for m in messages {
        if let Some(calls) = &m.tool_calls {
            for c in calls {
                by_id.insert(c.id.as_str(), (c.name.as_str(), c.arguments.as_str()));
            }
        }
    }
    let mut out = HashMap::new();
    for (i, m) in messages.iter().enumerate() {
        if m.role != Role::Tool {
            continue;
        }
        let meta = m
            .tool_call_id
            .as_deref()
            .and_then(|id| by_id.get(id))
            .map(|(name, args)| {
                let file_key = file_key(name, args);
                // Range/mutation classification only matters for file-addressed
                // calls (staleness is keyed on a shared file). Non-file tools
                // (bash, grep without a path) never enter the same-file scan.
                let (read_range, mutates) = if file_key.is_some() {
                    classify_file_touch(args)
                } else {
                    (None, false)
                };
                ToolMeta {
                    label: tool_label(name, args),
                    file_key,
                    read_range,
                    mutates,
                }
            })
            .unwrap_or_default();
        out.insert(i, meta);
    }
    out
}

/// Whether a tool result's file target is referenced in any message *after*
/// `index` — by full path or by its bare file name, in natural-language content
/// or in the arguments of a later tool call. This is the keep-alive signal: a
/// result whose target is still being talked about or re-touched is left intact.
/// Looking forward from `index` (rather than at a global recent window) is what
/// prevents a result's own originating call from self-referencing and sparing it.
fn mentioned_after(messages: &[Message], index: usize, file_key: Option<&str>) -> bool {
    let Some(fk) = file_key else {
        return false;
    };
    if fk.is_empty() {
        return false;
    }
    let base = fk.rsplit(['/', '\\']).next().unwrap_or(fk);
    let base_match = base != fk && !base.is_empty();
    for m in messages.iter().skip(index + 1) {
        if m.content.contains(fk) || (base_match && m.content.contains(base)) {
            return true;
        }
        if let Some(calls) = &m.tool_calls {
            for c in calls {
                if c.arguments.contains(fk) || (base_match && c.arguments.contains(base)) {
                    return true;
                }
            }
        }
    }
    false
}

fn is_cleared(content: &str) -> bool {
    content.starts_with(CLEARED_TOOL_PREFIX)
}

fn is_truncated(content: &str) -> bool {
    content.contains(ELIDED_MARKER)
}

fn parsed_args(arguments: &str) -> serde_json::Value {
    serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null)
}

fn arg_str<'a>(args: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
}

/// Short label for a tool call, e.g. `read src/main.rs`, `search_text "TODO"`, or just
/// `bash` when no salient argument is found.
fn tool_label(name: &str, arguments: &str) -> String {
    let args = parsed_args(arguments);
    match arg_str(
        &args,
        &[
            "path",
            "file_path",
            "file",
            "filename",
            "pattern",
            "query",
            "command",
            "cmd",
            "url",
        ],
    ) {
        Some(detail) => {
            let detail = detail.trim();
            let short: String = detail.chars().take(60).collect();
            if detail.chars().count() > 60 {
                format!("{name} {short}…")
            } else {
                format!("{name} {short}")
            }
        }
        None => name.to_string(),
    }
}

/// The file path a tool touched, used for staleness/dedup. `None` for tools that
/// are not file-addressed (e.g. `bash`, `search_text` without a file).
fn file_key(_name: &str, arguments: &str) -> Option<String> {
    let args = parsed_args(arguments);
    arg_str(&args, &["path", "file_path", "file", "filename"]).map(|s| s.to_string())
}

/// Classify a file-addressed tool call for staleness: `(read_range, mutates)`.
///
/// A call is a *mutation* when it carries write-shaped arguments (`content` for
/// `write`, `old_string`/`new_string` for `edit`) — keyed on arg shape, not tool
/// name, so it survives tool renames the same way [`file_key`] does. Otherwise it
/// is treated as a *read* covering the 1-based line range `[offset, offset+limit)`
/// — a missing/`0` `limit` means open-ended (to EOF), encoded as `usize::MAX`. A
/// read with neither field (`offset` defaults to 1) covers the whole file
/// `[1, MAX)`, which still supersedes/dedups other full reads exactly as before.
fn classify_file_touch(arguments: &str) -> (Option<(usize, usize)>, bool) {
    let args = parsed_args(arguments);
    let mutates = args.get("content").is_some()
        || args.get("new_string").is_some()
        || args.get("old_string").is_some();
    if mutates {
        return (None, true);
    }
    let offset = args
        .get("offset")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let end = if limit == 0 {
        usize::MAX
    } else {
        offset.saturating_add(limit)
    };
    (Some((offset, end)), false)
}

/// Informative cleared-placeholder carrying the tool label and the size that was
/// dropped, so the model can decide whether to re-fetch. The size is in
/// **tokens** (ADR-0120) — the model's own unit of context.
fn cleared_placeholder(meta: &ToolMeta, content: &str) -> String {
    let label = if meta.label.is_empty() {
        "tool"
    } else {
        meta.label.as_str()
    };
    let lines = content.lines().count().max(1);
    format!(
        "{CLEARED_TOOL_PREFIX} {label} ({lines} lines, {} tokens)]",
        tokenizer::count_tokens(content)
    )
}

/// Keep head + tail, eliding the middle with a recognisable marker. Returns the
/// content unchanged when it is too short for truncation to help. Head and tail
/// are bounded in **tokens** (ADR-0120): the cut lands on exact token
/// boundaries, so what survives costs precisely what the budget says.
fn truncate_middle(content: &str) -> String {
    let keep = TRUNCATE_KEEP_EACH_SIDE_TOKENS;
    let total = tokenizer::count_tokens(content);
    if total <= keep * 2 + 16 {
        return content.to_string();
    }
    let (head, head_tokens) = tokenizer::truncate_to_tokens(content, keep);
    let reversed: String = content.chars().rev().collect();
    let (tail_rev, _tail_tokens) = tokenizer::truncate_to_tokens(&reversed, keep);
    let tail: String = tail_rev.chars().rev().collect();
    let dropped = total - head_tokens - tokenizer::count_tokens(&tail);
    format!("{head}\n[... {dropped}{ELIDED_MARKER}\n{tail}")
}

pub fn estimate_message_tokens(message: &Message) -> i64 {
    let mut tokens = if message.content.is_empty() {
        0
    } else {
        tokenizer::count_tokens(&message.content) as i64
    };
    // Chat framing overhead per message (chatml-style `<|im_start|>role …
    // <|im_end|>` measured ≈ 4 tokens on cl100k_base); the first system
    // message carries an extra priming block in most templates.
    if !message.content.is_empty() || message.tool_calls.is_some() {
        tokens += 4;
    }
    if let Some(calls) = message.tool_calls.as_ref() {
        for c in calls {
            // Tool-call name is a known function identifier — a short ASCII
            // name is 1-3 tokens under BPE, same as before but exact now.
            if !c.name.is_empty() {
                tokens += tokenizer::count_tokens(&c.name) as i64;
            }
            // Count the semantic JSON values/keys while omitting punctuation
            // and transport envelopes. Malformed/incremental arguments fall
            // back to text tokenization rather than disappearing.
            if let Ok(arguments) = serde_json::from_str(&c.arguments) {
                tokens += estimate_semantic_json_tokens(&arguments);
            } else if !c.arguments.is_empty() {
                tokens += tokenizer::count_tokens(&c.arguments) as i64;
            }
            // Framing per tool call (open/close tags of the call structure).
            tokens += 2;
        }
    }
    tokens
}

pub fn estimate_string_tokens(s: &str) -> i64 {
    tokenizer::count_tokens(s) as i64
}

/// Estimated token shape of a provider request.
///
/// `history_tokens` is the prepared, non-system conversation, including any
/// skill messages injected for this request. `overhead_tokens` covers the
/// freshly composed system message and visible tool schemas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct RequestTokenEstimate {
    pub history_tokens: usize,
    pub overhead_tokens: usize,
    pub total_tokens: usize,
}

impl RequestTokenEstimate {
    pub fn new(history_tokens: usize, overhead_tokens: usize) -> Self {
        Self {
            history_tokens,
            overhead_tokens,
            total_tokens: history_tokens.saturating_add(overhead_tokens),
        }
    }
}

/// Estimate the tokens of a draft user prompt in the composer before sending.
pub fn estimate_draft_tokens(s: &str) -> usize {
    if s.is_empty() {
        0
    } else {
        // Message framing overhead ≈ 4 tokens
        tokenizer::count_tokens(s).saturating_add(4)
    }
}

/// Estimate the model-visible semantic content of structured JSON without
/// charging for transport-only JSON syntax or generic envelope keys.
///
/// Object property names and scalar values remain significant: tool argument
/// keys and JSON-Schema property names carry meaning for the model. Only the
/// braces, brackets, quotes, commas and repeated protocol/container labels are
/// omitted. Empty structures therefore contribute zero tokens.
pub fn estimate_semantic_json_tokens(value: &serde_json::Value) -> i64 {
    const ENVELOPE_KEYS: &[&str] = &[
        "$schema",
        "additionalProperties",
        "description",
        "enum",
        "function",
        "items",
        "name",
        "parameters",
        "properties",
        "required",
        "type",
    ];

    fn estimate(value: &serde_json::Value) -> i64 {
        let count = |s: &str| tokenizer::count_tokens(s) as i64;
        match value {
            serde_json::Value::Null => 0,
            serde_json::Value::Bool(value) => count(if *value { "true" } else { "false" }),
            serde_json::Value::Number(value) => count(&value.to_string()),
            serde_json::Value::String(value) => {
                if value.is_empty() {
                    0
                } else {
                    count(value)
                }
            }
            serde_json::Value::Array(values) => values.iter().map(estimate).sum(),
            serde_json::Value::Object(values) => values
                .iter()
                .map(|(key, value)| {
                    let key_tokens = if ENVELOPE_KEYS.contains(&key.as_str()) {
                        0
                    } else {
                        count(key)
                    };
                    key_tokens + estimate(value)
                })
                .sum(),
        }
    }

    estimate(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolCall;

    fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        }
    }

    /// Realistic Assistant message carrying a tool call — the way every Tool
    /// result is produced in an OpenAI-protocol transcript. The pruner correlates
    /// a result back to its call (name + arguments) via `tool_call_id`, so tests
    /// that exercise labels / staleness / keep-alive need this originating call
    /// to exist, not just the bare Tool result.
    fn assistant_with_call(id: &str, name: &str, args: &str) -> Message {
        let mut message = Message::new(Role::Assistant, "");
        message.tool_calls = Some(vec![call(id, name, args)]);
        message
    }

    #[test]
    fn large_fresh_result_is_truncated_then_cleared_on_next_pass() {
        // ~64 BPE tokens per 256 'Y's: 5 000 chars ≈ 1 250 tokens, well past
        // the 512-token truncate tier; head/tail keeps 128 tokens each side.
        let big = "Y".repeat(5_000);
        let mut messages = vec![
            Message::new(Role::User, "q1"),
            Message::tool_result(&call("c1", "bash", "{}"), big),
            Message::new(Role::User, "q2"),
        ];

        // Pass 1: large + fresh -> truncated (head/tail kept), not cleared.
        let out1 = prune_tool_results(&mut messages, 0, 1).unwrap();
        assert_eq!(out1.cleared_count, 1);
        assert!(is_truncated(&messages[1].content));
        assert!(!is_cleared(&messages[1].content));
        assert!(messages[1].content.len() < 5_000);

        // Pass 2: already truncated -> escalated to a full clear. The reclaim
        // gate must see a real gain: the truncated form still holds ~260
        // tokens (128 head + 128 tail + marker), the clear drops it to ~20.
        let out2 = prune_tool_results(&mut messages, 0, 1).unwrap();
        assert_eq!(out2.cleared_count, 1);
        assert!(is_cleared(&messages[1].content));

        // Pass 3: nothing left to do -> None (converged / idempotent).
        assert!(prune_tool_results(&mut messages, 0, 50).is_none());
    }

    #[test]
    fn short_result_is_cleared_directly_with_informative_placeholder() {
        // Below TRUNCATE_MIN_CHARS, so it skips the truncate tier and is cleared
        // outright — but still larger than the placeholder, so clearing reclaims
        // real space (a result shorter than its placeholder is correctly left
        // alone: clearing it would *grow* the window).
        let body = format!(
            "{}\n{}\n{}",
            "a".repeat(100),
            "b".repeat(100),
            "c".repeat(100)
        );
        let mut messages = vec![
            assistant_with_call("c1", "read", r#"{"path":"src/config.rs"}"#),
            Message::tool_result(&call("c1", "read", r#"{"path":"src/config.rs"}"#), body),
        ];

        let out = prune_tool_results(&mut messages, 0, 1).unwrap();
        assert_eq!(out.cleared_count, 1);
        // Informative: carries tool label, line count, and char count.
        assert!(messages[1].content.starts_with(CLEARED_TOOL_PREFIX));
        assert!(messages[1].content.contains("read src/config.rs"));
        assert!(messages[1].content.contains("3 lines"));
    }

    #[test]
    fn stale_read_superseded_by_later_edit_is_cleared_first() {
        let body = "X".repeat(3_000);
        let mut messages = vec![
            // Round 1: read config.rs ...
            assistant_with_call("c1", "read", r#"{"path":"config.rs"}"#),
            Message::tool_result(&call("c1", "read", r#"{"path":"config.rs"}"#), body.clone()),
            // ... superseded by a later edit of the same file (round 2).
            assistant_with_call("c2", "edit", r#"{"path":"config.rs"}"#),
            Message::tool_result(
                &call("c2", "edit", r#"{"path":"config.rs"}"#),
                "ok".to_string(),
            ),
        ];

        let out = prune_tool_results(&mut messages, 0, 1).unwrap();
        assert_eq!(out.cleared_count, 1);
        // The stale read (index 1) is cleared outright (not merely truncated),
        // because a later op touched the same file.
        assert!(is_cleared(&messages[1].content));
        assert!(messages[1].content.contains("read config.rs"));
    }

    #[test]
    fn paging_different_ranges_of_one_file_do_not_evict_each_other() {
        // The regression this fix targets: under prune pressure, reading two
        // *different* pages of one large file used to mark the earlier page
        // stale (path-only staleness), so the model lost it and re-read — an
        // oscillation. Different ranges are complementary, so neither is stale.
        let page1 = "A".repeat(3_000);
        let page2 = "B".repeat(3_000);
        let mut messages = vec![
            assistant_with_call(
                "c1",
                "read_text",
                r#"{"path":"big.rs","offset":1,"limit":800}"#,
            ),
            Message::tool_result(
                &call(
                    "c1",
                    "read_text",
                    r#"{"path":"big.rs","offset":1,"limit":800}"#,
                ),
                page1,
            ),
            assistant_with_call(
                "c2",
                "read_text",
                r#"{"path":"big.rs","offset":900,"limit":800}"#,
            ),
            Message::tool_result(
                &call(
                    "c2",
                    "read_text",
                    r#"{"path":"big.rs","offset":900,"limit":800}"#,
                ),
                page2,
            ),
        ];

        // Zero recency protection so nothing is spared by recency: the only thing
        // keeping page 1 alive is that page 2 does not supersede it. Both pages
        // are large and fresh, so the worst that happens is a gentle truncate —
        // never a full clear of a still-live page.
        let out = prune_tool_results(&mut messages, 0, 1).unwrap();
        assert!(
            !is_cleared(&messages[1].content),
            "page 1 must not be cleared by a read of a different page"
        );
        // Both are merely truncated (head/tail kept), not evicted.
        assert!(is_truncated(&messages[1].content) || messages[1].content.len() >= 3_000);
        assert!(out.cleared_count >= 1);
    }

    #[test]
    fn full_reread_covering_an_earlier_page_clears_it() {
        // A later read whose range fully covers an earlier read *does* supersede
        // it (a genuine re-read), so dedup still works for overlapping reads.
        let page = "A".repeat(3_000);
        let whole = "W".repeat(3_000);
        let mut messages = vec![
            assistant_with_call(
                "c1",
                "read_text",
                r#"{"path":"big.rs","offset":10,"limit":50}"#,
            ),
            Message::tool_result(
                &call(
                    "c1",
                    "read_text",
                    r#"{"path":"big.rs","offset":10,"limit":50}"#,
                ),
                page,
            ),
            // Open-ended read from line 1 covers [10,60) -> earlier page is stale.
            assistant_with_call("c2", "read_text", r#"{"path":"big.rs"}"#),
            Message::tool_result(&call("c2", "read_text", r#"{"path":"big.rs"}"#), whole),
        ];

        let out = prune_tool_results(&mut messages, 0, 1).unwrap();
        assert!(
            is_cleared(&messages[1].content),
            "the earlier page is fully re-covered, so it is stale and cleared"
        );
        assert!(out.cleared_count >= 1);
    }

    #[test]
    fn keep_alive_spares_a_result_mentioned_in_recent_messages() {
        let body = "Z".repeat(3_000);
        let mut messages = vec![
            assistant_with_call("c1", "read", r#"{"path":"important.rs"}"#),
            Message::tool_result(&call("c1", "read", r#"{"path":"important.rs"}"#), body),
            // A later user message references the file by name -> keep alive.
            Message::new(Role::User, "now fix the bug in important.rs please"),
        ];

        // Even with zero recency protection, the mentioned result is spared.
        assert!(prune_tool_results(&mut messages, 0, 1).is_none());
    }

    #[test]
    fn result_shorter_than_its_placeholder_is_left_alone() {
        // A result tinier than the informative placeholder that would replace it
        // yields negative reclaim — clearing it would *grow* the window. Such a
        // candidate is skipped entirely (no real gain), so it is left verbatim.
        let tiny = "ok".to_string();
        let mut messages = vec![Message::tool_result(&call("c1", "bash", "{}"), tiny)];
        assert!(prune_tool_results(&mut messages, 0, 1).is_none());
        assert_eq!(messages[0].content, "ok");
    }

    #[test]
    fn recency_protection_and_min_reclaim_gate() {
        let big = "Z".repeat(3_000);
        let mut messages = vec![Message::tool_result(&call("c", "bash", "{}"), big)];

        // Fully protected by a large recency budget -> None, untouched.
        assert!(prune_tool_results(&mut messages, 10_000, 1).is_none());
        // Reclaim floor larger than anything available -> None, untouched.
        assert!(prune_tool_results(&mut messages, 0, 1_000_000).is_none());
        assert!(!is_cleared(&messages[0].content));
        assert!(!is_truncated(&messages[0].content));
    }

    #[test]
    fn policy_resolves_thresholds_relative_to_window() {
        let policy = CompactionPolicy::default();
        let budget = policy.resolve(200_000);
        assert_eq!(budget.window_tokens, 200_000);
        assert_eq!(budget.prune_threshold_tokens, 130_000); // 65%
        assert_eq!(budget.compaction_threshold_tokens, 170_000); // 85%
        assert_eq!(budget.target_tokens, 50_000); // 25%
        // Pruning trips before full compaction, and compaction leaves a target
        // well below its trigger — the escalation ladder the harness relies on.
        assert!(budget.prune_threshold_tokens < budget.compaction_threshold_tokens);
        assert!(budget.target_tokens < budget.prune_threshold_tokens);
    }

    #[test]
    fn policy_falls_back_for_unknown_window() {
        let policy = CompactionPolicy::default();
        let budget = policy.resolve(0);
        assert_eq!(budget.window_tokens, 32_000);
        assert_eq!(budget.compaction_threshold_tokens, 27_200); // 85% of 32_000
    }

    #[test]
    fn policy_round_trips_through_serde_with_defaults() {
        // A config with no compaction table keeps the documented defaults.
        let policy: CompactionPolicy = toml::from_str("").unwrap();
        assert_eq!(policy, CompactionPolicy::default());

        // Explicit overrides survive a round-trip.
        let toml = r#"
            utilization = 0.9
            target_utilization = 0.2
            prune_utilization = 0.7
            fallback_window_tokens = 64_000
        "#;
        let policy: CompactionPolicy = toml::from_str(toml).unwrap();
        let budget = policy.resolve(100_000);
        assert_eq!(budget.prune_threshold_tokens, 70_000);
        assert_eq!(budget.compaction_threshold_tokens, 90_000);
    }

    #[test]
    fn removed_working_set_keys_are_ignored_for_compatibility() {
        let policy: CompactionPolicy =
            toml::from_str("max_active_tokens = 96000\nprompt_reserve_tokens = 8000\n").unwrap();

        assert_eq!(policy, CompactionPolicy::default());
        let serialized = toml::to_string(&policy).unwrap();
        assert!(!serialized.contains("max_active_tokens"));
        assert!(!serialized.contains("prompt_reserve_tokens"));
    }

    #[test]
    fn default_policy_uses_the_full_large_model_window() {
        let budget = CompactionPolicy::default().resolve(1_000_000);
        assert_eq!(budget.window_tokens, 1_000_000);
        assert_eq!(budget.prune_threshold_tokens, 650_000);
        assert_eq!(budget.compaction_threshold_tokens, 850_000);
        assert_eq!(budget.target_tokens, 250_000);
    }

    #[test]
    fn semantic_json_ignores_transport_syntax_but_keeps_domain_content() {
        let empty = serde_json::json!({"type": "object", "properties": {}});
        let meaningful = serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to read"}
            },
            "required": ["path"]
        });
        assert!(
            estimate_semantic_json_tokens(&empty) > 0,
            "schema type is semantic"
        );
        assert!(
            estimate_semantic_json_tokens(&meaningful) > estimate_semantic_json_tokens(&empty),
            "property names and descriptions must remain model-visible"
        );
        assert_eq!(estimate_semantic_json_tokens(&serde_json::json!({})), 0);
    }

    #[test]
    fn message_estimate_treats_empty_content_and_empty_arguments_as_framing_only() {
        let mut message = Message::new(Role::Assistant, "");
        message.tool_calls = Some(vec![crate::ToolCall {
            id: "call_1".into(),
            name: String::new(),
            arguments: "{}".into(),
        }]);
        // No content, no name, empty JSON object: only the per-message (4)
        // and per-tool-call (2) framing overhead remains.
        assert_eq!(estimate_message_tokens(&message), 6);
    }

    #[test]
    fn message_estimate_with_no_content_and_no_calls_is_zero() {
        let message = Message::new(Role::Assistant, "");
        assert_eq!(estimate_message_tokens(&message), 0);
    }

    #[test]
    fn estimate_tokens_excludes_children_recursion_is_consistent() {
        // A message with tool calls: the name (read_text) + JSON args +
        // content all get counted via the char-class path.
        let mut m = Message::new(Role::Tool, "读取结果：你好");
        m.tool_calls = None;
        let est = crate::estimate_tokens(&[m]);
        // CJK content alone is 6 glyphs; plus the ASCII prefix ~3 tokens.
        assert!(est >= 6, "got {est}");
    }
}
