use chrono::{DateTime, Utc};
use cron::Schedule;
use std::time::Duration;

/// Defines when a background job should be executed.
#[derive(Debug, Clone)]
pub enum Trigger {
    /// Execute the job repeatedly at a fixed interval.
    Interval(Duration),
    /// Execute the job according to a standard unix cron expression.
    Cron(Schedule),
}

impl Trigger {
    /// Creates a new `Trigger::Cron` from a cron expression string.
    ///
    /// The string must follow the format required by the `cron` crate.
    /// Example: `"0 0 * * * *"` (every hour)
    pub fn cron(expr: &str) -> Result<Self, cron::error::Error> {
        let schedule = expr.parse::<Schedule>()?;
        Ok(Self::Cron(schedule))
    }

    /// Creates a new `Trigger::Interval` with the given duration.
    pub fn interval(duration: Duration) -> Self {
        Self::Interval(duration)
    }

    /// Calculates the next execution time based on this trigger.
    pub fn next(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Trigger::Interval(dur) => {
                let dur_chrono = match chrono::Duration::from_std(*dur) {
                    Ok(d) => d,
                    Err(_) => return None,
                };
                after.checked_add_signed(dur_chrono)
            }
            Trigger::Cron(schedule) => schedule.after(&after).next(),
        }
    }
}
