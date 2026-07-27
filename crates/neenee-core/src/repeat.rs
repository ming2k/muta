//! Value types for `/repeat` cron jobs.
//!
//! A `/repeat` job is a `(cron expression, prompt)` pair plus scheduling
//! timestamps. Jobs are **session-scoped state**: they live on the session
//! that created them, persisted through its event log alongside the todos,
//! round counter, and provider selection (see `SessionEvent::RepeatJobsSet`).
//! Resume/fork carries them with the session; the background scheduler polls
//! the live session and dispatches each due job as a normal chat round.
//!
//! Only the pure domain types live here, so `neenee-core` stays free of I/O
//! (ADR-0005). The session store owns the persistence; the scheduler in
//! `neenee-agent` owns the firing.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Recurring jobs auto-expire after this many days (a safety bound so a
/// forgotten `/repeat` does not run forever).
pub const DEFAULT_MAX_AGE_DAYS: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepeatJob {
    pub id: String,
    pub cron: String,
    pub prompt: String,
    pub created_at: DateTime<Utc>,
    pub next_fire: DateTime<Utc>,
    pub last_fire: Option<DateTime<Utc>>,
}
