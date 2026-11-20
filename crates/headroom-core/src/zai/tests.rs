use chrono::{DateTime, Utc};
use wiremock::matchers::{bearer_token, body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::mapper::*;
use super::resets::*;
use super::*;
use crate::models::MetricLine;

const LIVE_QUOTA: &str = r#"{"code":200,"msg":"Operation successful","data":{"limits":[
  {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":17,"nextResetTime":1782724971179},
  {"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":3,"nextResetTime":1783305486997},
  {"type":"TIME_LIMIT","unit":5,"number":1,"usage":1000,"currentValue":0,"remaining":1000,"percentage":0,"nextResetTime":1785292686976}
],"level":"pro"},"success":true}"#;
/// Captured from a GLM Coding Lite plan: the percentage quota type is CREDIT_LIMIT, with no reset for the 5-hour window.
const CREDIT_QUOTA: &str = r#"{"code":200,"msg":"Operation successful","data":{"limits":[
  {"type":"CREDIT_LIMIT","unit":3,"number":5,"usage":2000,"currentValue":0,"remaining":2000,"percentage":0},
  {"type":"CREDIT_LIMIT","unit":6,"number":1,"usage":10000,"currentValue":9855,"remaining":145,"percentage":98,"nextResetTime":1786685679998}
],"level":"lite"},"success":true}"#;
const SUBSCRIPTION: &str = r#"{"code":200,"data":[{"productName":"GLM Coding Pro","status":"VALID"}],"success":true}"#;
/// Session record 11 and Weekly record 21 share an expiry; 13 is spent and 14 has expired.
const LIST: &str = r#"{"code":200,"success":true,"data":{
  "fiveHourResets":[
    {"recordId":12,"available":true,"expireTime":"2026-11-10 08:00:00","grantType":"GLOBAL"},
    {"recordId":11,"available":true,"expireTime":"2026-11-06 20:00:00","grantType":"GLOBAL"},
    {"recordId":13,"available":false,"expireTime":"2026-11-07 08:00:00","grantType":"GLOBAL"},
    {"recordId":14,"available":true,"expireTime":"2026-11-03 08:00:00","grantType":"GLOBAL"}],
  "weekResets":[{"recordId":21,"available":true,"expireTime":"2026-11-06 20:00:00","grantType":"GLOBAL"}]}}"#;
const QUOTA: &str = r#"{"code":200,"data":{"limits":[
  {"type":"CREDIT_LIMIT","unit":3,"number":5,"percentage":64,"nextResetTime":1793760000000},
  {"type":"CREDIT_LIMIT","unit":6,"number":1,"percentage":38,"nextResetTime":1794100000000}]},"success":true}"#;

/// 2026-11-04 00:00 UTC.
fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_793_750_400, 0).unwrap()
}

fn utc(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).unwrap().into()
}

fn reset(window: ResetWindow, record_id: i64, expires: &str) -> Reset {
    Reset { window, record_id, expires_at: utc(expires), grant_type: "GLOBAL".into() }
}

fn session11() -> Reset {
    reset(ResetWindow::FiveHour, 11, "2026-11-06T12:00:00Z")
}
fn weekly21() -> Reset {
    reset(ResetWindow::Week, 21, "2026-11-06T12:00:00Z")
}
fn session12() -> Reset {
    reset(ResetWindow::FiveHour, 12, "2026-11-10T00:00:00Z")
}

fn progress(lines: &[MetricLine], label: &str) -> (f64, f64, Option<i64>) {
    match lines.iter().find(|l| l.label() == label) {
        Some(MetricLine::Progress { used, limit, period_ms, .. }) => (*used, *limit, *period_ms),
        other => panic!("no {label} progress line: {other:?}"),
    }
}

#[test]
fn live_response_maps_to_session_weekly_and_web_searches() {
    let lines = map_quota(LIVE_QUOTA.as_bytes()).unwrap();
    assert_eq!(plan_name(SUBSCRIPTION.as_bytes()).as_deref(), Some("GLM Coding Pro"));
    assert_eq!(progress(&lines, "Session"), (17.0, 100.0, Some(5 * 3600 * 1000)));
    assert_eq!(progress(&lines, "Weekly"), (3.0, 100.0, Some(7 * 24 * 3600 * 1000)));
    assert_eq!(progress(&lines, "Web Searches").0, 0.0);
    assert_eq!(progress(&lines, "Web Searches").1, 1000.0);
}

