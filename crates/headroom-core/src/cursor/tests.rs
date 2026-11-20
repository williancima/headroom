use std::collections::HashMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::models::MetricLine;

const EXPIRY: i64 = 4_000_000_000;
const DASH: &str = "/aiserver.v1.DashboardService";

fn expiry() -> DateTime<Utc> {
    DateTime::from_timestamp(EXPIRY, 0).unwrap()
}

fn jwt(user: &str, exp: i64) -> String {
    format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(json!({"sub": format!("auth0|user_{user}"), "exp": exp}).to_string()))
}

fn tokens(access: &str, expires_at: Option<DateTime<Utc>>) -> OAuthTokens {
    OAuthTokens {
        access_token: access.into(),
        refresh_token: Some(access.into()),
        expires_at,
        extras: HashMap::from([("keep".into(), "me".into())]),
    }
}

fn client(server: &MockServer) -> CursorClient {
    let base = server.uri();
    CursorClient::new(CursorConfig {
        login_url: format!("{base}/loginDeepControl"),
        poll_url: format!("{base}/auth/poll"),
        poll_interval: std::time::Duration::from_millis(10), // the poller floors it at one second
        poll_expiry: std::time::Duration::from_secs(10),
        token_url: format!("{base}/oauth/token"),
        api_base: base.clone(),
        web_base: base,
    })
}

