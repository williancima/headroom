//! ChatGPT's analytics reports on UTC days. Token counts
//! and supplemental workspace credits have separate ledgers, so a refused or rate-limited credits report
//! cannot hide the primary counts.

use super::CodexClient;
use crate::history::{refresh_history, retry_after, Calendar, Day, History, HistoryOutput, HistoryRow, Ledger, Measure, Read, Run, Source, Totals, Window};
use crate::openrouter::number;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Report {
    Counts,
    TeamCredits,
    EnterpriseCredits,
}

impl Report {
    /// The credits report a workspace plan reports its charged credits in.
    pub(super) fn workspace_credits(plan_type: Option<&str>) -> Option<Self> {
        match plan_type?.to_lowercase().as_str() {
            "team" | "self_serve_business_prolite" | "self_serve_business_usage_based" | "sci" | "sci_plus" => Some(Self::TeamCredits),
            "business" | "ent26" | "enterprise_cbp_automation" | "enterprise_cbp_usage_based" | "enterprise" | "edu" | "edu_plus" | "edu_pro" => {
                Some(Self::EnterpriseCredits)
            }
            _ => None,
        }
    }
}

pub(super) fn rows() -> Vec<(String, HistoryRow)> {
    [Window::Today, Window::Yesterday, Window::LastDays(30)].into_iter().map(|w| (w.title(), HistoryRow::Window(w, Measure::Tokens, None))).collect()
}

/// Reads the counts, and the credits of a workspace plan beside them, and maps them onto the card's rows.
pub(super) async fn history(
    client: &CodexClient,
    token: &str,
    account_id: Option<&str>,
    plan_type: Option<&str>,
    now: DateTime<Utc>,
    counts: Run<'_>,
    credits: &mut Ledger,
) -> HistoryOutput {
    let rows = rows();
    let source = |report| ReportSource { client, token, account_id, account: counts.account, report };
    if counts.forced {
        counts.ledger.forget_last_read();
        credits.forget_last_read();
    }
    let counted = source(Report::Counts);
    let history = match Report::workspace_credits(plan_type) {
        Some(report) => {
            let credited = source(report);
            let (counted, credits) = tokio::join!(refresh_history(&counted, counts.ledger, &rows, now), refresh_history(&credited, credits, &rows, now));
            counted.map(|counted| combining(counted, credits))
        }
        None => refresh_history(&counted, counts.ledger, &rows, now).await,
    };
    history.map(|h| HistoryOutput::of(&h, &rows)).unwrap_or_default()
}

struct ReportSource<'a> {
    client: &'a CodexClient,
    token: &'a str,
    account_id: Option<&'a str>,
    account: &'a str,
    report: Report,
}

/// A day's UTC `yyyy-MM-dd`, the date format of the reports and their range parameters.
fn date(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

impl Source for ReportSource<'_> {
    fn clock(&self) -> Calendar {
        Calendar::Utc
    }

    fn reread_interval(&self) -> Option<Duration> {
        Some(Duration::hours(1))
    }

    async fn read(&self, days: &[NaiveDate], as_of: DateTime<Utc>, _clock: Calendar) -> Read {
        let (Some(first), Some(last)) = (days.iter().min(), days.iter().max()) else { return Read::default() };
        let dates = format!("start_date={}&end_date={}", date(*first), date(*last));
        type Parse = fn(&[u8]) -> Option<Reported>;
        let (path, query, parse): (&str, String, Parse) = match self.report {
            Report::Counts => ("analytics/daily-workspace-usage-counts", format!("{dates}&group_by=day&workspace_user=true"), counts),
            Report::TeamCredits => ("usage/daily-workspace-user-token-usage-breakdown", format!("{dates}&group_by=day"), workspace_credits),
            Report::EnterpriseCredits => ("usage/daily-workspace-user-credit-usage", format!("{dates}&breakdown=model"), enterprise_credits),
        };
        let response = match self.client.report(path, &query, self.token, self.account_id).await {
            Ok(response) => response,
            Err(_) => {
                eprintln!("headroom: {}: usage history: {path} request failed", self.account);
                return Read::default();
            }
        };
        if response.status == 429 {
            let wait = retry_after(response.retry_after.as_deref(), as_of).unwrap_or_else(|| Duration::hours(1));
            eprintln!("headroom: {}: usage history: {path} rate limited; next read in {}s", self.account, wait.num_seconds());
            return Read { days: BTreeMap::new(), rate_limited_for: Some(wait) };
        }
        if !(200..300).contains(&response.status) {
            eprintln!("headroom: {}: usage history: {path} HTTP {}", self.account, response.status);
            return Read::default();
        }
        let Some(parsed) = parse(&response.body) else {
            eprintln!("headroom: {}: usage history: {path} invalid response", self.account);
            return Read::default();
        };
        Read { days: days.iter().map(|d| (*d, parsed.days.get(&date(*d)).cloned().unwrap_or_else(|| parsed.quiet_day.clone()))).collect(), rate_limited_for: None }
    }
}

