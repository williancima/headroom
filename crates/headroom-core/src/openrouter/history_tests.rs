//! A Management key's card with `/api/v1/activity` shaped as OpenRouter's API reference documents it.
//! Yesterday is 28 October 2026 UTC.

use super::*;
use crate::history::{Ledger, Measure, ModelFigure};
use crate::priority::values_text;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const MANAGEMENT_KEY: &str = r#"{"data":{"label":"sk-or-v1-0e6...1c96","limit":null,"limit_remaining":null,"limit_reset":null,
    "usage":0,"usage_daily":0,"usage_weekly":0,"usage_monthly":0,"byok_usage":0,"byok_usage_daily":0,"byok_usage_weekly":0,
    "byok_usage_monthly":0,"is_free_tier":false,"is_management_key":true,"is_provisioning_key":true}}"#;
const CREDITS: &str = r#"{"data":{"total_credits":100,"total_usage":22.25}}"#;
const QUIET_DAY: &str = "2026-10-20";

fn at(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn now() -> DateTime<Utc> {
    at("2026-10-29T11:00:00Z")
}

fn rows_of(date: &str) -> Vec<String> {
    if date == QUIET_DAY {
        return vec![];
    }
    vec![
        format!(r#"{{"date":"{date}","model":"anthropic/claude-sonnet-4.5","model_permaslug":"anthropic/claude-sonnet-4.5","endpoint_id":"e1","provider_name":"Anthropic","usage":1.25,"byok_usage_inference":0.5,"requests":10,"prompt_tokens":1000,"completion_tokens":200,"reasoning_tokens":50,"cached_tokens":600}}"#),
        format!(r#"{{"date":"{date}","model":"openai/gpt-5","model_permaslug":"openai/gpt-5","endpoint_id":"e2","provider_name":"OpenAI","usage":0.25,"byok_usage_inference":0,"requests":2,"prompt_tokens":3000,"completion_tokens":100,"reasoning_tokens":0,"cached_tokens":0}}"#),
    ]
}

/// The last 30 finished UTC days, or the one `date` asks for.
fn activity(request: &Request) -> String {
    let dates: Vec<String> = match request.url.query_pairs().find(|(k, _)| k == "date") {
        Some((_, date)) => vec![date.into_owned()],
        None => (1..=30).map(|d| (at("2026-10-29T11:00:00Z").date_naive() - Duration::days(d)).to_string()).collect(),
    };
    format!(r#"{{"data":[{}]}}"#, dates.iter().flat_map(|d| rows_of(d)).collect::<Vec<_>>().join(","))
}

fn keys_page(count: usize, daily: f64, weekly: f64, monthly: f64) -> String {
    let key = format!(r#"{{"hash":"f01d","name":"Agent","disabled":false,"usage":{monthly},"usage_daily":{daily},"usage_weekly":{weekly},"usage_monthly":{monthly},"byok_usage":0,"byok_usage_daily":0,"byok_usage_weekly":0,"byok_usage_monthly":0}}"#);
    format!(r#"{{"data":[{}]}}"#, vec![key; count].join(","))
}

struct Fixture {
    server: MockServer,
    /// 0 answers with the activity, anything else is the status of an empty answer.
    activity_status: Arc<AtomicU16>,
    ledger: Ledger,
}

impl Fixture {
    async fn new() -> Self {
        let server = MockServer::start().await;
        let json = |body: &str| ResponseTemplate::new(200).set_body_string(body.to_string());
        Mock::given(method("GET")).and(path("/api/v1/key")).respond_with(json(MANAGEMENT_KEY)).mount(&server).await;
        Mock::given(method("GET")).and(path("/api/v1/credits")).respond_with(json(CREDITS)).mount(&server).await;
        Mock::given(method("GET")).and(path("/api/v1/keys")).and(query_param("offset", "0")).respond_with(json(&keys_page(100, 0.25, 0.5, 1.0))).mount(&server).await;
        Mock::given(method("GET")).and(path("/api/v1/keys")).and(query_param("offset", "100")).respond_with(json(&keys_page(1, 1.5, 4.0, 12.0))).mount(&server).await;
        let activity_status = Arc::new(AtomicU16::new(0));
        let status = activity_status.clone();
        Mock::given(method("GET"))
            .and(path("/api/v1/activity"))
            .respond_with(move |request: &Request| match status.load(Ordering::SeqCst) {
                0 => ResponseTemplate::new(200).set_body_string(activity(request)),
                code => ResponseTemplate::new(code),
            })
            .mount(&server)
            .await;
        Self { server, activity_status, ledger: Ledger::default() }
    }

    async fn refresh(&mut self, at: DateTime<Utc>, forced: bool) -> ProviderSnapshot {
        let client = OpenRouterClient::new(&format!("{}/api/v1", self.server.uri()));
        client.fetch("key", at, Run { ledger: &mut self.ledger, forced, account: "openrouter@test" }).await.unwrap()
    }

    async fn activity_dates(&self) -> Vec<String> {
        let requests = self.server.received_requests().await.unwrap();
        requests.iter().filter(|r| r.url.path() == "/api/v1/activity").map(|r| r.url.query().map_or("all".into(), |q| q.trim_start_matches("date=").to_string())).collect()
    }
}

fn rows(snapshot: &ProviderSnapshot) -> BTreeMap<String, String> {
    let titles = ["Today", "Yesterday", "This Week", "This Month", "Prior 30 Days"];
    snapshot
        .lines
        .iter()
        .filter(|l| titles.contains(&l.label()))
        .map(|l| match l {
            MetricLine::Values { label, values } => (label.clone(), values_text(values)),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn expected(pairs: [(&str, &str); 5]) -> BTreeMap<String, String> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[tokio::test]
async fn a_management_key_shows_reported_spend_and_the_activity_of_the_prior_30_days() {
    let mut fixture = Fixture::new().await;
    let snapshot = fixture.refresh(now(), true).await;

    assert_eq!(
        rows(&snapshot),
        expected([("Today", "$26.50"), ("Yesterday", "$1.50"), ("This Week", "$54"), ("This Month", "$112"), ("Prior 30 Days", "$43.50")])
    );
    assert_eq!(fixture.activity_dates().await, ["all"]);
    let labels: Vec<_> = snapshot.lines.iter().map(MetricLine::label).collect();
    assert_eq!(labels, ["Balance", "Today", "Yesterday", "This Week", "This Month", "Prior 30 Days"], "right after Balance");
}

#[tokio::test]
async fn each_window_row_has_a_breakdown_of_what_openrouter_reported() {
    let mut fixture = Fixture::new().await;
    let snapshot = fixture.refresh(now(), true).await;
    let breakdown = |title: &str| &snapshot.breakdowns[title];

    let prior = breakdown("Prior 30 Days");
    let totals: BTreeMap<Measure, f64> = prior.totals.values.clone();
    use Measure::*;
    assert_eq!(
        totals,
        BTreeMap::from([(Spend, 43.5), (ByokSpend, 14.5), (Tokens, 124_700.0), (InputTokens, 98_600.0), (CacheReadTokens, 17_400.0), (OutputTokens, 7_250.0), (ReasoningTokens, 1_450.0)])
    );
    assert!(prior.token_parts_fill_total());
    assert_eq!(prior.models().iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["anthropic/claude-sonnet-4.5", "openai/gpt-5"]);
    assert_eq!(prior.models().iter().map(|m| m.dollars).collect::<Vec<_>>(), [Some(36.25), Some(7.25)]);
    assert_eq!(prior.models().iter().map(|m| m.tokens).collect::<Vec<_>>(), [Some(34_800.0), Some(89_900.0)]);
    assert_eq!(prior.figures(), ["$43.50", "124.7K tokens", "$14.50 BYOK"]);
    let today = breakdown("Today");
    assert_eq!(today.totals.values, BTreeMap::from([(Spend, 26.5), (ByokSpend, 0.0)]));
    assert!(today.models().is_empty());
    let week = breakdown("This Week");
    assert_eq!(week.figures(), ["$54", "12.9K tokens"], "the reported spend, with the tokens of Monday to yesterday");
    assert_eq!(week.models().iter().map(|m| m.tokens).collect::<Vec<_>>(), [Some(3_600.0), Some(9_300.0)]);
    assert_eq!(week.model_figures(), [ModelFigure::Cost, ModelFigure::Tokens]);
    assert_eq!(breakdown("This Month").figures(), ["$112", "116.1K tokens"], "a day with no activity adds no tokens");
}

#[tokio::test]
async fn activity_is_read_again_hourly_and_a_new_utc_day_at_once() {
    let mut fixture = Fixture::new().await;
    fixture.refresh(now(), true).await;

    let soon = fixture.refresh(now() + Duration::seconds(300), false).await;
    assert_eq!(fixture.activity_dates().await, ["all"], "a pass within the hour reads no activity");
    assert_eq!(rows(&soon)["Prior 30 Days"], "$43.50");

    fixture.refresh(now() + Duration::seconds(3_660), false).await;
    assert_eq!(fixture.activity_dates().await, ["all", "2026-10-28"], "an hour later only yesterday is read again");

    fixture.refresh(now() + Duration::seconds(3_660), true).await;
    assert_eq!(fixture.activity_dates().await, ["all", "2026-10-28", "2026-10-28"], "a forced refresh reads yesterday again");

    let next = fixture.refresh(at("2026-10-30T00:05:00Z"), false).await;
    assert_eq!(fixture.activity_dates().await, ["all", "2026-10-28", "2026-10-28", "2026-10-29"], "the new yesterday is read right after UTC midnight");
    assert_eq!(rows(&next)["Prior 30 Days"], "$43.50");
}

#[tokio::test]
async fn a_failed_activity_read_zeroes_its_ranges_keeps_the_card_and_waits_an_hour() {
    let mut fixture = Fixture::new().await;
    fixture.activity_status.store(500, Ordering::SeqCst);
    let failed = fixture.refresh(now(), true).await;
    fixture.activity_status.store(0, Ordering::SeqCst);

    assert_eq!(
        rows(&failed),
        expected([("Today", "$26.50"), ("Yesterday", "$0"), ("This Week", "$54"), ("This Month", "$112"), ("Prior 30 Days", "$0")])
    );
    fixture.refresh(now() + Duration::seconds(300), false).await;
    assert_eq!(fixture.activity_dates().await, ["all"]);

    let later = fixture.refresh(now() + Duration::seconds(3_660), false).await;
    assert_eq!(fixture.activity_dates().await, ["all", "all"]);
    assert_eq!(rows(&later)["Prior 30 Days"], "$43.50");
}

#[test]
fn a_standard_key_shows_only_the_spend_it_reports() {
    let key = parse_key(
        r#"{"data":{"is_management_key":false,"usage_daily":1.5,"usage_weekly":4,"usage_monthly":12,"byok_usage_daily":2.5}}"#,
        now(),
    )
    .unwrap();
    assert_eq!(key.spend[&Window::Today], Totals::from([(Measure::Spend, 1.5), (Measure::ByokSpend, 2.5)]));
    assert_eq!(key.spend[&Window::ThisMonth], Totals::from([(Measure::Spend, 12.0)]));
}

#[test]
fn activity_rows_are_summed_per_day_and_any_malformed_row_rejects_the_read() {
    let row = |extra: &str| format!(r#"{{"data":[{{"date":"2026-10-28 00:00:00","model":"m","usage":1,"prompt_tokens":10,"completion_tokens":4,"cached_tokens":6,"reasoning_tokens":1{extra}}}]}}"#);
    let days = history::activity_days(&row("")).unwrap();
    let day = &days[&"2026-10-28".parse().unwrap()];
    assert_eq!(day.totals.get(Measure::Tokens), Some(14.0));
    assert_eq!(day.totals.get(Measure::InputTokens), Some(4.0), "cached tokens are counted in Cache Read");
    assert_eq!(day.totals.get(Measure::OutputTokens), Some(3.0), "reasoning tokens are counted in Reasoning");
    for bad in [r#"{"data":[{"date":"28/10/2026","model":"m","usage":1,"prompt_tokens":1,"completion_tokens":1}]}"#, r#"{"data":[{"date":"2026-10-28","usage":1}]}"#, r#"{"data":{}}"#] {
        assert!(history::activity_days(bad).is_none(), "{bad}");
    }
}
