use super::history::{combining, counts, enterprise_credits, workspace_credits, Report};
use super::*;
use crate::history::{Calendar, History, Ledger, Measure, ModelFigure, Run};
use crate::priority::values_text;
use crate::MetricLine;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// 08:00 UTC on 31 October 2026 plus eight hours.
fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_793_433_600 + 8 * 3600, 0).unwrap()
}

fn usage(plan_type: &str) -> String {
    format!(
        r#"{{"plan_type":"{plan_type}","rate_limit":{{"allowed":true,"limit_reached":false,
         "primary_window":{{"limit_window_seconds":604800,"reset_after_seconds":481662,"reset_at":1793915535,"used_percent":29}},
         "secondary_window":null}},"additional_rate_limits":null,"code_review_rate_limit":null,
         "credits":{{"approx_cloud_messages":[0,0],"approx_local_messages":[0,0],"balance":"0","has_credits":false,"overage_limit_reached":false,"unlimited":false}},
         "spend_control":{{"individual_limit":null,"reached":false}},"rate_limit_reached_type":null,"promo":null,
         "rate_limit_reset_credits":{{"applicable_available_count":0,"available_count":0}}}}"#
    )
}

const ACTIVE_COUNTS: &str = r#"{"balance_unit":"credit","group_by":"day","data":[
 {"date":"2026-10-02","clients":[],"totals":{"cached_text_input_tokens":470000000,"credits":16259.4,"text_output_tokens":363029,
  "text_total_tokens":495363029,"threads":40,"turns":392,"uncached_text_input_tokens":25000000,"users":1}},
 {"date":"2026-10-29",
  "clients":[{"cached_text_input_tokens":471808,"client_id":"CODEX_CLI","credits":6.335318,"text_output_tokens":2238,"text_total_tokens":567638,"threads":0,"turns":0,"uncached_text_input_tokens":93592,"users":0},
   {"client_id":"CODEX_DESKTOP_APP","credits":0.0,"threads":51,"turns":71,"users":1}],
  "models":[{"credits":0.0,"model":"gpt-6.1-sol","threads":40,"turns":55,"users":1},{"credits":0.0,"model":"gpt-6-astra","threads":3,"turns":31,"users":1}],
  "totals":{"cached_text_input_tokens":1475954688,"credits":68877.04709850001,"text_output_tokens":2180573,
   "text_total_tokens":1511437552,"threads":60,"turns":86,"uncached_text_input_tokens":33302291,"users":1}},
 {"date":"2026-10-30","clients":[],"totals":{"cached_text_input_tokens":7183232,"credits":105.64575,"text_output_tokens":27164,
  "text_total_tokens":8139475,"threads":0,"turns":0,"uncached_text_input_tokens":929079,"users":0}}]}"#;

fn counts_body(reports_today: bool) -> String {
    if !reports_today {
        return ACTIVE_COUNTS.to_string();
    }
    let today = r#"{"date":"2026-10-31","clients":[],"totals":{"text_total_tokens":0,"turns":0}},"#;
    ACTIVE_COUNTS.replacen(r#""data":["#, &format!(r#""data":[{today}"#), 1)
}

const WORKSPACE_BREAKDOWN: &str = r#"{"data":[
 {"date":"2026-10-29","premium_usage_values":{"credit_usage_credits":{"cli":3.0}},"models":[{"model":"gpt-6-astra","on_demand_credits":3.0}]},
 {"date":"2026-10-30","product_surface_usage_values":{"desktop_app":20.0},
  "premium_usage_values":{"credit_usage_credits":{"cli":4.0,"desktop_app":8.5},"total_usage_credits":{},"uncached_text_input_tokens_by_surface":{},"cached_text_input_tokens_by_surface":{},"text_output_tokens_by_surface":{},"text_total_tokens_by_surface":{}},
  "models":[{"credits":12.5,"model":"gpt-6-astra","speed":"fast","on_demand_credits":8.0},{"credits":2.5,"model":"gpt-6-astra","speed":"standard","on_demand_credits":2.0},{"credits":5.0,"model":"gpt-6.1-sol","speed":"standard","on_demand_credits":2.5}]}]}"#;
