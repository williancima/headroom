use super::history::{parse_page, EventsSource};
use super::*;
use crate::history::{history_lines, refresh_history, Calendar, History, HistoryRow, Ledger, Measure, ModelFigure, Source, Window};
use crate::priority::values_text;
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const MIXED_PAGE: &str = r#"{"totalUsageEventsCount":7,"usageEventsDisplay":[
 {"timestamp":"1793000000000","model":"claude-4.6-opus-high-thinking","kind":"USAGE_EVENT_KIND_USAGE_BASED","requestsCosts":30.4,"usageBasedCosts":"$1.25","isTokenBasedCall":true,
  "tokenUsage":{"inputTokens":3,"outputTokens":20525,"cacheWriteTokens":112151,"totalCents":121.41},"cursorTokenFee":3.32,"isChargeable":true,"isHeadless":false,"chargedCents":124.73},
 {"timestamp":"1793000001000","model":"composer-2","kind":"USAGE_EVENT_KIND_INCLUDED_IN_PRO","usageBasedCosts":"-","tokenUsage":{"inputTokens":3438,"outputTokens":76,"totalCents":40},"isChargeable":false,"chargedCents":40},
 {"timestamp":"1793000002000","model":"gpt-5","kind":"USAGE_EVENT_KIND_USAGE_BASED","isChargeable":false,"chargedCents":50},
 {"timestamp":"1793000003000","model":"gpt-5","kind":"USAGE_EVENT_KIND_ERRORED_NOT_CHARGED"},
 {"timestamp":"1793000003500","model":"composer-2","kind":"USAGE_EVENT_KIND_FREE_CREDIT","isChargeable":false,"chargedCents":40},
 {"timestamp":1793000004000,"model":"composer-2","kind":"USAGE_EVENT_KIND_USAGE_BASED","chargedCents":"3.5"},
 {"timestamp":"1793000005000","model":"composer-2","kind":"USAGE_EVENT_KIND_USAGE_BASED","isChargeable":true}]}"#;

const DUBAI: Calendar = Calendar::Fixed(4 * 3600);

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_793_000_000, 0).unwrap()
}

