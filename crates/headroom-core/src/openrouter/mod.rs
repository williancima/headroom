//! OpenRouter usage: key, credits and spend reads and mapping.

mod history;
#[cfg(test)]
mod history_tests;

use crate::history::{Calendar, History, Measure, Run, Totals, Window};
use crate::models::{MetricLine, MetricValue, ProgressFormat, ProviderSnapshot};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use reqwest::{header, Response, StatusCode};
use serde_json::{Map, Value};
use std::collections::HashMap;

pub const BASE_URL: &str = "https://openrouter.ai/api/v1";
const KEYS_PAGE_SIZE: usize = 100;
/// Bounds the page loop should OpenRouter ever ignore `offset`.
const MAX_KEYS_PAGES: usize = 50;

pub use crate::provider::ProviderError;

fn key_rejected() -> ProviderError {
    ProviderError::KeyRejected("OpenRouter rejected the API key.".into())
}

/// What `/key` says about the key that calls it.
#[derive(Debug, PartialEq)]
pub struct OpenRouterKey {
    pub management: bool,
    pub plan: Option<String>,
    /// Key Limit, Budget and Free Requests, each when OpenRouter reports one.
    pub lines: Vec<MetricLine>,
    /// What a standard key spent today, this week and this month; empty for a management key, whose own
    /// spend is always zero.
    pub spend: HashMap<Window, Totals>,
}

pub struct OpenRouterClient {
    http: reqwest::Client,
    base_url: String,
}

impl OpenRouterClient {
    pub fn new(base_url: &str) -> Self {
        Self { http: crate::http::client_builder().build().unwrap_or_default(), base_url: base_url.trim_end_matches('/').to_string() }
    }

    /// `/key`, then `/credits`. Unlike upstream, standard keys read `/credits` too: OpenRouter answers
    /// them with the account balance. History follows the balance: a standard key's reported spend, or a
    /// management key's `/keys` sum with `/activity` days. A failed history read never fails the card.
    pub async fn fetch(&self, api_key: &str, now: DateTime<Utc>, run: Run<'_>) -> Result<ProviderSnapshot, ProviderError> {
        let key = self.key(api_key, now).await?;
        let mut lines = self.credits_lines(api_key).await?;
        let at = lines.len();
        lines.extend(key.lines);
        let mut snapshot = ProviderSnapshot { plan: key.plan, lines, refreshed_at: now, reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() };
        let output = if key.management {
            let spend = self.listed_keys_spend(api_key).await?;
            history::management_history(self, api_key, spend, now, run).await
        } else {
            let mut history = History::new(Calendar::IsoUtc, now, Default::default());
            history.reported = key.spend;
            crate::history::HistoryOutput::of(&history, &history::rows())
        };
        output.splice(&mut snapshot, at);
        Ok(snapshot)
    }

    /// Disabled and expired keys count too: what they spent before they stopped is still this period's spend.
    pub async fn listed_keys_spend(&self, api_key: &str) -> Result<HashMap<Window, Totals>, ProviderError> {
        let mut total: HashMap<Window, Totals> = HashMap::new();
        for page in 0..MAX_KEYS_PAGES {
            let response = self.send(&format!("keys?include_disabled=true&include_expired=true&offset={}", page * KEYS_PAGE_SIZE), api_key).await?;
            let keys = parse_keys_page(&require_success(response).await?)?;
            for spend in &keys {
                for (window, totals) in spend {
                    let sum = total.get(window).map_or_else(|| totals.clone(), |have| have.adding(totals));
                    total.insert(*window, sum);
                }
            }
            if keys.len() < KEYS_PAGE_SIZE {
                return Ok(total);
            }
        }
        Err(ProviderError::InvalidResponse)
    }

    pub async fn key(&self, api_key: &str, now: DateTime<Utc>) -> Result<OpenRouterKey, ProviderError> {
        let response = self.send("key", api_key).await?;
        parse_key(&require_success(response).await?, now)
    }

    /// Empty for a management key scoped to one workspace: OpenRouter answers it 403.
    pub async fn credits_lines(&self, api_key: &str) -> Result<Vec<MetricLine>, ProviderError> {
        let response = self.send("credits", api_key).await?;
        if response.status() == StatusCode::FORBIDDEN {
            return Ok(vec![]);
        }
        parse_credits(&require_success(response).await?)
    }

    async fn send(&self, path: &str, api_key: &str) -> Result<Response, ProviderError> {
        self.http
            .get(format!("{}/{path}", self.base_url))
            .bearer_auth(api_key)
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| ProviderError::ConnectionFailed)
    }
}

