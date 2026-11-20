//! Cursor's usage events, a local day at a time: at most 4 days at once, for at most 20 seconds.

use super::{session, CursorClient};
use crate::history::{refresh_history, retry_after, Calendar, Day, HistoryOutput, HistoryRow, Measure, Read, Run, Source, Totals, Window};
use crate::openrouter::number;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use reqwest::header;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration as Wait;
use tokio::task::JoinSet;

/// On-demand usage, the only kind Cursor bills. Included, errored, and free events carry a `chargedCents` too,
/// but for them it is the value taken from the plan, not a charge.
const CHARGEABLE_KIND: &str = "USAGE_EVENT_KIND_USAGE_BASED";
const MEASURES: [Measure; 7] = [
    Measure::Spend,
    Measure::UsageEvents,
    Measure::Tokens,
    Measure::InputTokens,
    Measure::CacheReadTokens,
    Measure::CacheWriteTokens,
    Measure::OutputTokens,
];
const TOKEN_KINDS: [(&str, Measure); 4] = [
    ("inputTokens", Measure::InputTokens),
    ("cacheReadTokens", Measure::CacheReadTokens),
    ("cacheWriteTokens", Measure::CacheWriteTokens),
    ("outputTokens", Measure::OutputTokens),
];

pub(super) fn rows() -> Vec<(String, HistoryRow)> {
    [Window::Today, Window::Yesterday, Window::LastDays(30)]
        .into_iter()
        .map(|w| (w.title(), HistoryRow::Window(w, Measure::Spend, Some(Measure::Tokens))))
        .collect()
}

pub(super) async fn history(client: &CursorClient, token: &str, now: DateTime<Utc>, run: Run<'_>) -> HistoryOutput {
    let rows = rows();
    if run.forced {
        run.ledger.forget_last_read();
    }
    let source = EventsSource::new(client, token, run.account);
    let history = refresh_history(&source, run.ledger, &rows, now).await;
    history.map(|h| HistoryOutput::of(&h, &rows)).unwrap_or_default()
}

#[derive(Clone)]
pub(super) struct EventsSource {
    http: reqwest::Client,
    web_base: String,
    token: String,
    account: String,
    pub clock: Calendar,
    pub budget: Wait,
    pub max_concurrent_days: usize,
    pub page_size: usize,
}

enum DayResult {
    Read(NaiveDate, Day),
    Failed(String),
    RateLimited(Option<Duration>),
}

impl EventsSource {
    pub(super) fn new(client: &CursorClient, token: &str, account: &str) -> Self {
        Self {
            http: client.http.clone(),
            web_base: client.config.web_base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            account: account.to_string(),
            clock: Calendar::Local,
            budget: Wait::from_secs(20),
            max_concurrent_days: 4,
            page_size: 1000,
        }
    }

    async fn read_day(self: std::sync::Arc<Self>, day: NaiveDate, as_of: DateTime<Utc>, clock: Calendar) -> DayResult {
        let (Some(start), Some(next)) = (clock.day_start(day), day.succ_opt().and_then(|d| clock.day_start(d))) else {
            return DayResult::Failed("invalid day".into());
        };
        let end_ms = next.min(as_of).timestamp_millis() - 1;
        let mut events: Vec<Event> = vec![];
        let mut expected_total: Option<usize> = None;
        for page in 1.. {
            let Some((_, cookie)) = session(&self.token) else {
                return DayResult::Failed("request could not be prepared from the current session".into());
            };
            let body = json!({ "startDate": start.timestamp_millis().to_string(), "endDate": end_ms.to_string(), "page": page, "pageSize": self.page_size });
            let request = self
                .http
                .post(format!("{}/api/dashboard/get-filtered-usage-events", self.web_base))
                .header(header::COOKIE, format!("WorkosCursorSessionToken={cookie}"))
                .header(header::ORIGIN, "https://cursor.com")
                .timeout(Wait::from_secs(15))
                .json(&body);
            let Ok(response) = request.send().await else { return DayResult::Failed("request failed".into()) };
            let status = response.status().as_u16();
            let retry = response.headers().get(header::RETRY_AFTER).and_then(|v| v.to_str().ok()).map(String::from);
            if status == 429 {
                return DayResult::RateLimited(retry_after(retry.as_deref(), as_of));
            }
            let Ok(bytes) = response.bytes().await else { return DayResult::Failed("request failed".into()) };
            if !(200..300).contains(&status) {
                return DayResult::Failed(format!("HTTP {status}"));
            }
            let Some((total, page_events)) = parse_page(&bytes).filter(|(_, events)| events.iter().all(|e| clock.today(e.date) == day)) else {
                return DayResult::Failed("invalid response".into());
            };
            if expected_total.is_some_and(|expected| expected != total) {
                return DayResult::Failed("incomplete pages".into());
            }
            expected_total = Some(total);
            let none_new = page_events.is_empty();
            events.extend(page_events);
            if events.len() == total {
                return DayResult::Read(day, sum_day(&events));
            }
            if events.len() > total || none_new {
                return DayResult::Failed("incomplete pages".into());
            }
        }
        DayResult::Failed("incomplete pages".into())
    }
}

