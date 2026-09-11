//! ADR-0190 task digest: a compact, model-facing rendering of a finished
//! task. One block, fact-first — state, duration, tail summary, log path —
//! so the wake turn spends context on signal, not ceremony.

use muta_contracts::{BackgroundJobOutcome, JobSpec, JobState};

/// First `n` chars of a string on char boundaries.
fn chars_head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Human-facing label for a task spec: the caller's label when it supplied one,
/// otherwise the command / timer prompt. Shared by the completion digest and the
/// continuation report so both name a job the same way.
pub fn job_label(spec: &JobSpec) -> String {
    match spec {
        JobSpec::Process { command, label, .. } => label.as_deref().unwrap_or(command).to_string(),
        JobSpec::Timer { label, prompt, .. } => label
            .clone()
            .unwrap_or_else(|| format!("timer: {}", chars_head(prompt, 40))),
    }
}

/// Render the digest for a settled task.
pub fn outcome_digest(outcome: &BackgroundJobOutcome) -> String {
    let command = job_label(&outcome.spec);

    let state_line = match &outcome.state {
        JobState::Succeeded {
            duration_ms,
            exit_code: _,
        } => {
            format!("exited 0 (ok) after {}s", duration_ms / 1000)
        }
        JobState::Failed {
            duration_ms,
            exit_code,
            error,
        } => format!(
            "FAILED with exit {exit_code} after {}s — {error}",
            duration_ms / 1000
        ),
        JobState::Killed { duration_ms } => {
            format!("stopped by operator after {}s", duration_ms / 1000)
        }
        JobState::TimedOut { duration_ms } => {
            format!("timed out after {}s", duration_ms / 1000)
        }
        _ => "finished".to_string(),
    };

    let mut digest = format!(
        "[background task finished: `{}` — {}.\njob: {}",
        command, state_line, outcome.job_id.0
    );

    if !outcome.summary.trim().is_empty() {
        digest.push_str("\noutput tail:\n");
        digest.push_str(outcome.summary.trim());
        digest.push('\n');
    }
    if let Some(log) = &outcome.log_path {
        digest.push_str(&format!("full log: {}\n", log.display()));
    }
    digest.push_str("Review the result and continue your task; do not repeat this command.]");
    digest
}