/// 401, or 403 that is not a rate limit: key rejected. 429, or 403 with `x-ratelimit-remaining: 0`
/// or `retry-after`: rate limited. Other non-2xx: request failed.
async fn require_success(response: Response) -> Result<String, ProviderError> {
    let status = response.status();
    let headers = response.headers();
    let retry_after = headers.get(header::RETRY_AFTER);
    let rate_limited = status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN
            && (headers.get("x-ratelimit-remaining").is_some_and(|v| v == "0") || retry_after.is_some()));
    if rate_limited {
        let retry_after_secs = retry_after.and_then(|v| v.to_str().ok()?.trim().parse().ok());
        return Err(ProviderError::RateLimited { retry_after_secs });
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(key_rejected());
    }
    if !status.is_success() {
        return Err(ProviderError::RequestFailed(status.as_u16()));
    }
    response.text().await.map_err(|_| ProviderError::ConnectionFailed)
}

/// JSON numbers and numeric strings; never booleans or non-finite values.
pub(crate) fn number(value: Option<&Value>) -> Option<f64> {
    let n = match value? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse().ok()?,
        _ => return None,
    };
    n.is_finite().then_some(n)
}

fn data_object(body: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str::<Value>(body).ok()? {
        Value::Object(mut root) => match root.remove("data")? {
            Value::Object(data) => Some(data),
            _ => None,
        },
        _ => None,
    }
}

pub fn parse_key(body: &str, now: DateTime<Utc>) -> Result<OpenRouterKey, ProviderError> {
    let invalid = ProviderError::InvalidResponse;
    let data = data_object(body).ok_or(invalid)?;
    let management = data.get("is_management_key").and_then(Value::as_bool).ok_or(ProviderError::InvalidResponse)?;
    let plan = data.get("is_free_tier").and_then(Value::as_bool).map(|free| if free { "Free" } else { "Pay-as-you-go" });
    let lines = [key_limit_line(&data, now)?, budget_line(&data, now)?, free_requests_line(&data, now)?];
    let spend = if management { HashMap::new() } else { spend(&data)? };
    Ok(OpenRouterKey { management, plan: plan.map(String::from), lines: lines.into_iter().flatten().collect(), spend })
}

/// A key's spend today, this week and this month, with what it spent on BYOK when reported.
fn spend(key: &Map<String, Value>) -> Result<HashMap<Window, Totals>, ProviderError> {
    let mut windows = HashMap::new();
    for (window, period) in [(Window::Today, "daily"), (Window::ThisWeek, "weekly"), (Window::ThisMonth, "monthly")] {
        let spent = number(key.get(&format!("usage_{period}"))).ok_or(ProviderError::InvalidResponse)?;
        let mut totals = Totals::from([(Measure::Spend, spent.max(0.0))]);
        if let Some(byok) = number(key.get(&format!("byok_usage_{period}"))) {
            totals.values.insert(Measure::ByokSpend, byok.max(0.0));
        }
        windows.insert(window, totals);
    }
    Ok(windows)
}

fn parse_keys_page(body: &str) -> Result<Vec<HashMap<Window, Totals>>, ProviderError> {
    let Some(Value::Object(mut root)) = serde_json::from_str(body).ok() else { return Err(ProviderError::InvalidResponse) };
    let Some(Value::Array(keys)) = root.remove("data") else { return Err(ProviderError::InvalidResponse) };
    keys.iter().map(|key| spend(key.as_object().ok_or(ProviderError::InvalidResponse)?)).collect()
}

pub fn parse_credits(body: &str) -> Result<Vec<MetricLine>, ProviderError> {
    let data = data_object(body).ok_or(ProviderError::InvalidResponse)?;
    let (Some(credits), Some(usage)) = (number(data.get("total_credits")), number(data.get("total_usage"))) else {
        return Err(ProviderError::InvalidResponse);
    };
    let balance = (credits.max(0.0) - usage.max(0.0)).max(0.0);
    Ok(vec![MetricLine::Values {
        label: "Balance".into(),
        values: vec![MetricValue { number: balance, currency_code: Some("usd".into()), label: None }],
    }])
}

