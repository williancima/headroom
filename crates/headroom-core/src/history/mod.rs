//! Usage a provider reports per day, its windows and breakdowns, and the persisted ledger of read days
//! Sources read the provider API; nothing is estimated.

mod breakdown;
mod ledger;
mod panel;
mod row;
#[cfg(test)]
mod tests;

pub use breakdown::{Breakdown, Model, ModelFigure, TokenPart};
pub use ledger::{refresh_history, Ledger, Read, Source};
pub use panel::{BreakdownView, ModelLine, TokenPartLine};
pub use row::{history_lines, HistoryOutput, HistoryRow, Run};

use crate::models::MetricValue;
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// How long to wait after a rate limit, from a `Retry-After` header in seconds or as an HTTP date; at most
/// an hour, and `None` when it is missing or not in the future.
pub fn retry_after(header: Option<&str>, now: DateTime<Utc>) -> Option<Duration> {
    let value = header?.trim();
    let seconds = value.parse::<f64>().ok().or_else(|| {
        DateTime::parse_from_rfc2822(value).ok().map(|at| (at.with_timezone(&Utc) - now).num_milliseconds() as f64 / 1000.0)
    })?;
    (seconds.is_finite() && seconds > 0.0).then(|| Duration::seconds(seconds.min(3600.0) as i64))
}

/// The calendar whose days a provider reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Calendar {
    Local,
    Utc,
    /// UTC with ISO 8601 weeks, which start on Monday (OpenRouter's calendar).
    IsoUtc,
    /// A fixed offset east of UTC in seconds.
    Fixed(i32),
}

impl Calendar {
    /// The day that holds `now`.
    pub fn today(self, now: DateTime<Utc>) -> NaiveDate {
        let offset = match self {
            Self::Local => chrono::Local.offset_from_utc_datetime(&now.naive_utc()).local_minus_utc(),
            Self::Utc | Self::IsoUtc => 0,
            Self::Fixed(seconds) => seconds,
        };
        (now + Duration::seconds(i64::from(offset))).date_naive()
    }

    /// The instant `day` starts on this calendar.
    pub fn day_start(self, day: NaiveDate) -> Option<DateTime<Utc>> {
        let midnight = day.and_hms_opt(0, 0, 0)?;
        match self {
            Self::Local => chrono::Local.from_local_datetime(&midnight).earliest().map(|t| t.with_timezone(&Utc)),
            Self::Utc | Self::IsoUtc => Some(Utc.from_utc_datetime(&midnight)),
            Self::Fixed(seconds) => Some(Utc.from_utc_datetime(&midnight) - Duration::seconds(i64::from(seconds))),
        }
    }
}

/// One quantity a provider reports for a span. Never estimated, never priced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Measure {
    Spend,
    /// Spend on the user's own provider keys; never added to `spend`.
    ByokSpend,
    UsageValue,
    Credits,
    UsageEvents,
    Requests,
    Turns,
    Tokens,
    InputTokens,
    CacheReadTokens,
    CacheWriteTokens,
    OutputTokens,
    ReasoningTokens,
}

impl Measure {
    /// The kinds of token a provider may split its total into, with their short names.
    pub const TOKEN_PARTS: [(Measure, &'static str); 5] = [
        (Self::InputTokens, "Input"),
        (Self::CacheReadTokens, "Cache Read"),
        (Self::CacheWriteTokens, "Cache Write"),
        (Self::OutputTokens, "Output"),
        (Self::ReasoningTokens, "Reasoning"),
    ];

    pub fn metric_value(self, number: f64) -> MetricValue {
        let (dollars, label) = match self {
            Self::Spend | Self::UsageValue => (true, None),
            Self::ByokSpend => (true, Some("BYOK")),
            Self::Credits => (false, Some("credits")),
            Self::UsageEvents => (false, Some("events")),
            Self::Requests => (false, Some("requests")),
            Self::Turns => (false, Some("turns")),
            _ => (false, Some("tokens")),
        };
        MetricValue { number, currency_code: dollars.then(|| "USD".to_string()), label: label.map(String::from) }
    }
}

/// The measures a provider reported for one span. A missing measure was not reported, which is never zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Totals {
    pub values: BTreeMap<Measure, f64>,
}

impl<const N: usize> From<[(Measure, f64); N]> for Totals {
    fn from(pairs: [(Measure, f64); N]) -> Self {
        Self { values: pairs.into() }
    }
}

impl Totals {
    pub fn zero(measures: &[Measure]) -> Self {
        Self { values: measures.iter().map(|m| (*m, 0.0)).collect() }
    }

    pub fn get(&self, measure: Measure) -> Option<f64> {
        self.values.get(&measure).copied()
    }

