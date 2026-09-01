//! Scheduling, recurrence, cron parsing, and delegate/jail argument parsing.

use std::sync::Arc;
use tokio::sync::mpsc;

use super::record::{record_command, record_error};
use muta_contracts::{
    AgentRequest, AgentResponse, CommandResult, CronExpr, Schedule, ScheduledJob,
};
use muta_persistence::session::SessionStore;

/// `/schedule list` / `/repeat list`: list every scheduled job sorted by next
/// fire, showing kind, trigger, next-fire, and prompt.
pub(crate) async fn list_scheduled_jobs(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
) {
    let mut jobs = session.scheduled_jobs().await;
    if jobs.is_empty() {
        record_command(
            session,
            resp_tx,
            name,
            args,
            CommandResult::ScheduledList {
                entries: Vec::new(),
            },
        )
        .await;
        return;
    }
    jobs.sort_by_key(|j| j.next_fire);
    let mut lines = Vec::new();
    for j in &jobs {
        lines.push(format!(
            "  {} · {} · `{}` · next {} · {}",
            &j.id[..8.min(j.id.len())],
            j.trigger.kind_label(),
            j.trigger.display(),
            j.next_fire.format("%Y-%m-%d %H:%M"),
            j.prompt,
        ));
    }
    record_command(
        session,
        resp_tx,
        name,
        args,
        CommandResult::ScheduledList { entries: lines },
    )
    .await;
}

/// `/schedule cancel <id>` / `/repeat cancel <id>`: drop the job with that id.
pub(crate) async fn cancel_scheduled_job(
    session: &Arc<SessionStore>,
    id: &str,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
) {
    let mut jobs = session.scheduled_jobs().await;
    let before = jobs.len();
    jobs.retain(|j| j.id != id);
    if before == jobs.len() {
        record_command(
            session,
            resp_tx,
            name,
            args,
            CommandResult::Text(format!("No scheduled job with id {id}.")),
        )
        .await;
        return;
    }
    match session.set_scheduled_jobs(jobs).await {
        Ok(()) => {
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!("Cancelled scheduled job {id}.")),
            )
            .await;
        }
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
        }
    }
}

/// `/repeat <cron> <prompt>` shared add path: build a cron `ScheduledJob`,
/// persist it, confirm, and fire the first run immediately.
pub(crate) async fn add_scheduled_job(
    session: &Arc<SessionStore>,
    cron: &str,
    prompt: &str,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    req_tx: &mpsc::UnboundedSender<AgentRequest>,
    name: &str,
    args: &str,
) {
    let now = chrono::Utc::now();
    let next = match CronExpr::parse(cron)
        .and_then(|c| c.next_fire(now).ok_or_else(|| "never fires".to_string()))
    {
        Ok(n) => n,
        Err(error) => {
            record_error(
                session,
                resp_tx,
                name,
                args,
                format!("Invalid cron `{cron}`: {error}"),
            )
            .await;
            return;
        }
    };
    let mut jobs = session.scheduled_jobs().await;
    let id = uuid::Uuid::new_v4().to_string();
    let short_id = id[..8.min(id.len())].to_string();
    let job = ScheduledJob {
        id,
        trigger: Schedule::Cron {
            cron: cron.to_string(),
        },
        prompt: prompt.to_string(),
        created_at: now,
        next_fire: next,
        last_fire: None,
    };
    jobs.push(job);
    match session.set_scheduled_jobs(jobs).await {
        Ok(()) => {
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Scheduled {
                    kind: "cron".to_string(),
                    id: short_id,
                    trigger: format!("`{cron}`"),
                    next: format!("{} Running now.", next.format("%Y-%m-%d %H:%M")),
                },
            )
            .await;
            let _ = req_tx.send(AgentRequest::Prompt {
                text: prompt.to_string(),
                images: Vec::new(),
                sent_at_ms: None,
            });
        }
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
        }
    }
}

pub(crate) fn parse_delegate_arg(arg: &str) -> Result<Option<bool>, String> {
    match arg {
        "" => Ok(None),
        "on" | "true" | "1" | "delegate" | "auto" | "yolo" => Ok(Some(true)),
        "off" | "false" | "0" => Ok(Some(false)),
        other => Err(format!(
            "Unknown value '{other}'. Use `/delegate` to toggle, or `/delegate on|off`."
        )),
    }
}

pub(crate) fn parse_jail_arg(arg: &str) -> Result<Option<bool>, String> {
    match arg.trim() {
        "" => Ok(None),
        "on" | "true" | "1" | "enable" | "enabled" | "confined" | "jail" => Ok(Some(true)),
        "off" | "false" | "0" | "disable" | "disabled" | "unconfined" | "escape" => Ok(Some(false)),
        other => Err(format!(
            "Unknown value '{other}'. Use `/jail` to toggle, or `/jail on|off`."
        )),
    }
}

pub(crate) fn split_schedule_spec(rest: &str) -> Option<(String, String)> {
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    if tokens.len() >= 6 && CronExpr::parse(&tokens[..5].join(" ")).is_ok() {
        let spec = tokens[..5].join(" ");
        let prompt = tokens[5..].join(" ");
        return (!prompt.is_empty()).then_some((spec, prompt));
    }

    let first = tokens[0].to_ascii_lowercase();
    let is_phrase = matches!(first.as_str(), "in" | "today" | "tomorrow" | "at");

    let mut spec_end = 1;
    if is_phrase {
        while spec_end < tokens.len() && is_time_continuation(tokens[spec_end]) {
            spec_end += 1;
        }
    }

    if spec_end >= tokens.len() {
        return None;
    }

    let spec = tokens[..spec_end].join(" ");
    let prompt = tokens[spec_end..].join(" ");
    (!prompt.is_empty()).then_some((spec, prompt))
}

pub(crate) fn is_time_continuation(tok: &str) -> bool {
    let lower = tok.to_ascii_lowercase();
    let num_leading = lower.chars().next().is_some_and(|c| c.is_ascii_digit());
    num_leading
        || matches!(
            lower.as_str(),
            "and"
                | "s"
                | "sec"
                | "secs"
                | "second"
                | "seconds"
                | "m"
                | "min"
                | "mins"
                | "minute"
                | "minutes"
                | "h"
                | "hr"
                | "hrs"
                | "hour"
                | "hours"
                | "d"
                | "day"
                | "days"
        )
        || tok.contains(':')
        || (tok.contains('-') && tok.chars().next().is_some_and(|c| c.is_ascii_digit()))
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SessionRoute<'a> {
    Open(Option<&'a str>),
    New,
    Fork,
    Status,
}

pub(crate) fn session_route<'a>(name: &str, parts: &'a [&str]) -> Result<SessionRoute<'a>, String> {
    if name != "session" {
        return Ok(SessionRoute::Open(parts.get(1).copied()));
    }
    match parts.get(1).copied().unwrap_or("") {
        "open" | "resume" => Ok(SessionRoute::Open(parts.get(2).copied())),
        "" => Ok(SessionRoute::Open(None)),
        "list" => Ok(SessionRoute::Open(None)),
        "new" => Ok(SessionRoute::New),
        "fork" => Ok(SessionRoute::Fork),
        "status" => Ok(SessionRoute::Status),
        unknown => Err(format!(
            "Unknown session command '{unknown}'. /session is retired: use /sessions to browse \
             or open, /new, or /fork."
        )),
    }
}