fn key_limit_line(key: &Map<String, Value>, now: DateTime<Utc>) -> Result<Option<MetricLine>, ProviderError> {
    let Some(limit) = number(key.get("limit")).filter(|l| *l > 0.0) else { return Ok(None) };
    let remaining = number(key.get("limit_remaining")).ok_or(ProviderError::InvalidResponse)?;
    let used = (limit - remaining.max(0.0)).max(0.0);
    Ok(Some(progress("Key Limit", used, limit, ProgressFormat::Dollars, key.get("limit_reset"), now)))
}

/// The budget OpenRouter checks this key's requests against.
fn budget_line(key: &Map<String, Value>, now: DateTime<Utc>) -> Result<Option<MetricLine>, ProviderError> {
    let Some(budget) = key.get("effective_budget").and_then(Value::as_object) else { return Ok(None) };
    let Some(limit) = number(budget.get("limit_usd")).filter(|l| *l > 0.0) else { return Ok(None) };
    let spend = number(budget.get("spend_usd")).ok_or(ProviderError::InvalidResponse)?;
    Ok(Some(progress("Budget", spend.max(0.0), limit, ProgressFormat::Dollars, budget.get("reset_interval"), now)))
}

/// The account's free-model requests for the UTC day.
fn free_requests_line(key: &Map<String, Value>, now: DateTime<Utc>) -> Result<Option<MetricLine>, ProviderError> {
    let Some(free) = key.get("free_model_daily_requests").and_then(Value::as_object) else { return Ok(None) };
    let Some(limit) = number(free.get("limit")).filter(|l| *l > 0.0) else { return Ok(None) };
    let used = number(free.get("used")).ok_or(ProviderError::InvalidResponse)?;
    let format = ProgressFormat::Count("requests".into());
    Ok(Some(progress("Free Requests", used.max(0.0), limit, format, Some(&Value::from("daily")), now)))
}

fn progress(label: &str, used: f64, limit: f64, format: ProgressFormat, period: Option<&Value>, now: DateTime<Utc>) -> MetricLine {
    let window = period.and_then(Value::as_str).and_then(|p| reset_window(p, now));
    MetricLine::Progress {
        label: label.into(),
        used,
        limit,
        format,
        resets_at: window.map(|(_, end)| end),
        period_ms: window.map(|(start, end)| (end - start).num_milliseconds()),
    }
}

