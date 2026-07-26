//! Pursuit-related display and parsing helpers.
//!
//! `format_pursuit_status` is the textual form surfaced in the TUI for `/pursuit`.
//! `parse_pursuit_budget` / `format_pursuit_budget` back the `/pursue budget`
//! subcommand (ADR-0069).

use neenee_core::{Pursuit, PursuitBudget};

/// Single textual rendering of a [`Pursuit`] for `/pursuit` and exports: state
/// label and objective.
pub fn format_pursuit_status(pursuit: &Pursuit) -> String {
    let state = if pursuit.is_complete {
        "complete"
    } else if pursuit.terminal_reason.is_some() {
        "stopped"
    } else {
        "active"
    };
    let mut out = format!("Pursuit [{}]: {}", state, pursuit.objective);
    if let Some(reason) = pursuit.terminal_reason.as_deref() {
        out.push_str(&format!("\nStopped: {reason}"));
    }
    if let Some(budget) = pursuit.budget
        && !budget.is_empty()
    {
        out.push_str(&format!(
            "\nBudget: {}",
            format_pursuit_budget(Some(budget))
        ));
    }
    out
}

/// Render a [`PursuitBudget`] with canonical
/// `passes=N tokens=N time=Ms` spellings, or "cleared" when `None` or empty.
pub fn format_pursuit_budget(budget: Option<PursuitBudget>) -> String {
    let Some(b) = budget else {
        return "cleared".to_string();
    };
    if b.is_empty() {
        return "cleared".to_string();
    }
    let mut parts = Vec::new();
    if let Some(passes) = b.max_passes {
        parts.push(format!("passes={passes}"));
    }
    if let Some(t) = b.max_tokens {
        parts.push(format!("tokens={t}"));
    }
    if let Some(t) = b.max_wall_clock_ms {
        parts.push(format!("time={t}ms"));
    }
    parts.join(" ")
}

/// Parse a `/pursue budget` argument string into a [`PursuitBudget`].
///
/// Grammar: a whitespace-separated list of `axis=value` tokens, where `axis` is
/// one of `passes` / `tokens` / `time` (case-insensitive, `time` in
/// milliseconds) and `value` is a positive integer. The former `turns` axis
/// remains a read-compatible alias for `passes`. Any subset may be given;
/// omitted axes stay uncapped. An empty string clears the budget (`Ok(None)`).
/// Unknown axes or non-positive / non-integer values are rejected.
pub fn parse_pursuit_budget(args: &str) -> Result<Option<PursuitBudget>, String> {
    let args = args.trim();
    if args.is_empty() {
        return Ok(None);
    }
    let mut budget = PursuitBudget::default();
    for token in args.split_whitespace() {
        let Some((axis, value)) = token.split_once('=') else {
            return Err(format!(
                "invalid budget token '{token}': expected axis=value (e.g. passes=20)"
            ));
        };
        let n: u64 = value
            .parse()
            .map_err(|_| format!("invalid budget value '{value}': must be a positive integer"))?;
        if n == 0 {
            return Err(format!("budget value for {axis} must be positive, got 0"));
        }
        match axis.to_ascii_lowercase().as_str() {
            "passes" | "turns" => {
                if n > u32::MAX as u64 {
                    return Err("passes budget too large".to_string());
                }
                budget.max_passes = Some(n as u32);
            }
            "tokens" => budget.max_tokens = Some(n),
            "time" => budget.max_wall_clock_ms = Some(n),
            other => {
                return Err(format!(
                    "unknown budget axis '{other}': use passes, tokens, or time"
                ));
            }
        }
    }
    Ok(Some(budget))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pursuit_status_shows_active_state() {
        let pursuit = Pursuit {
            objective: "ship".to_string(),
            is_complete: false,
            ..Default::default()
        };
        let status = format_pursuit_status(&pursuit);
        assert!(status.contains("Pursuit [active]: ship"));
    }

    #[test]
    fn pursuit_status_shows_complete_state() {
        let pursuit = Pursuit {
            objective: "ship".to_string(),
            is_complete: true,
            ..Default::default()
        };
        let status = format_pursuit_status(&pursuit);
        assert!(status.contains("Pursuit [complete]: ship"));
    }

    #[test]
    fn budget_parser_accepts_legacy_turns_but_formatter_emits_passes() {
        let budget = parse_pursuit_budget("turns=7 tokens=100")
            .unwrap()
            .expect("non-empty budget");
        assert_eq!(budget.max_passes, Some(7));
        assert_eq!(format_pursuit_budget(Some(budget)), "passes=7 tokens=100");
    }

    #[test]
    fn budget_parser_uses_passes_as_the_canonical_axis() {
        let budget = parse_pursuit_budget("passes=3 time=5000")
            .unwrap()
            .expect("non-empty budget");
        assert_eq!(budget.max_passes, Some(3));
        assert_eq!(format_pursuit_budget(Some(budget)), "passes=3 time=5000ms");
    }
}
