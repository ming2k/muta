//! Value types for the scheduled-prompt system (`/schedule`, formerly `/repeat`).
//!
//! A scheduled job is a `(trigger, prompt)` pair plus scheduling timestamps.
//! The **trigger** is either:
//!
//! - [`Schedule::Cron`] — a five-field cron expression that fires **repeatedly**
//!   (the original `/repeat` semantics); or
//! - [`Schedule::Once`] — a one-shot fire at a single absolute instant
//!   (the new countdown / system-time semantics, e.g. "in 10 minutes" or
//!   "tomorrow 09:00"). A once-job runs exactly once and is then dropped.
//!
//! Jobs are **session-scoped state**: they live on the session that created
//! them, persisted through its event log alongside the todos, round counter,
//! and provider selection (see `SessionEvent::ScheduledJobsSet`, aliased from
//! the legacy `RepeatJobsSet` for back-compat). Resume/fork carries them with
//! the session; the background scheduler polls the live session and dispatches
//! each due job as a normal chat round.
//!
//! Only the pure domain types and the time-expression parser live here, so
//! `muta-contracts` stays free of I/O (ADR-0005). The session store owns the
//! persistence; the scheduler in `muta-agent` owns the firing.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

/// Alias for the JSON map type; the pinned `serde_json` takes two type
/// parameters (`K`, `V`) for indexmap compatibility.
type JsonMap = serde_json::Map<String, serde_json::Value>;

/// Recurring jobs auto-expire after this many days (a safety bound so a
/// forgotten schedule does not run forever). Once-jobs are exempt — a
/// one-shot future fire is its own expiry.
pub const DEFAULT_MAX_AGE_DAYS: i64 = 30;

/// The firing rule for a [`ScheduledJob`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Schedule {
    /// Fire repeatedly on a five-field cron expression. The next fire time is
    /// recomputed (via [`crate::CronExpr::next_fire`]) after every firing.
    Cron { cron: String },
    /// Fire exactly once at `fire_at`, then the job is dropped. Used for
    /// countdown ("in 10 minutes") and absolute ("tomorrow 09:00") prompts.
    Once { fire_at: DateTime<Utc> },
}

impl Schedule {
    /// The next instant this schedule fires strictly after `now`.
    /// `None` means it will never fire again (a once-job already passed, or an
    /// impossible cron such as `30 2 30 2 *`).
    pub fn next_fire(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Schedule::Cron { cron } => crate::CronExpr::parse(cron)
                .ok()
                .and_then(|c| c.next_fire(now)),
            Schedule::Once { fire_at } => {
                if *fire_at > now {
                    Some(*fire_at)
                } else {
                    None
                }
            }
        }
    }

    /// `true` for [`Schedule::Once`].
    pub fn is_once(&self) -> bool {
        matches!(self, Schedule::Once { .. })
    }

    /// A short human label (`cron` / `once`), for list output.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Schedule::Cron { .. } => "cron",
            Schedule::Once { .. } => "once",
        }
    }

    /// The displayable form of the trigger: the cron expression for cron jobs,
    /// the `YYYY-MM-DD HH:MM` fire instant for once jobs.
    pub fn display(&self) -> String {
        match self {
            Schedule::Cron { cron } => cron.clone(),
            Schedule::Once { fire_at } => fire_at.format("%Y-%m-%d %H:%M").to_string(),
        }
    }
}

/// A scheduled prompt. The unified type behind both the legacy `/repeat` cron
/// jobs and the new one-shot `/schedule` jobs.
///
/// Serialises to a tagged shape: `{"id":..,"kind":"cron","cron":..,..}` or
/// `{"id":..,"kind":"once","fire_at":..,..}`. **Deserialises** from that shape
/// *and* from the legacy flat shape (a bare `cron: String`, no `kind`), so old
/// `/repeat` session snapshots and event logs load unchanged — see the manual
/// [`Deserialize`] impl below.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScheduledJob {
    pub id: String,
    /// The firing rule. Flattened into the parent JSON object so the wire shape
    /// is `{…, "kind":"cron"|"once", <cron|fire_at>, …}`.
    #[serde(flatten)]
    pub trigger: Schedule,
    pub prompt: String,
    pub created_at: DateTime<Utc>,
    pub next_fire: DateTime<Utc>,
    pub last_fire: Option<DateTime<Utc>>,
}

