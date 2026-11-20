use std::collections::HashMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use wiremock::matchers::{bearer_token, body_partial_json, body_string, header, method, path};
use wiremock::{Mock, MockBuilder, MockServer, ResponseTemplate};

use super::*;
use crate::models::MetricLine;

const USAGE: &str = r#"{"plan_type":"pro","rate_limit":{"primary_window":{"used_percent":10,"limit_window_seconds":18000}}}"#;
const EXPIRY: i64 = 1_800_000_000;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-19T12:00:00Z").unwrap().into()
}

fn jwt(payload: Value) -> String {
    format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(payload.to_string()))
}

fn tokens(access: &str, expires_in_min: i64) -> OAuthTokens {
    OAuthTokens {
        access_token: access.into(),
        refresh_token: Some("r1".into()),
        expires_at: Some(now() + Duration::minutes(expires_in_min)),
        extras: HashMap::from([(ACCOUNT_ID_KEY.into(), "acct-456".into()), ("keep".into(), "me".into())]),
    }
}

fn config(server: &MockServer, loopback_port: u16) -> CodexConfig {
    let base = server.uri();
    CodexConfig {
        authorize_url: format!("{base}/oauth/authorize"),
        token_url: format!("{base}/oauth/token"),
        loopback_port,
        device_usercode_url: format!("{base}/device/usercode"),
        device_token_url: format!("{base}/device/token"),
        device_verify_url: format!("{base}/codex/device"),
        device_redirect_uri: format!("{base}/deviceauth/callback"),
        api_base: format!("{base}/backend-api"),
    }
}

fn client(server: &MockServer) -> CodexClient {
    CodexClient::new(config(server, 0))
}

