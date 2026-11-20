use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use wiremock::matchers::{bearer_token, body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockBuilder, MockServer, ResponseTemplate};

use super::*;
use crate::models::MetricLine;

const ORG: &str = "6a9f1c2e-1b4d-4c3a-9e8f-0123456789ab";
const USAGE: &str = r#"{"five_hour":{"utilization":10,"resets_at":"2026-11-19T18:00:00Z"}}"#;
const GRANTS: &str = r#"{"cedar_ember":{"eligible":true,"grants":[{"id":"g1","resets_left":1,"ends_at":"2099-01-01T00:00:00Z","usable_now":true}]}}"#;
const PROFILE: &str = r#"{"account":{"uuid":"acc","email":"me@x.io"},"organization":{"uuid":"6a9f1c2e-1b4d-4c3a-9e8f-0123456789ab","organization_type":"claude_max"}}"#;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-19T12:00:00Z").unwrap().into()
}

fn tokens(access: &str, expires_in_min: i64) -> OAuthTokens {
    OAuthTokens {
        access_token: access.into(),
        refresh_token: Some("r1".into()),
        expires_at: Some(now() + Duration::minutes(expires_in_min)),
        extras: HashMap::from([("keep".into(), "me".into())]),
    }
}

fn client(server: &MockServer) -> ClaudeClient {
    ClaudeClient::new(ClaudeConfig {
        authorize_url: format!("{}/oauth/authorize", server.uri()),
        token_url: format!("{}/v1/oauth/token", server.uri()),
        api_base: server.uri(),
    })
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

/// `header()` splits values on commas, so the exact User-Agent needs its own matcher.
fn claude_user_agent(request: &wiremock::Request) -> bool {
    request.headers.get("user-agent").is_some_and(|v| v == "claude-cli/2.1.294 (external, cli)")
}

fn usage_builder(bearer: &str) -> MockBuilder {
    Mock::given(method("GET")).and(path("/api/oauth/usage")).and(bearer_token(bearer))
}

fn usage_mock(bearer: &str, response: ResponseTemplate) -> Mock {
    usage_builder(bearer).respond_with(response)
}

#[tokio::test]
async fn usage_request_carries_the_claude_code_headers() {
    let server = MockServer::start().await;
    mount(
        &server,
        usage_builder("tok")
            .and(claude_user_agent)
            .and(header("anthropic-beta", "oauth-2025-04-20"))
            .and(header("accept", "application/json"))
            .respond_with(json_ok(USAGE))
            .expect(1),
    )
    .await;
    let fetch = client(&server).fetch(&mut tokens("tok", 60), false, now()).await.unwrap();
    assert_eq!(fetch.snapshot.lines.len(), 1);
    assert!(fetch.snapshot.plan.is_none());
}

#[tokio::test]
async fn a_token_inside_the_margin_is_refreshed_before_the_read() {
    let server = MockServer::start().await;
    mount(
        &server,
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .and(body_partial_json(json!({"grant_type":"refresh_token","refresh_token":"r1","client_id":CLIENT_ID,"scope":SCOPES})))
            .respond_with(token_ok("new"))
            .expect(1),
    )
    .await;
    mount(&server, usage_mock("new", json_ok(USAGE)).expect(1)).await;
    let mut t = tokens("old", 2);
    client(&server).fetch(&mut t, false, now()).await.unwrap();
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref()), ("new", Some("r2")));
    assert_eq!(t.expires_at, Some(now() + Duration::hours(1)));
    assert_eq!(t.extras["keep"], "me");
}

#[tokio::test]
async fn a_401_refreshes_once_and_retries_once() {
    let server = MockServer::start().await;
    mount(&server, usage_mock("old", ResponseTemplate::new(401)).expect(1)).await;
    mount(&server, Mock::given(method("POST")).and(path("/v1/oauth/token")).respond_with(token_ok("new")).expect(1)).await;
    mount(&server, usage_mock("new", json_ok(USAGE)).expect(1)).await;
    let mut t = tokens("old", 60);
    client(&server).fetch(&mut t, false, now()).await.unwrap();
    assert_eq!(t.access_token, "new");
}