impl ScheduledJob {
    /// Build a cron (recurring) job. `next_fire` is computed from the cron
    /// relative to `now`. `None` if the cron never fires.
    pub fn cron(id: String, cron: String, prompt: String, now: DateTime<Utc>) -> Option<Self> {
        let next = crate::CronExpr::parse(&cron).ok()?.next_fire(now)?;
        Some(Self {
            id,
            trigger: Schedule::Cron { cron },
            prompt,
            created_at: now,
            next_fire: next,
            last_fire: None,
        })
    }

    /// Build a one-shot job firing at `fire_at`.
    pub fn once(id: String, fire_at: DateTime<Utc>, prompt: String, now: DateTime<Utc>) -> Self {
        Self {
            id,
            trigger: Schedule::Once { fire_at },
            prompt,
            created_at: now,
            next_fire: fire_at,
            last_fire: None,
        }
    }
}

// ── Legacy `RepeatJob` compatibility ──────────────────────────────────────
//
// Existing session snapshots serialised before this change used a flat
// `RepeatJob { cron: String, … }`. We:
//   - keep `RepeatJob` as a thin newtype that converts to/from `ScheduledJob`,
//     so in-tree code referencing it keeps compiling; and
//   - implement `Deserialize` for `ScheduledJob` by hand so it accepts *both*
//     the new tagged shape (`{"kind":"cron",…}` / `{"kind":"once",…}`) and the
//     legacy flat shape (`{"cron":"*/5 * * * *",…}` with no `kind`). That is
//     what lets old `/repeat` snapshots and event logs load unchanged.

/// Legacy alias kept for source-level compatibility. A `RepeatJob` is exactly a
/// cron [`ScheduledJob`]. Prefer `ScheduledJob` in new code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepeatJob(pub ScheduledJob);

impl From<ScheduledJob> for RepeatJob {
    fn from(j: ScheduledJob) -> Self {
        Self(j)
    }
}

impl From<RepeatJob> for ScheduledJob {
    fn from(j: RepeatJob) -> Self {
        j.0
    }
}

impl<'de> Deserialize<'de> for RepeatJob {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        ScheduledJob::deserialize(deserializer).map(RepeatJob)
    }
}

/// Manual `Deserialize` that accepts both the new tagged `Schedule` shape and
/// the legacy flat `cron` field. We deserialize into a generic JSON object,
/// inspect `kind`, and fall back to `cron` when `kind` is absent.
impl<'de> Deserialize<'de> for ScheduledJob {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let mut map: JsonMap = serde_json::Map::deserialize(deserializer)?;

        let trigger = match map.remove("kind") {
            Some(serde_json::Value::String(kind)) => match kind.as_str() {
                "cron" => {
                    let cron = map
                        .remove("cron")
                        .and_then(|v| v.as_str().map(str::to_string))
                        .ok_or_else(|| {
                            D::Error::custom("scheduled job of kind 'cron' missing 'cron' field")
                        })?;
                    Schedule::Cron { cron }
                }
                "once" => {
                    let v = map.remove("fire_at").ok_or_else(|| {
                        D::Error::custom("scheduled job of kind 'once' missing 'fire_at' field")
                    })?;
                    let fire_at =
                        serde_json::from_value::<DateTime<Utc>>(v).map_err(D::Error::custom)?;
                    Schedule::Once { fire_at }
                }
                other => {
                    return Err(D::Error::custom(format!("unknown schedule kind '{other}'")));
                }
            },
            // Legacy flat shape: no `kind`, just a `cron` field.
            _ => {
                let cron = map
                    .remove("cron")
                    .and_then(|v| v.as_str().map(str::to_string))
                    .ok_or_else(|| {
                        D::Error::custom(
                            "scheduled job has neither 'kind' nor a legacy 'cron' field",
                        )
                    })?;
                Schedule::Cron { cron }
            }
        };