impl Source for EventsSource {
    fn clock(&self) -> Calendar {
        self.clock
    }

    async fn read(&self, days: &[NaiveDate], as_of: DateTime<Utc>, clock: Calendar) -> Read {
        let shared = std::sync::Arc::new(self.clone());
        let mut due: VecDeque<NaiveDate> = days.iter().copied().collect();
        let (mut read, mut failures, mut deadline_hit, mut rate_limited_for) = (BTreeMap::new(), vec![], false, None);
        let mut set = JoinSet::new();
        let mut reading = 0;
        let mut next = |set: &mut JoinSet<DayResult>, reading: &mut usize| {
            if let Some(day) = due.pop_front() {
                *reading += 1;
                set.spawn(shared.clone().read_day(day, as_of, clock));
            }
        };
        for _ in 0..self.max_concurrent_days {
            next(&mut set, &mut reading);
        }
        let deadline = tokio::time::sleep(self.budget);
        tokio::pin!(deadline);
        while reading > 0 {
            tokio::select! {
                _ = &mut deadline => {
                    deadline_hit = true;
                    break;
                }
                joined = set.join_next() => match joined {
                    Some(Ok(DayResult::Read(day, value))) => {
                        read.insert(day, value);
                    }
                    Some(Ok(DayResult::Failed(reason))) => failures.push(reason),
                    Some(Ok(DayResult::RateLimited(wait))) => {
                        rate_limited_for = Some(wait.unwrap_or_else(|| Duration::hours(1)));
                        break;
                    }
                    Some(Err(_)) => failures.push("read task failed".into()),
                    None => break,
                },
            }
            reading -= 1;
            next(&mut set, &mut reading);
        }
        drop(set);
        if let Some(first) = failures.first() {
            eprintln!("headroom: {}: usage history: {} of {} days failed ({first})", self.account, failures.len(), days.len());
        }
        if deadline_hit {
            eprintln!("headroom: {}: usage history: ran out of time with {} of {} days missing", self.account, days.len() - read.len(), days.len());
        }
        if let Some(wait) = rate_limited_for {
            eprintln!("headroom: {}: usage history: rate limited; next read in {}s", self.account, wait.num_seconds());
        }
        Read { days: read, rate_limited_for }
    }
}

pub(super) struct Event {
    pub date: DateTime<Utc>,
    pub model: Option<String>,
    pub totals: Totals,
}

/// `None` when the page or any event on it is not what Cursor sends, so a day is never summed from part of its
/// events. Cursor leaves out an empty event list, a zero `chargedCents`, and a zero token kind. An event
/// without `tokenUsage` counts zero tokens.
pub(super) fn parse_page(body: &[u8]) -> Option<(usize, Vec<Event>)> {
    let Value::Object(object) = serde_json::from_slice(body).ok()? else { return None };
    let total = number(object.get("totalUsageEventsCount"))?;
    if total < 0.0 || total.round() != total || total >= 1e15 {
        return None;
    }
    let raw_events = match object.get("usageEventsDisplay") {
        None => &Vec::new(),
        Some(Value::Array(events)) => events,
        Some(_) => return None,
    };
    let mut events = vec![];
    for raw in raw_events {
        let event = raw.as_object()?;
        let milliseconds = number(event.get("timestamp")).filter(|ms| *ms >= 0.0)?;
        let mut totals = Totals::from([(Measure::Spend, 0.0), (Measure::UsageEvents, 1.0)]);
        if event.get("kind").and_then(Value::as_str) == Some(CHARGEABLE_KIND) && event.get("isChargeable").and_then(Value::as_bool) != Some(false) {
            if let Some(raw_cents) = event.get("chargedCents") {
                let cents = number(Some(raw_cents)).filter(|c| *c >= 0.0)?;
                totals.values.insert(Measure::Spend, cents / 100.0);
            }
        }
        let usage = match event.get("tokenUsage") {
            None => &serde_json::Map::new(),
            Some(Value::Object(usage)) => usage,
            Some(_) => return None,
        };
        let mut tokens = 0.0;
        for (field, measure) in TOKEN_KINDS {
            let count = match usage.get(field) {
                None => 0.0,
                raw => number(raw).filter(|n| *n >= 0.0)?,
            };
            totals.values.insert(measure, count);
            tokens += count;
        }
        totals.values.insert(Measure::Tokens, tokens);
        events.push(Event {
            date: DateTime::from_timestamp_millis(milliseconds as i64)?,
            model: event.get("model").and_then(Value::as_str).map(String::from),
            totals,
        });
    }
    Some((total as usize, events))
}

/// One day's events summed: a day with no events reports zeros.
pub(super) fn sum_day(events: &[Event]) -> Day {
    let sum = |events: &[&Event]| events.iter().fold(Totals::zero(&MEASURES), |sum, e| sum.adding(&e.totals));
    let mut named: BTreeMap<String, Vec<&Event>> = BTreeMap::new();
    for event in events {
        if let Some(model) = &event.model {
            named.entry(model.clone()).or_default().push(event);
        }
    }
    Day { totals: sum(&events.iter().collect::<Vec<_>>()), by_model: Some(named.into_iter().map(|(model, list)| (model, sum(&list))).collect()) }
}
