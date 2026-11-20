//! The strings of the Priority and All Usage tabs: durations, run-out and reset texts, and numbers in row style.

use super::menu_bar::{compact, currency_symbol, tray_number, trim_fraction};
use super::meter::{Meter, Unit};
use super::{PriorityItem, Reason};
use crate::models::MetricValue;
use chrono::{DateTime, Datelike, Months, Utc};

/// The time from `start` to `end`, rounded up to the minute, in its largest unit and the next one down, like
/// "4h 12m" or "3mo 17d". None unless `end` is later. Months are UTC calendar months.
pub fn duration(start: DateTime<Utc>, end: DateTime<Utc>) -> Option<String> {
    let secs = (end - start).num_seconds() as f64 + f64::from((end - start).subsec_nanos()) / 1e9;
    if secs <= 0.0 {
        return None;
    }
    let end = start + chrono::Duration::seconds((secs / 60.0).ceil() as i64 * 60);
    let mut months = (end.year() - start.year()) * 12 + end.month() as i32 - start.month() as i32;
    while months > 0 && start.checked_add_months(Months::new(months as u32)).is_none_or(|d| d > end) {
        months -= 1;
    }
    let minutes = (end - start.checked_add_months(Months::new(months as u32))?).num_minutes();
    let parts = [(months as i64 / 12, "y"), (months as i64 % 12, "mo"), (minutes / 1440, "d"), (minutes % 1440 / 60, "h"), (minutes % 60, "m")];
    let first = parts.iter().position(|p| p.0 > 0)?;
    Some(parts[first..].iter().take(2).filter(|p| p.0 > 0).map(|p| format!("{}{}", p.0, p.1)).collect::<Vec<_>>().join(" "))
}

/// `MetricFormatter.number(.row)`: dollars show cents unless whole.
fn row_number(value: f64, unit: Unit, currency: Option<&str>) -> String {
    if unit == Unit::Dollars && value.abs() < 1000.0 {
        let whole = (value * 100.0).round() % 100.0 == 0.0;
        let symbol = currency_symbol(currency.unwrap_or("USD"));
        return format!("{symbol}{}", if whole { format!("{value:.0}") } else { format!("{value:.2}") });
    }
    tray_number(value, unit, currency)
}

impl Meter {
    /// What the bar shows, used or left.
    pub fn value_text(&self, show_remaining: bool) -> String {
        row_number(self.shown(show_remaining), self.unit, self.currency.as_deref())
    }

    /// How far the bar fills, 0 to 1.
    pub fn fill_fraction(&self, show_remaining: bool) -> f64 {
        if self.limit > 0.0 { (self.shown(show_remaining) / self.limit).clamp(0.0, 1.0) } else { 0.0 }
    }

    fn shown(&self, show_remaining: bool) -> f64 {
        let value = if show_remaining { (self.limit - self.used).max(0.0) } else { self.used };
        if self.unit == Unit::Percent { value.clamp(0.0, 100.0) } else { value }
    }
}

/// A values line's figures, joined with " · ". A currency code makes a value dollars.
pub fn values_text(values: &[MetricValue]) -> String {
    if values.is_empty() {
        return "No data".into();
    }
    let one = |v: &MetricValue| {
        let number = match &v.currency_code {
            Some(code) => row_number(v.number, Unit::Dollars, Some(code)),
            None if v.number.abs() >= 1000.0 => compact(v.number),
            None => trim_fraction(v.number, 1),
        };
        match &v.label {
            Some(label) => format!("{number} {label}"),
            None => number,
        }
    };
    values.iter().map(one).collect::<Vec<_>>().join(" · ")
}

impl PriorityItem {
    /// "Out in 2d 12h" for a limit listed only for burning fast, while it would run out before it resets.
    pub fn run_out_text(&self, now: DateTime<Utc>) -> Option<String> {
        let Reason::BurningFast { runway } = self.reason else { return None };
        let reset = self.meter.as_ref()?.resets_at?;
        let until_reset = (reset - now).num_milliseconds() as f64 / 1000.0;
        if runway >= until_reset {
            return None;
        }
        let out = now + chrono::Duration::milliseconds((runway * 1000.0) as i64);
        duration(now, out).map(|d| format!("Out in {d}"))
    }

    /// "Resets in 18h" for a limit listed to use before it resets.
    pub fn resets_in_text(&self, now: DateTime<Utc>) -> Option<String> {
        let Reason::UseItOrLoseIt { resets_at } = self.reason else { return None };
        duration(now, resets_at).map(|d| format!("Resets in {d}"))
    }

    /// The trailing text of a Used Up row: the time until it resets, or for a balance, which has no reset, its
    /// amount. ponytail: a per-provider word ("$0 left") would need descriptors, which are not here, so the bare amount.
    pub fn used_up_note(&self, now: DateTime<Utc>) -> Option<String> {
        match self.reason {
            Reason::EmptyBalance => Some(self.line.menu_bar_value.clone()),
            _ => self.meter.as_ref()?.reset_text(now),
        }
    }
}