#[tokio::test]
async fn a_401_that_survives_the_retry_is_sign_in_expired_and_without_refresh_token_is_not_retried() {
    let server = MockServer::start().await;
    mount(&server, usage_mock("old", ResponseTemplate::new(401)).expect(1)).await;
    mount(&server, Mock::given(method("POST")).and(path("/v1/oauth/token")).respond_with(token_ok("new")).expect(1)).await;
    mount(&server, usage_mock("new", ResponseTemplate::new(401)).expect(1)).await;
    let err = client(&server).fetch(&mut tokens("old", 60), false, now()).await.unwrap_err();
    assert_eq!(err, ProviderError::SignInExpired);

    let server = MockServer::start().await;
    mount(&server, usage_mock("old", ResponseTemplate::new(401)).expect(1)).await;
    let mut t = tokens("old", 60);
    t.refresh_token = None;
    assert_eq!(client(&server).fetch(&mut t, false, now()).await.unwrap_err(), ProviderError::SignInExpired);
}

#[tokio::test]
async fn a_refused_refresh_token_is_sign_in_expired() {
    let server = MockServer::start().await;
    let refused = ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid_grant"}"#);
    mount(&server, Mock::given(method("POST")).and(path("/v1/oauth/token")).respond_with(refused)).await;
    // Expired token: the failed early refresh cannot fall back to it.
    let err = client(&server).fetch(&mut tokens("old", -1), false, now()).await.unwrap_err();
    assert_eq!(err, ProviderError::SignInExpired);
}

#[tokio::test]
async fn a_failed_early_refresh_keeps_a_token_that_has_not_expired() {
    let server = MockServer::start().await;
    mount(&server, Mock::given(method("POST")).and(path("/v1/oauth/token")).respond_with(ResponseTemplate::new(500))).await;
    mount(&server, usage_mock("old", json_ok(USAGE)).expect(1)).await;
    let mut t = tokens("old", 2);
    client(&server).fetch(&mut t, false, now()).await.unwrap();
    assert_eq!(t, tokens("old", 2));
}

#[tokio::test]
async fn invalid_scope_renews_with_the_profile_scope_only() {
    let server = MockServer::start().await;
    let scope_refused = ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid_scope"}"#);
    mount(
        &server,
        Mock::given(method("POST")).and(body_partial_json(json!({"scope": SCOPES}))).respond_with(scope_refused).expect(1),
    )
    .await;
    mount(
        &server,
        Mock::given(method("POST")).and(body_partial_json(json!({"scope": "user:profile"}))).respond_with(token_ok("new")).expect(1),
    )
    .await;
    let renewed = client(&server).refresh(&tokens("old", 1), now()).await.unwrap();
    assert_eq!(renewed.access_token, "new");
}

#[tokio::test]
async fn a_scope_refusal_403_is_not_sign_in_expired() {
    let server = MockServer::start().await;
    let body = r#"{"error":{"type":"permission_error","message":"OAuth token does not meet scope requirement user:profile"}}"#;
    mount(&server, usage_mock("tok", ResponseTemplate::new(403).set_body_string(body))).await;
    let err = client(&server).fetch(&mut tokens("tok", 60), false, now()).await.unwrap_err();
    assert!(matches!(err, ProviderError::Unavailable(_)), "{err:?}");
    let server = MockServer::start().await;
    mount(&server, usage_mock("tok", ResponseTemplate::new(403))).await;
    assert_eq!(client(&server).fetch(&mut tokens("tok", 60), false, now()).await.unwrap_err(), ProviderError::SignInExpired);
}

#[tokio::test]
async fn rate_limits_and_failures_map_to_errors() {
    let cases = [
        (ResponseTemplate::new(429).insert_header("retry-after", "42"), ProviderError::RateLimited { retry_after_secs: Some(42) }),
        (ResponseTemplate::new(429), ProviderError::RateLimited { retry_after_secs: None }),
        (ResponseTemplate::new(500), ProviderError::RequestFailed(500)),
        (json_ok("[]"), ProviderError::InvalidResponse),
    ];
    for (response, expected) in cases {
        let server = MockServer::start().await;
        mount(&server, usage_mock("tok", response)).await;
        assert_eq!(client(&server).fetch(&mut tokens("tok", 60), false, now()).await.unwrap_err(), expected);
    }
}

#[tokio::test]
async fn secondary_reads_add_the_resets_line_plan_org_and_identity() {
    let server = MockServer::start().await;
    let grants = usage_builder("tok").and(query_param("cedar_ember", "1")).and(query_param("skip_spend", "1"));
    mount(&server, grants.respond_with(json_ok(GRANTS)).with_priority(1).expect(1)).await;
    mount(&server, usage_mock("tok", json_ok(USAGE)).with_priority(2)).await;
    mount(
        &server,
        Mock::given(method("GET"))
            .and(path("/api/oauth/profile"))
            .and(bearer_token("tok"))
            .and(header("anthropic-beta", "oauth-2025-04-20"))
            .respond_with(json_ok(PROFILE))
            .expect(1),
    )
    .await;
    let mut t = tokens("tok", 60);
    let fetch = client(&server).fetch(&mut t, true, now()).await.unwrap();
    let labels: Vec<_> = fetch.snapshot.lines.iter().map(MetricLine::label).collect();
    assert_eq!(labels, ["Session", "Rate Limit Resets"]);
    assert_eq!(fetch.snapshot.plan.as_deref(), Some("Max"));
    assert_eq!(fetch.organization_uuid.as_deref(), Some(ORG));
    assert!(fetch.grants.is_some());
    assert_eq!(t.extras[OAuthTokens::IDENTITY_KEY], format!("acc/{ORG}"));
}

