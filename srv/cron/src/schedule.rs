//! Structured schedules: filled in directly by the LLM caller, not parsed
//! from free-text cron expressions. Everything is computed in UTC via jiff;
//! no chrono/croner in this crate.

use anyhow::Context;
use jiff::Timestamp;
use jiff::ToSpan;
use jiff::Zoned;
use jiff::civil::Time;
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Day of week for `Schedule::Weekly`. A local enum (rather than
/// `jiff::civil::Weekday`) because jiff's `Weekday` doesn't derive
/// serde/schemars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    fn to_jiff(self) -> jiff::civil::Weekday {
        match self {
            Self::Mon => jiff::civil::Weekday::Monday,
            Self::Tue => jiff::civil::Weekday::Tuesday,
            Self::Wed => jiff::civil::Weekday::Wednesday,
            Self::Thu => jiff::civil::Weekday::Thursday,
            Self::Fri => jiff::civil::Weekday::Friday,
            Self::Sat => jiff::civil::Weekday::Saturday,
            Self::Sun => jiff::civil::Weekday::Sunday,
        }
    }
}

/// A schedule for a recurring cron job. Filled in directly by the LLM
/// caller via the MCP tool schema; no string parsing involved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Schedule {
    /// Run every N seconds.
    Interval { seconds: i64 },
    /// Run once a day at a fixed UTC hour:minute.
    Daily { hour: u8, minute: u8 },
    /// Run once a week on a fixed UTC weekday + hour:minute.
    Weekly {
        weekday: Weekday,
        hour: u8,
        minute: u8,
    },
}

impl Schedule {
    /// Deserialize a schedule from its stored JSON form.
    pub fn parse(input: &str) -> anyhow::Result<Self> {
        let schedule: Self =
            serde_json::from_str(input).context("invalid schedule (expected schedule JSON)")?;
        schedule.validate()?;
        Ok(schedule)
    }

    /// Serialize to the JSON form stored in the `cron_jobs.schedule` column.
    pub fn to_json(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).context("failed to serialize schedule")
    }

    /// Check that field values are in range (e.g. `hour < 24`).
    pub fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::Interval { seconds } => {
                anyhow::ensure!(*seconds > 0, "interval seconds must be positive");
            }
            Self::Daily { hour, minute } | Self::Weekly { hour, minute, .. } => {
                anyhow::ensure!(*hour < 24, "hour must be in 0..24");
                anyhow::ensure!(*minute < 60, "minute must be in 0..60");
            }
        }
        Ok(())
    }

    /// Compute the next run time strictly after `after`. Returns epoch seconds.
    pub fn next_after(&self, after: Timestamp) -> anyhow::Result<i64> {
        self.validate()?;
        match self {
            Self::Interval { seconds } => Ok(after.as_second() + seconds),
            Self::Daily { hour, minute } => {
                let next = next_daily(after, *hour, *minute)?;
                Ok(next.timestamp().as_second())
            }
            Self::Weekly {
                weekday,
                hour,
                minute,
            } => {
                let next = next_weekly(after, weekday.to_jiff(), *hour, *minute)?;
                Ok(next.timestamp().as_second())
            }
        }
    }
}

fn candidate_at(zdt: &Zoned, hour: u8, minute: u8) -> anyhow::Result<Zoned> {
    let time = Time::new(hour as i8, minute as i8, 0, 0).context("invalid hour/minute")?;
    Ok(zdt.with().time(time).build()?)
}

fn next_daily(after: Timestamp, hour: u8, minute: u8) -> anyhow::Result<Zoned> {
    let zdt = after.to_zoned(TimeZone::UTC);
    let mut candidate = candidate_at(&zdt, hour, minute)?;
    if candidate.timestamp() <= after {
        candidate = candidate_at(&candidate.checked_add(1.day())?, hour, minute)?;
    }
    Ok(candidate)
}

