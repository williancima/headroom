//! A bounded limit and what its pace says.

use crate::models::{MetricLine, ProgressFormat, ProviderKind};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const USE_IT_LEAD_SECS: f64 = 24.0 * 60.0 * 60.0;
const RESET_JITTER_MS: i64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Normal,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterState {
    Spent,
    RunningOut,
    CloseToLimit,
    Healthy,
    Unpaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unit {
    Percent,
    Dollars,
    Count,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    ZeroUsage,
    MissingResetDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaceStatus {
    Ahead,
    OnTrack,
    Behind,
}

/// Where a limit's usage is read, with the provider's percent thresholds (`PriorityLimits`, standard 30/10).
#[derive(Debug, Clone, PartialEq)]
pub struct Meter {
    pub used: f64,
    pub limit: f64,
    pub(super) unit: Unit,
    pub(super) currency: Option<String>,
    pub resets_at: Option<DateTime<Utc>>,
    pub period_ms: Option<i64>,
    signal: Option<Signal>,
}

/// Which accounts' lines start fresh windows.
fn session_signal(kind: ProviderKind, label: &str) -> Option<Signal> {
    match (kind, label) {
        (ProviderKind::Claude | ProviderKind::Muse | ProviderKind::Opencode, "Session") => Some(Signal::MissingResetDate),
        (ProviderKind::Antigravity, "Session" | "Claude") => Some(Signal::ZeroUsage),
        _ => None,
    }
}

impl Meter {
    /// Only a `Progress` line is a bounded meter.
    pub fn from_line(kind: ProviderKind, line: &MetricLine) -> Option<Self> {
        let MetricLine::Progress { label, used, limit, format, resets_at, period_ms } = line else { return None };
        let (unit, currency) = match format {
            ProgressFormat::Percent => (Unit::Percent, None),
            ProgressFormat::Dollars => (Unit::Dollars, None),
            ProgressFormat::Currency(code) => (Unit::Dollars, Some(code.clone())),
            ProgressFormat::Count(_) => (Unit::Count, None),
        };
        Some(Self {
            used: *used,
            limit: *limit,
            unit,
            currency,
            resets_at: *resets_at,
            period_ms: *period_ms,
            signal: session_signal(kind, label),
        })
    }

    pub fn remaining_fraction(&self) -> f64 {
        if self.limit > 0.0 { ((self.limit - self.used) / self.limit).clamp(0.0, 1.0) } else { 0.0 }
    }

    /// From what's left alone. `warning`/`critical` are percents left.
    pub fn severity(&self, warning: i64, critical: i64) -> Severity {
        if self.limit <= 0.0 {
            return Severity::Normal;
        }
        let left = (self.remaining_fraction() * 100.0).round() as i64;
        if left <= critical {
            Severity::Critical
        } else if left <= warning {
            Severity::Warning
        } else {
            Severity::Normal
        }
    }

    fn at_display_precision(&self, value: f64) -> f64 {
        match self.unit {
            Unit::Percent => value.round(),
            Unit::Count => (value * 10.0).round() / 10.0,
            Unit::Dollars => (value * 100.0).round() / 100.0,
        }
    }

    /// Nothing is left at the precision the row shows.
    pub fn is_spent(&self) -> bool {
        self.limit > 0.0 && self.at_display_precision(self.limit - self.used) <= 0.0
    }

    fn fresh_session_window(&self, now: DateTime<Utc>) -> bool {
        if self.used > 0.0 {
            return false;
        }
        match self.signal {
            Some(Signal::ZeroUsage) => self.resets_at.is_some_and(|reset| now < reset),
            Some(Signal::MissingResetDate) => self.resets_at.is_none(),
            None => false,
        }
    }

    /// Share of the window passed: nil until a minute and 1% of it have, and once it resets.
    fn elapsed_share(reset: DateTime<Utc>, period: f64, now: DateTime<Utc>) -> Option<f64> {
        if period <= 0.0 {
            return None;
        }
        let elapsed = (now - reset).num_milliseconds() as f64 / 1000.0 + period;
        (elapsed >= f64::max(60.0, period * 0.01) && now < reset).then_some(elapsed / period)
    }

    /// Status and projected usage as a fraction of the limit.
    fn pace(&self, now: DateTime<Utc>) -> Option<(PaceStatus, f64)> {
        let (reset, period) = (self.resets_at?, self.period_ms.filter(|ms| *ms > 0)? as f64 / 1000.0);
        if self.limit <= 0.0 || self.used <= 0.0 || self.fresh_session_window(now) {
            return None;
        }
        let projected = self.used / Self::elapsed_share(reset, period, now)?;
        let status = if self.used >= self.limit || projected > self.limit {
            PaceStatus::Behind
        } else if projected <= self.limit * 0.9 {
            PaceStatus::Ahead
        } else {
            PaceStatus::OnTrack
        };
        (status == PaceStatus::Ahead || self.used / self.limit >= 0.05).then_some((status, projected / self.limit))
    }

    /// The meter's state.
    pub fn state(&self, now: DateTime<Utc>) -> MeterState {
        if self.limit <= 0.0 {
            return MeterState::Unpaced;
        }
        if self.is_spent() {
            return MeterState::Spent;
        }
        match self.pace(now) {
            None => MeterState::Unpaced,
            Some((PaceStatus::Ahead, _)) => MeterState::Healthy,
            Some((PaceStatus::Behind, _)) => MeterState::RunningOut,
            Some((PaceStatus::OnTrack, projected)) => {
                if ((1.0 - projected) * 100.0).round() as i64 >= 1 { MeterState::CloseToLimit } else { MeterState::RunningOut }
            }
        }
    }

    /// Seconds until the limit at the current pace, or None without a pace.
    pub fn runway(&self, now: DateTime<Utc>) -> Option<f64> {
        let (_, projected) = self.pace(now)?;
        Some((1.0 - self.used / self.limit).max(0.0) * (self.period_ms? as f64 / 1000.0) / projected)
    }

    /// Over half left, resets within 24 hours, window longer than a day, not a dollar cap.
    pub fn is_unused_near_reset(&self, now: DateTime<Utc>) -> bool {
        let (Some(reset), Some(period)) = (self.resets_at, self.period_ms) else { return false };
        if self.unit == Unit::Dollars || period <= DAY_MS {
            return false;
        }
        let until = (reset - now).num_milliseconds() as f64 / 1000.0;
        until > 0.0 && until <= USE_IT_LEAD_SECS && (self.remaining_fraction() * 100.0).round() as i64 > 50
    }
}

/// Limits that were burning fast at their last reading, with the reset of the window they burned in.
/// Once burning, a limit stays listed until its pace eases to 90% of the limit.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BurningFast {
    pub latched: HashMap<String, DateTime<Utc>>,
}

impl BurningFast {
    pub fn contains(&self, id: &str, meter: &Meter, now: DateTime<Utc>) -> bool {
        match meter.state(now) {
            MeterState::RunningOut => true,
            MeterState::CloseToLimit => self.was_burning(id, meter.resets_at, now),
            _ => false,
        }
    }

    /// A new window starts afresh, whatever the last one did.
    pub fn was_burning(&self, id: &str, resets_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        self.latched.get(id).is_some_and(|reset| !reset_window_advanced(resets_at, Some(*reset), now))
    }
}

/// A new window starts only once the old reset has passed.
pub fn reset_window_advanced(resets_at: Option<DateTime<Utc>>, previous: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    let Some(reset) = resets_at else { return false };
    let Some(previous) = previous else { return true };
    previous <= now && (reset - previous).num_milliseconds() > RESET_JITTER_MS
}

impl Meter {
    /// The countdown under a limit: "soon" once it is due, nothing for a fresh session window or no reset.
    pub fn reset_text(&self, now: DateTime<Utc>) -> Option<String> {
        let reset = self.resets_at.filter(|_| !self.fresh_session_window(now))?;
        if reset <= now { Some("soon".into()) } else { super::text::duration(now, reset) }
    }
}
