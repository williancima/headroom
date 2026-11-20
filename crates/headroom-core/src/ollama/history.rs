//! Ollama's UTC days in one call: the last 7 days when every due day is among them, else 30.

use super::OllamaClient;
use crate::history::{refresh_history, Calendar, Day, HistoryOutput, HistoryRow, Measure, Read, Run, Source, Totals, Window};
use crate::openrouter::number;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Today, yesterday and the last 30 days with their cost and tokens, then the 30 days' requests.
pub(super) fn rows() -> Vec<(String, HistoryRow)> {
    let window = |window: Window| (window.title(), HistoryRow::Window(window, Measure::UsageValue, Some(Measure::Tokens)));
    vec![
        window(Window::Today),
        window(Window::Yesterday),
        window(Window::LastDays(30)),
        ("Requests".into(), HistoryRow::Measure("Requests", Measure::Requests, Window::LastDays(30))),
    ]
}

pub(super) async fn history(client: &OllamaClient, api_key: &str, now: DateTime<Utc>, run: Run<'_>) -> HistoryOutput {
    let rows = rows();
    if run.forced {
        run.ledger.forget_last_read();
    }
    let source = UsageSource { client, api_key, account: run.account };
    let Some(history) = refresh_history(&source, run.ledger, &rows, now).await else { return HistoryOutput::default() };
    HistoryOutput::of(&history, &rows)
}

pub(super) struct UsageSource<'a> {
    pub client: &'a OllamaClient,
    pub api_key: &'a str,
    pub account: &'a str,
}

impl Source for UsageSource<'_> {
    fn clock(&self) -> Calendar {
        Calendar::Utc
    }

    /// Hourly, so history adds one call an hour per account.
    fn reread_interval(&self) -> Option<Duration> {
        Some(Duration::hours(1))
    }

    async fn read(&self, days: &[NaiveDate], as_of: DateTime<Utc>, clock: Calendar) -> Read {
        let week_ago = clock.today(as_of) - Duration::days(7);
        let range = if days.iter().all(|d| *d >= week_ago) { "7d" } else { "30d" };
        let response = match self.client.usage(self.api_key, range).await {
            Ok(response) => response,
            Err(_) => {
                eprintln!("headroom: {}: usage history: request failed", self.account);
                return Read::default();
            }
        };
        if !response.is_success() {
            eprintln!("headroom: {}: usage history: HTTP {}", self.account, response.status);
            return Read::default();
        }
        let Some(read) = parse_days(&response.body) else {
            eprintln!("headroom: {}: usage history: invalid response", self.account);
            return Read::default();
        };
        // The request covered these dates even when Ollama supplied no figures for one of them.
        Read { days: days.iter().map(|d| (*d, read.get(d).cloned().unwrap_or_default())).collect(), rate_limited_for: None }
    }
}

const MEASURES: [(&str, Measure); 5] = [
    ("request_count", Measure::Requests),
    ("usage_usd", Measure::UsageValue),
    ("input_tokens", Measure::InputTokens),
    ("cached_input_tokens", Measure::CacheReadTokens),
    ("output_tokens", Measure::OutputTokens),
];

/// Every bucket keyed by its UTC day, or `None` when any bucket is not one, so a read is never partly used.
/// A day is split by model only when no usage falls in Ollama's `remainder` beyond the named models.
pub(super) fn parse_days(body: &[u8]) -> Option<BTreeMap<NaiveDate, Day>> {
    let Value::Object(root) = serde_json::from_slice(body).ok()? else { return None };
    if root.get("granularity")?.as_str()? != "day" {
        return None;
    }
    let mut days = BTreeMap::new();
    for bucket in root.get("buckets")?.as_array()? {
        let bucket = bucket.as_object()?;
        let instant = |key: &str| DateTime::parse_from_rfc3339(bucket.get(key)?.as_str()?).ok().map(|t| t.with_timezone(&Utc));
        let (from, until) = (instant("from")?, instant("until")?);
        let day = from.date_naive();
        let end = from + Duration::days(1);
        if Calendar::Utc.day_start(day)? != from || days.contains_key(&day) || from >= until || until > end {
            return None;
        }
        let totals = totals(bucket)?;
        let mut by_model = None;
        if let Some(groups) = bucket.get("groups") {
            let mut models = BTreeMap::new();
            for group in groups.as_array()? {
                let group = group.as_object()?;
                let model = group.get("model")?.as_str().filter(|m| !m.is_empty())?;
                if models.contains_key(model) {
                    return None;
                }
                models.insert(model.to_string(), self::totals(group)?);
            }
            by_model = (!bucket.contains_key("remainder")).then_some(models);
        }
        days.insert(day, Day { totals, by_model });
    }
    Some(days)
}

/// Cached input is part of input, so tokens are input plus output. `None` without a request count, or when a
/// figure is not a number of zero or more.
fn totals(object: &Map<String, Value>) -> Option<Totals> {
    let mut totals = Totals::default();
    for (field, measure) in MEASURES {
        match object.get(field) {
            None | Some(Value::Null) => continue,
            value => {
                totals.values.insert(measure, number(value).filter(|n| *n >= 0.0)?);
            }
        }
    }
    totals.get(Measure::Requests)?;
    if let (Some(input), Some(output)) = (totals.get(Measure::InputTokens), totals.get(Measure::OutputTokens)) {
        totals.values.insert(Measure::Tokens, input + output);
    }
    Some(totals)
}
