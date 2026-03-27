use std::{str::FromStr, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};
use cron::Schedule;

#[derive(Debug, Clone)]
pub enum ScheduleSpec {
    Interval(Duration),
    Cron {
        expression: String,
        schedule: Schedule,
    },
}

impl ScheduleSpec {
    pub fn from_args(interval: Option<&str>, cron: Option<&str>) -> Result<Self> {
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
                Ok(Self::Interval(parsed))
            }
            (None, None) => Ok(Self::Interval(Duration::from_secs(3600))),
        }
    }

    pub fn next_after(&self, now: DateTime<Local>) -> Result<DateTime<Local>> {
        match self {
            Self::Interval(interval) => {
                let chrono_duration = chrono::Duration::from_std(*interval)
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
            Self::Interval(_) => None,
            Self::Cron { expression, .. } => Some(expression.as_str()),
        }
    }
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
    use chrono::Local;

    #[test]
    fn accepts_five_field_cron() {
        let schedule = ScheduleSpec::from_args(None, Some("*/15 * * * *")).unwrap();
        assert_eq!(schedule.expression(), Some("0 */15 * * * *"));
    }

    #[test]
    fn expands_cron_aliases() {
        let schedule = ScheduleSpec::from_args(None, Some("@hourly")).unwrap();
        assert_eq!(schedule.expression(), Some("0 0 * * * *"));
    }

    #[test]
    fn interval_schedule_moves_forward() {
        let schedule = ScheduleSpec::from_args(Some("30m"), None).unwrap();
        let next = schedule.next_after(Local::now()).unwrap();
        assert!(next > Local::now());
    }
}