#[test]
fn the_current_credit_limit_response_maps_to_session_and_weekly() {
    let lines = map_quota(CREDIT_QUOTA.as_bytes()).unwrap();
    assert_eq!(progress(&lines, "Session"), (0.0, 100.0, Some(5 * 3600 * 1000)));
    assert_eq!(progress(&lines, "Weekly").0, 98.0);
}

#[test]
fn missing_required_values_never_become_zero_usage() {
    for body in [
        r#"{"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5}]}}"#,
        r#"{"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":true}]}}"#,
        r#"{"data":{"limits":[{"type":"TIME_LIMIT","usage":1000}]}}"#,
        r#"{"data":{"limits":[{"type":"TIME_LIMIT","currentValue":10}]}}"#,
        r#"{"data":{"limits":[{"type":"TIME_LIMIT","currentValue":-1,"usage":1000}]}}"#,
    ] {
        assert_eq!(map_quota(body.as_bytes()).unwrap_err(), ProviderError::InvalidResponse, "{body}");
    }
}

#[test]
fn a_malformed_envelope_is_rejected_but_explicit_empty_limits_are_valid() {
    for body in ["not-json", r#"{"data":[]}"#, r#"{"data":{}}"#, r#"{"data":{"limits":{}}}"#] {
        assert_eq!(map_quota(body.as_bytes()).unwrap_err(), ProviderError::InvalidResponse, "{body}");
    }
    assert!(map_quota(br#"{"data":{"limits":[]}}"#).unwrap().is_empty());
}

#[test]
fn a_key_with_no_coding_plan_says_so_and_any_other_failed_body_stays_invalid() {
    let no_plan = r#"{"code":500,"msg":"当前用户不存在coding plan","success":false}"#;
    assert_eq!(map_quota(no_plan.as_bytes()).unwrap_err(), ProviderError::Unavailable("No active GLM Coding Plan. Subscribe at z.ai/subscribe to see usage.".into()));
    assert_eq!(map_quota(br#"{"code":500,"msg":"internal error","success":false}"#).unwrap_err(), ProviderError::InvalidResponse);
}

#[test]
fn a_key_zai_refuses_inside_an_ok_response_is_rejected() {
    let refused = br#"{"code":401,"msg":"token expired or incorrect","success":false}"#;
    assert_eq!(map_quota(refused).unwrap_err(), ProviderError::SignInExpired);
}

#[test]
fn unknown_limits_show_nothing_and_do_not_hide_known_meters() {
    let unknown = r#"{"type":"FUTURE_LIMIT"},{"type":"TOKENS_LIMIT","unit":99,"number":1,"percentage":70}"#;
    assert!(map_quota(format!(r#"{{"data":{{"limits":[{unknown}]}}}}"#).as_bytes()).unwrap().is_empty());
    let known = r#"{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":25}"#;
    let lines = map_quota(format!(r#"{{"data":{{"limits":[{unknown},{known}]}}}}"#).as_bytes()).unwrap();
    assert_eq!(lines.iter().map(MetricLine::label).collect::<Vec<_>>(), ["Session"]);
}

#[test]
fn the_list_keeps_available_unexpired_resets_earliest_first_in_utc_plus_8() {
    assert_eq!(parse_resets(LIST.as_bytes(), now()).unwrap(), [session11(), weekly21(), session12()]);
}

#[test]
fn the_list_rejects_an_unreadable_record_or_a_failed_answer() {
    for body in [
        r#"{"success":true,"data":{"fiveHourResets":[{"recordId":1,"available":true,"expireTime":"2026-11-06T20:00:00Z"}]}}"#,
        r#"{"success":true,"data":{"weekResets":[{"available":true,"expireTime":"2026-11-06 20:00:00"}]}}"#,
        r#"{"success":true,"data":{"weekResets":[{"recordId":1,"available":true,"expireTime":"2026-11-06 20:00:00"}]}}"#,
        r#"{"success":true,"data":{"weekResets":[{"recordId":1,"available":true,"expireTime":"2026-11-06 20:00:00","grantType":""}]}}"#,
        r#"{"success":false,"msg":"error","data":null}"#,
    ] {
        assert!(parse_resets(body.as_bytes(), now()).is_err(), "{body}");
    }
}

#[test]
fn only_success_true_counts_as_spent() {
    assert_eq!(claim_outcome(200, false, br#"{"success":true}"#), ClaimOutcome::Success);
    assert_eq!(claim_outcome(200, false, br#"{"success":false,"msg":"no"}"#), ClaimOutcome::Failed(ClaimFailure::Refused));
    assert_eq!(claim_outcome(500, false, br#"{"success":true}"#), ClaimOutcome::Failed(ClaimFailure::Refused));
    assert_eq!(claim_outcome(401, true, b"{}"), ClaimOutcome::Failed(ClaimFailure::SignInExpired));
}

fn client(server: &MockServer) -> ZaiClient {
    ZaiClient::new(&server.uri())
}

fn ok(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

async fn mount_reads(server: &MockServer, list: ResponseTemplate) {
    Mock::given(method("GET")).and(path("/api/monitor/usage/quota/limit")).and(bearer_token("key")).and(header("accept", "application/json")).respond_with(ok(QUOTA)).mount(server).await;
    Mock::given(method("GET")).and(path("/api/biz/subscription/list")).respond_with(ok(SUBSCRIPTION)).mount(server).await;
    Mock::given(method("GET")).and(path("/api/biz/customer-package-reset/list")).and(query_param("targetType", "PERSONAL")).respond_with(list).mount(server).await;
}

#[tokio::test]
async fn a_refresh_adds_the_resets_row_with_their_expiries_and_caches_the_secondary_reads() {
    let server = MockServer::start().await;
    mount_reads(&server, ok(LIST)).await;
    let fetched = client(&server).fetch("key", None, true, now()).await.unwrap();
    assert_eq!(fetched.snapshot.plan.as_deref(), Some("GLM Coding Pro"));
    assert_eq!(fetched.snapshot.lines.iter().map(MetricLine::label).collect::<Vec<_>>(), ["Session", "Weekly", "Rate Limit Resets"]);
    assert_eq!(fetched.snapshot.lines.last(), Some(&resets::line(&[session11(), weekly21(), session12()])));
    assert_eq!(fetched.snapshot.reset_expiries, [utc("2026-11-06T12:00:00Z"), utc("2026-11-06T12:00:00Z"), utc("2026-11-10T00:00:00Z")]);
    assert_eq!(ZaiCache::decode(Some(&fetched.cache.encode())), Some(fetched.cache));
}

#[test]
fn only_the_earliest_reset_of_each_window_offers_use() {
    let items = resets::items(&[session12(), weekly21(), session11()]);
    let view: Vec<_> = items.iter().map(|i| (i.claim.clone(), i.usable, i.kind)).collect();
    use crate::claude::resets::ResetKind::{Session, Weekly};
    use crate::claim::ResetRef::Zai;
    assert_eq!(view, [(Zai(session11()), true, Session), (Zai(weekly21()), true, Weekly), (Zai(session12()), false, Session)]);
    assert_eq!(items[0].confirm_detail.as_deref(), Some("Refills your session limit now. This can't be undone."));
}

#[tokio::test]
async fn between_hourly_reads_the_cache_is_used_and_expired_resets_drop_out() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/monitor/usage/quota/limit")).respond_with(ok(QUOTA)).mount(&server).await;
    let cache = ZaiCache { plan: Some("Kept".into()), resets: Some(vec![session11(), session12()]) };
    let later = utc("2026-11-07T00:00:00Z");
    let fetched = client(&server).fetch("key", Some(cache), false, later).await.unwrap();
    assert_eq!(fetched.snapshot.plan.as_deref(), Some("Kept"));
    assert_eq!(fetched.snapshot.lines.last(), Some(&resets::line(&[session12()])));
    assert_eq!(server.received_requests().await.unwrap().len(), 1, "no secondary read");
}

#[tokio::test]
async fn a_failed_reset_list_leaves_the_meters_working() {
    for failure in [ResponseTemplate::new(500).set_body_string("{}"), ok("not json")] {
        let server = MockServer::start().await;
        mount_reads(&server, failure).await;
        let fetched = client(&server).fetch("key", None, true, now()).await.unwrap();
        assert_eq!(fetched.snapshot.lines.iter().map(MetricLine::label).collect::<Vec<_>>(), ["Session", "Weekly"]);
        assert_eq!(fetched.cache.resets, None);
    }
}

#[tokio::test]
async fn http_failures_and_a_refused_key_map_to_key_rejected() {
    let rejected = || ProviderError::KeyRejected("Z.ai rejected the API key.".into());
    for (status, expected) in [(401, rejected()), (403, rejected()), (429, ProviderError::RateLimited { retry_after_secs: None }), (500, ProviderError::RequestFailed(500))] {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(status)).mount(&server).await;
        assert_eq!(client(&server).fetch("key", None, true, now()).await.unwrap_err(), expected, "{status}");
    }
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ok(r#"{"code":401,"msg":"token expired","success":false}"#)).mount(&server).await;
    assert_eq!(client(&server).fetch("key", None, true, now()).await.unwrap_err(), rejected());
}

#[tokio::test]
async fn a_claim_names_the_exact_record_and_window_and_spends_it_once() {
    let server = MockServer::start().await;
    mount_reads(&server, ok(LIST)).await;
    Mock::given(method("POST")).and(path("/api/biz/customer-package-reset/use")).and(bearer_token("key-1")).and(header("content-type", "application/json"))
        .and(body_json(serde_json::json!({"targetType": "PERSONAL", "resetType": "WEEK", "recordId": 21, "requestId": "request-1", "grantType": "GLOBAL"})))
        .respond_with(ok(r#"{"code":200,"success":true}"#)).expect(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/api/biz/customer-package-reset/list")).and(bearer_token("key-1")).respond_with(ok(LIST)).mount(&server).await;
    let outcome = client(&server).claim_reset("key-1", &weekly21(), "request-1", now()).await.unwrap();
    assert_eq!(outcome, ClaimOutcome::Success);
}

#[tokio::test]
async fn a_stale_record_is_never_spent_in_place_of_another() {
    let stale = [
        reset(ResetWindow::FiveHour, 99, "2026-11-06T12:00:00Z"),
        session12(),
        reset(ResetWindow::FiveHour, 11, "2026-11-06T12:01:00Z"),
    ];
    for confirmed in stale {
        let server = MockServer::start().await;
        mount_reads(&server, ok(LIST)).await;
        Mock::given(method("POST")).and(path("/api/biz/customer-package-reset/use")).respond_with(ok(r#"{"success":true}"#)).expect(0).mount(&server).await;
        let outcome = client(&server).claim_reset("key", &confirmed, "request-1", now()).await.unwrap();
        assert_eq!(outcome, ClaimOutcome::NoCredit, "{confirmed:?}");
    }
}

#[tokio::test]
async fn claim_failures_map_to_outcomes() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/biz/customer-package-reset/list")).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    assert_eq!(client(&server).claim_reset("key", &session11(), "r", now()).await.unwrap(), ClaimOutcome::Failed(ClaimFailure::SignInExpired));
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/biz/customer-package-reset/list")).respond_with(ok(LIST)).mount(&server).await;
    Mock::given(method("POST")).and(path("/api/biz/customer-package-reset/use")).respond_with(ok(r#"{"success":false,"msg":"no"}"#)).mount(&server).await;
    assert_eq!(client(&server).claim_reset("key", &session11(), "r", now()).await.unwrap(), ClaimOutcome::Failed(ClaimFailure::Refused));
}

#[test]
fn the_list_is_due_hourly() {
    assert!(list_due(None, now()));
    assert!(!list_due(Some(now() - chrono::Duration::minutes(59)), now()));
    assert!(list_due(Some(now() - chrono::Duration::minutes(60)), now()));
}