fn json_ok(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

async fn mount(server: &MockServer, m: Mock) {
    m.mount(server).await;
}

fn token_ok(access: &str) -> ResponseTemplate {
    json_ok(&json!({"access_token": access, "refresh_token": "r2", "expires_in": 3600}).to_string())
}

fn token_post() -> MockBuilder {
    Mock::given(method("POST")).and(path("/oauth/token"))
}

fn usage_builder(bearer: &str) -> MockBuilder {
    Mock::given(method("GET")).and(path("/backend-api/wham/usage")).and(bearer_token(bearer))
}

fn usage_mock(bearer: &str, response: ResponseTemplate) -> Mock {
    usage_builder(bearer).respond_with(response)
}

async fn fetch(server: &MockServer, t: &mut OAuthTokens) -> Result<CodexFetch, ProviderError> {
    client(server).fetch(t, None, false, now()).await
}

#[tokio::test]
async fn usage_request_carries_the_expected_headers() {
    let server = MockServer::start().await;
    mount(
        &server,
        usage_builder("tok")
            .and(header("accept", "application/json"))
            .and(header("user-agent", "Headroom"))
            .and(header("chatgpt-account-id", "acct-456"))
            .respond_with(json_ok(USAGE))
            .expect(1),
    )
    .await;
    let fetch = fetch(&server, &mut tokens("tok", 60)).await.unwrap();
    assert_eq!(fetch.snapshot.plan.as_deref(), Some("Pro 200"));
    assert_eq!(fetch.snapshot.lines.len(), 1);
    // Only the usage call went out: the credit list was not requested.
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn usage_omits_the_account_header_without_an_account_id() {
    let server = MockServer::start().await;
    mount(&server, usage_mock("tok", json_ok(USAGE))).await;
    let mut t = tokens("tok", 60);
    t.extras.clear();
    fetch(&server, &mut t).await.unwrap();
    assert!(!server.received_requests().await.unwrap()[0].headers.contains_key("chatgpt-account-id"));
}

#[tokio::test]
async fn header_fallbacks_reach_the_mapper() {
    let server = MockServer::start().await;
    let response = json_ok("{}").insert_header("x-codex-secondary-used-percent", "7").insert_header("x-codex-credits-balance", "5");
    mount(&server, usage_mock("tok", response)).await;
    let lines = fetch(&server, &mut tokens("tok", 60)).await.unwrap().snapshot.lines;
    assert_eq!(lines.iter().map(MetricLine::label).collect::<Vec<_>>(), ["Weekly", "Credits"]);
}

#[tokio::test]
async fn refresh_is_a_form_post_without_scope_and_takes_its_expiry_from_the_jwt() {
    let server = MockServer::start().await;
    let access = jwt(json!({"exp": EXPIRY}));
    mount(
        &server,
        token_post()
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string(format!("client_id={CLIENT_ID}&grant_type=refresh_token&refresh_token=refresh%20token%26%3D%2B%2F%3F%25")))
            .respond_with(json_ok(&json!({"access_token": access, "refresh_token": "r2", "expires_in": 5}).to_string()))
            .expect(1),
    )
    .await;
    let mut t = tokens("old", 1);
    t.refresh_token = Some("refresh token&=+/?%".into());
    let renewed = client(&server).refresh(&t).await.unwrap();
    assert_eq!(renewed.expires_at, DateTime::from_timestamp(EXPIRY, 0), "JWT exp wins over expires_in");
    assert_eq!((renewed.access_token, renewed.refresh_token.as_deref()), (access, Some("r2")));
    assert_eq!(renewed.extras, t.extras);
}

#[tokio::test]
async fn a_refreshed_token_without_exp_has_no_expiry_and_keeps_the_old_refresh_token() {
    let server = MockServer::start().await;
    mount(&server, token_post().respond_with(json_ok(r#"{"access_token":"new-token"}"#))).await;
    let renewed = client(&server).refresh(&tokens("old", 1)).await.unwrap();
    assert_eq!((renewed.expires_at, renewed.refresh_token.as_deref()), (None, Some("r1")));
}

#[tokio::test]
async fn a_token_inside_the_margin_is_refreshed_before_the_read() {
    let server = MockServer::start().await;
    mount(&server, token_post().respond_with(token_ok("new")).expect(1)).await;
    mount(&server, usage_mock("new", json_ok(USAGE)).expect(1)).await;
    let mut t = tokens("old", 2);
    fetch(&server, &mut t).await.unwrap();
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref()), ("new", Some("r2")));
    assert_eq!(t.extras["keep"], "me");
}

#[tokio::test]
async fn a_401_refreshes_once_and_retries_once() {
    let server = MockServer::start().await;
    mount(&server, usage_mock("old", ResponseTemplate::new(401)).expect(1)).await;
    mount(&server, token_post().respond_with(token_ok("new")).expect(1)).await;
    mount(&server, usage_mock("new", json_ok(USAGE)).expect(1)).await;
    let mut t = tokens("old", 60);
    fetch(&server, &mut t).await.unwrap();
    assert_eq!(t.access_token, "new");
}

#[tokio::test]
async fn a_401_that_survives_the_retry_is_sign_in_expired_and_without_refresh_token_is_not_retried() {
    let server = MockServer::start().await;
    mount(&server, usage_mock("old", ResponseTemplate::new(401)).expect(1)).await;
    mount(&server, token_post().respond_with(token_ok("new")).expect(1)).await;
    mount(&server, usage_mock("new", ResponseTemplate::new(401)).expect(1)).await;
    assert_eq!(fetch(&server, &mut tokens("old", 60)).await.unwrap_err(), ProviderError::SignInExpired);

    let server = MockServer::start().await;
    mount(&server, usage_mock("old", ResponseTemplate::new(401)).expect(1)).await;
    let mut t = tokens("old", 60);
    t.refresh_token = None;
    assert_eq!(fetch(&server, &mut t).await.unwrap_err(), ProviderError::SignInExpired);
}

#[tokio::test]
async fn a_refused_refresh_token_is_sign_in_expired() {
    for body in [r#"{"error":"invalid_grant"}"#, r#"{"error":{"code":"refresh_token_expired"}}"#] {
        let server = MockServer::start().await;
        mount(&server, token_post().respond_with(ResponseTemplate::new(400).set_body_string(body))).await;
        // Expired token: the failed early refresh cannot fall back to it.
        assert_eq!(fetch(&server, &mut tokens("old", -1)).await.unwrap_err(), ProviderError::SignInExpired, "{body}");
    }
}

#[tokio::test]
async fn a_failed_early_refresh_keeps_a_token_that_has_not_expired() {
    let server = MockServer::start().await;
    mount(&server, token_post().respond_with(ResponseTemplate::new(500))).await;
    mount(&server, usage_mock("old", json_ok(USAGE)).expect(1)).await;
    let mut t = tokens("old", 2);
    fetch(&server, &mut t).await.unwrap();
    assert_eq!(t, tokens("old", 2));
}

#[tokio::test]
async fn a_rotated_refresh_token_survives_a_failing_read_after_the_refresh() {
    let server = MockServer::start().await;
    mount(&server, token_post().respond_with(token_ok("new")).expect(1)).await;
    mount(&server, usage_mock("new", ResponseTemplate::new(500))).await;
    let mut t = tokens("old", 2);
    assert_eq!(fetch(&server, &mut t).await.unwrap_err(), ProviderError::RequestFailed(500));
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref()), ("new", Some("r2")));

    // Also when the retry after a 401 fails.
    let server = MockServer::start().await;
    mount(&server, usage_mock("old", ResponseTemplate::new(401))).await;
    mount(&server, token_post().respond_with(token_ok("new")).expect(1)).await;
    mount(&server, usage_mock("new", ResponseTemplate::new(429))).await;
    let mut t = tokens("old", 60);
    assert!(fetch(&server, &mut t).await.is_err());
    assert_eq!(t.refresh_token.as_deref(), Some("r2"));
}

