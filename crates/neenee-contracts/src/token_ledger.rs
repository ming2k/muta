//! Token-source accounting: how many tokens came from authoritative upstream
//! usage reports vs. local estimates.
//!
//! When a provider reports real `usage` ([`crate::Provider::take_last_usage`] or
//! a [`crate::ProviderStreamEvent::Usage`]), the harness books those tokens as
//! **reported**. When it does not, the harness falls back to the local
//! char-class estimator ([`crate::estimate_tokens`]) and books them as
//! **estimated**. This module keeps a running tally so the UI can answer
//! "how accurate is my context meter?" and surface which providers/models
//! are measured vs. guessed.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Receiver of terminal request records for the durable cross-session usage
/// statistics (ADR-0122). Implemented by the persistence layer
/// (`neenee-persistence`'s `UsageStatsStore`); installed into a
/// [`TokenSourceLedger`] by the daemon bootstrap so every settled request is
/// mirrored into the day-partitioned store that survives session cleanup.
///
/// The sink must be non-blocking and non-fatal from the ledger's
/// perspective: implementations buffer or write synchronously at their own
/// discretion and swallow/report errors on their own channels — a stats
/// failure must never break request accounting.
pub trait UsageStatSink: Send + Sync {
    /// Called once per terminally settled request attempt. `recorded_at_ms`
    /// is the wall-clock settlement time; `project` is the project bucket
    /// name (empty = unknown).
    fn record_usage(&self, recorded_at_ms: u64, project: &str, record: &RequestUsageRecord);
}

/// Lifecycle state of one concrete provider request attempt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestUsageStatus {
    #[default]
    InFlight,
    Completed,
    Interrupted,
    Failed,
    /// Restored after a crash while the request was still marked in-flight.
    Abandoned,
}

impl RequestUsageStatus {
    pub fn is_terminal(self) -> bool {
        self != Self::InFlight
    }
}

/// Provenance of the counts attached to a request attempt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestUsageSource {
    #[default]
    Unknown,
    Reported,
    Estimated,
}

/// Stable identity of a concrete network attempt. A ReAct turn may have
/// multiple attempts when the transport retries; those attempts can each be
/// billed and therefore must never overwrite one another.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RequestUsageKey {
    pub session_id: String,
    #[serde(default = "default_request_actor")]
    pub actor_id: String,
    /// User-perceived exchange (ADR-0047 vocabulary).
    pub round: u64,
    /// Model request within the round.
    pub turn: u32,
    pub attempt: u32,
}

fn default_request_actor() -> String {
    "principal".to_string()
}

/// One request attempt's lifecycle and token accounting. This is the durable
/// fact from which provider/model and turn-level aggregates are derived.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestUsageRecord {
    pub key: RequestUsageKey,
    pub provider: String,
    pub model: String,
    pub status: RequestUsageStatus,
    pub source: RequestUsageSource,
    /// Estimate of the exact pre-wire request input. Kept even after reported
    /// usage arrives so the UI can explain estimate-vs-provider drift.
    pub projected_prompt_tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_read_tokens: i64,
    /// Milliseconds the provider spent *generating* this attempt — measured
    /// from request dispatch to a validated assistant response, so it excludes
    /// tool execution and human-decision pauses. Together with
    /// `completion_tokens` this yields the attempt's honest output rate
    /// (`completion_tokens / generation_ms`). `0` for in-flight attempts,
    /// attempts that failed before any response was validated, and records
    /// persisted before this field existed (they deserialize to the default).
    #[serde(default)]
    pub generation_ms: u64,
    /// Epoch timestamp in milliseconds when this attempt was dispatched.
    #[serde(default)]
    pub started_at_ms: u64,
    /// Detailed failure reason / error payload if this attempt failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RequestUsageRecord {
    /// Physically implausible *implied output rate* (tokens/sec), used to
    /// detect records poisoned by the quadratic `observe_output` bug (a
    /// stream that re-counted every early token once per later delta). Real
    /// models peak in the low hundreds of tok/s (the fastest rate ever
    /// observed across this install's reported records is 138 tok/s); 10 000
    /// is ~70× that and still orders of magnitude below the bug's output
    /// (up to 172 134 tok/s). Only a measured `generation_ms` can express a
    /// rate, so untimed records keep the absolute companion ceiling below.
    pub const IMPLAUSIBLE_TOKENS_PER_SECOND: f64 = 10_000.0;

    /// Companion absolute ceiling for records with no measured generation
    /// span (legacy rows, or a failure before the clock sealed): no single
    /// assistant response reaches eight figures in tokens.
    pub const IMPLAUSIBLE_COMPLETION_TOKENS: i64 = 10_000_000;

    /// Clamp a poisoned estimated completion count in place, returning
    /// whether the record was repaired. Only estimated records are touched
    /// (a provider-reported count is authoritative by definition, however
    /// surprising), and only when the count is physically impossible —
    /// either its implied tokens/sec rate or, without a measured span, its
    /// absolute size. The repaired shape is `total = prompt,
    /// completion = 0`: the honest statement for an interrupted attempt
    /// whose stream was never validated is "no trustworthy completion
    /// count" — which renders as a `–` rate — not a fabricated
    /// millions-strong figure.
    pub fn sanitize_poisoned_estimate(&mut self) -> bool {
        if self.source != RequestUsageSource::Estimated || self.completion_tokens <= 0 {
            return false;
        }
        let implausible = if self.generation_ms > 0 {
            // Implied rate vs the physical ceiling.
            (self.completion_tokens as f64) * 1000.0 / (self.generation_ms as f64)
                > Self::IMPLAUSIBLE_TOKENS_PER_SECOND
        } else {
            self.completion_tokens > Self::IMPLAUSIBLE_COMPLETION_TOKENS
                || self.total_tokens > Self::IMPLAUSIBLE_COMPLETION_TOKENS
        };
        if !implausible {
            return false;
        }
        self.completion_tokens = 0;
        self.total_tokens = self.prompt_tokens;
        true
    }

    fn totals(&self) -> TokenSourceTotals {
        match self.source {
            RequestUsageSource::Reported => TokenSourceTotals {
                reported_tokens: self.total_tokens,
                prompt_tokens: self.prompt_tokens,
                completion_tokens: self.completion_tokens,
                cache_write_tokens: self.cache_write_tokens,
                cache_read_tokens: self.cache_read_tokens,
                ..Default::default()
            },
            RequestUsageSource::Estimated => TokenSourceTotals {
                estimated_tokens: self.total_tokens,
                ..Default::default()
            },
            RequestUsageSource::Unknown => TokenSourceTotals::default(),
        }
    }

    fn as_turn(&self) -> TokenTurn {
        TokenTurn {
            round: self.key.round,
            turn: self.key.turn,
            reported: self.source == RequestUsageSource::Reported,
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            total_tokens: self.total_tokens,
            cache_write_tokens: self.cache_write_tokens,
            cache_read_tokens: self.cache_read_tokens,
        }
    }
}