fn json_ok(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

async fn post(server: &MockServer, name: &str, response: ResponseTemplate) {
    Mock::given(method("POST")).and(path(format!("{DASH}/{name}"))).respond_with(response).mount(server).await;
}

async fn get(server: &MockServer, p: &str, response: ResponseTemplate) {
    Mock::given(method("GET")).and(path(p)).respond_with(response).mount(server).await;
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

async fn usage_for(server: &MockServer, token: &str, response: ResponseTemplate) {
    Mock::given(method("POST")).and(path(format!("{DASH}/GetCurrentPeriodUsage"))).and(header("authorization", bearer(token).as_str())).respond_with(response).mount(server).await;
}

fn requests_to(server_requests: &[wiremock::Request], p: &str) -> Vec<String> {
    server_requests
        .iter()
        .filter(|r| r.url.path() == p)
        .map(|r| r.headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").replace("Bearer ", ""))
        .collect()
}

const USAGE: &str = r#"{"billingCycleEnd":"1764547200000","billingCycleStart":"1761955200000","enabled":true,
 "planUsage":{"limit":2000,"remaining":1500,"totalPercentUsed":25}}"#;
const PLAN_PRO: &str = r#"{"planInfo":{"planName":"Pro"}}"#;

fn refresh_ok(new: &str) -> ResponseTemplate {
    json_ok(&json!({"access_token": new, "id_token": "fake", "shouldLogout": false}).to_string())
}

async fn refresh_mock(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST")).and(path("/oauth/token")).respond_with(response).mount(server).await;
}

fn total_used(fetch: &CursorFetch) -> f64 {
    match fetch.snapshot.line("Total usage") {
        Some(MetricLine::Progress { used, .. }) => *used,
        other => panic!("{other:?}"),
    }
}

#[test]
fn session_is_read_from_the_subject() {
    let token = jwt("abc", EXPIRY);
    assert_eq!(session(&token), Some(("user_abc".into(), format!("user_abc%3A%3A{token}"))));
    let bare = format!("e30.{}.s", URL_SAFE_NO_PAD.encode(r#"{"sub":" plain "}"#));
    assert_eq!(session(&bare).unwrap().0, "plain");
    assert_eq!(session("nope"), None);
}

#[tokio::test]
async fn a_token_within_a_week_of_expiry_is_refreshed_first_and_its_access_token_doubles_as_refresh_token() {
    let (old, new) = (jwt("old", EXPIRY), jwt("new", EXPIRY + 60 * 86_400));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({"grant_type": "refresh_token", "client_id": CLIENT_ID, "refresh_token": old})))
        .respond_with(refresh_ok(&new))
        .expect(1)
        .mount(&server)
        .await;
    usage_for(&server, &new, json_ok(USAGE)).await;
    post(&server, "GetPlanInfo", json_ok(PLAN_PRO)).await;
    let mut t = tokens(&old, Some(expiry()));
    let now = expiry() - Duration::days(1);
    let fetch = client(&server).fetch(&mut t, now).await.unwrap();
    assert_eq!(total_used(&fetch), 25.0);
    assert_eq!(fetch.snapshot.plan.as_deref(), Some("Pro"));
    let want = OAuthTokens {
        access_token: new.clone(),
        refresh_token: Some(new),
        expires_at: DateTime::from_timestamp(EXPIRY + 60 * 86_400, 0),
        extras: HashMap::from([("keep".into(), "me".into())]),
    };
    assert_eq!(t, want);
    let seen = server.received_requests().await.unwrap();
    assert_eq!(requests_to(&seen, &format!("{DASH}/GetCurrentPeriodUsage")), [want.access_token]);
}

#[tokio::test]
async fn a_token_outside_the_lead_is_not_refreshed_and_gets_its_expiry_from_the_jwt() {
    let old = jwt("old", EXPIRY);
    let server = MockServer::start().await;
    refresh_mock(&server, refresh_ok("never").set_delay(std::time::Duration::ZERO)).await;
    usage_for(&server, &old, json_ok(USAGE)).await;
    let mut t = tokens(&old, None);
    client(&server).fetch(&mut t, expiry() - Duration::days(30)).await.unwrap();
    assert_eq!((t.access_token.as_str(), t.expires_at), (old.as_str(), Some(expiry())));
    let seen = server.received_requests().await.unwrap();
    assert!(seen.iter().all(|r| r.url.path() != "/oauth/token"));
}

#[tokio::test]
async fn a_rejected_token_is_refreshed_once_and_retried() {
    let (old, new) = (jwt("old", EXPIRY), jwt("new", EXPIRY + 60 * 86_400));
    let server = MockServer::start().await;
    refresh_mock(&server, refresh_ok(&new)).await;
    usage_for(&server, &old, ResponseTemplate::new(401)).await;
    usage_for(&server, &new, json_ok(USAGE)).await;
    let mut t = tokens(&old, Some(expiry()));
    client(&server).fetch(&mut t, expiry() - Duration::days(30)).await.unwrap();
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref()), (new.as_str(), Some(new.as_str())));
    let seen = server.received_requests().await.unwrap();
    assert_eq!(requests_to(&seen, &format!("{DASH}/GetCurrentPeriodUsage")), [old, new]);
}

#[tokio::test]
async fn a_renewed_token_that_is_rejected_too_reads_as_an_expired_sign_in_and_keeps_the_renewal() {
    let (old, new) = (jwt("old", EXPIRY), jwt("new", EXPIRY + 60 * 86_400));
    let server = MockServer::start().await;
    refresh_mock(&server, refresh_ok(&new)).await;
    usage_for(&server, &old, ResponseTemplate::new(401)).await;
    usage_for(&server, &new, ResponseTemplate::new(401)).await;
    let mut t = tokens(&old, Some(expiry()));
    let err = client(&server).fetch(&mut t, expiry() - Duration::days(30)).await.unwrap_err();
    assert_eq!((err, t.access_token.as_str()), (ProviderError::SignInExpired, new.as_str()));
}

#[tokio::test]
async fn a_renewal_survives_a_failing_read_after_it() {
    let (old, new) = (jwt("old", EXPIRY), jwt("new", EXPIRY + 60 * 86_400));
    let server = MockServer::start().await;
    refresh_mock(&server, refresh_ok(&new)).await;
    usage_for(&server, &new, ResponseTemplate::new(500)).await;
    let mut t = tokens(&old, Some(expiry()));
    let err = client(&server).fetch(&mut t, expiry() - Duration::days(1)).await.unwrap_err();
    assert_eq!(err, ProviderError::RequestFailed(500));
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref()), (new.as_str(), Some(new.as_str())));
}