const RELATIVE_BREAKDOWN: &str = r#"{"units":"percent","group_by":"day","data":[{"date":"2026-10-30","product_surface_usage_values":{"desktop_app":100.0},"models":[{"credits":100.0,"model":"gpt-6-astra","speed":"fast"}]}]}"#;
const ENTERPRISE_CREDITS: &str = r#"{"breakdown":"model","unit":"credits","series":[{"key":"gpt-6-astra","label":"GPT-6 Astra","total":10.0},{"key":"gpt-6.1-sol","label":"GPT-6.1 Sol","total":2.5}],"data":[{"date":"2026-10-30","values":{"gpt-6-astra":10.0,"gpt-6.1-sol":2.5}}]}"#;

struct Fixture {
    server: MockServer,
    reports: Arc<AtomicU16>,
    credits: Arc<AtomicU16>,
    reports_today: Arc<AtomicBool>,
    counts: Ledger,
    credits_ledger: Ledger,
    plan: &'static str,
}

impl Fixture {
    async fn new(plan: &'static str, workspace_breakdown: &'static str) -> Self {
        let server = MockServer::start().await;
        let (reports, credits, reports_today) = (Arc::new(AtomicU16::new(0)), Arc::new(AtomicU16::new(0)), Arc::new(AtomicBool::new(true)));
        let usage_body = usage(plan);
        let (r, c, t) = (reports.clone(), credits.clone(), reports_today.clone());
        Mock::given(method("GET"))
            .respond_with(move |request: &Request| {
                let route = request.url.path().trim_start_matches("/backend-api/wham/");
                if route == "usage" {
                    return ResponseTemplate::new(200).set_body_string(usage_body.clone());
                }
                let body = match route {
                    "analytics/daily-workspace-usage-counts" => counts_body(t.load(Ordering::SeqCst)),
                    "usage/daily-workspace-user-token-usage-breakdown" => workspace_breakdown.to_string(),
                    "usage/daily-workspace-user-credit-usage" => ENTERPRISE_CREDITS.to_string(),
                    _ => return ResponseTemplate::new(404),
                };
                if route.starts_with("usage/") && c.load(Ordering::SeqCst) != 0 {
                    return ResponseTemplate::new(c.load(Ordering::SeqCst)).insert_header("retry-after", "3600");
                }
                match r.load(Ordering::SeqCst) {
                    0 => ResponseTemplate::new(200).set_body_string(body),
                    code => ResponseTemplate::new(code),
                }
            })
            .mount(&server)
            .await;
        Self { server, reports, credits, reports_today, counts: Ledger::default(), credits_ledger: Ledger::default(), plan }
    }

    fn client(&self) -> CodexClient {
        let base = self.server.uri();
        CodexClient::new(CodexConfig { api_base: format!("{base}/backend-api"), ..CodexConfig::default() })
    }

    async fn refresh(&mut self, at: DateTime<Utc>, forced: bool) -> ProviderSnapshot {
        let mut tokens = OAuthTokens {
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at: None,
            extras: std::collections::HashMap::from([(ACCOUNT_ID_KEY.to_string(), "acct-fake".to_string())]),
        };
        let client = self.client();
        let mut fetched = client.fetch(&mut tokens, None, false, at).await.unwrap();
        assert_eq!(fetched.plan_type.as_deref(), Some(self.plan));
        let counts = Run { ledger: &mut self.counts, forced, account: "codex@test" };
        client.add_history(&mut fetched, &tokens, at, counts, &mut self.credits_ledger).await;
        fetched.snapshot
    }

    async fn report_requests(&self) -> Vec<Request> {
        let requests = self.server.received_requests().await.unwrap();
        requests.into_iter().filter(|r| r.url.path().starts_with("/backend-api/wham/usage/") || r.url.path().starts_with("/backend-api/wham/analytics/")).collect()
    }

    async fn requests_for(&self, route: &str) -> Vec<Request> {
        self.report_requests().await.into_iter().filter(|r| r.url.path().ends_with(route)).collect()
    }
}

fn query(request: &Request) -> String {
    request.url.query().unwrap_or_default().to_string()
}