/// One provider+model pair's accumulated token totals, split by source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSourceTotals {
    /// Tokens reported authoritatively by the provider's `usage` object.
    pub reported_tokens: i64,
    /// Tokens filled in by the local char-class estimator (provider reported
    /// no usage for those turns).
    pub estimated_tokens: i64,
    /// Reported input tokens (Anthropic: includes cache write+read). `0` for
    /// estimated turns, which carry no input/output split.
    pub prompt_tokens: i64,
    /// Reported output tokens. `0` for estimated turns.
    pub completion_tokens: i64,
    /// Tokens written to a prompt cache (Anthropic `cache_creation_input_tokens`
    /// — billed at a premium). A subset of `reported_tokens`, broken out so the
    /// report can show cache write volume and verify the breakpoints are
    /// creating cache entries.
    pub cache_write_tokens: i64,
    /// Tokens served from a prompt cache (Anthropic `cache_read_input_tokens` —
    /// billed at a ~0.1× discount). A subset of `reported_tokens`, broken out
    /// so the report can show cache hit volume (the payoff of caching).
    pub cache_read_tokens: i64,
}

impl TokenSourceTotals {
    /// Total tokens regardless of source.
    pub fn total(&self) -> i64 {
        self.reported_tokens + self.estimated_tokens
    }

    /// Accumulate another entry's counts into this one.
    fn add(&mut self, other: TokenSourceTotals) {
        self.reported_tokens += other.reported_tokens;
        self.estimated_tokens += other.estimated_tokens;
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
    }
}

/// One ReAct turn's token counts, kept per `(provider, model)` as a bill line
/// item. `round` identifies the enclosing user exchange; `turn` identifies the
/// model request within it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenTurn {
    /// 1-based user-round index. `0` means unknown/legacy.
    #[serde(default)]
    pub round: u64,
    /// 1-based model-request index within the round. `0` means unknown/legacy.
    #[serde(default)]
    pub turn: u32,
    /// `true` = authoritative provider usage; `false` = local char-class estimate.
    pub reported: bool,
    /// Reported input tokens (includes cache write+read for Anthropic).
    pub prompt_tokens: i64,
    /// Reported output tokens.
    pub completion_tokens: i64,
    /// Total tokens booked this turn.
    pub total_tokens: i64,
    /// Anthropic `cache_creation_input_tokens` for this turn.
    pub cache_write_tokens: i64,
    /// Anthropic `cache_read_input_tokens` for this turn.
    pub cache_read_tokens: i64,
}

/// Internal per-key accumulator: running totals plus the ordered line items.
#[derive(Debug, Default)]
struct Entry {
    totals: TokenSourceTotals,
    turns: Vec<TokenTurn>,
}

/// The key under which a provider+model's totals are accumulated: a
/// `(provider_id, model)` tuple so a session that switches providers or models
/// keeps each one's accuracy picture separate. Using a tuple (rather than a
/// `\u{1f}`-joined string) sidesteps any ambiguity when a provider/model value
/// happens to contain the separator.
fn key(provider: &str, model: &str) -> (String, String) {
    (provider.to_string(), model.to_string())
}

/// A thread-safe running ledger of token counts split by source (reported vs.
/// estimated), keyed by `(provider_id, model)`. Shared between the agent (the
/// writer — books each turn) and the TUI (the reader — renders the report).
#[derive(Default)]
pub struct TokenSourceLedger {
    /// `(provider, model)` → accumulator (totals + per-turn line items). A
    /// [`BTreeMap`] so the report iterates in a stable order.
    entries: Mutex<BTreeMap<(String, String), Entry>>,
    /// Lifecycle-aware request records. Legacy `record*` callers continue to
    /// use `entries`; production request accounting uses this keyed map so a
    /// terminal event updates exactly one attempt and duplicate events are
    /// idempotent.
    requests: Mutex<BTreeMap<RequestUsageKey, RequestUsageRecord>>,
    /// Session selected by the harness. `snapshot()` filters lifecycle records
    /// to this id, preventing usage from another opened session leaking into
    /// the current report.
    active_session: Mutex<Option<String>>,
    /// Optional durable mirror (ADR-0122): every terminally settled request
    /// is forwarded to this sink. `None` in tests / when no store is bound.
    usage_sink: Mutex<Option<Arc<dyn UsageStatSink>>>,
    /// Project bucket name stamped onto sink records (empty = unknown).
    usage_project: Mutex<String>,
}