/// OpenRouter's days, limits and budgets reset at midnight UTC, and its weeks start on Monday.
fn reset_window(period: &str, now: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let day = now.date_naive();
    let (start, end) = match period {
        "daily" => (day, day + Duration::days(1)),
        "weekly" => {
            let monday = day - Duration::days(day.weekday().num_days_from_monday().into());
            (monday, monday + Duration::days(7))
        }
        "monthly" => {
            let first = NaiveDate::from_ymd_opt(day.year(), day.month(), 1)?;
            let next = if day.month() == 12 {
                NaiveDate::from_ymd_opt(day.year() + 1, 1, 1)?
            } else {
                NaiveDate::from_ymd_opt(day.year(), day.month() + 1, 1)?
            };
            (first, next)
        }
        _ => return None,
    };
    let midnight = |d: NaiveDate| Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap());
    Some((midnight(start), midnight(end)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::Ledger;
    use wiremock::matchers::{header as header_is, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const MANAGEMENT_KEY: &str = r#"{"data":{"label":"sk-or-v1-0e6...1c96","limit":null,"limit_remaining":null,"limit_reset":null,
        "usage":0,"usage_daily":0,"usage_weekly":0,"usage_monthly":0,"is_free_tier":false,"is_management_key":true}}"#;
    const STANDARD_KEY: &str = r#"{"data":{"label":"sk-or-v1-au7...890","limit":100,"limit_remaining":74.5,"limit_reset":"monthly",
        "usage":25.5,"usage_daily":1.5,"usage_weekly":4,"usage_monthly":12,
        "is_free_tier":false,"is_management_key":false,
        "effective_budget":{"limit_usd":50,"spend_usd":32.5,"reset_interval":"weekly"},
        "free_model_daily_requests":{"used":12,"limit":50,"remaining":38}}}"#;
    const CREDITS: &str = r#"{"data":{"total_credits":100,"total_usage":22.25}}"#;

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn thursday() -> DateTime<Utc> {
        at("2026-10-29T11:00:00Z")
    }

    fn key_limit(fields: &str) -> Result<Option<MetricLine>, ProviderError> {
        let text = format!(
            r#"{{"data":{{"is_management_key":false,"usage_daily":0,"usage_weekly":0,"usage_monthly":0,{fields}}}}}"#
        );
        Ok(parse_key(&text, thursday())?.lines.into_iter().find(|l| l.label() == "Key Limit"))
    }

    fn progress_line(label: &str, used: f64, limit: f64, format: ProgressFormat, reset: Option<(&str, i64)>) -> MetricLine {
        MetricLine::Progress {
            label: label.into(),
            used,
            limit,
            format,
            resets_at: reset.map(|(d, _)| at(d)),
            period_ms: reset.map(|(_, ms)| ms),
        }
    }

    #[test]
    fn a_key_response_missing_what_the_card_needs_is_invalid() {
        for text in [
            r#"{"data":{"usage_daily":1.5,"usage_weekly":4,"usage_monthly":12}}"#,
            r#"{"data":{"is_management_key":false,"usage_daily":1.5,"usage_weekly":4}}"#,
            r#"{"data":{"is_management_key":false,"usage_daily":1.5,"usage_weekly":4,"usage_monthly":12,"limit":10,"limit_remaining":null}}"#,
            r#"{"error":{"code":500,"message":"Internal Server Error"}}"#,
            "not json",
        ] {
            assert_eq!(parse_key(text, thursday()), Err(ProviderError::InvalidResponse), "{text}");
        }
    }

    #[test]
    fn a_key_limit_without_a_reset_has_no_reset_and_no_limit_has_no_row() {
        let line = key_limit(r#""limit":10,"limit_remaining":7.5,"limit_reset":null"#).unwrap();
        assert_eq!(line, Some(progress_line("Key Limit", 2.5, 10.0, ProgressFormat::Dollars, None)));
        assert_eq!(key_limit(r#""limit":null,"limit_remaining":null,"limit_reset":null"#), Ok(None));
    }

    #[test]
    fn reset_windows_are_utc_with_monday_weeks() {
        const DAY: i64 = 86_400_000;
        let line = |reset| key_limit(&format!(r#""limit":10,"limit_remaining":10,"limit_reset":"{reset}""#)).unwrap().unwrap();
        assert_eq!(line("daily"), progress_line("Key Limit", 0.0, 10.0, ProgressFormat::Dollars, Some(("2026-10-30T00:00:00Z", DAY))));
        assert_eq!(line("weekly"), progress_line("Key Limit", 0.0, 10.0, ProgressFormat::Dollars, Some(("2026-11-02T00:00:00Z", 7 * DAY))));
        assert_eq!(line("monthly"), progress_line("Key Limit", 0.0, 10.0, ProgressFormat::Dollars, Some(("2026-11-01T00:00:00Z", 31 * DAY))));
        assert_eq!(reset_window("monthly", at("2026-12-15T00:00:00Z")).unwrap().1, at("2027-01-01T00:00:00Z"));
        assert_eq!(line("hourly"), progress_line("Key Limit", 0.0, 10.0, ProgressFormat::Dollars, None));
    }

    #[test]
    fn a_standard_key_maps_plan_limit_budget_and_free_requests() {
        let key = parse_key(STANDARD_KEY, thursday()).unwrap();
        assert!(!key.management);
        assert_eq!(key.plan.as_deref(), Some("Pay-as-you-go"));
        let labels: Vec<_> = key.lines.iter().map(MetricLine::label).collect();
        assert_eq!(labels, ["Key Limit", "Budget", "Free Requests"]);
        assert_eq!(key.lines[1], progress_line("Budget", 32.5, 50.0, ProgressFormat::Dollars, Some(("2026-11-02T00:00:00Z", 7 * 86_400_000))));
        assert_eq!(
            key.lines[2],
            progress_line("Free Requests", 12.0, 50.0, ProgressFormat::Count("requests".into()), Some(("2026-10-30T00:00:00Z", 86_400_000)))
        );
    }

    #[test]
    fn credits_show_only_the_balance() {
        let balance = |n| {
            vec![MetricLine::Values { label: "Balance".into(), values: vec![MetricValue { number: n, currency_code: Some("usd".into()), label: None }] }]
        };
        assert_eq!(parse_credits(r#"{"data":{"total_credits":10,"total_usage":12.5}}"#), Ok(balance(0.0)));
        assert_eq!(parse_credits(CREDITS), Ok(balance(77.75)));
        assert_eq!(parse_credits(r#"{"data":{"total_credits":100}}"#), Err(ProviderError::InvalidResponse));
    }

    async fn server_with(routes: Vec<(&str, ResponseTemplate)>) -> MockServer {
        let server = MockServer::start().await;
        for (route, response) in routes {
            Mock::given(method("GET")).and(path(format!("/api/v1/{route}"))).respond_with(response).mount(&server).await;
        }
        server
    }

    fn run(ledger: &mut crate::history::Ledger) -> Run<'_> {
        Run { ledger, forced: false, account: "openrouter@test" }
    }

    fn client(server: &MockServer) -> OpenRouterClient {
        OpenRouterClient::new(&format!("{}/api/v1", server.uri()))
    }

    fn ok(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_string(body)
    }

    #[tokio::test]
    async fn a_standard_key_also_shows_the_balance_and_sends_the_bearer_header() {
        let server = MockServer::start().await;
        for (route, body) in [("key", STANDARD_KEY), ("credits", CREDITS)] {
            Mock::given(method("GET"))
                .and(path(format!("/api/v1/{route}")))
                .and(header_is("authorization", "Bearer sk-test"))
                .and(header_is("accept", "application/json"))
                .respond_with(ok(body))
                .expect(1)
                .mount(&server)
                .await;
        }
        let snapshot = client(&server).fetch("sk-test", thursday(), run(&mut Ledger::default())).await.unwrap();
        assert_eq!(snapshot.plan.as_deref(), Some("Pay-as-you-go"));
        assert!(snapshot.line("Balance").is_some());
        assert!(snapshot.line("Key Limit").is_some());
    }

    #[tokio::test]
    async fn a_management_key_also_shows_the_balance_first() {
        let server = server_with(vec![("key", ok(MANAGEMENT_KEY)), ("credits", ok(CREDITS)), ("keys", ok(r#"{"data":[]}"#)), ("activity", ok(r#"{"data":[]}"#))]).await;
        let snapshot = client(&server).fetch("k", thursday(), run(&mut Ledger::default())).await.unwrap();
        assert_eq!(snapshot.lines[0].label(), "Balance");
        assert_eq!(snapshot.lines[1].label(), "Today", "the history rows follow the balance");
    }

    #[tokio::test]
    async fn a_credits_403_leaves_out_the_balance_line() {
        let forbidden = ResponseTemplate::new(403).set_body_string(r#"{"error":{"code":403,"message":"Forbidden"}}"#);
        let server = server_with(vec![("key", ok(MANAGEMENT_KEY)), ("credits", forbidden), ("keys", ok(r#"{"data":[]}"#)), ("activity", ok(r#"{"data":[]}"#))]).await;
        let snapshot = client(&server).fetch("k", thursday(), run(&mut Ledger::default())).await.unwrap();
        assert!(snapshot.line("Balance").is_none());
        assert_eq!(snapshot.plan.as_deref(), Some("Pay-as-you-go"));
    }

    #[tokio::test]
    async fn auth_rate_limit_and_failure_statuses_map_to_errors() {
        let cases = [
            (ResponseTemplate::new(401), key_rejected()),
            (ResponseTemplate::new(403), key_rejected()),
            (ResponseTemplate::new(429).insert_header("retry-after", "30"), ProviderError::RateLimited { retry_after_secs: Some(30) }),
            (ResponseTemplate::new(403).insert_header("x-ratelimit-remaining", "0"), ProviderError::RateLimited { retry_after_secs: None }),
            (ResponseTemplate::new(403).insert_header("retry-after", "5"), ProviderError::RateLimited { retry_after_secs: Some(5) }),
            (ResponseTemplate::new(500), ProviderError::RequestFailed(500)),
            (ok("{}"), ProviderError::InvalidResponse),
        ];
        for (response, expected) in cases {
            let server = server_with(vec![("key", response)]).await;
            assert_eq!(client(&server).fetch("k", thursday(), run(&mut Ledger::default())).await.unwrap_err(), expected);
        }
    }

    #[tokio::test]
    async fn a_credits_failure_other_than_403_fails_the_refresh() {
        let server = server_with(vec![("key", ok(MANAGEMENT_KEY)), ("credits", ResponseTemplate::new(500))]).await;
        assert_eq!(client(&server).fetch("k", thursday(), run(&mut Ledger::default())).await.unwrap_err(), ProviderError::RequestFailed(500));
    }

    #[tokio::test]
    async fn an_unreachable_server_is_a_connection_failure() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let client = OpenRouterClient::new(&format!("http://127.0.0.1:{port}/api/v1"));
        assert_eq!(client.fetch("k", thursday(), run(&mut Ledger::default())).await.unwrap_err(), ProviderError::ConnectionFailed);
    }
}