#[tokio::test]
async fn rate_limits_and_failures_map_to_errors() {
    let cases = [
        (ResponseTemplate::new(429).insert_header("retry-after", "42"), ProviderError::RateLimited { retry_after_secs: Some(42) }),
        (ResponseTemplate::new(429), ProviderError::RateLimited { retry_after_secs: None }),
        (ResponseTemplate::new(403).insert_header("retry-after", "9"), ProviderError::RateLimited { retry_after_secs: Some(9) }),
        (ResponseTemplate::new(403), ProviderError::SignInExpired),
        (ResponseTemplate::new(500), ProviderError::RequestFailed(500)),
        (json_ok("[]"), ProviderError::InvalidResponse),
    ];
    for (response, expected) in cases {
        let server = MockServer::start().await;
        mount(&server, usage_mock("tok", response)).await;
        assert_eq!(fetch(&server, &mut tokens("tok", 60)).await.unwrap_err(), expected);
    }
}

fn list_body(status: &str) -> String {
    let iso = |secs: i64| DateTime::from_timestamp(secs, 0).unwrap().to_rfc3339();
    json!({"credits": [
        {"id": "RateLimitResetCredit_other", "status": "available", "expires_at": iso(EXPIRY + 999_999)},
        {"id": "RateLimitResetCredit_target", "status": status, "expires_at": iso(EXPIRY)}
    ], "available_count": 2})
    .to_string()
}

fn list_builder(bearer: &str) -> MockBuilder {
    Mock::given(method("GET")).and(path("/backend-api/wham/rate-limit-reset-credits")).and(bearer_token(bearer))
}

fn resets_usage() -> String {
    r#"{"rate_limit_reset_credits":{"available_count":2}}"#.into()
}

#[tokio::test]
async fn the_credit_list_is_read_with_the_codex_desktop_headers_and_feeds_the_expiries() {
    let server = MockServer::start().await;
    mount(&server, usage_mock("tok", json_ok(&resets_usage()))).await;
    mount(
        &server,
        list_builder("tok")
            .and(header("openai-beta", "codex-1"))
            .and(header("originator", "Codex Desktop"))
            .and(header("user-agent", "Headroom"))
            .and(header("chatgpt-account-id", "acct-456"))
            .respond_with(json_ok(&list_body("available")))
            .expect(1),
    )
    .await;
    let c = client(&server);
    let read = c.fetch(&mut tokens("tok", 60), None, true, now()).await.unwrap();
    let want = vec![DateTime::from_timestamp(EXPIRY, 0).unwrap(), DateTime::from_timestamp(EXPIRY + 999_999, 0).unwrap()];
    assert_eq!(read.reset_expiries, want);
    assert_eq!(read.snapshot.reset_items.iter().map(|i| (i.kind, i.expires_at, i.claim.clone())).collect::<Vec<_>>(), [
        (crate::claude::resets::ResetKind::Full, want[0], crate::claim::ResetRef::CodexCredit(want[0])),
        (crate::claude::resets::ResetKind::Full, want[1], crate::claim::ResetRef::CodexCredit(want[1])),
    ]);
    let cached = read.credit_list.expect("the fresh list is handed back");
    // The next pass is not due: no list request, the cached one still supplies the expiries.
    let again = c.fetch(&mut tokens("tok", 60), Some(cached), false, now()).await.unwrap();
    assert_eq!(again.reset_expiries, want);
}