    /// Sums what either reports, so a day missing a measure doesn't cost the range the rest of it.
    pub fn adding(&self, other: &Totals) -> Totals {
        let mut values = self.values.clone();
        for (measure, number) in &other.values {
            *values.entry(*measure).or_insert(0.0) += number;
        }
        Totals { values }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Day {
    pub totals: Totals,
    /// What the provider reported by model for the day; `None` when it named none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_model: Option<BTreeMap<String, Totals>>,
}

impl Day {
    pub fn new(totals: Totals) -> Self {
        Self { totals, by_model: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Window {
    Today,
    Yesterday,
    /// Today and the days before it.
    LastDays(i64),
    /// The days before today.
    PriorDays(i64),
    /// The calendar's week or month so far.
    ThisWeek,
    ThisMonth,
}

impl Window {
    pub fn title(self) -> String {
        match self {
            Self::Today => "Today".into(),
            Self::Yesterday => "Yesterday".into(),
            Self::LastDays(count) => format!("Last {count} Days"),
            Self::PriorDays(count) => format!("Prior {count} Days"),
            Self::ThisWeek => "This Week".into(),
            Self::ThisMonth => "This Month".into(),
        }
    }

    pub fn phrase(self) -> String {
        match self {
            Self::Today => "today".into(),
            Self::Yesterday => "yesterday".into(),
            Self::LastDays(count) => format!("in {count} days"),
            Self::PriorDays(count) => format!("in prior {count} days"),
            Self::ThisWeek => "this week".into(),
            Self::ThisMonth => "this month".into(),
        }
    }

    /// `None` for a window whose length depends on the calendar.
    pub fn day_offsets(self) -> Option<std::ops::RangeInclusive<i64>> {
        match self {
            Self::Today => Some(0..=0),
            Self::Yesterday => Some(1..=1),
            Self::LastDays(count) => Some(0..=count - 1),
            Self::PriorDays(count) => Some(1..=count),
            Self::ThisWeek | Self::ThisMonth => None,
        }
    }
}

/// Usage a provider reported per day, as of one moment.
#[derive(Debug, Clone)]
pub struct History {
    pub calendar: Calendar,
    pub as_of: DateTime<Utc>,
    /// A day that is absent was not read.
    pub days: BTreeMap<NaiveDate, Day>,
    /// A window's totals as the provider reported them; each measure they report wins over the sum of days.
    pub reported: HashMap<Window, Totals>,
}

impl History {
    pub fn new(calendar: Calendar, as_of: DateTime<Utc>, days: BTreeMap<NaiveDate, Day>) -> Self {
        Self { calendar, as_of, days, reported: HashMap::new() }
    }

    /// The days `offsets` back from the day holding `now`.
    pub fn day_starts(offsets: impl IntoIterator<Item = i64>, now: DateTime<Utc>, calendar: Calendar) -> Vec<NaiveDate> {
        let today = calendar.today(now);
        offsets.into_iter().filter_map(|offset| today.checked_sub_signed(Duration::days(offset))).collect()
    }

    /// The sum of the window's days that have been read, under what the provider reported for the window;
    /// `None` when neither exists.
    pub fn totals(&self, window: Window) -> Option<Totals> {
        let summed = self.days_in(window).into_iter().map(|d| &d.totals).fold(None::<Totals>, |sum, totals| {
            Some(sum.map_or_else(|| totals.clone(), |sum| sum.adding(totals)))
        });
        let Some(reported) = self.reported.get(&window) else { return summed };
        let mut values = summed.map(|s| s.values).unwrap_or_default();
        values.extend(reported.values.iter().map(|(m, n)| (*m, *n)));
        Some(Totals { values })
    }

    /// What the window's days report by model, added up.
    pub fn models(&self, window: Window) -> BTreeMap<String, Totals> {
        let mut models: BTreeMap<String, Totals> = BTreeMap::new();
        for day in self.days_in(window) {
            for (name, totals) in day.by_model.iter().flatten() {
                let sum = models.get(name).map_or_else(|| totals.clone(), |have| have.adding(totals));
                models.insert(name.clone(), sum);
            }
        }
        models
    }

    /// Sources record a successfully read date even when the provider had no activity to report.
    pub fn unread_days(&self, window: Window) -> usize {
        let Some(offsets) = window.day_offsets().filter(|_| !self.reported.contains_key(&window)) else { return 0 };
        Self::day_starts(offsets, self.as_of, self.calendar).iter().filter(|d| !self.days.contains_key(d)).count()
    }

    fn days_in(&self, window: Window) -> Vec<&Day> {
        if let Some(offsets) = window.day_offsets() {
            return Self::day_starts(offsets, self.as_of, self.calendar).iter().filter_map(|d| self.days.get(d)).collect();
        }
        let today = self.calendar.today(self.as_of);
        // ponytail: weeks start on Sunday (the gregorian default) or Monday for `IsoUtc`; a source on a
        // locale calendar whose week starts elsewhere would need its first weekday here.
        let in_window = |day: &NaiveDate| match window {
            Window::ThisWeek => {
                let since = if self.calendar == Calendar::IsoUtc { today.weekday().num_days_from_monday() } else { today.weekday().num_days_from_sunday() };
                let start = today - Duration::days(i64::from(since));
                (start..start + Duration::days(7)).contains(day)
            }
            _ => (day.year(), day.month()) == (today.year(), today.month()),
        };
        self.days.iter().filter(|(day, _)| in_window(day)).map(|(_, d)| d).collect()
    }
}