impl std::fmt::Debug for TokenSourceLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSourceLedger")
            .field("active_session", &self.active_session())
            .field("usage_project", &self.usage_snapshot())
            .finish_non_exhaustive()
    }
}

impl TokenSourceLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// A cheap shared handle (the canonical way the agent and TUI share one
    /// ledger).
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    pub fn set_active_session(&self, session_id: impl Into<String>) {
        *self
            .active_session
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(session_id.into());
    }

    pub fn active_session(&self) -> Option<String> {
        self.active_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Install the durable usage-statistics sink (ADR-0122). Every
    /// terminally settled request attempt is forwarded to it from
    /// [`Self::settle_request`]. Replaces any prior sink.
    pub fn install_usage_sink(&self, sink: Arc<dyn UsageStatSink>) {
        *self.usage_sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    }

    /// Stamp the project bucket name forwarded with sink records. Called by
    /// the driver on session open/switch so records group by project.
    pub fn set_usage_project(&self, project: impl Into<String>) {
        *self.usage_project.lock().unwrap_or_else(|e| e.into_inner()) = project.into();
    }

    /// Current project bucket name (test/diagnostics).
    pub fn usage_snapshot(&self) -> String {
        self.usage_project
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Insert an in-flight request and allocate the next attempt number for
    /// its `(session, actor, round, turn)` tuple.
    pub fn begin_request(
        &self,
        session_id: &str,
        provider: &str,
        model: &str,
        round: u64,
        turn: u32,
        projected_prompt_tokens: i64,
    ) -> RequestUsageKey {
        self.begin_request_for_actor(
            session_id,
            "principal",
            provider,
            model,
            round,
            turn,
            projected_prompt_tokens,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn begin_request_for_actor(
        &self,
        session_id: &str,
        actor_id: &str,
        provider: &str,
        model: &str,
        round: u64,
        turn: u32,
        projected_prompt_tokens: i64,
    ) -> RequestUsageKey {
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        let attempt = requests
            .keys()
            .filter(|key| {
                key.session_id == session_id
                    && key.actor_id == actor_id
                    && key.round == round
                    && key.turn == turn
            })
            .map(|key| key.attempt)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let key = RequestUsageKey {
            session_id: session_id.to_string(),
            actor_id: actor_id.to_string(),
            round,
            turn,
            attempt,
        };
        requests.insert(
            key.clone(),
            RequestUsageRecord {
                key: key.clone(),
                provider: provider.to_string(),
                model: model.to_string(),
                status: RequestUsageStatus::InFlight,
                source: RequestUsageSource::Unknown,
                projected_prompt_tokens: projected_prompt_tokens.max(0),
                started_at_ms: now_epoch_ms(),
                ..Default::default()
            },
        );
        key
    }

    /// Terminally settle one attempt. Replaying the same event is harmless;
    /// authoritative reported usage can upgrade an estimate, but an estimate
    /// can never downgrade an already reported record. `generation_ms` is the
    /// attempt's provider-generation span (request dispatch → validated
    /// response; `0` when none was measured) and backs the per-attempt output
    /// rate shown by the Context Usage modal.
    pub fn settle_request(
        &self,
        key: &RequestUsageKey,
        status: RequestUsageStatus,
        usage: Option<crate::TokenUsage>,
        estimated_completion_tokens: i64,
        generation_ms: u64,
    ) {
        self.settle_request_with_error(
            key,
            status,
            usage,
            estimated_completion_tokens,
            generation_ms,
            None,
        );
    }

    /// Terminally settle one attempt with optional failure error payload.
    pub fn settle_request_with_error(
        &self,
        key: &RequestUsageKey,
        status: RequestUsageStatus,
        usage: Option<crate::TokenUsage>,
        estimated_completion_tokens: i64,
        generation_ms: u64,
        error: Option<String>,
    ) {
        if !status.is_terminal() {
            return;
        }
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        let Some(record) = requests.get_mut(key) else {
            return;
        };
        if record.status.is_terminal() && record.source == RequestUsageSource::Reported {
            return;
        }
        record.status = status;
        record.generation_ms = generation_ms;
        if error.is_some() {
            record.error = error;
        }
        if let Some(usage) = usage {
            record.source = RequestUsageSource::Reported;
            record.prompt_tokens = usage.prompt_tokens.max(0);
            record.completion_tokens = usage.completion_tokens.max(0);
            record.total_tokens = usage.total_tokens.max(0);
            record.cache_write_tokens = usage.cache_creation_input_tokens.max(0);
            record.cache_read_tokens = usage.cache_read_input_tokens.max(0);
        } else {
            record.source = RequestUsageSource::Estimated;
            record.prompt_tokens = record.projected_prompt_tokens.max(0);
            record.completion_tokens = estimated_completion_tokens.max(0);
            record.total_tokens = record
                .prompt_tokens
                .saturating_add(record.completion_tokens);
            // Belt-and-braces: a caller bug cannot be allowed to persist a
            // physically impossible streamed count (this exact class of bug
            // once booked 14.7M completion tokens for one interrupted
            // attempt, which rendered as a 130 050 tok/s rate). Silently
            // clamped — this crate carries no tracing dependency, and the
            // repair is visible in the report itself.
            record.sanitize_poisoned_estimate();
        }
        // Mirror the terminal record into the durable cross-session usage
        // store (ADR-0122). The sink owns its error handling; a stats failure
        // must never propagate into request accounting. Drop the requests
        // lock first so the sink (which may read the ledger) cannot
        // deadlock.
        let sink = self
            .usage_sink
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(sink) = sink {
            let project = self
                .usage_project
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let settled = record.clone();
            drop(requests);
            sink.record_usage(now_epoch_ms(), &project, &settled);
        }
    }

    /// Owned lifecycle records for one session, in stable request order.
    pub fn records_for_session(&self, session_id: &str) -> Vec<RequestUsageRecord> {
        let mut records = self
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|record| record.key.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|a, b| request_display_order(a).cmp(&request_display_order(b)));
        records
    }

    /// Replace one session's records from durable state. Any persisted
    /// in-flight request is crash residue and becomes `Abandoned` with an
    /// estimated prompt lower-bound before being exposed. Records persisted
    /// by the quadratic `observe_output` bug (see
    /// [`RequestUsageRecord::sanitize_poisoned_estimate`]) are repaired on
    /// load so a resumed session's report and rates stop showing the poison.
    pub fn restore_session(&self, session_id: &str, records: Vec<RequestUsageRecord>) {
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        requests.retain(|key, _| key.session_id != session_id);
        for mut record in records {
            record.key.session_id = session_id.to_string();
            if record.status == RequestUsageStatus::InFlight {
                record.status = RequestUsageStatus::Abandoned;
                record.source = RequestUsageSource::Estimated;
                record.prompt_tokens = record.projected_prompt_tokens.max(0);
                record.total_tokens = record.prompt_tokens;
            }
            // Repair records persisted by the quadratic double-count bug
            // (silently — this crate carries no tracing dependency; the
            // repair is visible in the report itself, and the round/turn
            // identity stays intact).
            record.sanitize_poisoned_estimate();
            requests.insert(record.key.clone(), record);
        }
    }

    /// Book one turn as a line item — the single entry point all the public
    /// recorders funnel through. It appends the turn and folds it into the
    /// running totals. Non-positive totals are ignored; negative io/cache
    /// counts are clamped to zero.
    pub fn record_turn(&self, provider: &str, model: &str, turn: TokenTurn) {
        if turn.total_tokens <= 0 {
            return;
        }
        let turn = TokenTurn {
            prompt_tokens: turn.prompt_tokens.max(0),
            completion_tokens: turn.completion_tokens.max(0),
            cache_write_tokens: turn.cache_write_tokens.max(0),
            cache_read_tokens: turn.cache_read_tokens.max(0),
            ..turn
        };
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.entry(key(provider, model)).or_default();
        if turn.reported {
            entry.totals.reported_tokens += turn.total_tokens;
            entry.totals.prompt_tokens += turn.prompt_tokens;
            entry.totals.completion_tokens += turn.completion_tokens;
            entry.totals.cache_write_tokens += turn.cache_write_tokens;
            entry.totals.cache_read_tokens += turn.cache_read_tokens;
        } else {
            entry.totals.estimated_tokens += turn.total_tokens;
        }
        entry.turns.push(turn);
    }

    /// Book one turn's token usage. When `reported` is `true`, the provider
    /// reported authoritative usage and `tokens` are real counts; when `false`,
    /// `tokens` are a local estimate.
    pub fn record(&self, provider: &str, model: &str, tokens: i64, reported: bool) {
        self.record_turn(
            provider,
            model,
            TokenTurn {
                reported,
                total_tokens: tokens,
                ..Default::default()
            },
        );
    }

    /// Book one turn's reported usage, including its prompt-cache split. The
    /// cache write/read counts are tracked as a breakout (they're already
    /// folded into `tokens` by the provider's usage parser); `cache_*` are
    /// clamped to non-negative. Callers with no caching pass `0, 0`.
    pub fn record_reported(
        &self,
        provider: &str,
        model: &str,
        tokens: i64,
        cache_write: i64,
        cache_read: i64,
    ) {
        self.record_turn(
            provider,
            model,
            TokenTurn {
                reported: true,
                total_tokens: tokens,
                cache_write_tokens: cache_write,
                cache_read_tokens: cache_read,
                ..Default::default()
            },
        );
    }

    /// The most recent *reported* turn for a `(provider, model)`, if any.
    ///
    /// Used by the TUI context meter as the authoritative anchor: the
    /// provider-reported `prompt_tokens` already measures the serialized
    /// request size (system prompt + every prior turn + tool schemas + per-
    /// message template overhead), which is more accurate than any local
    /// estimate of the transcript. `completion_tokens` is included because the
    /// assistant's last reply is now part of history.
    pub fn last_reported_turn(&self, provider: &str, model: &str) -> Option<TokenTurn> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get(&key(provider, model))?;
        entry.turns.iter().rev().copied().find(|turn| turn.reported)
    }

    /// A snapshot of the ledger suitable for rendering (owned, no lock held).
    pub fn snapshot(&self) -> TokenSourceReport {
        let active_session = self.active_session();
        self.snapshot_filtered(active_session.as_deref())
    }

    pub fn snapshot_for_session(&self, session_id: &str) -> TokenSourceReport {
        self.snapshot_filtered(Some(session_id))
    }

    fn snapshot_filtered(&self, session_id: Option<&str>) -> TokenSourceReport {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<TokenSourceRow> = entries
            .iter()
            .map(|((provider, model), entry)| TokenSourceRow {
                provider: provider.to_string(),
                model: model.to_string(),
                totals: entry.totals,
                turns: entry.turns.clone(),
                requests: Vec::new(),
            })
            .collect();
        drop(entries);

        let requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        for record in requests.values().filter(|record| {
            session_id.is_none_or(|session_id| record.key.session_id == session_id)
        }) {
            let row = if let Some(row) = rows
                .iter_mut()
                .find(|row| row.provider == record.provider && row.model == record.model)
            {
                row
            } else {
                let index = rows.len();
                rows.push(TokenSourceRow {
                    provider: record.provider.clone(),
                    model: record.model.clone(),
                    totals: TokenSourceTotals::default(),
                    turns: Vec::new(),
                    requests: Vec::new(),
                });
                &mut rows[index]
            };
            row.requests.push(record.clone());
            if record.status.is_terminal() {
                row.totals.add(record.totals());
                row.turns.push(record.as_turn());
            }
        }
        rows.sort_by(|a, b| (&a.provider, &a.model).cmp(&(&b.provider, &b.model)));
        for row in &mut rows {
            row.requests
                .sort_by(|a, b| request_display_order(a).cmp(&request_display_order(b)));
        }
        let grand_total =
            rows.iter()
                .map(|r| r.totals)
                .fold(TokenSourceTotals::default(), |mut acc, t| {
                    acc.add(t);
                    acc
                });
        TokenSourceReport { rows, grand_total }
    }
}

fn request_display_order(record: &RequestUsageRecord) -> (u64, u8, u32, u32, &str) {
    (
        record.key.round,
        u8::from(record.key.actor_id != "principal"),
        record.key.turn,
        record.key.attempt,
        record.key.actor_id.as_str(),
    )
}

/// Wall-clock epoch milliseconds. Kept here (rather than at each call site)
/// so the usage-stat day bucket derives from one definition of "now".
fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One row of the report: a single provider+model and its source split.
///
/// Serialisable so an attached frontend can receive the daemon-side report
/// over the wire ([`crate::AgentResponse::TokenUsageReport`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSourceRow {
    pub provider: String,
    pub model: String,
    pub totals: TokenSourceTotals,
    /// The ordered per-turn line items behind `totals`.
    pub turns: Vec<TokenTurn>,
    /// Lifecycle-aware attempts behind this provider/model row.
    pub requests: Vec<RequestUsageRecord>,
}

