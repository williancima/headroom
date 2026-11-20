//! What one history row shows, and the glue that turns a source's days into a provider's rows.

use super::ledger::{refresh_history, Ledger, Source};
use super::{Breakdown, History, Measure, Window};
use crate::models::{MetricLine, MetricValue};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// A row holds two figures at most, because a third overflows the value column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HistoryRow {
    /// "Today  $1.20 · 1.2M tokens"
    Window(Window, Measure, Option<Measure>),
    /// "Usage Events  4.1K in 30 days"
    Measure(&'static str, Measure, Window),
}

impl HistoryRow {
    pub fn title(&self) -> String {
        match self {
            Self::Window(window, ..) => window.title(),
            Self::Measure(title, ..) => (*title).to_string(),
        }
    }

    pub fn window(&self) -> Window {
        match self {
            Self::Window(window, ..) | Self::Measure(_, _, window) => *window,
        }
    }

    /// `None` when none of the row's figures was reported.
    pub fn line(&self, label: &str, history: &History) -> Option<MetricLine> {
        let totals = history.totals(self.window())?;
        let values: Vec<MetricValue> = match self {
            Self::Window(_, first, then) => {
                [Some(*first), *then].into_iter().flatten().filter_map(|m| totals.get(m).map(|n| m.metric_value(n))).collect()
            }
            Self::Measure(_, measure, window) => totals
                .get(*measure)
                .map(|n| MetricValue { label: Some(window.phrase()), ..measure.metric_value(n) })
                .into_iter()
                .collect(),
        };
        (!values.is_empty()).then(|| MetricLine::Values { label: label.to_string(), values })
    }

    /// What a window row reads when the provider reports nothing for it: zeros.
    /// A measure row is hidden instead.
    pub fn unreported(&self, label: &str) -> Option<MetricLine> {
        match self {
            Self::Window(_, first, then) => {
                Some(MetricLine::Values { label: label.to_string(), values: [Some(*first), *then].into_iter().flatten().map(|m| m.metric_value(0.0)).collect() })
            }
            Self::Measure(..) => None,
        }
    }

    /// The window rows' detail; a measure row has none.
    pub fn breakdown(&self, history: &History) -> Option<Breakdown> {
        match self {
            Self::Window(window, ..) => Some(Breakdown::new(*window, history)),
            Self::Measure(..) => None,
        }
    }

    /// How many days back from today the rows reach, so a source reads exactly those.
    pub fn day_count(rows: &[(String, HistoryRow)]) -> Option<i64> {
        rows.iter().filter_map(|(_, row)| row.window().day_offsets().map(|o| *o.end())).max().map(|last| last + 1)
    }
}

/// What a provider's fetch needs to read its history: the account's ledger, whether the refresh was forced,
/// and the card id its log lines name.
pub struct Run<'a> {
    pub ledger: &'a mut Ledger,
    pub forced: bool,
    pub account: &'a str,
}

/// The lines and breakdowns a provider adds to its snapshot; the breakdowns are keyed by line label.
#[derive(Debug, Default)]
pub struct HistoryOutput {
    pub lines: Vec<MetricLine>,
    pub breakdowns: HashMap<String, Breakdown>,
}

/// Reads what is due and maps it onto `rows` (label, row), in row order. A forced refresh reads today and
/// yesterday again at once. Never fails the card: a source logs its own errors, and what it could not read is
/// left out.
pub async fn history_lines(source: &impl Source, ledger: &mut Ledger, rows: &[(String, HistoryRow)], now: DateTime<Utc>, forced: bool) -> HistoryOutput {
    if forced {
        ledger.forget_last_read();
    }
    let Some(history) = refresh_history(source, ledger, rows, now).await else { return HistoryOutput::default() };
    HistoryOutput::of(&history, rows)
}

impl HistoryOutput {
    /// Puts the lines at `at` (the end when past it) and the breakdowns into the snapshot.
    pub fn splice(self, snapshot: &mut crate::models::ProviderSnapshot, at: usize) {
        let at = at.min(snapshot.lines.len());
        snapshot.lines.splice(at..at, self.lines);
        snapshot.breakdowns.extend(self.breakdowns);
    }

    /// The lines and breakdowns of `rows` over a history already built.
    pub fn of(history: &History, rows: &[(String, HistoryRow)]) -> Self {
        let mut output = Self::default();
        for (label, row) in rows {
            output.lines.extend(row.line(label, history).or_else(|| row.unreported(label)));
            if let Some(breakdown) = row.breakdown(history) {
                output.breakdowns.insert(label.clone(), breakdown);
            }
        }
        output
    }
}