#[tokio::test]
async fn a_failing_credit_list_keeps_the_cached_one_and_never_fails_the_read() {
    let server = MockServer::start().await;
    mount(&server, usage_mock("tok", json_ok(&resets_usage()))).await;
    mount(&server, list_builder("tok").respond_with(ResponseTemplate::new(503))).await;
    let fetch = client(&server).fetch(&mut tokens("tok", 60), Some(list_body("available").into_bytes()), true, now()).await.unwrap();
    assert_eq!(fetch.reset_expiries.len(), 2);
    let none = client(&server).fetch(&mut tokens("tok", 60), None, true, now()).await.unwrap();
    assert!(none.reset_expiries.is_empty() && none.credit_list.is_none());
    assert_eq!(none.snapshot.line("Rate Limit Resets").map(MetricLine::label), Some("Rate Limit Resets"));
}

fn consume_builder(bearer: &str) -> MockBuilder {
    Mock::given(method("POST")).and(path("/backend-api/wham/rate-limit-reset-credits/consume")).and(bearer_token(bearer))
}

fn consume_ok(code: &str) -> ResponseTemplate {
    json_ok(&json!({"code": code, "windows_reset": 2}).to_string())
}

fn expiry() -> DateTime<Utc> {
    DateTime::from_timestamp(EXPIRY, 0).unwrap()
}

#[tokio::test]
async fn claim_matches_the_credit_and_consumes_it_with_the_accounts_login() {
    let server = MockServer::start().await;
    mount(&server, list_builder("tok").and(header("chatgpt-account-id", "acct-456")).respond_with(json_ok(&list_body("available"))).expect(1)).await;
    mount(
        &server,
        consume_builder("tok")
            .and(header("content-type", "application/json"))
            .and(header("openai-beta", "codex-1"))
            .and(header("originator", "Codex Desktop"))
            .and(header("chatgpt-account-id", "acct-456"))
            .and(body_partial_json(json!({"credit_id": "RateLimitResetCredit_target", "redeem_request_id": "redeem-1"})))
            .respond_with(consume_ok("reset"))
            .expect(1),
    )
    .await;
    let mut matched = None;
    let outcome = client(&server).claim_reset_credit(&mut tokens("tok", 60), expiry(), "redeem-1", &mut matched, now()).await.unwrap();
    assert_eq!(outcome, ClaimOutcome::Success);
    assert_eq!(matched.as_deref(), Some("RateLimitResetCredit_target"));
}

#[tokio::test]
async fn a_claim_refused_with_a_cut_short_token_renews_once_and_retries() {
    let server = MockServer::start().await;
    mount(&server, list_builder("old").respond_with(ResponseTemplate::new(401)).expect(1)).await;
    mount(&server, token_post().respond_with(token_ok("new")).expect(1)).await;
    mount(&server, list_builder("new").respond_with(json_ok(&list_body("available"))).expect(1)).await;
    mount(&server, consume_builder("new").respond_with(consume_ok("reset")).expect(1)).await;
    let mut t = tokens("old", 60);
    let outcome = client(&server).claim_reset_credit(&mut t, expiry(), "redeem-1", &mut None, now()).await.unwrap();
    assert_eq!((outcome, t.access_token.as_str()), (ClaimOutcome::Success, "new"));
}