/// A full snapshot of the ledger: per-row breakdown + a grand total.
///
/// Serialisable so an attached frontend can receive the daemon-side report
/// over the wire ([`crate::AgentResponse::TokenUsageReport`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSourceReport {
    pub rows: Vec<TokenSourceRow>,
    pub grand_total: TokenSourceTotals,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Collecting sink for tests: records everything it receives.
    #[derive(Default)]
    struct CollectingSink {
        received: StdMutex<Vec<(u64, String, RequestUsageRecord)>>,
    }

    impl UsageStatSink for CollectingSink {
        fn record_usage(&self, recorded_at_ms: u64, project: &str, record: &RequestUsageRecord) {
            self.received.lock().unwrap().push((
                recorded_at_ms,
                project.to_string(),
                record.clone(),
            ));
        }
    }

    #[test]
    fn settled_requests_are_mirrored_to_the_usage_sink() {
        let ledger = TokenSourceLedger::new();
        let sink = Arc::new(CollectingSink::default());
        ledger.install_usage_sink(sink.clone());
        ledger.set_usage_project("bucket-42");

        let key = ledger.begin_request("s1", "openai", "gpt", 3, 1, 1_000);
        ledger.settle_request(
            &key,
            RequestUsageStatus::Completed,
            Some(crate::TokenUsage {
                prompt_tokens: 900,
                completion_tokens: 100,
                total_tokens: 1_000,
                ..Default::default()
            }),
            0,
            2_000,
        );
        // A duplicate terminal event must not mirror twice (the reported
        // idempotency fence fires before the sink forward).
        ledger.settle_request(&key, RequestUsageStatus::Failed, None, 5, 0);

        let received = sink.received.lock().unwrap();
        assert_eq!(received.len(), 1, "one terminal settle → one sink record");
        assert_eq!(received[0].1, "bucket-42");
        assert_eq!(received[0].2.total_tokens, 1_000);
        assert_eq!(received[0].2.status, RequestUsageStatus::Completed);
    }

    #[test]
    fn ledger_without_sink_still_settles() {
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s1", "openai", "gpt", 1, 1, 100);
        ledger.settle_request(&key, RequestUsageStatus::Failed, None, 10, 0);
        assert_eq!(ledger.records_for_session("s1").len(), 1);
    }

    #[test]
    fn lifecycle_attempts_are_keyed_idempotent_and_session_scoped() {
        let ledger = TokenSourceLedger::new();
        let first = ledger.begin_request("s1", "openai", "gpt", 3, 1, 1_000);
        let retry = ledger.begin_request("s1", "openai", "gpt", 3, 1, 1_000);
        let other = ledger.begin_request("s2", "anthropic", "claude", 1, 1, 500);
        let envoy =
            ledger.begin_request_for_actor("s1", "envoy:call-1", "openai", "gpt", 3, 1, 300);
        assert_eq!(first.attempt, 1);
        assert_eq!(retry.attempt, 2);
        assert_eq!(other.attempt, 1);
        assert_eq!(envoy.attempt, 1, "a distinct actor has its own attempts");

        ledger.settle_request(&first, RequestUsageStatus::Failed, None, 25, 0);
        ledger.settle_request(
            &retry,
            RequestUsageStatus::Completed,
            Some(crate::TokenUsage {
                prompt_tokens: 990,
                completion_tokens: 110,
                total_tokens: 1_100,
                ..Default::default()
            }),
            0,
            2_000,
        );
        // A duplicate weaker terminal event cannot downgrade reported usage.
        ledger.settle_request(&retry, RequestUsageStatus::Failed, None, 999, 9_999);
        ledger.settle_request(&envoy, RequestUsageStatus::Completed, None, 30, 1_000);

        let report = ledger.snapshot_for_session("s1");
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].requests.len(), 3);
        assert_eq!(report.rows[0].totals.reported_tokens, 1_100);
        assert_eq!(report.rows[0].totals.estimated_tokens, 1_355);
        assert_eq!(
            report.rows[0].requests[1].status,
            RequestUsageStatus::Completed
        );
        assert_eq!(
            report.rows[0].requests[1].source,
            RequestUsageSource::Reported
        );
        // The per-attempt generation span is booked at settle, and the
        // idempotency fence keeps a replayed settle from overwriting it.
        assert_eq!(report.rows[0].requests[1].generation_ms, 2_000);
        assert_eq!(report.rows[0].requests[0].generation_ms, 0);
    }

    #[test]
    fn attempt_records_timestamp_and_error() {
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s1", "p1", "m1", 1, 1, 500);
        assert!(ledger.records_for_session("s1")[0].started_at_ms > 0);

        ledger.settle_request_with_error(
            &key,
            RequestUsageStatus::Failed,
            None,
            0,
            120,
            Some("429 Too Many Requests: Rate limit exceeded".to_string()),
        );

        let records = ledger.records_for_session("s1");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, RequestUsageStatus::Failed);
        assert_eq!(records[0].generation_ms, 120);
        assert_eq!(
            records[0].error.as_deref(),
            Some("429 Too Many Requests: Rate limit exceeded")
        );
    }

    #[test]
    fn restore_marks_crash_residue_abandoned() {
        let ledger = TokenSourceLedger::new();
        let key = RequestUsageKey {
            session_id: "old".to_string(),
            actor_id: "principal".to_string(),
            round: 4,
            turn: 2,
            attempt: 1,
        };
        ledger.restore_session(
            "restored",
            vec![RequestUsageRecord {
                key,
                provider: "relay".to_string(),
                model: "model".to_string(),
                status: RequestUsageStatus::InFlight,
                projected_prompt_tokens: 700,
                ..Default::default()
            }],
        );
        let records = ledger.records_for_session("restored");
        assert_eq!(records[0].status, RequestUsageStatus::Abandoned);
        assert_eq!(records[0].source, RequestUsageSource::Estimated);
        assert_eq!(records[0].prompt_tokens, 700);
        assert_eq!(records[0].total_tokens, 700);
    }

    /// The quadratic `observe_output` bug (summing `StreamingCounter::push`'s
    /// *running total* once per delta) persisted absurd completion counts on
    /// interrupted/failed attempts — e.g. a real turn booked 14 786 219
    /// completion tokens over 113 s and rendered as 130 050 tok/s. Both the
    /// load path and the settle path must repair such records, judging by the
    /// implied rate (real models peak ≈138 tok/s; the ceiling is 10 000).
    #[test]
    fn implausible_estimated_completion_is_repaired() {
        // Settle path: a caller passing a poisoned estimate is clamped.
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s1", "p1", "m1", 1, 1, 800);
        ledger.settle_request(
            &key,
            RequestUsageStatus::Interrupted,
            None,
            14_786_219,
            113_696,
        );
        let records = ledger.records_for_session("s1");
        assert_eq!(records[0].status, RequestUsageStatus::Interrupted);
        assert_eq!(records[0].completion_tokens, 0);
        assert_eq!(records[0].total_tokens, records[0].prompt_tokens);

        // A *small* poisoned count whose implied rate is still impossible
        // (2 393 tokens in 2.165 s → 1 105 tok/s... is under the 10 000
        // ceiling and survives; 9 500 in 2 s → 4 750 tok/s also survives).
        // The rate ceiling only fires far beyond physical reality, so these
        // remain — the ceiling catches the quadratic blow-up (which always
        // rockets past 10 000 tok/s within a few hundred deltas), not
        // merely-fast streams.
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s2", "p1", "m1", 1, 1, 800);
        ledger.settle_request(&key, RequestUsageStatus::Interrupted, None, 9_500, 2_000);
        let records = ledger.records_for_session("s2");
        assert_eq!(records[0].completion_tokens, 9_500);

        // A count implying >10 000 tok/s is repaired even at modest size.
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s3", "p1", "m1", 1, 1, 800);
        ledger.settle_request(&key, RequestUsageStatus::Failed, None, 25_000, 2_000);
        let records = ledger.records_for_session("s3");
        assert_eq!(records[0].completion_tokens, 0);
        assert_eq!(records[0].total_tokens, 800);

        // Untimed poison: an eight-figure count with no measured span is
        // repaired via the absolute companion ceiling.
        let ledger = TokenSourceLedger::new();
        let key = ledger.begin_request("s4", "p1", "m1", 1, 1, 800);
        ledger.settle_request(&key, RequestUsageStatus::Failed, None, 98_732_687, 0);
        let records = ledger.records_for_session("s4");
        assert_eq!(records[0].completion_tokens, 0);

        // Load path: a poisoned record persisted by an older build is
        // repaired on restore.
        let ledger = TokenSourceLedger::new();
        let poisoned = RequestUsageRecord {
            key: RequestUsageKey {
                session_id: "old".to_string(),
                actor_id: "principal".to_string(),
                round: 1,
                turn: 44,
                attempt: 1,
            },
            provider: "relay".to_string(),
            model: "model".to_string(),
            status: RequestUsageStatus::Interrupted,
            source: RequestUsageSource::Estimated,
            projected_prompt_tokens: 64_572,
            prompt_tokens: 64_572,
            completion_tokens: 14_786_219,
            total_tokens: 14_850_791,
            generation_ms: 113_696,
            ..Default::default()
        };
        ledger.restore_session("repaired", vec![poisoned]);
        let records = ledger.records_for_session("repaired");
        assert_eq!(records[0].completion_tokens, 0);
        assert_eq!(records[0].total_tokens, 64_572);
        // The generation span survives — the *rate* column falls back to `–`
        // (zero completion), not a fabricated figure.

        // A plausible estimated completion is untouched.
        let plausible = RequestUsageRecord {
            key: RequestUsageKey {
                session_id: "old".to_string(),
                actor_id: "principal".to_string(),
                round: 2,
                turn: 1,
                attempt: 1,
            },
            provider: "relay".to_string(),
            model: "model".to_string(),
            status: RequestUsageStatus::Completed,
            source: RequestUsageSource::Estimated,
            projected_prompt_tokens: 1_000,
            prompt_tokens: 1_000,
            completion_tokens: 2_400,
            total_tokens: 3_400,
            generation_ms: 40_000,
            ..Default::default()
        };
        ledger.restore_session("plausible", vec![plausible.clone()]);
        let restored = ledger.records_for_session("plausible");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].completion_tokens, 2_400);
        assert_eq!(restored[0].total_tokens, 3_400);

        // A provider-reported count is authoritative and never clamped.
        let reported = RequestUsageRecord {
            key: RequestUsageKey {
                session_id: "old".to_string(),
                actor_id: "principal".to_string(),
                round: 3,
                turn: 1,
                attempt: 1,
            },
            provider: "relay".to_string(),
            model: "model".to_string(),
            status: RequestUsageStatus::Completed,
            source: RequestUsageSource::Reported,
            prompt_tokens: 50_000,
            completion_tokens: 12_000_000,
            total_tokens: 12_050_000,
            generation_ms: 600_000,
            ..Default::default()
        };
        ledger.restore_session("reported", vec![reported.clone()]);
        let restored = ledger.records_for_session("reported");
        assert_eq!(restored.len(), 1);
        // Authoritative provider counts are never clamped.
        assert_eq!(restored[0].completion_tokens, 12_000_000);
        assert_eq!(restored[0].total_tokens, 12_050_000);
    }

    #[test]
    fn records_reported_and_estimated_separately() {
        let ledger = TokenSourceLedger::new();
        ledger.record("openai", "gpt-4o", 100, true);
        ledger.record("openai", "gpt-4o", 50, false);
        let report = ledger.snapshot();
        assert_eq!(report.rows.len(), 1);
        let row = &report.rows[0];
        assert_eq!(row.provider, "openai");
        assert_eq!(row.model, "gpt-4o");
        assert_eq!(row.totals.reported_tokens, 100);
        assert_eq!(row.totals.estimated_tokens, 50);
        assert_eq!(row.totals.total(), 150);
    }

    #[test]
    fn separates_providers_and_models() {
        let ledger = TokenSourceLedger::new();
        ledger.record("openai", "gpt-4o", 100, true);
        ledger.record("google", "gemini-2.5", 80, true);
        ledger.record("kimi", "k2", 30, false);
        let report = ledger.snapshot();
        assert_eq!(report.rows.len(), 3);
        assert_eq!(report.grand_total.reported_tokens, 180);
        assert_eq!(report.grand_total.estimated_tokens, 30);
    }

    #[test]
    fn ignores_non_positive_tokens() {
        let ledger = TokenSourceLedger::new();
        ledger.record("openai", "gpt-4o", 0, true);
        ledger.record("openai", "gpt-4o", -5, false);
        assert!(ledger.snapshot().rows.is_empty());
    }

    #[test]
    fn snapshot_is_stable_order() {
        let ledger = TokenSourceLedger::new();
        ledger.record("zeta", "z1", 10, true);
        ledger.record("alpha", "a1", 10, true);
        let report = ledger.snapshot();
        // BTreeMap keeps alphabetical order by the composite key.
        assert_eq!(report.rows[0].provider, "alpha");
        assert_eq!(report.rows[1].provider, "zeta");
    }

    #[test]
    fn round_trips_provider_model_containing_the_old_separator() {
        // Regression: the old `\u{1f}`-joined string key would mis-split a
        // provider/model that itself contained the separator byte. A tuple key
        // makes the boundary structural and unambiguous.
        let ledger = TokenSourceLedger::new();
        ledger.record("custom\u{1f}relay", "model\u{1f}v2", 40, true);
        let row = &ledger.snapshot().rows[0];
        assert_eq!(row.provider, "custom\u{1f}relay");
        assert_eq!(row.model, "model\u{1f}v2");
        assert_eq!(row.totals.reported_tokens, 40);
    }

    #[test]
    fn record_reported_books_cache_breakout() {
        // The cache-aware overload folds write/read into reported_tokens AND
        // accumulates them as a separate breakout, so the report can show
        // hit-rate without losing the real billed total.
        let ledger = TokenSourceLedger::new();
        // Turn 1: a cache write (the first turn populates the cache).
        ledger.record_reported("anthropic", "claude-sonnet-4-5", 13200, 5000, 0);
        // Turn 2: a cache read (subsequent turn hits the cache).
        ledger.record_reported("anthropic", "claude-sonnet-4-5", 8200, 0, 8000);
        let row = &ledger.snapshot().rows[0];
        assert_eq!(
            row.totals.reported_tokens, 21400,
            "all reported tokens summed"
        );
        assert_eq!(row.totals.cache_write_tokens, 5000);
        assert_eq!(row.totals.cache_read_tokens, 8000);
        assert_eq!(row.totals.estimated_tokens, 0);
    }

    #[test]
    fn record_reported_clamps_negative_cache_counts() {
        // A malformed usage object shouldn't corrupt the ledger: negative cache
        // counts are clamped to zero rather than subtracting from the total.
        let ledger = TokenSourceLedger::new();
        ledger.record_reported("anthropic", "claude", 1000, -50, -10);
        let row = &ledger.snapshot().rows[0];
        assert_eq!(row.totals.reported_tokens, 1000);
        assert_eq!(row.totals.cache_write_tokens, 0);
        assert_eq!(row.totals.cache_read_tokens, 0);
    }

    #[test]
    fn record_reported_ignores_non_positive_total() {
        // Parity with the plain `record` guard: a zero/negative total is a
        // no-op even when cache counts are present.
        let ledger = TokenSourceLedger::new();
        ledger.record_reported("anthropic", "claude", 0, 100, 200);
        ledger.record_reported("anthropic", "claude", -5, 100, 200);
        assert!(ledger.snapshot().rows.is_empty());
    }

    #[test]
    fn grand_total_aggregates_cache_counters() {
        // `snapshot` folds cache counters into the grand total via `add`, so a
        // multi-provider report surfaces the session-wide cache hit volume.
        let ledger = TokenSourceLedger::new();
        ledger.record_reported("anthropic", "claude-opus", 5000, 1000, 3000);
        ledger.record_reported("openai", "gpt-4o", 2000, 0, 0);
        let report = ledger.snapshot();
        assert_eq!(report.grand_total.reported_tokens, 7000);
        assert_eq!(report.grand_total.cache_write_tokens, 1000);
        assert_eq!(report.grand_total.cache_read_tokens, 3000);
    }

    #[test]
    fn record_keeps_per_round_line_items() {
        // Each booking appends an ordered line item and splits input/output for
        // reported turns, powering the detail drill-in.
        let ledger = TokenSourceLedger::new();
        ledger.record_turn(
            "anthropic",
            "claude",
            TokenTurn {
                turn: 0,
                round: 0,
                reported: true,
                prompt_tokens: 1000,
                completion_tokens: 200,
                total_tokens: 1200,
                cache_write_tokens: 800,
                cache_read_tokens: 0,
            },
        );
        ledger.record("anthropic", "claude", 50, false);
        let row = &ledger.snapshot().rows[0];
        assert_eq!(row.turns.len(), 2);
        assert!(row.turns[0].reported);
        assert_eq!(row.turns[0].prompt_tokens, 1000);
        assert_eq!(row.turns[0].completion_tokens, 200);
        assert!(!row.turns[1].reported);
        assert_eq!(row.turns[1].total_tokens, 50);
        assert_eq!(row.totals.prompt_tokens, 1000);
        assert_eq!(row.totals.completion_tokens, 200);
        assert_eq!(row.totals.reported_tokens, 1200);
        assert_eq!(row.totals.estimated_tokens, 50);
    }

    #[test]
    fn last_reported_turn_returns_most_recent_reported_for_key() {
        // The context meter anchors on the newest reported turn for the
        // active (provider, model). Estimated turns are skipped, other keys
        // are ignored, and the most-recent reported turn wins.
        let ledger = TokenSourceLedger::new();
        // Older reported turn for the active key.
        ledger.record_turn(
            "openai",
            "gpt-4o",
            TokenTurn {
                reported: true,
                prompt_tokens: 500,
                completion_tokens: 50,
                total_tokens: 550,
                ..Default::default()
            },
        );
        // A stray estimated turn for the active key must not be returned.
        ledger.record("openai", "gpt-4o", 40, false);
        // Noise in a different key.
        ledger.record_reported("anthropic", "claude", 9999, 0, 0);

        // Newest reported turn for the active key.
        ledger.record_turn(
            "openai",
            "gpt-4o",
            TokenTurn {
                reported: true,
                prompt_tokens: 4000,
                completion_tokens: 300,
                total_tokens: 4300,
                ..Default::default()
            },
        );

        let last = ledger
            .last_reported_turn("openai", "gpt-4o")
            .expect("a reported turn exists for the key");
        assert_eq!(last.prompt_tokens, 4000);
        assert_eq!(last.completion_tokens, 300);
        assert_eq!(last.total_tokens, 4300);

        // Missing key / never-reported key -> None.
        assert!(ledger.last_reported_turn("openai", "gpt-5").is_none());
        assert!(ledger.last_reported_turn("mistral", "large").is_none());
    }
}
