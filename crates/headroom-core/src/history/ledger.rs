//! An account's read days. Persisted in `ProviderState`, because
//! the CLI refresh is one-shot and the app and the CLI share the file.

use super::row::HistoryRow;
use super::{Calendar, Day, History};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;

/// What a source read: the days read in full, and how long to wait when the provider rate-limited it.
#[derive(Debug, Default)]
pub struct Read {
    /// A day that could not be read is left out.
    pub days: BTreeMap<NaiveDate, Day>,
    pub rate_limited_for: Option<Duration>,
}

pub trait Source: Send + Sync {
    /// The calendar whose days the provider reports.
    fn clock(&self) -> Calendar {
        Calendar::Local
    }

    /// How long today and yesterday stay read; `None` rereads them on every refresh.
    fn reread_interval(&self) -> Option<Duration> {
        None
    }

    /// Today's read ends at `as_of`.
    fn read(&self, days: &[NaiveDate], as_of: DateTime<Utc>, clock: Calendar) -> impl Future<Output = Read> + Send;
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    days: BTreeMap<NaiveDate, Day>,
    /// Nothing is read before this once the provider rate-limited a read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resume_at: Option<DateTime<Utc>>,
    /// Today at the last record, whose read ended before the day did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    partial_day: Option<NaiveDate>,
    /// When the last read was sent, and the day that was today then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_read: Option<(DateTime<Utc>, NaiveDate)>,
}

impl Ledger {
    pub fn days(&self) -> &BTreeMap<NaiveDate, Day> {
        &self.days
    }

    /// `span` runs back from today. Today and yesterday are read on every refresh, an older day until one
    /// read of it after its end succeeds.
    pub fn due(&self, span: &[NaiveDate]) -> Vec<NaiveDate> {
        let old = |d: &&NaiveDate| !self.days.contains_key(*d) || self.partial_day == Some(**d);
        span.iter().take(2).chain(span.iter().skip(2).filter(old)).copied().collect()
    }

    /// Days outside `span` are dropped, and a due day that did not come back is unread, so a failed re-read
    /// removes the day instead of showing an old value.
    pub fn record(&mut self, read: BTreeMap<NaiveDate, Day>, due: &[NaiveDate], span: &[NaiveDate]) {
        self.days.retain(|day, _| span.contains(day) && !due.contains(day));
        self.days.extend(read);
        self.partial_day = span.first().copied();
    }

    pub fn is_held(&self, now: DateTime<Utc>) -> bool {
        self.resume_at.is_some_and(|resume| now < resume)
    }

    pub fn hold(&mut self, wait: Duration, now: DateTime<Utc>) {
        self.resume_at = Some(now + wait);
    }

    /// Whether nothing is due for a source that rereads once per `interval`: the last read was sent on this
    /// same day, less than `interval` ago. A new day is read at once.
    pub fn is_resting(&self, span: &[NaiveDate], now: DateTime<Utc>, interval: Duration) -> bool {
        match self.last_read {
            Some((at, today)) if span.first() == Some(&today) => now < at + interval,
            _ => false,
        }
    }

    pub fn note_read(&mut self, now: DateTime<Utc>, today: NaiveDate) {
        self.last_read = Some((now, today));
    }

    /// A forced refresh reads today and yesterday again at once.
    pub fn forget_last_read(&mut self) {
        self.last_read = None;
    }
}

/// Reads the due days and returns every read day; `None` when no row asks for history.
pub async fn refresh_history(source: &impl Source, ledger: &mut Ledger, rows: &[(String, HistoryRow)], now: DateTime<Utc>) -> Option<History> {
    let day_count = HistoryRow::day_count(rows)?;
    let calendar = source.clock();
    let span = History::day_starts(0..day_count, now, calendar);
    if source.reread_interval().is_some_and(|interval| ledger.is_resting(&span, now, interval)) {
        return Some(History::new(calendar, now, ledger.days().clone()));
    }
    let due = ledger.due(&span);
    if ledger.is_held(now) {
        ledger.record(BTreeMap::new(), &due, &span);
    } else {
        ledger.note_read(now, span[0]);
        let read = source.read(&due, now, calendar).await;
        ledger.record(read.days, &due, &span);
        if let Some(wait) = read.rate_limited_for {
            ledger.hold(wait, now);
        }
    }
    Some(History::new(calendar, now, ledger.days().clone()))
}