        let take_string = |m: &mut JsonMap, key: &str| -> Result<String, D::Error> {
            m.remove(key)
                .and_then(|v| v.as_str().map(str::to_string))
                .ok_or_else(|| D::Error::custom(format!("missing field '{key}'")))
        };
        let take_datetime = |m: &mut JsonMap, key: &str| -> Result<DateTime<Utc>, D::Error> {
            let v = m
                .remove(key)
                .ok_or_else(|| D::Error::custom(format!("missing field '{key}'")))?;
            serde_json::from_value(v).map_err(D::Error::custom)
        };

        let id = take_string(&mut map, "id")?;
        let prompt = take_string(&mut map, "prompt")?;
        let created_at = take_datetime(&mut map, "created_at")?;
        let next_fire = take_datetime(&mut map, "next_fire")?;
        let last_fire = match map.remove("last_fire") {
            Some(serde_json::Value::Null) | None => None,
            Some(v) => Some(serde_json::from_value(v).map_err(D::Error::custom)?),
        };

        Ok(ScheduledJob {
            id,
            trigger,
            prompt,
            created_at,
            next_fire,
            last_fire,
        })
    }
}

/// Parsed representation of a `/schedule` time argument. The caller resolves
/// it against `now` to produce a concrete fire instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleAt {
    /// A recurring five-field cron expression.
    Cron(String),
    /// Fire once at this absolute instant.
    Once(DateTime<Utc>),
}

impl ScheduleAt {
    /// Resolve to a concrete [`ScheduledJob`] trigger + first fire time.
    /// Returns `None` for an impossible cron (e.g. Feb 30) or a once-time
    /// that already passed.
    pub fn resolve(&self, now: DateTime<Utc>) -> Option<(Schedule, DateTime<Utc>)> {
        match self {
            ScheduleAt::Cron(cron) => {
                let parsed = crate::CronExpr::parse(cron).ok()?;
                let next = parsed.next_fire(now)?;
                Some((Schedule::Cron { cron: cron.clone() }, next))
            }
            ScheduleAt::Once(at) => {
                if *at > now {
                    Some((Schedule::Once { fire_at: *at }, *at))
                } else {
                    None
                }
            }
        }
    }

    /// A one-line human description for confirmation messages.
    pub fn describe(&self) -> String {
        match self {
            ScheduleAt::Cron(c) => format!("cron `{c}`"),
            ScheduleAt::Once(at) => format!("once at {}", at.format("%Y-%m-%d %H:%M")),
        }
    }
}

/// Parse a free-form `/schedule` time argument.
///
/// Accepted shapes (case-insensitive):
///
/// - **Cron** — exactly five whitespace-separated cron fields
///   (`*/5 * * * *`, `0 9 * * 1-5`). Detected structurally, so it never
///   collides with the time forms below.
/// - **Relative countdown** — an optional leading `in `, then one or more
///   `<number><unit>` pairs (`10m`, `2h30m`, `1d12h`, `in 10 minutes`,
///   `in 2 hours 30 minutes`). Units: `s`/`sec`/`secs`/`second`/`seconds`,
///   `m`/`min`/`mins`/`minute`/`minutes`, `h`/`hr`/`hrs`/`hour`/`hours`,
///   `d`/`day`/`days`.
/// - **Absolute time** — `HH:MM[:SS]` today (or tomorrow if already passed),
///   `today HH:MM`, `tomorrow HH:MM`, `tomorrow`, `YYYY-MM-DD HH:MM`, or
///   `YYYY-MM-DDTHH:MM` (ISO).
///
/// Returns `None` when the argument matches none of these shapes; the caller
/// surfaces an error. Parsing is deliberately lenient on whitespace and the
/// `in`/`at` leading words, and never panics.
pub fn parse_schedule_arg(raw: &str, now: DateTime<Utc>) -> Option<ScheduleAt> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // ── Cron: exactly five whitespace-separated fields. ──
    let fields: Vec<&str> = trimmed.split_whitespace().collect();
    if fields.len() >= 5 {
        let maybe_cron = fields[..5].join(" ");
        if crate::CronExpr::parse(&maybe_cron).is_ok() {
            return Some(ScheduleAt::Cron(maybe_cron));
        }
    }

    let lower = trimmed.to_ascii_lowercase();

    // ── Relative countdown. (case-insensitive keywords/units) ──
    if let Some(at) = parse_relative_countdown(&lower, now) {
        return Some(ScheduleAt::Once(at));
    }

    // ── Absolute time. (case-insensitive keywords; the ISO `T` separator is
    // matched case-insensitively inside `parse_dated_time`.) ──
    if let Some(at) = parse_absolute_time(&lower, now) {
        return Some(ScheduleAt::Once(at));
    }

    None
}

