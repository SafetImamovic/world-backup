use std::{str::FromStr, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, LocalResult, NaiveTime, TimeZone};
use cron::Schedule;

#[derive(Debug, Clone)]
pub enum ScheduleSpec {
    Interval {
        duration: Duration,
        align_to_midnight: bool,
    },
    Cron {
        expression: String,
        schedule: Schedule,
    },
}

impl ScheduleSpec {
    pub fn from_args(
        interval: Option<&str>,
        cron: Option<&str>,
        align_to_midnight: bool,
    ) -> Result<Self> {
        match (interval, cron) {
            (Some(_), Some(_)) => bail!("only one of --interval or --cron may be used"),
            (_, Some(expression)) => {
                let normalized = normalize_cron(expression)?;
                let schedule = Schedule::from_str(&normalized)
                    .with_context(|| format!("invalid cron expression '{expression}'"))?;
                Ok(Self::Cron {
                    expression: normalized,
                    schedule,
                })
            }
            (Some(interval), None) => {
                let parsed = humantime::parse_duration(interval)
                    .with_context(|| format!("invalid interval '{interval}'"))?;
                if parsed.is_zero() {
                    bail!("interval must be greater than zero");
                }
                validate_aligned_interval(parsed, align_to_midnight)?;
                Ok(Self::Interval {
                    duration: parsed,
                    align_to_midnight,
                })
            }
            (None, None) => {
                let parsed = Duration::from_secs(3600);
                validate_aligned_interval(parsed, align_to_midnight)?;
                Ok(Self::Interval {
                    duration: parsed,
                    align_to_midnight,
                })
            }
        }
    }

    pub fn next_after(&self, now: DateTime<Local>) -> Result<DateTime<Local>> {
        match self {
            Self::Interval {
                duration,
                align_to_midnight,
            } => {
                if *align_to_midnight {
                    return next_aligned_interval_after(now, *duration);
                }

                let chrono_duration = chrono::Duration::from_std(*duration)
                    .context("failed to convert interval into a schedulable duration")?;
                Ok(now + chrono_duration)
            }
            Self::Cron { schedule, .. } => schedule
                .after(&now)
                .next()
                .context("cron expression produced no future execution time"),
        }
    }

    pub fn expression(&self) -> Option<&str> {
        match self {
            Self::Interval { .. } => None,
            Self::Cron { expression, .. } => Some(expression.as_str()),
        }
    }
}

fn validate_aligned_interval(interval: Duration, align_to_midnight: bool) -> Result<()> {
    if !align_to_midnight {
        return Ok(());
    }

    let day = Duration::from_secs(24 * 60 * 60);
    if interval > day {
        bail!("--run-immediately-aligned requires an interval of at most 24h");
    }
    if day.as_nanos() % interval.as_nanos() != 0 {
        bail!(
            "--run-immediately-aligned requires an interval that divides evenly into 24h, for example 15m, 30m, 1h, or 6h"
        );
    }

    Ok(())
}

fn next_aligned_interval_after(
    now: DateTime<Local>,
    interval: Duration,
) -> Result<DateTime<Local>> {
    let midnight_naive = now.date_naive().and_time(
        NaiveTime::from_hms_opt(0, 0, 0).context("failed to construct local midnight time")?,
    );
    let midnight = match Local.from_local_datetime(&midnight_naive) {
        LocalResult::Single(value) => value,
        LocalResult::Ambiguous(earliest, _) => earliest,
        LocalResult::None => bail!("local midnight could not be represented for the current date"),
    };

    let elapsed_ns = now
        .signed_duration_since(midnight)
        .num_nanoseconds()
        .context("aligned interval could not represent elapsed nanoseconds")?;
    let interval_ns = interval.as_nanos();
    let next_multiple = (elapsed_ns as u128 / interval_ns) + 1;
    let next_offset_ns = interval_ns
        .checked_mul(next_multiple)
        .context("aligned interval overflowed while calculating the next run")?;
    let next_offset_ns = i64::try_from(next_offset_ns)
        .context("aligned interval exceeded supported scheduler precision")?;

    Ok(midnight + chrono::Duration::nanoseconds(next_offset_ns))
}

fn normalize_cron(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("cron expression cannot be empty");
    }

    let alias = match trimmed {
        "@yearly" | "@annually" => Some("0 0 0 1 1 *"),
        "@monthly" => Some("0 0 0 1 * *"),
        "@weekly" => Some("0 0 0 * * 0"),
        "@daily" | "@midnight" => Some("0 0 0 * * *"),
        "@hourly" => Some("0 0 * * * *"),
        _ => None,
    };
    if let Some(expanded) = alias {
        return Ok(expanded.to_string());
    }

    let fields: Vec<&str> = trimmed.split_whitespace().collect();
    match fields.len() {
        5 => Ok(format!("0 {trimmed}")),
        6 | 7 => Ok(trimmed.to_string()),
        count => bail!("cron expression must have 5, 6, or 7 fields; found {count}"),
    }
}

#[cfg(test)]
mod tests {
    use super::ScheduleSpec;
    use chrono::{Local, TimeZone};

    #[test]
    fn accepts_five_field_cron() {
        let schedule = ScheduleSpec::from_args(None, Some("*/15 * * * *"), false).unwrap();
        assert_eq!(schedule.expression(), Some("0 */15 * * * *"));
    }

    #[test]
    fn expands_cron_aliases() {
        let schedule = ScheduleSpec::from_args(None, Some("@hourly"), false).unwrap();
        assert_eq!(schedule.expression(), Some("0 0 * * * *"));
    }

    #[test]
    fn interval_schedule_moves_forward() {
        let schedule = ScheduleSpec::from_args(Some("30m"), None, false).unwrap();
        let next = schedule.next_after(Local::now()).unwrap();
        assert!(next > Local::now());
    }

    #[test]
    fn aligned_interval_snaps_to_next_boundary() {
        let schedule = ScheduleSpec::from_args(Some("15m"), None, true).unwrap();
        let now = Local
            .with_ymd_and_hms(2026, 3, 27, 9, 39, 54)
            .single()
            .unwrap();
        let next = schedule.next_after(now).unwrap();
        assert_eq!(
            next,
            Local
                .with_ymd_and_hms(2026, 3, 27, 9, 45, 0)
                .single()
                .unwrap()
        );
    }

    #[test]
    fn aligned_interval_skips_current_boundary_after_immediate_run() {
        let schedule = ScheduleSpec::from_args(Some("30m"), None, true).unwrap();
        let now = Local
            .with_ymd_and_hms(2026, 3, 27, 9, 30, 0)
            .single()
            .unwrap();
        let next = schedule.next_after(now).unwrap();
        assert_eq!(
            next,
            Local
                .with_ymd_and_hms(2026, 3, 27, 10, 0, 0)
                .single()
                .unwrap()
        );
    }

    #[test]
    fn aligned_interval_rejects_non_divisible_day_interval() {
        assert!(ScheduleSpec::from_args(Some("7m"), None, true).is_err());
    }
}