fn next_weekly(
    after: Timestamp,
    weekday: jiff::civil::Weekday,
    hour: u8,
    minute: u8,
) -> anyhow::Result<Zoned> {
    let zdt = after.to_zoned(TimeZone::UTC);
    let day_offset = zdt.weekday().until(weekday);
    let mut candidate = candidate_at(&zdt.checked_add((day_offset as i64).days())?, hour, minute)?;
    if candidate.timestamp() <= after {
        candidate = candidate_at(&candidate.checked_add(7.days())?, hour, minute)?;
    }
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_round_trips_and_advances() {
        let sched = Schedule::Interval { seconds: 300 };
        let json = sched.to_json().expect("serialize");
        let parsed = Schedule::parse(&json).expect("parse");
        let ts = Timestamp::from_second(1000).expect("ts");
        assert_eq!(parsed.next_after(ts).expect("next"), 1300);
    }

    #[test]
    fn interval_rejects_non_positive_seconds() {
        let sched = Schedule::Interval { seconds: 0 };
        let json = sched.to_json().expect("serialize");
        assert!(Schedule::parse(&json).is_err());
    }

    #[test]
    fn daily_advances_to_same_day_if_still_ahead() {
        // 2026-06-01T00:00:00Z, ask for 10:30 the same day.
        let after = Timestamp::from_second(1_780_272_000).expect("ts");
        let sched = Schedule::Daily {
            hour: 10,
            minute: 30,
        };
        let next = sched.next_after(after).expect("next");
        let zdt = Timestamp::from_second(next)
            .expect("ts")
            .to_zoned(TimeZone::UTC);
        assert_eq!(zdt.date().to_string(), "2026-06-01");
        assert_eq!((zdt.hour(), zdt.minute()), (10, 30));
    }

    #[test]
    fn daily_rolls_to_next_day_when_time_has_passed() {
        // 2026-06-01T12:00:00Z, ask for 10:30 (already past today), rolls to Jan 2.
        let after = Timestamp::from_second(1_780_315_200).expect("ts");
        let sched = Schedule::Daily {
            hour: 10,
            minute: 30,
        };
        let next = sched.next_after(after).expect("next");
        let zdt = Timestamp::from_second(next)
            .expect("ts")
            .to_zoned(TimeZone::UTC);
        assert_eq!(zdt.date().to_string(), "2026-06-02");
        assert_eq!((zdt.hour(), zdt.minute()), (10, 30));
    }

    #[test]
    fn weekly_rolls_to_next_matching_weekday() {
        // 2026-06-01 is a Monday. Ask for Wednesday 09:00.
        let after = Timestamp::from_second(1_780_272_000).expect("ts");
        let sched = Schedule::Weekly {
            weekday: Weekday::Wed,
            hour: 9,
            minute: 0,
        };
        let next = sched.next_after(after).expect("next");
        let zdt = Timestamp::from_second(next)
            .expect("ts")
            .to_zoned(TimeZone::UTC);
        assert_eq!(zdt.date().to_string(), "2026-06-03");
        assert_eq!(zdt.weekday(), jiff::civil::Weekday::Wednesday);
        assert_eq!((zdt.hour(), zdt.minute()), (9, 0));
    }

    #[test]
    fn weekly_rolls_a_full_week_when_same_day_but_time_passed() {
        // 2026-06-01T12:00:00Z is Monday noon. Ask for Monday 09:00, already
        // passed today, should roll a full week to 2026-06-08.
        let after = Timestamp::from_second(1_780_315_200).expect("ts");
        let sched = Schedule::Weekly {
            weekday: Weekday::Mon,
            hour: 9,
            minute: 0,
        };
        let next = sched.next_after(after).expect("next");
        let zdt = Timestamp::from_second(next)
            .expect("ts")
            .to_zoned(TimeZone::UTC);
        assert_eq!(zdt.date().to_string(), "2026-06-08");
        assert_eq!((zdt.hour(), zdt.minute()), (9, 0));
    }

    #[test]
    fn invalid_hour_is_rejected() {
        let sched = Schedule::Daily {
            hour: 24,
            minute: 0,
        };
        let json = sched.to_json().expect("serialize");
        assert!(Schedule::parse(&json).is_err());
    }
}
