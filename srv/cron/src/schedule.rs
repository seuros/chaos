//! Schedule parsing — cron expressions and interval shorthands.
//!
//! Croner uses chrono internally; we bridge through epoch seconds so the rest
//! of the codebase stays on jiff.

use anyhow::Context;
use chrono::{DateTime, TimeZone, Utc};
use croner::Cron;
use jiff::Timestamp;

/// Parsed schedule that can compute the next run time.
pub enum Schedule {
    /// Standard cron expression (e.g., "*/5 * * * *").
    Cron(Box<Cron>),
    /// Fixed interval in seconds (e.g., "5m" → 300).
    Interval(i64),
}

impl Schedule {
    /// Parse a schedule string. Accepts cron expressions or interval shorthands
    /// like "30s", "5m", "2h", "1d".
    pub fn parse(input: &str) -> anyhow::Result<Self> {
        if let Some(seconds) = parse_interval(input) {
            anyhow::ensure!(seconds > 0, "interval must be positive");
            return Ok(Self::Interval(seconds));
        }
        let cron: Cron = input
            .parse()
            .with_context(|| format!("invalid cron expression: {input}"))?;
        Ok(Self::Cron(Box::new(cron)))
    }

    /// Compute the next run time after the given timestamp.
    /// Returns epoch seconds.
    pub fn next_after(&self, after: Timestamp) -> anyhow::Result<i64> {
        match self {
            Self::Interval(secs) => Ok(after.as_second() + secs),
            Self::Cron(cron) => {
                let chrono_dt: DateTime<Utc> = Utc
                    .timestamp_opt(after.as_second(), 0)
                    .single()
                    .context("invalid timestamp for chrono conversion")?;
                let next = cron
                    .find_next_occurrence(&chrono_dt, false)
                    .context("no next occurrence found for cron expression")?;
                Ok(next.timestamp())
            }
        }
    }
}

/// Parse interval shorthands: "30s", "5m", "2h", "1d".
fn parse_interval(input: &str) -> Option<i64> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let (digits, suffix) = input.split_at(input.len() - 1);
    let n: i64 = digits.parse().ok()?;
    let multiplier = match suffix {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return None,
    };
    Some(n * multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_interval_shorthands() {
        assert_eq!(parse_interval("30s"), Some(30));
        assert_eq!(parse_interval("5m"), Some(300));
        assert_eq!(parse_interval("2h"), Some(7200));
        assert_eq!(parse_interval("1d"), Some(86400));
        assert_eq!(parse_interval("nope"), None);
        assert_eq!(parse_interval("* * * * *"), None);
    }

    #[test]
    fn cron_expression_parses() {
        let sched = Schedule::parse("*/5 * * * *");
        assert!(sched.is_ok());
        assert!(matches!(sched.as_ref().ok(), Some(Schedule::Cron(_))));
    }

    #[test]
    fn interval_parses() {
        let sched = Schedule::parse("10m");
        assert!(sched.is_ok());
        assert!(matches!(sched.as_ref().ok(), Some(Schedule::Interval(600))));
    }

    #[test]
    fn interval_next_after() {
        let sched = Schedule::parse("5m").ok();
        let ts = Timestamp::from_second(1000).ok();
        if let (Some(s), Some(t)) = (sched, ts) {
            let next = s.next_after(t).ok();
            assert_eq!(next, Some(1300));
        }
    }

    #[test]
    fn cron_next_after() {
        // Every minute — next occurrence after epoch 0 should be 60
        let sched = Schedule::parse("* * * * *").ok();
        let ts = Timestamp::from_second(0).ok();
        if let (Some(s), Some(t)) = (sched, ts) {
            let next = s.next_after(t).ok();
            assert_eq!(next, Some(60));
        }
    }
}