/// A report's days keyed by their UTC `yyyy-MM-dd` date. A report with any malformed day is rejected whole,
/// so a day is never partly read.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Reported {
    pub days: BTreeMap<String, Day>,
    /// What a date the report leaves out stands for.
    pub quiet_day: Day,
}

/// A credits report without charged credits, which reports none for any day.
fn no_credits() -> Reported {
    Reported { days: BTreeMap::new(), quiet_day: Day::default() }
}

const COUNTED_TOKENS: [(&str, Measure); 4] = [
    ("text_total_tokens", Measure::Tokens),
    ("uncached_text_input_tokens", Measure::InputTokens),
    ("cached_text_input_tokens", Measure::CacheReadTokens),
    ("text_output_tokens", Measure::OutputTokens),
];

fn count(value: Option<&Value>) -> Option<f64> {
    number(value).filter(|n| *n >= 0.0)
}

fn parse_records(body: &[u8]) -> Option<Vec<Map<String, Value>>> {
    let Value::Object(mut root) = serde_json::from_slice(body).ok()? else { return None };
    let Value::Array(items) = root.remove("data")? else { return None };
    items.into_iter().map(|item| if let Value::Object(object) = item { Some(object) } else { None }).collect()
}

fn record_date(record: &Map<String, Value>) -> Option<String> {
    let date = record.get("date")?.as_str()?;
    (date.chars().count() >= 10).then(|| date.chars().take(10).collect())
}

/// `analytics/daily-workspace-usage-counts`: each day's turns, its text tokens by kind when reported, and each
/// model's turns. A date it leaves out is not reported yet, so it has none of them.
pub(super) fn counts(body: &[u8]) -> Option<Reported> {
    let mut days = BTreeMap::new();
    for record in parse_records(body)? {
        let date = record_date(&record)?;
        let totals = record.get("totals")?.as_object()?;
        let mut values = BTreeMap::from([(Measure::Turns, count(totals.get("turns"))?)]);
        for (key, measure) in COUNTED_TOKENS {
            match totals.get(key) {
                None | Some(Value::Null) => {}
                tokens => {
                    values.insert(measure, count(tokens)?);
                }
            }
        }
        add(Day { totals: Totals { values }, by_model: counted_models(&record) }, date, &mut days);
    }
    Some(Reported { days, quiet_day: Day::default() })
}

/// `None` when the record names no models.
fn counted_models(record: &Map<String, Value>) -> Option<BTreeMap<String, Totals>> {
    let models: Vec<&Map<String, Value>> = record.get("models")?.as_array()?.iter().map(Value::as_object).collect::<Option<_>>()?;
    if models.is_empty() {
        return None;
    }
    let mut totals: BTreeMap<String, Totals> = BTreeMap::new();
    for model in models {
        let name = model.get("model").and_then(Value::as_str).unwrap_or("Unknown");
        for (key, measure) in [("turns", Measure::Turns), ("text_total_tokens", Measure::Tokens)] {
            if let Some(n) = count(model.get(key)) {
                *totals.entry(name.to_string()).or_default().values.entry(measure).or_insert(0.0) += n;
            }
        }
    }
    Some(totals)
}

