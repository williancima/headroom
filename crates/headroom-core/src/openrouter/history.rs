//! A management key's `/activity`, which ends at yesterday, so today is never read: `?date=` when one day is
//! due, else the last 30 finished UTC days, in one call either way.

use super::OpenRouterClient;
use crate::history::{refresh_history, Calendar, Day, History, HistoryOutput, HistoryRow, Measure, Read, Run, Source, Totals, Window};
use crate::provider::ProviderError;
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// Today, yesterday, this week and month, and the 30 days before today, in the order the card shows them.
pub(super) fn rows() -> Vec<(String, HistoryRow)> {
    [Window::Today, Window::Yesterday, Window::ThisWeek, Window::ThisMonth, Window::PriorDays(30)]
        .into_iter()
        .map(|window| (window.title(), HistoryRow::Window(window, Measure::Spend, None)))
        .collect()
}

/// The reported spend of the account's keys under the days `/activity` gave.
pub(super) async fn management_history(
    client: &OpenRouterClient,
    api_key: &str,
    spend: HashMap<Window, Totals>,
    now: DateTime<Utc>,
    run: Run<'_>,
) -> HistoryOutput {
    let rows = rows();
    if run.forced {
        run.ledger.forget_last_read();
    }
    let source = ActivitySource { client, api_key, account: run.account };
    let mut history = refresh_history(&source, run.ledger, &rows, now).await.unwrap_or_else(|| History::new(Calendar::IsoUtc, now, BTreeMap::new()));
    history.reported = spend;
    HistoryOutput::of(&history, &rows)
}

struct ActivitySource<'a> {
    client: &'a OpenRouterClient,
    api_key: &'a str,
    account: &'a str,
}

impl ActivitySource<'_> {
    async fn fetch(&self, day: Option<NaiveDate>) -> Result<(u16, String), ProviderError> {
        let path = day.map_or("activity".to_string(), |d| format!("activity?date={d}"));
        let response = self.client.send(&path, self.api_key).await?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(|_| ProviderError::ConnectionFailed)?;
        Ok((status, body))
    }
}

impl Source for ActivitySource<'_> {
    fn clock(&self) -> Calendar {
        Calendar::IsoUtc
    }

    /// Hourly, so history adds one call an hour per account.
    fn reread_interval(&self) -> Option<Duration> {
        Some(Duration::hours(1))
    }

    async fn read(&self, due: &[NaiveDate], as_of: DateTime<Utc>, clock: Calendar) -> Read {
        let today = clock.today(as_of);
        let days: Vec<NaiveDate> = due.iter().copied().filter(|d| *d < today).collect();
        if days.is_empty() {
            return Read::default();
        }
        let (status, body) = match self.fetch((days.len() == 1).then(|| days[0])).await {
            Ok(response) => response,
            Err(error) => {
                eprintln!("headroom: {}: activity request failed: {error}", self.account);
                return Read::default();
            }
        };
        if !(200..300).contains(&status) {
            eprintln!("headroom: {}: activity returned HTTP {status}", self.account);
            return Read::default();
        }
        let Some(read) = activity_days(&body) else {
            eprintln!("headroom: {}: activity response was invalid", self.account);
            return Read::default();
        };
        Read { days: days.into_iter().map(|d| (d, read.get(&d).cloned().unwrap_or_else(quiet_day))).collect(), rate_limited_for: None }
    }
}

/// A UTC day with no activity row.
pub(super) fn quiet_day() -> Day {
    use Measure::*;
    Day { totals: Totals::zero(&[Spend, ByokSpend, Tokens, InputTokens, CacheReadTokens, OutputTokens, ReasoningTokens]), by_model: Some(BTreeMap::new()) }
}

/// `/api/v1/activity`'s rows summed per UTC day and per model; `None` when any row is malformed.
pub(super) fn activity_days(body: &str) -> Option<BTreeMap<NaiveDate, Day>> {
    let Value::Object(mut root) = serde_json::from_str(body).ok()? else { return None };
    let Value::Array(rows) = root.remove("data")? else { return None };
    let mut days: BTreeMap<NaiveDate, Day> = BTreeMap::new();
    for row in &rows {
        let row = row.as_object()?;
        let date = row_date(row.get("date")?.as_str()?)?;
        let model = row.get("model")?.as_str()?;
        let totals = activity_totals(row)?;
        let day = days.remove(&date).unwrap_or_else(quiet_day);
        let mut models = day.by_model.unwrap_or_default();
        let sum = models.get(model).map_or_else(|| totals.clone(), |have| have.adding(&totals));
        models.insert(model.to_string(), sum);
        days.insert(date, Day { totals: day.totals.adding(&totals), by_model: Some(models) });
    }
    Some(days)
}

/// `yyyy-MM-dd`, optionally followed by ` HH:mm:ss`.
fn row_date(text: &str) -> Option<NaiveDate> {
    let (date, time) = match text.len() {
        10 => (text, None),
        19 => (&text[..10], Some(text.get(10..)?)),
        _ => return None,
    };
    let digits = |s: &str, at: &[usize]| s.bytes().enumerate().all(|(i, b)| if at.contains(&i) { b == b'-' || b == b':' || b == b' ' } else { b.is_ascii_digit() });
    if !digits(date, &[4, 7]) {
        return None;
    }
    if let Some(time) = time {
        if !time.starts_with(' ') || !digits(time, &[0, 3, 6]) || NaiveTime::parse_from_str(&time[1..], "%H:%M:%S").is_err() {
            return None;
        }
    }
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}

/// Prompt tokens include cached ones and completion tokens include reasoning, so tokens are the two summed
/// and Input and Output leave out what Cache Read and Reasoning already count.
fn activity_totals(row: &serde_json::Map<String, Value>) -> Option<Totals> {
    use Measure::*;
    let fields = [
        (Spend, "usage"),
        (ByokSpend, "byok_usage_inference"),
        (InputTokens, "prompt_tokens"),
        (CacheReadTokens, "cached_tokens"),
        (OutputTokens, "completion_tokens"),
        (ReasoningTokens, "reasoning_tokens"),
    ];
    let mut totals = Totals::default();
    for (measure, field) in fields {
        match row.get(field) {
            None | Some(Value::Null) => continue,
            value => {
                let number = super::number(value).filter(|n| *n >= 0.0)?;
                totals.values.insert(measure, number);
            }
        }
    }
    totals.get(Spend)?;
    let (prompt, completion) = (totals.get(InputTokens)?, totals.get(OutputTokens)?);
    totals.values.insert(Tokens, prompt + completion);
    totals.values.insert(InputTokens, prompt - totals.get(CacheReadTokens).unwrap_or(0.0));
    totals.values.insert(OutputTokens, completion - totals.get(ReasoningTokens).unwrap_or(0.0));
    Some(totals)
}