#[tokio::test]
async fn a_failing_secondary_read_only_costs_its_part() {
    let server = MockServer::start().await;
    let grants = usage_builder("tok").and(query_param("cedar_ember", "1"));
    mount(&server, grants.respond_with(ResponseTemplate::new(429)).with_priority(1)).await;
    mount(&server, usage_mock("tok", json_ok(USAGE)).with_priority(2)).await;
    mount(&server, Mock::given(method("GET")).and(path("/api/oauth/profile")).respond_with(ResponseTemplate::new(500))).await;
    let mut t = tokens("tok", 60);
    let fetch = client(&server).fetch(&mut t, true, now()).await.unwrap();
    assert_eq!(fetch.snapshot.lines.len(), 1);
    assert!(fetch.snapshot.plan.is_none() && fetch.grants.is_none());
    assert_eq!(t, tokens("tok", 60), "nothing learned, nothing to persist");
}

fn claim_builder(bearer: &str) -> MockBuilder {
    Mock::given(method("POST")).and(path(format!("/api/organizations/{ORG}/reset_rate_limits"))).and(bearer_token(bearer))
}

fn claim_mock(bearer: &str, response: ResponseTemplate) -> Mock {
    claim_builder(bearer).respond_with(response)
}

#[tokio::test]
async fn claim_request_matches_claude_code() {
    let server = MockServer::start().await;
    mount(
        &server,
        claim_builder("tok")
            .and(claude_user_agent)
            .and(header("anthropic-beta", "oauth-2025-04-20"))
            .and(header("content-type", "application/json"))
            .and(body_partial_json(json!({"program":"cedar_ember","grant_id":"opus55-launch","request_id":"11111111-2222"})))
            .respond_with(json_ok(r#"{"result":"reset","resets_left":0}"#))
            .expect(1),
    )
    .await;
    let mut t = tokens("tok", 60);
    let outcome = client(&server).claim_reset_grant(&mut t, ORG, "opus55-launch", "11111111-2222", now()).await.unwrap();
    assert_eq!((outcome, t), (ClaimOutcome::Success, tokens("tok", 60)));
}

#[tokio::test]
async fn claim_renews_once_on_401_then_reports_not_permitted_if_still_refused() {
    let server = MockServer::start().await;
    mount(&server, claim_mock("old", ResponseTemplate::new(401)).expect(1)).await;
    mount(&server, Mock::given(method("POST")).and(path("/v1/oauth/token")).respond_with(token_ok("new")).expect(1)).await;
    mount(&server, claim_mock("new", json_ok(r#"{"result":"reset"}"#)).expect(1)).await;
    let mut t = tokens("old", 60);
    let outcome = client(&server).claim_reset_grant(&mut t, ORG, "g1", "req-1", now()).await.unwrap();
    assert_eq!(outcome, ClaimOutcome::Success);
    assert_eq!(t.access_token, "new");

    let server = MockServer::start().await;
    mount(&server, claim_mock("old", ResponseTemplate::new(401))).await;
    mount(&server, Mock::given(method("POST")).and(path("/v1/oauth/token")).respond_with(token_ok("new"))).await;
    mount(&server, claim_mock("new", ResponseTemplate::new(401))).await;
    let outcome = client(&server).claim_reset_grant(&mut tokens("old", 60), ORG, "g1", "req-1", now()).await.unwrap();
    assert_eq!(outcome, ClaimOutcome::Failed(ClaimFailure::NotPermitted));
}

#[tokio::test]
async fn claim_refuses_malformed_ids_without_a_request() {
    let server = MockServer::start().await;
    let c = client(&server);
    for (org, grant, request) in [(ORG, "Bad Grant", "r"), (ORG, "g", "bad id!"), (ORG, "", "r"), ("not-a-uuid", "g", "r")] {
        let outcome = c.claim_reset_grant(&mut tokens("tok", 60), org, grant, request, now()).await.unwrap();
        assert_eq!(outcome, ClaimOutcome::Failed(ClaimFailure::Refused), "{org} {grant} {request}");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Drives the loopback with a real GET, as the browser would after the user approves.
fn approve(url: &Url) {
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    let target = format!("{}?code=abc&state={}", query["redirect_uri"], query["state"]);
    tokio::spawn(async move {
        let _ = reqwest::get(target).await;
    });
}

#[tokio::test]
async fn login_exchanges_the_code_and_stores_the_identity() {
    let server = MockServer::start().await;
    mount(
        &server,
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .and(body_partial_json(json!({"grant_type":"authorization_code","client_id":CLIENT_ID,"code":"abc"})))
            .respond_with(token_ok("fresh"))
            .expect(1),
    )
    .await;
    mount(&server, Mock::given(method("GET")).and(path("/api/oauth/profile")).and(bearer_token("fresh")).respond_with(json_ok(PROFILE)).expect(1)).await;
    let mut opened = None;
    let (label, tokens) = client(&server)
        .login(|url| {
            opened = Some(url.clone());
            approve(url);
        })
        .await
        .unwrap();
    assert_eq!(label, "me@x.io");
    assert_eq!(tokens.access_token, "fresh");
    assert_eq!(tokens.extras[OAuthTokens::IDENTITY_KEY], format!("acc/{ORG}"));
    let url = opened.unwrap();
    let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!((q["code"].as_str(), q["scope"].as_str(), q["client_id"].as_str()), ("true", SCOPES, CLIENT_ID));
    assert!(q["redirect_uri"].starts_with("http://localhost:") && q["redirect_uri"].ends_with("/callback"));
    // The code verifier sent matches the challenge in the URL.
    let sent: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(crate::oauth::pkce::challenge_for(sent["code_verifier"].as_str().unwrap()), q["code_challenge"]);
    assert_eq!(sent["state"], q["state"]);
}

#[tokio::test]
async fn a_profile_failure_after_the_exchange_fails_the_login() {
    let server = MockServer::start().await;
    mount(&server, Mock::given(method("POST")).and(path("/v1/oauth/token")).respond_with(token_ok("fresh"))).await;
    mount(&server, Mock::given(method("GET")).and(path("/api/oauth/profile")).respond_with(ResponseTemplate::new(500))).await;
    let err = client(&server).login(approve).await.unwrap_err();
    assert!(matches!(err, OAuthError::Failed(m) if m == "Claude didn't return your profile. Sign in again."));
}

#[tokio::test]
async fn a_refused_exchange_fails_the_login() {
    let server = MockServer::start().await;
    mount(&server, Mock::given(method("POST")).and(path("/v1/oauth/token")).respond_with(ResponseTemplate::new(400))).await;
    assert!(matches!(client(&server).login(approve).await.unwrap_err(), OAuthError::RequestFailed(400)));
}

#[tokio::test]
async fn a_rotated_refresh_token_survives_a_failing_read_after_the_refresh() {
    let server = MockServer::start().await;
    mount(&server, Mock::given(method("POST")).and(path("/v1/oauth/token")).respond_with(token_ok("new")).expect(1)).await;
    mount(&server, usage_mock("new", ResponseTemplate::new(500))).await;
    let mut t = tokens("old", 2);
    let err = client(&server).fetch(&mut t, false, now()).await.unwrap_err();
    assert_eq!(err, ProviderError::RequestFailed(500));
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref()), ("new", Some("r2")));
}

#[tokio::test]
async fn a_rotated_refresh_token_survives_a_failing_claim_after_the_refresh() {
    let server = MockServer::start().await;
    mount(&server, Mock::given(method("POST")).and(path("/v1/oauth/token")).respond_with(token_ok("new")).expect(1)).await;
    mount(&server, claim_mock("new", ResponseTemplate::new(429))).await;
    let mut t = tokens("old", 2);
    let outcome = client(&server).claim_reset_grant(&mut t, ORG, "g1", "req-1", now()).await.unwrap();
    assert_eq!(outcome, ClaimOutcome::Failed(ClaimFailure::Refused));
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref()), ("new", Some("r2")));

    // Also when the retry after a 401 fails with a transport-level error status.
    let server = MockServer::start().await;
    mount(&server, claim_mock("old", ResponseTemplate::new(401))).await;
    mount(&server, Mock::given(method("POST")).and(path("/v1/oauth/token")).respond_with(token_ok("new"))).await;
    mount(&server, claim_mock("new", ResponseTemplate::new(500))).await;
    let mut t = tokens("old", 60);
    client(&server).claim_reset_grant(&mut t, ORG, "g1", "req-1", now()).await.unwrap();
    assert_eq!(t.refresh_token.as_deref(), Some("r2"));
}