#[tokio::test]
async fn should_logout_on_an_expired_token_is_an_expired_sign_in_and_leaves_the_tokens_alone() {
    let old = jwt("old", EXPIRY);
    let server = MockServer::start().await;
    refresh_mock(&server, json_ok(r#"{"access_token":"","id_token":"","shouldLogout":true}"#)).await;
    let mut t = tokens(&old, None);
    let err = client(&server).fetch(&mut t, expiry() + Duration::hours(1)).await.unwrap_err();
    assert_eq!((err, t), (ProviderError::SignInExpired, tokens(&old, Some(expiry()))));
    let seen = server.received_requests().await.unwrap();
    assert_eq!(seen.len(), 1, "the usage read is never attempted");
}

#[tokio::test]
async fn a_failed_early_refresh_keeps_the_still_valid_token() {
    let old = jwt("old", EXPIRY);
    let server = MockServer::start().await;
    refresh_mock(&server, ResponseTemplate::new(503)).await;
    usage_for(&server, &old, json_ok(USAGE)).await;
    let mut t = tokens(&old, Some(expiry()));
    client(&server).fetch(&mut t, expiry() - Duration::days(1)).await.unwrap();
    assert_eq!(t, tokens(&old, Some(expiry())));
    let seen = server.received_requests().await.unwrap();
    assert_eq!(seen.iter().filter(|r| r.url.path() == "/oauth/token").count(), 1);
}

#[tokio::test]
async fn a_refused_refresh_and_a_missing_refresh_token_are_expired_sign_ins() {
    let old = jwt("old", EXPIRY);
    let server = MockServer::start().await;
    refresh_mock(&server, ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid_grant"}"#)).await;
    usage_for(&server, &old, ResponseTemplate::new(401)).await;
    let mut t = tokens(&old, Some(expiry()));
    let err = client(&server).fetch(&mut t, expiry() - Duration::days(30)).await.unwrap_err();
    assert_eq!(err, ProviderError::SignInExpired);
    t.refresh_token = None;
    let err = client(&server).fetch(&mut t, expiry() - Duration::days(30)).await.unwrap_err();
    assert_eq!(err, ProviderError::SignInExpired);
}

#[tokio::test]
async fn errors_follow_the_status() {
    let old = jwt("old", EXPIRY);
    let cases = [
        (ResponseTemplate::new(429).insert_header("retry-after", "7"), ProviderError::RateLimited { retry_after_secs: Some(7) }),
        (ResponseTemplate::new(500), ProviderError::RequestFailed(500)),
        (json_ok("[]"), ProviderError::InvalidResponse),
    ];
    for (response, expected) in cases {
        let server = MockServer::start().await;
        usage_for(&server, &old, response).await;
        let err = client(&server).fetch(&mut tokens(&old, Some(expiry())), expiry() - Duration::days(30)).await.unwrap_err();
        assert_eq!(err, expected);
    }
}

fn individual_usage() -> &'static str {
    r#"{"billingCycleEnd":"1764547200000","billingCycleStart":"1761955200000","enabled":true,
 "planUsage":{"apiPercentUsed":40,"autoPercentUsed":20,"limit":40000,"remaining":30000,"totalPercentUsed":25},
 "spendLimitUsage":{"individualLimit":50000,"individualRemaining":38000,"limitType":"user"}}"#
}

#[tokio::test]
async fn an_individual_seat_shows_credits_pools_extra_usage_and_grok_bot_with_the_expected_headers() {
    let token = jwt("fake", EXPIRY);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{DASH}/GetCurrentPeriodUsage")))
        .and(header("authorization", bearer(&token).as_str()))
        .and(header("connect-protocol-version", "1"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({})))
        .respond_with(json_ok(individual_usage()))
        .expect(1)
        .mount(&server)
        .await;
    post(&server, "GetPlanInfo", json_ok(r#"{"planInfo":{"planName":"Ultra"}}"#)).await;
    post(&server, "GetCreditGrantsBalance", json_ok(r#"{"hasCreditGrants":true,"totalCents":10000,"usedCents":4000}"#)).await;
    post(&server, "GetSandUsageStatus", json_ok(r#"{"currentPeriodStart":"2026-10-20T00:00:00Z","hasNonZeroIncludedLimit":true,"nextResetTimestampUtc":"2026-10-27T00:00:00Z","usagePercent":30}"#)).await;
    let cookie = format!("WorkosCursorSessionToken=user_fake%3A%3A{token}");
    Mock::given(method("GET")).and(path("/api/auth/stripe")).and(header("cookie", cookie.as_str())).respond_with(json_ok(r#"{"customerBalance":-2500}"#)).expect(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/api/usage-summary")).and(header("cookie", cookie.as_str())).respond_with(json_ok(r#"{"membershipType":"ultra"}"#)).expect(1).mount(&server).await;

    let fetch = client(&server).fetch(&mut tokens(&token, Some(expiry())), expiry() - Duration::days(30)).await.unwrap();
    let labels: Vec<&str> = fetch.snapshot.lines.iter().map(MetricLine::label).collect();
    assert_eq!(labels, ["Credits", "Total usage", "Cursor Models", "Other Models", "On-demand", "Grok Bot usage"]);
    assert_eq!(fetch.snapshot.plan.as_deref(), Some("Ultra"));
    let Some(MetricLine::Values { values, .. }) = fetch.snapshot.line("Credits") else { panic!() };
    assert_eq!(values[0].number, 85.0, "grants $100 - $40 used + $25 prepaid");
    let Some(MetricLine::Progress { used, resets_at, .. }) = fetch.snapshot.line("Grok Bot usage") else { panic!() };
    assert_eq!((*used, resets_at.unwrap().timestamp()), (30.0, 1_793_059_200));
    assert!(fetch.line_titles.is_empty());
}

#[tokio::test]
async fn a_request_based_enterprise_seat_reads_requests_and_extra_usage_from_the_rest_endpoints() {
    let token = jwt("fake", EXPIRY);
    for plan in ["Enterprise", "Pro"] {
        let server = MockServer::start().await;
        usage_for(&server, &token, json_ok(r#"{"billingCycleEnd":"1764547200000","billingCycleStart":"1761955200000","enabled":true,"planUsage":{}}"#)).await;
        post(&server, "GetPlanInfo", json_ok(&json!({"planInfo": {"planName": plan}}).to_string())).await;
        post(&server, "GetSandUsageStatus", json_ok(r#"{"hasNonZeroIncludedLimit":true,"usagePercent":1}"#)).await;
        get(&server, "/api/usage-summary", json_ok(r#"{"billingCycleStart":"2026-10-01T00:00:00Z","billingCycleEnd":"2026-11-01T00:00:00Z","membershipType":"enterprise","individualUsage":{"onDemand":{"enabled":true,"used":2500,"limit":10000}}}"#)).await;
        Mock::given(method("GET")).and(path("/api/usage")).and(query_param("user", "user_fake"))
            .respond_with(json_ok(r#"{"gpt-4":{"numRequests":120,"maxRequestUsage":500},"startOfMonth":"2026-10-01T00:00:00Z"}"#))
            .expect(1).mount(&server).await;
        let fetch = client(&server).fetch(&mut tokens(&token, Some(expiry())), expiry() - Duration::days(30)).await.unwrap();
        let labels: Vec<&str> = fetch.snapshot.lines.iter().map(MetricLine::label).collect();
        assert_eq!(labels, ["Total usage", "Requests", "On-demand", "Grok Bot usage"], "{plan}");
        assert_eq!(fetch.snapshot.plan.as_deref(), Some("Enterprise"), "GetPlanInfo says {plan}");
    }
}

#[tokio::test]
async fn a_team_membership_relabels_a_pro_seat_and_a_team_seat_takes_extra_usage_from_the_summary() {
    let token = jwt("fake", EXPIRY);
    let server = MockServer::start().await;
    usage_for(&server, &token, json_ok(r#"{"billingCycleEnd":"1764547200000","billingCycleStart":"1761955200000","enabled":true,
     "planUsage":{"apiPercentUsed":10,"autoPercentUsed":5,"limit":2000,"totalSpend":300,"totalPercentUsed":7},"spendLimitUsage":{"limitType":"team"}}"#)).await;
    post(&server, "GetPlanInfo", json_ok(r#"{"planInfo":{"planName":" Pro "}}"#)).await;
    get(&server, "/api/usage-summary", json_ok(r#"{"billingCycleStart":"2026-10-01T00:00:00Z","billingCycleEnd":"2026-11-01T00:00:00Z","membershipType":"team","individualUsage":{"onDemand":{"enabled":true,"used":1500,"limit":20000}}}"#)).await;
    let fetch = client(&server).fetch(&mut tokens(&token, Some(expiry())), expiry() - Duration::days(30)).await.unwrap();
    assert_eq!(fetch.snapshot.plan.as_deref(), Some("Team"));
    assert_eq!(total_used(&fetch), 7.0);
    let Some(MetricLine::Progress { used, limit, .. }) = fetch.snapshot.line("On-demand") else { panic!() };
    assert_eq!((*used, *limit), (15.0, 200.0));
}

fn unified_status(monthly: &str) -> String {
    format!(r#"{{"availability":"available","monthlyUsage":{{{monthly},"creditsUsedCents":0,"periodStart":"2026-11-01T00:00:00.000Z","resetsAt":"2026-12-01T00:00:00.000Z"}}}}"#)
}

async fn unified_server(token: &str, status: &str, complat: &str) -> MockServer {
    let server = MockServer::start().await;
    usage_for(&server, token, json_ok(r#"{"billingCycleEnd":"1795333076119","billingCycleStart":"1794728276119","enabled":true,
     "planUsage":{"autoPercentUsed":0.1083130246412131,"totalPercentUsed":0.1083130246412131},"spendLimitUsage":{"limitType":"user"}}"#)).await;
    post(&server, "GetPlanInfo", json_ok(r#"{"isMerged":true,"planInfo":{"planName":"Ultra"}}"#)).await;
    post(&server, "GetComplatCreditBalance", json_ok(complat)).await;
    get(&server, "/api/dashboard/v2/usage-status", json_ok(status)).await;
    get(&server, "/api/usage-summary", json_ok(r#"{"membershipType":"unified_ultra"}"#)).await;
    for name in ["GetCreditGrantsBalance", "GetSandUsageStatus"] {
        Mock::given(method("POST")).and(path(format!("{DASH}/{name}"))).respond_with(json_ok("{}")).expect(0).mount(&server).await;
    }
    Mock::given(method("GET")).and(path("/api/auth/stripe")).respond_with(json_ok("{}")).expect(0).mount(&server).await;
    server
}

#[tokio::test]
async fn a_unified_account_shows_one_weekly_pool_additional_usage_and_its_credit_balance() {
    let token = jwt("fake", EXPIRY);
    let status = unified_status(r#""additionalUsageOff":false,"limitCents":null,"usedCents":0"#);
    let server = unified_server(&token, &status, r#"{"usdRemainingCents":"581149"}"#).await;
    let fetch = client(&server).fetch(&mut tokens(&token, Some(expiry())), expiry() - Duration::days(30)).await.unwrap();
    let labels: Vec<&str> = fetch.snapshot.lines.iter().map(MetricLine::label).collect();
    assert_eq!(labels, ["Credits", "Total usage", "On-demand"]);
    assert_eq!(fetch.snapshot.plan.as_deref(), Some("Ultra"));
    let Some(MetricLine::Progress { used, resets_at, .. }) = fetch.snapshot.line("Total usage") else { panic!() };
    assert!((used - 0.1083).abs() < 1e-4);
    assert_eq!(resets_at.unwrap().timestamp_millis(), 1_795_333_076_119);
    let Some(MetricLine::Values { values, .. }) = fetch.snapshot.line("Credits") else { panic!() };
    assert_eq!(values[0].number, 5811.49);
    let Some(MetricLine::Values { values, .. }) = fetch.snapshot.line("On-demand") else { panic!() };
    assert_eq!(values[0].number, 0.0, "no cap: the month's spend, shown at $0");
}

#[tokio::test]
async fn a_unified_account_shows_additional_usage_against_its_limit_and_hides_it_when_off() {
    let token = jwt("fake", EXPIRY);
    let now = expiry() - Duration::days(30);
    let status = unified_status(r#""additionalUsageOff":false,"limitCents":10000,"usedCents":2500"#);
    let server = unified_server(&token, &status, "{}").await;
    let fetch = client(&server).fetch(&mut tokens(&token, Some(expiry())), now).await.unwrap();
    let Some(MetricLine::Progress { used, limit, resets_at, .. }) = fetch.snapshot.line("On-demand") else { panic!() };
    assert_eq!((*used, *limit, resets_at.unwrap().timestamp()), (25.0, 100.0, 1_796_083_200));
    let Some(MetricLine::Values { values, .. }) = fetch.snapshot.line("Credits") else { panic!() };
    assert_eq!(values[0].number, 0.0, "Connect JSON leaves out a zero balance");
    for monthly in [r#""additionalUsageOff":true,"limitCents":null,"usedCents":0"#, r#""limitCents":0,"usedCents":0"#] {
        let server = unified_server(&token, &unified_status(monthly), "{}").await;
        let fetch = client(&server).fetch(&mut tokens(&token, Some(expiry())), now).await.unwrap();
        assert!(fetch.snapshot.line("On-demand").is_none(), "{monthly}");
        assert!(fetch.snapshot.line("Credits").is_some(), "{monthly}");
    }
}

#[tokio::test]
async fn an_account_that_is_not_unified_never_reads_the_unified_endpoints() {
    let token = jwt("fake", EXPIRY);
    for plan in [r#"{"planInfo":{"planName":"Ultra"}}"#, r#"{"isMerged":false,"planInfo":{"planName":"Ultra"}}"#] {
        let server = MockServer::start().await;
        usage_for(&server, &token, json_ok(individual_usage())).await;
        post(&server, "GetPlanInfo", json_ok(plan)).await;
        post(&server, "GetCreditGrantsBalance", json_ok("{}")).await;
        post(&server, "GetSandUsageStatus", json_ok(r#"{"hasNonZeroIncludedLimit":true,"usagePercent":3}"#)).await;
        get(&server, "/api/auth/stripe", json_ok(r#"{"customerBalance":0}"#)).await;
        Mock::given(method("POST")).and(path(format!("{DASH}/GetComplatCreditBalance"))).respond_with(json_ok("{}")).expect(0).mount(&server).await;
        Mock::given(method("GET")).and(path("/api/dashboard/v2/usage-status")).respond_with(json_ok("{}")).expect(0).mount(&server).await;
        let fetch = client(&server).fetch(&mut tokens(&token, Some(expiry())), expiry() - Duration::days(30)).await.unwrap();
        assert!(fetch.snapshot.line("Cursor Models").is_some() && fetch.snapshot.line("Grok Bot usage").is_some(), "{plan}");
        let Some(MetricLine::Values { values, .. }) = fetch.snapshot.line("Credits") else { panic!() };
        assert_eq!(values[0].number, 0.0, "grants {{}} and Stripe 0");
    }
}

#[tokio::test]
async fn optional_reads_that_fail_do_not_fail_the_read() {
    let token = jwt("fake", EXPIRY);
    let server = MockServer::start().await;
    usage_for(&server, &token, json_ok(USAGE)).await;
    for name in ["GetPlanInfo", "GetCreditGrantsBalance", "GetSandUsageStatus"] {
        post(&server, name, ResponseTemplate::new(500)).await;
    }
    let fetch = client(&server).fetch(&mut tokens(&token, Some(expiry())), expiry() - Duration::days(30)).await.unwrap();
    let labels: Vec<&str> = fetch.snapshot.lines.iter().map(MetricLine::label).collect();
    assert_eq!(labels, ["Total usage"]);
    assert_eq!(fetch.snapshot.plan, None);
}

// Login. The poller's floor is one second, so the pending -> complete test takes about that long.

async fn poll_mock(server: &MockServer, status: u16, body: Option<&str>, up_to: Option<u64>) {
    let mut response = ResponseTemplate::new(status);
    if let Some(body) = body {
        response = response.set_body_raw(body.as_bytes().to_vec(), "application/json");
    }
    let mut mock = Mock::given(method("GET")).and(path("/auth/poll")).respond_with(response);
    if let Some(n) = up_to {
        mock = mock.up_to_n_times(n);
    }
    mock.mount(server).await;
}

#[tokio::test]
async fn login_opens_the_deep_link_polls_through_pending_and_names_the_account_by_its_email() {
    let access = jwt("abc", EXPIRY);
    let server = MockServer::start().await;
    poll_mock(&server, 404, None, Some(1)).await;
    poll_mock(&server, 200, Some(&json!({"accessToken": access, "refreshToken": "cursor-refresh", "authId": "auth0|user_abc"}).to_string()), None).await;
    Mock::given(method("GET"))
        .and(path("/api/auth/me"))
        .and(header("cookie", format!("WorkosCursorSessionToken=user_abc%3A%3A{access}").as_str()))
        .respond_with(json_ok(r#"{"email":"dev@example.com","name":"Dev"}"#))
        .expect(1)
        .mount(&server)
        .await;
    let mut opened = None;
    let (label, t) = client(&server).login(|url| opened = Some(url.clone())).await.unwrap();
    assert_eq!(label, "dev@example.com");
    assert_eq!(t.extras.get(OAuthTokens::IDENTITY_KEY).map(String::as_str), Some("cursor:user_abc"));
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref(), t.expires_at), (access.as_str(), Some("cursor-refresh"), Some(expiry())));

    let url = opened.unwrap();
    assert_eq!(url.path(), "/loginDeepControl");
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!((query["mode"].as_str(), query["redirectTarget"].as_str()), ("login", "cli"));
    let seen = server.received_requests().await.unwrap();
    let polls: Vec<_> = seen.iter().filter(|r| r.url.path() == "/auth/poll").collect();
    assert_eq!(polls.len(), 2, "pending, then complete");
    let poll_query: HashMap<_, _> = polls[0].url.query_pairs().into_owned().collect();
    assert_eq!(poll_query["uuid"], query["uuid"]);
    assert_eq!(crate::oauth::pkce::challenge_for(&poll_query["verifier"]), query["challenge"]);
}

#[tokio::test]
async fn login_keeps_the_default_name_when_the_profile_read_fails() {
    let access = jwt("abc", EXPIRY);
    let server = MockServer::start().await;
    poll_mock(&server, 200, Some(&json!({"accessToken": access, "refreshToken": "r"}).to_string()), None).await;
    get(&server, "/api/auth/me", ResponseTemplate::new(500)).await;
    let (label, t) = client(&server).login(|_| {}).await.unwrap();
    assert_eq!(label, "Cursor Account");
    assert_eq!(t.extras.get(OAuthTokens::IDENTITY_KEY).map(String::as_str), Some("cursor:user_abc"));
}

#[tokio::test]
async fn login_fails_when_the_poll_is_refused() {
    let server = MockServer::start().await;
    poll_mock(&server, 500, None, None).await;
    let err = client(&server).login(|_| {}).await.unwrap_err();
    assert_eq!(err.to_string(), "Cursor sign-in failed.");
}