fn access_token() -> String {
    format!("e30.{}.sig", STANDARD_NO_PAD.encode(r#"{"sub":"auth0|user_fake"}"#))
}

fn page_events(body: &str) -> Vec<(Option<String>, BTreeMap<Measure, f64>)> {
    parse_page(body.as_bytes()).unwrap().1.into_iter().map(|e| (e.model, e.totals.values)).collect()
}

#[test]
fn only_on_demand_events_count_what_cursor_charged() {
    let (total, events) = parse_page(MIXED_PAGE.as_bytes()).unwrap();
    assert_eq!(total, 7);
    let spend: Vec<f64> = events.iter().map(|e| e.totals.get(Measure::Spend).unwrap()).collect();
    assert_eq!(spend, [124.73, 0.0, 0.0, 0.0, 0.0, 3.5, 0.0].map(|c| c / 100.0));
    assert_eq!(events[0].date.timestamp(), 1_793_000_000);
    assert_eq!(events[6].date.timestamp(), 1_793_000_005);
}

#[test]
fn tokens_are_every_kind_cursor_reported_on_any_event_kind_and_zero_without_token_usage() {
    let events = page_events(MIXED_PAGE);
    let tokens: Vec<f64> = events.iter().map(|(_, t)| t[&Measure::Tokens]).collect();
    assert_eq!(tokens, [132_679.0, 3_514.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    use Measure::*;
    assert_eq!(
        events[0].1,
        BTreeMap::from([(Spend, 1.2473), (UsageEvents, 1.0), (Tokens, 132_679.0), (InputTokens, 3.0), (CacheReadTokens, 0.0), (CacheWriteTokens, 112_151.0), (OutputTokens, 20_525.0)])
    );

    let (_, mut parsed) = parse_page(MIXED_PAGE.as_bytes()).unwrap();
    parsed[0].model = None;
    let day = history::sum_day(&parsed);
    assert_eq!(day.by_model.unwrap()["composer-2"].get(Tokens), Some(3_514.0), "an unnamed event preserves the named model totals");
    assert_eq!(day.totals.get(Tokens), Some(136_193.0), "the total includes the event with no model name");
    assert_eq!(day.totals.get(UsageEvents), Some(7.0));
    assert_eq!(history::sum_day(&[]).totals, crate::history::Totals::zero(&[Spend, UsageEvents, Tokens, InputTokens, CacheReadTokens, CacheWriteTokens, OutputTokens]), "a quiet day is zeros, not a gap");
}

#[test]
fn an_empty_page_leaves_out_its_event_list_but_not_its_count() {
    assert!(parse_page(br#"{"totalUsageEventsCount":0}"#).unwrap().1.is_empty());
    for body in [
        r#"{"error":"unauthorized"}"#,
        r#"{"usageEventsDisplay":[]}"#,
        r#"{"totalUsageEventsCount":1,"usageEventsDisplay":[{"kind":"USAGE_EVENT_KIND_USAGE_BASED","chargedCents":5}]}"#,
        r#"{"totalUsageEventsCount":1,"usageEventsDisplay":[{"timestamp":"1","kind":"USAGE_EVENT_KIND_USAGE_BASED","chargedCents":-5}]}"#,
        r#"{"totalUsageEventsCount":1,"usageEventsDisplay":[{"timestamp":"1","kind":"USAGE_EVENT_KIND_USAGE_BASED","chargedCents":"$0.05"}]}"#,
        r#"{"totalUsageEventsCount":1,"usageEventsDisplay":[{"timestamp":"1","tokenUsage":{"inputTokens":-1}}]}"#,
        r#"{"totalUsageEventsCount":1,"usageEventsDisplay":[{"timestamp":"1","tokenUsage":"lots"}]}"#,
    ] {
        assert!(parse_page(body.as_bytes()).is_none(), "{body}");
    }
}

/// Every day holds one on-demand event Cursor charged $1.25 for and one included event worth $9.99 of plan
/// usage, so only the charge counts.
fn events_page(request: &Request) -> ResponseTemplate {
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    let number = |key: &str| body[key].as_str().and_then(|v| v.parse::<i64>().ok());
    let (Some(start), Some(end)) = (number("startDate"), number("endDate")) else { return ResponseTemplate::new(400) };
    if body["page"] != 1 {
        return ResponseTemplate::new(400);
    }
    let timestamp = (start + 3_600_000).min(end);
    ResponseTemplate::new(200).set_body_string(format!(
        r#"{{"totalUsageEventsCount":2,"usageEventsDisplay":[
         {{"timestamp":"{timestamp}","model":"claude-4.6-opus","kind":"USAGE_EVENT_KIND_USAGE_BASED","isChargeable":true,"chargedCents":125,"tokenUsage":{{"inputTokens":3,"outputTokens":200,"totalCents":120}}}},
         {{"timestamp":"{timestamp}","model":"composer-2","kind":"USAGE_EVENT_KIND_INCLUDED_IN_PRO","isChargeable":false,"chargedCents":999,"tokenUsage":{{"inputTokens":3000,"outputTokens":100,"totalCents":999}}}}]}}"#
    ))
}

struct Fixture {
    server: MockServer,
    status: Arc<AtomicU16>,
    ledger: Ledger,
}

impl Fixture {
    async fn new(retry_after: Option<&'static str>) -> Self {
        let server = MockServer::start().await;
        let status = Arc::new(AtomicU16::new(0));
        let override_status = status.clone();
        Mock::given(method("POST"))
            .and(path("/api/dashboard/get-filtered-usage-events"))
            .respond_with(move |request: &Request| match override_status.load(Ordering::SeqCst) {
                0 => events_page(request),
                code => {
                    let response = ResponseTemplate::new(code);
                    retry_after.map_or(response.clone(), |value| response.insert_header("retry-after", value))
                }
            })
            .mount(&server)
            .await;
        Self { server, status, ledger: Ledger::default() }
    }

    fn source(&self) -> EventsSource {
        let client = CursorClient::new(CursorConfig { web_base: self.server.uri(), ..CursorConfig::default() });
        let mut source = EventsSource::new(&client, &access_token(), "cursor@test");
        source.clock = DUBAI;
        source
    }

    async fn refresh(&mut self, at: DateTime<Utc>) -> crate::history::HistoryOutput {
        let source = self.source();
        history_lines(&source, &mut self.ledger, &history::rows(), at, false).await
    }

    async fn requests(&self) -> Vec<Request> {
        self.server.received_requests().await.unwrap()
    }
}

fn line_text(output: &crate::history::HistoryOutput, label: &str) -> String {
    match output.lines.iter().find(|l| l.label() == label) {
        Some(crate::MetricLine::Values { values, .. }) => values_text(values),
        other => panic!("{other:?}"),
    }
}

fn spend(output: &crate::history::HistoryOutput) -> Vec<f64> {
    ["Today", "Yesterday", "Last 30 Days"]
        .into_iter()
        .map(|label| match output.lines.iter().find(|l| l.label() == label) {
            Some(crate::MetricLine::Values { values, .. }) => values[0].number,
            other => panic!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn the_rows_show_what_cursor_charged_for_on_demand_usage_and_no_estimate() {
    let mut fixture = Fixture::new(None).await;
    let output = fixture.refresh(now()).await;

    assert_eq!(spend(&output), [1.25, 1.25, 37.5]);
    assert_eq!(line_text(&output, "Today"), "$1.25 · 3.3K tokens");
    assert_eq!(line_text(&output, "Last 30 Days"), "$37.50 · 99.1K tokens");
    let requests = fixture.requests().await;
    assert_eq!(requests.len(), 30, "one page for each of the 30 days");
    assert_eq!(requests[0].headers.get("cookie").unwrap().to_str().unwrap(), format!("WorkosCursorSessionToken=user_fake%3A%3A{}", access_token()));
    assert_eq!(requests[0].headers.get("origin").unwrap(), "https://cursor.com");
}

#[tokio::test]
async fn a_later_refresh_rereads_only_today_and_yesterday() {
    let mut fixture = Fixture::new(None).await;
    fixture.refresh(now()).await;
    let first_read = fixture.requests().await.len();

    let output = fixture.refresh(now() + chrono::Duration::seconds(30)).await;

    assert_eq!(fixture.requests().await.len() - first_read, 2);
    assert_eq!(spend(&output), [1.25, 1.25, 37.5]);
}

#[tokio::test]
async fn a_failed_reread_drops_unread_days_but_keeps_earlier_days_in_the_range() {
    let mut fixture = Fixture::new(None).await;
    fixture.status.store(500, Ordering::SeqCst);
    let failed = fixture.refresh(now()).await;
    assert_eq!(spend(&failed), [0.0, 0.0, 0.0], "nothing read yet, so every range reads $0");

    fixture.status.store(0, Ordering::SeqCst);
    fixture.refresh(now()).await;
    fixture.status.store(503, Ordering::SeqCst);
    let partial = fixture.refresh(now()).await;

    assert_eq!(spend(&partial), [0.0, 0.0, 35.0], "28 complete days at $1.25 after today and yesterday failed");
}

/// The wait starts when the 429 arrives, and each answer to it is sent at the moment of the call.
#[tokio::test]
async fn a_rate_limit_stops_the_sweep_and_waits_for_retry_after_or_an_hour() {
    for (retry_after, wait) in [(Some("120"), 120), (None, 3600)] {
        let mut fixture = Fixture::new(retry_after).await;
        fixture.status.store(429, Ordering::SeqCst);

        fixture.refresh(now()).await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let first_sweep = fixture.requests().await.len();
        assert!(first_sweep <= 4, "the sweep stops at the 429 instead of asking for all 30 days");

        fixture.status.store(0, Ordering::SeqCst);
        let held = fixture.refresh(now() + chrono::Duration::seconds(wait - 1)).await;
        assert_eq!(fixture.requests().await.len(), first_sweep, "no history read before the wait ends");
        assert_eq!(spend(&held), [0.0, 0.0, 0.0]);

        let later = fixture.refresh(now() + chrono::Duration::seconds(wait + 1)).await;
        assert_eq!(fixture.requests().await.len() - first_sweep, 30, "every unread day is still due");
        assert_eq!(spend(&later), [1.25, 1.25, 37.5]);
    }
}

#[tokio::test]
async fn each_window_row_has_a_breakdown_of_what_cursor_reported() {
    let mut fixture = Fixture::new(None).await;
    let output = fixture.refresh(now()).await;

    let today = &output.breakdowns["Today"];
    assert_eq!(today.window, Window::Today);
    use Measure::*;
    assert_eq!(
        today.totals.values,
        BTreeMap::from([(Spend, 1.25), (UsageEvents, 2.0), (Tokens, 3_303.0), (InputTokens, 3_003.0), (CacheReadTokens, 0.0), (CacheWriteTokens, 0.0), (OutputTokens, 300.0)])
    );
    assert_eq!(today.models().iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["claude-4.6-opus", "composer-2"]);
    assert_eq!(today.models().iter().map(|m| m.dollars).collect::<Vec<_>>(), [Some(1.25), Some(0.0)]);
    assert_eq!(today.models().iter().map(|m| m.tokens).collect::<Vec<_>>(), [Some(203.0), Some(3_100.0)]);
    assert_eq!(today.model_figures(), [ModelFigure::Cost, ModelFigure::Tokens]);
    assert_eq!(output.breakdowns["Last 30 Days"].totals.get(UsageEvents), Some(60.0));
    assert!(output.breakdowns.contains_key("Yesterday"));
}

fn request_body(request: &Request) -> (i64, i64, i64) {
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    let number = |key: &str| body[key].as_str().unwrap().parse::<i64>().unwrap();
    (number("startDate"), number("endDate"), body["page"].as_i64().unwrap())
}

fn events(total: usize, at: i64, cents: &[f64]) -> ResponseTemplate {
    let events: Vec<String> = cents.iter().map(|c| format!(r#"{{"timestamp":"{at}","kind":"USAGE_EVENT_KIND_USAGE_BASED","chargedCents":{c}}}"#)).collect();
    ResponseTemplate::new(200).set_body_string(format!(r#"{{"totalUsageEventsCount":{total},"usageEventsDisplay":[{}]}}"#, events.join(",")))
}

async fn source_for(server: &MockServer) -> EventsSource {
    let client = CursorClient::new(CursorConfig { web_base: server.uri(), ..CursorConfig::default() });
    let mut source = EventsSource::new(&client, &access_token(), "cursor@1");
    source.clock = DUBAI;
    source
}

#[tokio::test]
async fn a_day_over_several_pages_counts_every_page_and_a_short_page_fails_the_day() {
    let span = History::day_starts(0..30, now(), DUBAI);
    let short = DUBAI.day_start(span[2]).unwrap().timestamp_millis();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(move |request: &Request| {
            let (start, _, page) = request_body(request);
            if start == short {
                return events(3, start + 1, if page == 1 { &[100.0, 100.0] } else { &[] });
            }
            events(3, start + 1, if page == 1 { &[100.0, 100.0] } else { &[25.5] })
        })
        .mount(&server)
        .await;
    let mut source = source_for(&server).await;
    source.page_size = 2;

    let days = source.read(&span, now(), DUBAI).await.days;

    assert_eq!(days.len(), 29);
    assert!(!days.contains_key(&span[2]), "a page that ends before the count leaves the day unread");
    assert!((days[&span[0]].totals.get(Measure::Spend).unwrap() - 2.255).abs() < 1e-9);
    let history = History::new(DUBAI, now(), days);
    let breakdown = crate::history::Breakdown::new(Window::LastDays(30), &history);
    assert!((breakdown.totals.get(Measure::Spend).unwrap() - 65.395).abs() < 1e-9, "only the 29 complete days at $2.255 count");
    assert_eq!(breakdown.totals.get(Measure::UsageEvents), Some(87.0), "the two events on the incomplete day are never added");
}

#[tokio::test]
async fn the_time_budget_keeps_the_days_that_finished() {
    let span = History::day_starts(0..30, now(), DUBAI);
    let today = DUBAI.day_start(span[0]).unwrap().timestamp_millis();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(move |request: &Request| {
            let (start, _, _) = request_body(request);
            let response = events(1, start + 1, &[7.0]);
            if start == today { response } else { response.set_delay(std::time::Duration::from_secs(5)) }
        })
        .mount(&server)
        .await;
    let mut source = source_for(&server).await;
    source.budget = std::time::Duration::from_millis(300);
    let mut ledger = Ledger::default();
    let earlier = BTreeMap::from([(span[9], crate::history::Day::new(crate::history::Totals::from([(Measure::Spend, 0.03)])))]);
    ledger.record(earlier, &[span[9]], &span);

    let due = ledger.due(&span);
    let read = source.read(&due, now(), DUBAI).await;
    ledger.record(read.days, &due, &span);

    assert_eq!(due.len(), 29, "a day read before is not read again");
    let spend: Vec<_> = ledger.days().iter().map(|(d, day)| (*d, day.totals.get(Measure::Spend).unwrap())).collect();
    assert_eq!(spend, [(span[9], 0.03), (span[0], 0.07)]);
}

#[tokio::test]
async fn a_day_is_read_from_its_start_to_the_next_midnight_and_today_to_the_moment_of_the_read() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(events(0, 0, &[])).mount(&server).await;
    let source = source_for(&server).await;
    let span = History::day_starts(0..2, now(), DUBAI);

    let days = source.read(&span, now(), DUBAI).await.days;

    assert_eq!(days.len(), 2);
    let requests = server.received_requests().await.unwrap();
    let mut bodies: Vec<_> = requests.iter().map(request_body).collect();
    bodies.sort();
    let (today, yesterday) = (DUBAI.day_start(span[0]).unwrap().timestamp_millis(), DUBAI.day_start(span[1]).unwrap().timestamp_millis());
    assert_eq!(bodies, [(yesterday, today - 1, 1), (today, now().timestamp_millis() - 1, 1)]);
}

#[test]
fn a_day_last_read_while_it_was_today_is_read_again_after_a_gap_of_days() {
    let friday = History::day_starts(0..30, now(), DUBAI);
    let monday = History::day_starts(0..30, now() + chrono::Duration::days(3), DUBAI);
    let day = |spend| crate::history::Day::new(crate::history::Totals::from([(Measure::Spend, spend)]));
    let mut ledger = Ledger::default();
    let due = ledger.due(&friday);
    ledger.record(BTreeMap::from([(friday[0], day(1.0)), (friday[1], day(4.0))]), &due, &friday);

    let due = ledger.due(&monday);

    assert!(due.contains(&friday[0]), "Friday was read before it ended");
    assert!(!due.contains(&friday[1]), "Thursday was read after it ended");
    ledger.record(BTreeMap::from([(friday[0], day(4.0))]), &[friday[0]], &monday);
    assert_eq!(ledger.days()[&friday[0]], day(4.0));
    assert!(!ledger.due(&monday).contains(&friday[0]));
}

#[tokio::test]
async fn a_session_without_a_user_id_fails_every_day_and_never_the_card() {
    let server = MockServer::start().await;
    let client = CursorClient::new(CursorConfig { web_base: server.uri(), ..CursorConfig::default() });
    let source = EventsSource::new(&client, "not-a-jwt", "cursor@1");
    let mut ledger = Ledger::default();
    let rows = history::rows();

    let history = refresh_history(&source, &mut ledger, &rows, now()).await.unwrap();

    assert!(history.days.is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
    let output = crate::history::HistoryOutput::of(&history, &rows);
    assert_eq!(line_text(&output, "Today"), "$0 · 0 tokens");
    assert!(matches!(rows[0].1, HistoryRow::Window(Window::Today, Measure::Spend, Some(Measure::Tokens))));
}