#[tokio::test]
async fn claim_failures_collapse_to_friendly_outcomes() {
    use ClaimFailure::*;
    let ok_list = || json_ok(&list_body("available"));
    let cases: Vec<(&str, ResponseTemplate, Option<ResponseTemplate>, ClaimOutcome)> = vec![
        ("list down", ResponseTemplate::new(503), None, ClaimOutcome::Failed(Refused)),
        ("list rejected", ResponseTemplate::new(401), None, ClaimOutcome::Failed(SignInExpired)),
        ("list not json", json_ok("nope"), None, ClaimOutcome::Failed(Refused)),
        ("no match", json_ok(&list_body("redeemed")), None, ClaimOutcome::NoCredit),
        ("consume down", ok_list(), Some(ResponseTemplate::new(500).set_body_string("gateway error")), ClaimOutcome::Failed(Refused)),
        ("consume rejected", ok_list(), Some(ResponseTemplate::new(401)), ClaimOutcome::Failed(SignInExpired)),
        ("nothing to reset", ok_list(), Some(consume_ok("nothing_to_reset")), ClaimOutcome::NothingToReset),
    ];
    for (name, list, consume, expected) in cases {
        let server = MockServer::start().await;
        // 401s here have no refresh token to retry with.
        mount(&server, list_builder("tok").respond_with(list)).await;
        let consumed = consume.is_some();
        if let Some(consume) = consume {
            mount(&server, consume_builder("tok").respond_with(consume)).await;
        }
        let mut t = tokens("tok", 60);
        t.refresh_token = None;
        let outcome = client(&server).claim_reset_credit(&mut t, expiry(), "redeem-1", &mut None, now()).await.unwrap();
        assert_eq!(outcome, expected, "{name}");
        let posted = server.received_requests().await.unwrap().iter().any(|r| r.method.as_str() == "POST");
        assert_eq!(posted, consumed, "{name}: consume sent only after a match");
    }
}

#[tokio::test]
async fn a_lost_consume_answer_is_retried_with_the_same_key_and_credit() {
    let server = MockServer::start().await;
    mount(&server, list_builder("tok").respond_with(json_ok(&list_body("available"))).expect(1)).await;
    let body = json!({"credit_id": "RateLimitResetCredit_target", "redeem_request_id": "redeem-1"});
    mount(&server, consume_builder("tok").and(body_partial_json(&body)).respond_with(ResponseTemplate::new(500)).up_to_n_times(1)).await;
    mount(&server, consume_builder("tok").and(body_partial_json(&body)).respond_with(consume_ok("already_redeemed")).expect(1)).await;
    let c = client(&server);
    let mut matched = None;
    let first = c.claim_reset_credit(&mut tokens("tok", 60), expiry(), "redeem-1", &mut matched, now()).await.unwrap();
    assert_eq!(first, ClaimOutcome::Failed(ClaimFailure::Refused));
    let retry = c.claim_reset_credit(&mut tokens("tok", 60), expiry(), "redeem-1", &mut matched, now()).await.unwrap();
    assert_eq!(retry, ClaimOutcome::Success, "already_redeemed proves the original claim landed");
}

#[tokio::test]
async fn a_rotated_refresh_token_survives_a_failing_claim_after_the_refresh() {
    let server = MockServer::start().await;
    mount(&server, token_post().respond_with(token_ok("new")).expect(1)).await;
    mount(&server, list_builder("new").respond_with(ResponseTemplate::new(500))).await;
    let mut t = tokens("old", 2);
    let outcome = client(&server).claim_reset_credit(&mut t, expiry(), "redeem-1", &mut None, now()).await.unwrap();
    assert_eq!(outcome, ClaimOutcome::Failed(ClaimFailure::Refused));
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref()), ("new", Some("r2")));

    // A refused refresh after rotation never happened: a refresh that errors leaves the tokens untouched.
    let server = MockServer::start().await;
    mount(&server, token_post().respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid_grant"}"#))).await;
    let mut t = tokens("old", -1);
    let err = client(&server).claim_reset_credit(&mut t, expiry(), "r", &mut None, now()).await.unwrap_err();
    assert_eq!((err, t), (ProviderError::SignInExpired, tokens("old", -1)));
}


mod login;