fn rows(snapshot: &ProviderSnapshot) -> BTreeMap<String, String> {
    snapshot
        .lines
        .iter()
        .filter(|l| ["Today", "Yesterday", "Last 30 Days"].contains(&l.label()))
        .map(|l| match l {
            MetricLine::Values { label, values } => (label.clone(), values_text(values)),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn full_rows() -> BTreeMap<String, String> {
    [("Today", "0 tokens"), ("Yesterday", "8.1M tokens"), ("Last 30 Days", "2B tokens")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[tokio::test]
async fn a_personal_plan_shows_the_tokens_of_each_utc_range_and_no_dollars() {
    let mut fixture = Fixture::new("pro", WORKSPACE_BREAKDOWN).await;
    let snapshot = fixture.refresh(now(), true).await;

    assert_eq!(rows(&snapshot), full_rows());
    let requests = fixture.report_requests().await;
    assert_eq!(requests.iter().map(|r| r.url.path().to_string()).collect::<Vec<_>>(), ["/backend-api/wham/analytics/daily-workspace-usage-counts"], "personal plans only need counts");
    assert_eq!(query(&requests[0]), "start_date=2026-10-02&end_date=2026-10-31&group_by=day&workspace_user=true");
    assert_eq!(requests[0].headers.get("chatgpt-account-id").unwrap(), "acct-fake");
    assert_eq!(requests[0].headers.get("authorization").unwrap(), "Bearer access");
    let labels: Vec<_> = snapshot.lines.iter().map(MetricLine::label).collect();
    assert_eq!(&labels[labels.len() - 3..], ["Today", "Yesterday", "Last 30 Days"], "at the end");
}

#[tokio::test]
async fn a_personal_plans_model_shares_use_all_reported_turns_even_when_only_some_are_attributed() {
    let mut fixture = Fixture::new("pro", WORKSPACE_BREAKDOWN).await;
    let snapshot = fixture.refresh(now(), true).await;

    let last30 = &snapshot.breakdowns["Last 30 Days"];
    assert_eq!(last30.totals.get(Measure::Credits), None);
    assert_eq!(last30.totals.get(Measure::Turns), Some(478.0));
    assert_eq!(last30.totals.get(Measure::Tokens), Some(2_014_940_056.0));
    assert_eq!(last30.token_parts().iter().map(|p| p.title).collect::<Vec<_>>(), ["Cache Read", "Input", "Output"]);
    assert_eq!(last30.models().iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["gpt-6.1-sol", "gpt-6-astra"]);
    let shares: Vec<_> = last30.models().iter().filter_map(|m| m.share).map(|s| ModelFigure::Share.text(s)).collect();
    assert_eq!(shares, ["12%", "6%"], "55 and 31 named turns out of the 478 reported across the sparse month");

    let mut day = Fixture::new("pro", WORKSPACE_BREAKDOWN).await;
    let oct29 = day.refresh(DateTime::from_timestamp(1_793_232_000, 0).unwrap(), true).await;
    let today = &oct29.breakdowns["Today"];
    let shares: Vec<_> = today.models().iter().filter_map(|m| m.share).map(|s| ModelFigure::Share.text(s)).collect();
    assert_eq!(shares, ["64%", "36%"]);
}

#[tokio::test]
async fn a_team_plan_shows_the_charged_credits_of_the_workspace_report_and_ranks_models_by_them() {
    let mut fixture = Fixture::new("team", WORKSPACE_BREAKDOWN).await;
    let snapshot = fixture.refresh(now(), true).await;

    assert_eq!(rows(&snapshot), full_rows());
    let breakdown = fixture.requests_for("daily-workspace-user-token-usage-breakdown").await;
    assert_eq!(query(&breakdown[0]), "start_date=2026-10-02&end_date=2026-10-31&group_by=day");
    let yesterday = &snapshot.breakdowns["Yesterday"];
    assert_eq!(yesterday.model_figures(), [ModelFigure::Share], "a workspace reports only credits per model");
    assert_eq!(yesterday.models().iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["gpt-6-astra", "gpt-6.1-sol"]);
    let shares: Vec<_> = yesterday.models().iter().filter_map(|m| m.share).map(|s| ModelFigure::Share.text(s)).collect();
    assert_eq!(shares, ["80%", "20%"]);
}

#[tokio::test]
async fn a_workspace_report_without_charged_credits_shows_no_credits() {
    for breakdown in [RELATIVE_BREAKDOWN, "{}"] {
        let mut fixture = Fixture::new("team", breakdown).await;
        let snapshot = fixture.refresh(now(), true).await;

        assert_eq!(rows(&snapshot), full_rows(), "{breakdown}");
        let yesterday = &snapshot.breakdowns["Yesterday"];
        assert!(yesterday.models().is_empty(), "{breakdown}");
        assert_eq!(yesterday.totals.get(Measure::Credits), None, "{breakdown}");
    }
}

#[tokio::test]
async fn business_education_and_enterprise_plans_read_their_charged_credits_by_model() {
    for plan in ["business", "edu", "enterprise"] {
        let mut fixture = Fixture::new(plan, WORKSPACE_BREAKDOWN).await;
        let snapshot = fixture.refresh(now(), true).await;

        let credits = fixture.requests_for("daily-workspace-user-credit-usage").await;
        assert_eq!(query(&credits[0]), "start_date=2026-10-02&end_date=2026-10-31&breakdown=model", "{plan}");
        assert_eq!(rows(&snapshot)["Yesterday"], "8.1M tokens", "{plan}");
        let names: Vec<_> = snapshot.breakdowns["Yesterday"].models().iter().map(|m| m.name.clone()).collect();
        assert_eq!(names, ["gpt-6-astra", "gpt-6.1-sol"], "{plan}");
    }
}

#[tokio::test]
async fn a_day_the_counts_do_not_report_yet_reads_zero_tokens_with_nothing_to_show() {
    let mut fixture = Fixture::new("pro", WORKSPACE_BREAKDOWN).await;
    fixture.reports_today.store(false, Ordering::SeqCst);

    let snapshot = fixture.refresh(now(), true).await;
    assert_eq!(rows(&snapshot), full_rows(), "a day with no use still shows Today, and the range sums the days that report");
    assert!(snapshot.breakdowns["Today"].is_empty());

    fixture.reports_today.store(true, Ordering::SeqCst);
    let later = fixture.refresh(now() + chrono::Duration::minutes(61), true).await;
    assert_eq!(rows(&later)["Today"], "0 tokens");
}

#[tokio::test]
async fn history_is_read_once_an_hour_at_once_on_a_new_utc_day_and_on_a_forced_reread() {
    let mut fixture = Fixture::new("pro", WORKSPACE_BREAKDOWN).await;
    fixture.refresh(now(), false).await;
    assert_eq!(fixture.report_requests().await.len(), 1);

    let soon = fixture.refresh(now() + chrono::Duration::minutes(30), false).await;
    assert_eq!(fixture.report_requests().await.len(), 1, "an hour has not passed");
    assert_eq!(rows(&soon)["Yesterday"], "8.1M tokens", "the rows keep the last read");

    fixture.refresh(now() + chrono::Duration::minutes(61), false).await;
    assert_eq!(fixture.report_requests().await.len(), 2);

    fixture.refresh(now() + chrono::Duration::minutes(62), true).await;
    assert_eq!(fixture.report_requests().await.len(), 3, "a forced reread reads at once");

    fixture.refresh(DateTime::from_timestamp(1_793_491_500, 0).unwrap(), false).await;
    let requests = fixture.report_requests().await;
    assert_eq!(requests.len(), 4, "a new UTC day is read on the first refresh after midnight");
    assert!(query(&requests[3]).starts_with("start_date=2026-10-31&end_date=2026-11-01"), "today, and yesterday, which was read before it ended");
}

#[tokio::test]
async fn failed_counts_keep_the_card_and_sum_only_previously_read_days() {
    let mut fixture = Fixture::new("pro", WORKSPACE_BREAKDOWN).await;
    fixture.reports.store(500, Ordering::SeqCst);

    let failed = fixture.refresh(now(), true).await;
    let zero: BTreeMap<String, String> = ["Today", "Yesterday", "Last 30 Days"].into_iter().map(|k| (k.to_string(), "0 tokens".to_string())).collect();
    assert_eq!(rows(&failed), zero);

    fixture.reports.store(0, Ordering::SeqCst);
    fixture.refresh(now() + chrono::Duration::minutes(10), false).await;
    assert_eq!(fixture.report_requests().await.len(), 1, "a failed read waits an hour");

    let read = fixture.refresh(now() + chrono::Duration::minutes(61), false).await;
    assert_eq!(read.breakdowns["Last 30 Days"].totals.get(Measure::Tokens), Some(2_014_940_056.0));

    fixture.reports.store(503, Ordering::SeqCst);
    let partial = fixture.refresh(now() + chrono::Duration::minutes(122), false).await;
    assert_eq!(rows(&partial)["Today"], "0 tokens");
    assert_eq!(rows(&partial)["Yesterday"], "0 tokens");
    assert_eq!(partial.breakdowns["Last 30 Days"].totals.get(Measure::Tokens), Some(2_006_800_581.0), "Oct 2 and Oct 29 remain after the reread of today and yesterday fails");
}

#[tokio::test]
async fn missing_workspace_credits_do_not_hide_or_block_token_counts() {
    let mut fixture = Fixture::new("team", WORKSPACE_BREAKDOWN).await;
    fixture.credits.store(429, Ordering::SeqCst);
    let first = fixture.refresh(now(), true).await;
    assert!(first.breakdowns.contains_key("Last 30 Days"));

    fixture.credits.store(0, Ordering::SeqCst);
    fixture.refresh(now() + chrono::Duration::minutes(30), true).await;
    assert_eq!(fixture.requests_for("usage-counts").await.len(), 2);
    assert_eq!(fixture.requests_for("token-usage-breakdown").await.len(), 1, "the credits ledger still waits out the rate limit");

    let full = fixture.refresh(now() + chrono::Duration::minutes(61), false).await;
    let full = full.breakdowns["Last 30 Days"].clone();
    assert_eq!(full.totals.get(Measure::Credits), Some(15.5));

    fixture.credits.store(503, Ordering::SeqCst);
    let partial = fixture.refresh(now() + chrono::Duration::minutes(122), false).await;
    assert_eq!(rows(&partial), full_rows());
    let partial = &partial.breakdowns["Last 30 Days"];
    assert_eq!(partial.totals.get(Measure::Tokens), full.totals.get(Measure::Tokens));
    assert_eq!(partial.totals.get(Measure::Credits), Some(3.0), "only Oct 29's charge remains after the credit reread fails");
}

#[test]
fn a_malformed_day_rejects_the_whole_report() {
    for body in [
        r#"{"data":{}}"#,
        r#"{"data":[{"date":"2026-10-30"}]}"#,
        r#"{"data":[{"date":"2026-10-30","totals":{"turns":-1}}]}"#,
        r#"{"data":[{"date":"2026-10-30","totals":{"turns":3,"text_total_tokens":"lots"}}]}"#,
        r#"{"data":[{"totals":{"turns":3}}]}"#,
    ] {
        assert!(counts(body.as_bytes()).is_none(), "{body}");
    }
    assert!(workspace_credits(br#"{"data":[{"date":"2026-10-30","premium_usage_values":{"credit_usage_credits":{"cli":"lots"}}}]}"#).is_none());
    assert!(enterprise_credits(br#"{"data":[{"date":"2026-10-30","values":{"a":"lots"}}]}"#).is_none());
}

#[test]
fn records_for_one_date_add_up_and_a_date_the_counts_leave_out_has_no_turns() {
    let report = counts(
        br#"{"data":[{"date":"2026-10-30","totals":{"turns":10,"text_total_tokens":100}},
                    {"date":"2026-10-30","totals":{"turns":10,"text_total_tokens":100}},
                    {"date":"2026-10-29","totals":{"turns":3,"text_total_tokens":null}}]}"#,
    )
    .unwrap();
    assert_eq!(report.days["2026-10-30"].totals.values, BTreeMap::from([(Measure::Turns, 20.0), (Measure::Tokens, 200.0)]));
    assert_eq!(report.days["2026-10-29"].totals.values, BTreeMap::from([(Measure::Turns, 3.0)]));
    assert!(report.quiet_day.totals.values.is_empty());
}

#[test]
fn a_model_without_charged_credits_leaves_the_other_models_and_day_total_intact() {
    let report = workspace_credits(
        br#"{"data":[{"date":"2026-10-30","premium_usage_values":{"credit_usage_credits":{"cli":2.5,"desktop_app":10}},
                      "models":[{"model":"a","credits":9,"on_demand_credits":2},{"model":"b","credits":9}]}]}"#,
    )
    .unwrap();
    let day = &report.days["2026-10-30"];
    assert_eq!(day.totals.values, BTreeMap::from([(Measure::Credits, 12.5)]));
    assert_eq!(day.by_model, Some(BTreeMap::from([("a".to_string(), crate::history::Totals::from([(Measure::Credits, 2.0)]))])));
}

#[test]
fn workspace_plans_pick_their_credits_report_and_personal_plans_have_none() {
    assert_eq!(Report::workspace_credits(Some("Team")), Some(Report::TeamCredits));
    assert_eq!(Report::workspace_credits(Some("edu_pro")), Some(Report::EnterpriseCredits));
    assert_eq!(Report::workspace_credits(Some("pro")), None);
    assert_eq!(Report::workspace_credits(None), None);
    let counted = History::new(Calendar::Utc, now(), BTreeMap::new());
    assert!(combining(counted, None).days.is_empty());
}