/// Parse a relative countdown like `10m`, `2h30m`, `in 10 minutes`,
/// `2 hours 30 minutes`. Returns the absolute fire instant.
fn parse_relative_countdown(lower: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let body = lower.strip_prefix("in ").unwrap_or(lower).trim();
    if body.is_empty() {
        return None;
    }
    // Reject anything that still looks like an absolute date/time token.
    if body.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && (body.starts_with("today")
            || body.starts_with("tomorrow")
            || body.contains('-')
            || body.contains(':'))
    {
        return None;
    }

    // Tokenise into runs of digits followed by an optional unit word. We
    // support two spellings: compact (`2h30m`) and verbose (`2 hours 30
    // minutes`). Whitespace between number and unit is allowed.
    let mut total = Duration::zero();
    let mut bytes = body.as_bytes();
    let mut consumed_any = false;

    while !bytes.is_empty() {
        // Skip whitespace.
        while bytes.first() == Some(&b' ') {
            bytes = &bytes[1..];
        }
        if bytes.is_empty() {
            break;
        }
        // Read leading digits.
        let digit_end = bytes
            .iter()
            .position(|b| !b.is_ascii_digit())
            .unwrap_or(bytes.len());
        if digit_end == 0 {
            return None; // a non-digit where a number was expected → not a countdown
        }
        let n: i64 = std::str::from_utf8(&bytes[..digit_end])
            .ok()?
            .parse()
            .ok()?;
        bytes = &bytes[digit_end..];
        // Skip optional whitespace between number and unit.
        while bytes.first() == Some(&b' ') {
            bytes = &bytes[1..];
        }
        // Read the unit: longest leading alphabetic run.
        let unit_end = bytes
            .iter()
            .position(|b| !b.is_ascii_alphabetic())
            .unwrap_or(bytes.len());
        if unit_end == 0 {
            return None; // number with no unit → not a countdown
        }
        let unit = std::str::from_utf8(&bytes[..unit_end]).ok()?;
        bytes = &bytes[unit_end..];
        let add = match unit {
            "s" | "sec" | "secs" | "second" | "seconds" => Duration::seconds(n),
            "m" | "min" | "mins" | "minute" | "minutes" => Duration::minutes(n),
            "h" | "hr" | "hrs" | "hour" | "hours" => Duration::hours(n),
            "d" | "day" | "days" => Duration::days(n),
            _ => return None,
        };
        total += add;
        consumed_any = true;
    }

    if !consumed_any {
        return None;
    }
    Some(now + total)
}

/// Parse an absolute time/date expression in the supported forms.
fn parse_absolute_time(lower: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let today = now.date_naive();
    let tomorrow = today + Duration::days(1);

    // `today` / `tomorrow` alone, or `today HH:MM` / `tomorrow HH:MM`,
    // or `at HH:MM` (today at that clock time, rolled to tomorrow if passed).
    let (base_date, rest): (NaiveDate, &str) = if let Some(r) = lower.strip_prefix("today ") {
        (today, r.trim())
    } else if lower == "today" {
        (today, "23:59")
    } else if let Some(r) = lower.strip_prefix("tomorrow ") {
        (tomorrow, r.trim())
    } else if lower == "tomorrow" {
        (tomorrow, "09:00")
    } else if let Some(r) = lower.strip_prefix("at ") {
        (today, r.trim())
    } else {
        // Bare `HH:MM[:SS]`, `YYYY-MM-DD HH:MM`, or `YYYY-MM-DDTHH:MM`.
        if let Some(t) = parse_clock_only(lower) {
            return Some(roll_clock_time(now, today, tomorrow, t));
        }
        return parse_dated_time(lower);
    };

    let t = parse_clock_only(rest)?;
    let dt = base_date.and_time(t).and_local_timezone(Utc).single()?;
    // For `today`/`at`, if the clock time already passed, roll to tomorrow.
    if base_date == today && dt <= now {
        tomorrow.and_time(t).and_local_timezone(Utc).single()
    } else {
        Some(dt)
    }
}