/// `usage/daily-workspace-user-token-usage-breakdown`: each day's charged credits
/// (`premium_usage_values.credit_usage_credits`) and each model's (`on_demand_credits`).
pub(super) fn workspace_credits(body: &[u8]) -> Option<Reported> {
    let mut days = BTreeMap::new();
    for record in parse_records(body)? {
        let date = record_date(&record)?;
        let premium = record.get("premium_usage_values").and_then(|v| v.get("credit_usage_credits")).and_then(Value::as_object);
        let Some(premium) = premium else { return Some(no_credits()) };
        let charged = numbers(premium)?;
        let mut models: BTreeMap<String, f64> = BTreeMap::new();
        let listed: Vec<&Map<String, Value>> =
            record.get("models").and_then(Value::as_array).and_then(|a| a.iter().map(Value::as_object).collect()).unwrap_or_default();
        for model in listed {
            let Some(credits) = number(model.get("on_demand_credits")) else { continue };
            *models.entry(model.get("model").and_then(Value::as_str).unwrap_or("Unknown").to_string()).or_insert(0.0) += credits;
        }
        add(credit_day(charged.values().sum(), &models), date, &mut days);
    }
    Some(Reported { days, quiet_day: credit_day(0.0, &BTreeMap::new()) })
}

/// `usage/daily-workspace-user-credit-usage?breakdown=model`: each day's charged credits by model.
pub(super) fn enterprise_credits(body: &[u8]) -> Option<Reported> {
    let mut days = BTreeMap::new();
    for record in parse_records(body)? {
        let date = record_date(&record)?;
        let Some(values) = record.get("values").and_then(Value::as_object) else { return Some(no_credits()) };
        let models = numbers(values)?;
        add(credit_day(models.values().sum(), &models), date, &mut days);
    }
    Some(Reported { days, quiet_day: credit_day(0.0, &BTreeMap::new()) })
}

/// Token counts determine which days can be shown. When the credits report is available it also supplies the
/// charged credits by model, alongside the turns from the counts report.
pub(super) fn combining(mut counts: History, credits: Option<History>) -> History {
    let Some(credits) = credits else { return counts };
    for (date, charged) in credits.days {
        let Some(day) = counts.days.get_mut(&date) else { continue };
        day.totals = day.totals.adding(&charged.totals);
        if let Some(models) = charged.by_model {
            let merged = day.by_model.get_or_insert_with(BTreeMap::new);
            for (name, totals) in models {
                let sum = merged.get(&name).map_or_else(|| totals.clone(), |have| have.adding(&totals));
                merged.insert(name, sum);
            }
        }
    }
    counts
}

fn credit_day(credits: f64, models: &BTreeMap<String, f64>) -> Day {
    Day {
        totals: Totals::from([(Measure::Credits, credits)]),
        by_model: Some(models.iter().map(|(name, n)| (name.clone(), Totals::from([(Measure::Credits, *n)]))).collect()),
    }
}

/// Several records for one date add up, as the Codex client sums them.
fn add(day: Day, date: String, days: &mut BTreeMap<String, Day>) {
    let Some(earlier) = days.remove(&date) else {
        days.insert(date, day);
        return;
    };
    let nothing = earlier.by_model.is_none() && day.by_model.is_none();
    let mut by_model = earlier.by_model.unwrap_or_default();
    for (name, totals) in day.by_model.into_iter().flatten() {
        let sum = by_model.get(&name).map_or_else(|| totals.clone(), |have| have.adding(&totals));
        by_model.insert(name, sum);
    }
    days.insert(date, Day { totals: earlier.totals.adding(&day.totals), by_model: (!nothing).then_some(by_model) });
}

/// Credit amounts, which can be negative adjustments; `None` when any is not a number.
fn numbers(amounts: &Map<String, Value>) -> Option<BTreeMap<String, f64>> {
    amounts.iter().map(|(name, value)| number(Some(value)).map(|n| (name.clone(), n))).collect()
}
