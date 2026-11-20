use super::history::{parse_days, UsageSource};
use super::*;
use crate::history::{Calendar, Day, History, Ledger, Measure, ModelFigure, Source, Totals};
use chrono::NaiveDate;
use crate::priority::values_text;
use crate::MetricLine;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// 20:30 UTC on 30 October, already 31 October in Dubai, so rows read on the local clock would miss.
const NOW: i64 = 1_793_392_200;
const UTC_TODAY: i64 = 1_793_318_400;
const BALANCE: &str = r#"{"included":{"balance_usd":72.5,"allowance_usd":100},"purchased":{"balance_usd":25}}"#;

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(NOW, 0).unwrap()
}

fn iso(seconds: i64) -> String {
    DateTime::from_timestamp(seconds, 0).unwrap().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A day of 4 requests worth $0.25 over two models, today twice that so far, a quiet yesterday, and a costly
/// day 30 days back that Last 30 Days leaves out. A legacy plan reports request counts only.
fn usage(request: &Request, legacy: bool) -> String {
    let range = request.url.query_pairs().find(|(k, _)| k == "range").map_or("7d".to_string(), |(_, v)| v.into_owned());
    let days: i64 = if range == "30d" { 30 } else { 7 };
    let metrics = |scale: i64, (requests, usd, input, cached, output): (i64, f64, i64, i64, i64)| {
        let head = format!(r#""request_count":{}"#, requests * scale);
        if legacy {
            return head;
        }
        format!(r#"{head},"usage_usd":{},"input_tokens":{},"cached_input_tokens":{},"output_tokens":{}"#, usd * scale as f64, input * scale, cached * scale, output * scale)
    };
    let buckets: Vec<String> = (0..=days)
        .rev()
        .map(|ago| {
            let scale = match ago {
                0 => 2,
                1 => 0,
                30 => 400,
                _ => 1,
            };
            let from = UTC_TODAY - ago * 86_400;
            let until = if ago == 0 { NOW } else { from + 86_400 };
            let groups = if scale == 0 {
                String::new()
            } else {
                format!(r#"{{"model":"glm-5.3-flash",{}}},{{"model":"gpt-oss:120b",{}}}"#, metrics(scale, (3, 0.2, 2_500, 1_000, 200)), metrics(scale, (1, 0.05, 500, 0, 100)))
            };
            let partial = if ago == 0 { r#""partial":true,"# } else { "" };
            format!(r#"{{"from":"{}","until":"{}",{partial}{},"groups":[{groups}]}}"#, iso(from), iso(until), metrics(scale, (4, 0.25, 3_000, 1_000, 300)))
        })
        .collect();
    format!(r#"{{"range":"{range}","scope":"self","granularity":"day","group_by":"model","from":"{}","until":"{}","totals":{{"request_count":0}},"buckets":[{}]}}"#, iso(UTC_TODAY - days * 86_400), iso(NOW), buckets.join(","))
}

struct Fixture {
    server: MockServer,
    status: Arc<AtomicU16>,
    ledger: Ledger,
}

impl Fixture {
    async fn new(legacy: bool) -> Self {
        let server = MockServer::start().await;
        let ok = |body: &'static str| ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json");
        Mock::given(path("/api/balance")).respond_with(ok(BALANCE)).mount(&server).await;
        Mock::given(path("/api/me")).respond_with(ok(r#"{"Plan":"pro"}"#)).mount(&server).await;
        let status = Arc::new(AtomicU16::new(0));
        let override_status = status.clone();
        Mock::given(method("GET"))
            .and(path("/api/usage"))
            .and(header("authorization", "Bearer ollama-key"))
            .respond_with(move |request: &Request| match override_status.load(Ordering::SeqCst) {
                0 => ResponseTemplate::new(200).set_body_raw(usage(request, legacy).into_bytes(), "application/json"),
                code => ResponseTemplate::new(code),
            })
            .mount(&server)
            .await;
        Self { server, status, ledger: Ledger::default() }
    }

    fn client(&self) -> OllamaClient {
        OllamaClient::new(&self.server.uri())
    }

    async fn refresh(&mut self, at: DateTime<Utc>, forced: bool) -> ProviderSnapshot {
        let client = self.client();
        let run = Run { ledger: &mut self.ledger, forced, account: "ollama@test" };
        client.fetch_with_history("ollama-key", None, true, at, run).await.unwrap().snapshot
    }

    async fn queries(&self) -> Vec<String> {
        let requests = self.server.received_requests().await.unwrap();
        requests.iter().filter(|r| r.url.path() == "/api/usage").map(|r| r.url.query().unwrap_or_default().to_string()).collect()
    }
}

fn rows(snapshot: &ProviderSnapshot) -> BTreeMap<String, String> {
    let titles = ["Today", "Yesterday", "Last 30 Days", "Requests"];
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

fn full_rows() -> BTreeMap<String, String> {
    [("Today", "$0.50 · 6.6K tokens"), ("Yesterday", "$0 · 0 tokens"), ("Last 30 Days", "$7.50 · 99K tokens"), ("Requests", "120 in 30 days")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[tokio::test]
async fn the_rows_show_what_ollama_reported_for_each_utc_day_and_tokens_count_cached_input_once() {
    let mut fixture = Fixture::new(false).await;
    let snapshot = fixture.refresh(now(), true).await;

    assert_eq!(rows(&snapshot), full_rows());
    assert_eq!(fixture.queries().await, ["range=30d&group_by=model"], "one call reads all 30 days");
    let labels: Vec<_> = snapshot.lines.iter().map(MetricLine::label).collect();
    assert_eq!(labels, ["Monthly", "Purchased Credits", "Today", "Yesterday", "Last 30 Days", "Requests"], "after the balance lines, Requests last");
}

#[tokio::test]
async fn today_and_yesterday_are_read_again_hourly_from_the_last_seven_days_and_when_forced() {
    let mut fixture = Fixture::new(false).await;
    fixture.refresh(now(), true).await;

    let soon = fixture.refresh(now() + chrono::Duration::minutes(5), false).await;
    assert_eq!(fixture.queries().await.len(), 1, "no history read within the hour");
    assert_eq!(rows(&soon), full_rows(), "the rows keep the last read meanwhile");

    fixture.refresh(now() + chrono::Duration::minutes(61), false).await;
    let forced = fixture.refresh(now() + chrono::Duration::minutes(62), true).await;

    assert_eq!(fixture.queries().await, ["range=30d&group_by=model", "range=7d&group_by=model", "range=7d&group_by=model"]);
    assert_eq!(rows(&forced), full_rows());
}

#[tokio::test]
async fn a_legacy_plan_that_reports_only_requests_shows_its_requests_and_zero_ranges() {
    let mut fixture = Fixture::new(true).await;
    let snapshot = fixture.refresh(now(), true).await;

    let zero = "$0 · 0 tokens";
    let expected: BTreeMap<String, String> =
        [("Today", zero), ("Yesterday", zero), ("Last 30 Days", zero), ("Requests", "120 in 30 days")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    assert_eq!(rows(&snapshot), expected);
}

#[tokio::test]
async fn a_successful_read_covers_omitted_days_without_inventing_figures() {
    let server = MockServer::start().await;
    let sparse = r#"{"granularity":"day","buckets":[{"from":"2026-10-29T00:00:00Z","until":"2026-10-30T00:00:00Z","request_count":3}]}"#;
    Mock::given(path("/api/usage")).respond_with(ResponseTemplate::new(200).set_body_raw(sparse.as_bytes().to_vec(), "application/json")).up_to_n_times(1).mount(&server).await;
    Mock::given(path("/api/usage")).respond_with(ResponseTemplate::new(500)).mount(&server).await;
    let client = OllamaClient::new(&server.uri());
    let source = UsageSource { client: &client, api_key: "key", account: "ollama" };
    let dates = History::day_starts(0..2, now(), Calendar::Utc);

    let read = source.read(&dates, now(), Calendar::Utc).await;
    assert_eq!(read.days[&dates[0]], Day::default());
    assert_eq!(read.days[&dates[1]].totals, Totals::from([(Measure::Requests, 3.0)]));

    assert!(source.read(&dates, now(), Calendar::Utc).await.days.is_empty());
}

#[tokio::test]
async fn a_failed_read_zeroes_the_ranges_keeps_the_card_and_waits_an_hour() {
    let mut fixture = Fixture::new(false).await;
    fixture.status.store(500, Ordering::SeqCst);

    let failed = fixture.refresh(now(), true).await;
    let zero = "$0 · 0 tokens";
    let expected: BTreeMap<String, String> = [("Today", zero), ("Yesterday", zero), ("Last 30 Days", zero)].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    assert_eq!(rows(&failed), expected, "Requests has no range to show");
    assert!(failed.line("Monthly").is_some());

    fixture.status.store(0, Ordering::SeqCst);
    fixture.refresh(now() + chrono::Duration::minutes(5), false).await;
    assert_eq!(fixture.queries().await.len(), 1, "a failed read waits an hour too");

    let later = fixture.refresh(now() + chrono::Duration::minutes(61), false).await;
    assert_eq!(fixture.queries().await, ["range=30d&group_by=model", "range=30d&group_by=model"], "every day is still unread");
    assert_eq!(rows(&later), full_rows());
}

#[tokio::test]
async fn each_window_row_has_a_breakdown_on_utc_days_and_the_requests_row_has_none() {
    let mut fixture = Fixture::new(false).await;
    let snapshot = fixture.refresh(now(), true).await;

    let today = &snapshot.breakdowns["Today"];
    use Measure::*;
    assert_eq!(
        today.totals.values,
        BTreeMap::from([(UsageValue, 0.5), (Requests, 8.0), (Tokens, 6_600.0), (InputTokens, 6_000.0), (CacheReadTokens, 2_000.0), (OutputTokens, 600.0)])
    );
    assert_eq!(today.model_figures(), [ModelFigure::Cost, ModelFigure::Tokens]);
    assert_eq!(today.models().iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["glm-5.3-flash", "gpt-oss:120b"]);
    assert_eq!(today.models().iter().map(|m| m.dollars).collect::<Vec<_>>(), [Some(0.4), Some(0.1)]);
    assert_eq!(today.models().iter().map(|m| m.tokens).collect::<Vec<_>>(), [Some(5_400.0), Some(1_200.0)]);
    let last30 = &snapshot.breakdowns["Last 30 Days"];
    assert_eq!(last30.models().iter().map(|m| m.tokens).collect::<Vec<_>>(), [Some(81_000.0), Some(18_000.0)]);
    assert!(!snapshot.breakdowns.contains_key("Requests"));
}

#[test]
fn days_parse_ollamas_buckets_and_any_malformed_bucket_rejects_the_read() {
    let days = |buckets: &str, granularity: &str| parse_days(format!(r#"{{"granularity":"{granularity}","buckets":[{buckets}]}}"#).as_bytes());
    let september30: NaiveDate = "2026-09-30".parse().unwrap();
    let day = r#""from":"2026-09-30T00:00:00Z","until":"2026-10-01T00:00:00Z""#;
    let documented = format!(
        r#"{{{day},"request_count":8,"usage_usd":0.0166,"input_tokens":52000,"cached_input_tokens":30000,"output_tokens":7000,"groups":[{{"model":"glm-5.3-flash","request_count":6,"usage_usd":0.0066,"input_tokens":48000,"cached_input_tokens":30000,"output_tokens":6000}},{{"model":"glm-5.3","request_count":2,"usage_usd":0.01,"input_tokens":4000,"cached_input_tokens":0,"output_tokens":1000}}]}}"#
    );

    let parsed = days(&documented, "day").unwrap().remove(&september30).unwrap();
    use Measure::*;
    assert_eq!(
        parsed.totals.values,
        BTreeMap::from([(Requests, 8.0), (UsageValue, 0.0166), (Tokens, 59_000.0), (InputTokens, 52_000.0), (CacheReadTokens, 30_000.0), (OutputTokens, 7_000.0)])
    );
    assert_eq!(parsed.by_model.unwrap()["glm-5.3"].get(Tokens), Some(5_000.0));
    assert_eq!(days(&format!(r#"{{{day},"request_count":3}}"#), "day").unwrap()[&september30].totals.values, BTreeMap::from([(Requests, 3.0)]), "a legacy day has requests only");
    let remainder = format!(r#"{{{day},"request_count":9,"groups":[{{"model":"a","request_count":1}}],"remainder":{{"request_count":8}}}}"#);
    assert_eq!(days(&remainder, "day").unwrap()[&september30].by_model, None, "usage beyond the named models leaves the day unsplit");
    assert_eq!(days(&format!(r#"{{{day},"request_count":0,"groups":[]}}"#), "day").unwrap()[&september30].by_model, Some(BTreeMap::new()));

    for bucket in [
        format!("{{{day}}}"),
        format!(r#"{{{day},"request_count":-1}}"#),
        format!(r#"{{{day},"request_count":1,"usage_usd":"lots"}}"#),
        format!(r#"{{{day},"request_count":1,"groups":[{{"request_count":1}}]}}"#),
        format!(r#"{{{day},"request_count":2,"groups":[{{"model":"a","request_count":1}},{{"model":"a","request_count":1}}]}}"#),
        r#"{"from":"2026-09-30T02:00:00Z","until":"2026-09-30T03:00:00Z","request_count":1}"#.to_string(),
        r#"{"from":"2026-09-30T00:00:00Z","until":"2026-10-02T00:00:00Z","request_count":1}"#.to_string(),
        format!(r#"{{{day},"request_count":1}},{{{day},"request_count":1}}"#),
    ] {
        assert!(days(&bucket, "day").is_none(), "{bucket}");
    }
    assert!(days(&format!(r#"{{{day},"request_count":1}}"#), "hour").is_none());
}