/// `HH:MM` or `HH:MM:SS` → `NaiveTime`.
fn parse_clock_only(s: &str) -> Option<NaiveTime> {
    let s = s.trim();
    NaiveTime::parse_from_str(s, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M:%S"))
        .ok()
}

/// Decide whether a bare clock time lands today or tomorrow.
fn roll_clock_time(
    now: DateTime<Utc>,
    today: NaiveDate,
    tomorrow: NaiveDate,
    t: NaiveTime,
) -> DateTime<Utc> {
    let candidate = today
        .and_time(t)
        .and_local_timezone(Utc)
        .single()
        .unwrap_or(now);
    if candidate > now {
        candidate
    } else {
        tomorrow
            .and_time(t)
            .and_local_timezone(Utc)
            .single()
            .unwrap_or(now)
    }
}

/// `YYYY-MM-DD HH:MM` or `YYYY-MM-DDTHH:MM[:SS]` (ISO-ish). The ISO `T`
/// separator matches case-insensitively (`t` or `T`).
fn parse_dated_time(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    // Normalise a date/time `T` separator: `2026-03-15t14:00` → `…T14:00`.
    let normalised = if s.len() >= 11 && s.as_bytes().get(10) == Some(&b't') {
        let mut buf = s.as_bytes().to_vec();
        buf[10] = b'T';
        String::from_utf8(buf).ok()?
    } else {
        s.to_string()
    };
    for fmt in [
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&normalised, fmt)
            && let Some(zoned) = dt.and_local_timezone(Utc).single()
        {
            return Some(zoned);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    // ── Schedule ──

    #[test]
    fn schedule_cron_next_fire() {
        let s = Schedule::Cron {
            cron: "*/5 * * * *".to_string(),
        };
        assert_eq!(s.next_fire(t(2026, 1, 1, 0, 0)), Some(t(2026, 1, 1, 0, 5)));
    }

    #[test]
    fn schedule_once_future_fires_past_does_not() {
        let s = Schedule::Once {
            fire_at: t(2026, 1, 1, 12, 0),
        };
        assert_eq!(s.next_fire(t(2026, 1, 1, 0, 0)), Some(t(2026, 1, 1, 12, 0)));
        assert_eq!(s.next_fire(t(2026, 1, 2, 0, 0)), None);
        assert!(s.is_once());
    }

    // ── parse_schedule_arg: cron ──

    #[test]
    fn parses_cron_form() {
        let now = t(2026, 1, 1, 0, 0);
        assert!(matches!(
            parse_schedule_arg("*/5 * * * *", now),
            Some(ScheduleAt::Cron(_))
        ));
        assert!(matches!(
            parse_schedule_arg("0 9 * * 1-5", now),
            Some(ScheduleAt::Cron(_))
        ));
    }

    // ── parse_schedule_arg: relative countdown ──

    #[test]
    fn parses_compact_countdown() {
        let now = t(2026, 1, 1, 12, 0);
        assert_eq!(
            parse_schedule_arg("10m", now),
            Some(ScheduleAt::Once(t(2026, 1, 1, 12, 10)))
        );
        assert_eq!(
            parse_schedule_arg("2h30m", now),
            Some(ScheduleAt::Once(t(2026, 1, 1, 14, 30)))
        );
        assert_eq!(
            parse_schedule_arg("1d12h", now),
            Some(ScheduleAt::Once(t(2026, 1, 3, 0, 0)))
        );
    }

    #[test]
    fn parses_verbose_countdown() {
        let now = t(2026, 1, 1, 12, 0);
        assert_eq!(
            parse_schedule_arg("in 10 minutes", now),
            Some(ScheduleAt::Once(t(2026, 1, 1, 12, 10)))
        );
        assert_eq!(
            parse_schedule_arg("in 2 hours 30 minutes", now),
            Some(ScheduleAt::Once(t(2026, 1, 1, 14, 30)))
        );
    }

    #[test]
    fn rejects_number_without_unit() {
        assert!(parse_schedule_arg("10", t(2026, 1, 1, 0, 0)).is_none());
        assert!(parse_schedule_arg("10x", t(2026, 1, 1, 0, 0)).is_none());
    }

    // ── parse_schedule_arg: absolute time ──

    #[test]
    fn parses_bare_clock_today_or_tomorrow() {
        let now = t(2026, 1, 1, 10, 0); // 10:00
        assert_eq!(
            parse_schedule_arg("14:00", now),
            Some(ScheduleAt::Once(t(2026, 1, 1, 14, 0)))
        );
        assert_eq!(
            parse_schedule_arg("08:00", now),
            Some(ScheduleAt::Once(t(2026, 1, 2, 8, 0)))
        );
    }

    #[test]
    fn parses_today_tomorrow_keywords() {
        let now = t(2026, 1, 1, 10, 0);
        assert_eq!(
            parse_schedule_arg("today 18:00", now),
            Some(ScheduleAt::Once(t(2026, 1, 1, 18, 0)))
        );
        assert_eq!(
            parse_schedule_arg("tomorrow 09:00", now),
            Some(ScheduleAt::Once(t(2026, 1, 2, 9, 0)))
        );
        assert_eq!(
            parse_schedule_arg("tomorrow", now),
            Some(ScheduleAt::Once(t(2026, 1, 2, 9, 0)))
        );
    }

    #[test]
    fn parses_dated_iso_time() {
        let now = t(2026, 1, 1, 0, 0);
        assert_eq!(
            parse_dated_time("2026-03-15 14:00"),
            Some(t(2026, 3, 15, 14, 0))
        );
        assert_eq!(
            parse_dated_time("2026-03-15T14:00"),
            Some(t(2026, 3, 15, 14, 0))
        );
        assert_eq!(
            parse_schedule_arg("2026-03-15 14:00", now),
            Some(ScheduleAt::Once(t(2026, 3, 15, 14, 0)))
        );
        assert_eq!(
            parse_schedule_arg("2026-03-15T14:00", now),
            Some(ScheduleAt::Once(t(2026, 3, 15, 14, 0)))
        );
    }

    #[test]
    fn rejects_garbage() {
        let now = t(2026, 1, 1, 0, 0);
        assert!(parse_schedule_arg("", now).is_none());
        assert!(parse_schedule_arg("hello world", now).is_none());
        assert!(parse_schedule_arg("not a time", now).is_none());
    }

    // ── ScheduledJob serde back-compat ──

    #[test]
    fn scheduled_job_cron_round_trips() {
        let now = t(2026, 1, 1, 0, 0);
        let job = ScheduledJob::cron("abc".into(), "*/5 * * * *".into(), "p".into(), now).unwrap();
        let json = serde_json::to_string(&job).unwrap();
        let back: ScheduledJob = serde_json::from_str(&json).unwrap();
        assert_eq!(job, back);
    }

    #[test]
    fn scheduled_job_once_round_trips() {
        let now = t(2026, 1, 1, 0, 0);
        let job = ScheduledJob::once("abc".into(), t(2026, 1, 1, 12, 0), "p".into(), now);
        let json = serde_json::to_string(&job).unwrap();
        let back: ScheduledJob = serde_json::from_str(&json).unwrap();
        assert_eq!(job, back);
    }

    #[test]
    fn legacy_flat_cron_json_loads_as_cron_schedule() {
        // Old `/repeat` snapshot shape: flat `cron` field, no `kind`.
        let legacy = serde_json::json!({
            "id": "abc",
            "cron": "*/5 * * * *",
            "prompt": "run tests",
            "created_at": "2026-01-01T00:00:00Z",
            "next_fire": "2026-01-01T00:05:00Z",
            "last_fire": null,
        });
        let job: ScheduledJob = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            job.trigger,
            Schedule::Cron {
                cron: "*/5 * * * *".to_string()
            }
        );
        assert_eq!(job.id, "abc");
    }
}
