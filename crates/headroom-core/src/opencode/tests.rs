use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use wiremock::matchers::{bearer_token, body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

const USAGE: &str = r#"{"usage":{"rolling":{"percent":12,"resetsAt":"2026-07-12T13:30:00Z"},"weekly":{"percent":8,"resetsAt":"2026-07-13T00:00:00Z"},"monthly":{"percent":40,"resetsAt":"2026-08-04T11:18:32Z"}}}"#;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-19T12:00:00Z").unwrap().into()
}

fn tokens(access: &str, expires_in_min: i64) -> OAuthTokens {
    OAuthTokens {
        access_token: access.into(),
        refresh_token: Some("r1".into()),
        expires_at: Some(now() + Duration::minutes(expires_in_min)),
        extras: HashMap::from([(ORG_KEY.into(), "wrk_01".into())]),
    }
}

fn client(server: &MockServer) -> OpenCodeClient {
    let base = server.uri();
    OpenCodeClient::new(OpenCodeConfig {
        device_start_url: format!("{base}/device/code"),
        token_url: format!("{base}/device/token"),
        user_url: format!("{base}/user"),
        usage_url: format!("{base}/usage"),
        go_status_url: format!("{base}/go/status"),
    })
}

fn ok(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

#[tokio::test]
async fn a_signed_in_account_shows_the_plan_and_sends_the_workspace() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/usage")).and(bearer_token("console")).and(header("x-opencode-org-id", "wrk_01")).and(header("accept", "application/json"))
        .respond_with(ok(USAGE)).expect(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/go/status")).and(bearer_token("console")).and(header("x-org-id", "wrk_01"))
        .respond_with(ok(r#"{"product":"go-plus","access":{}}"#)).expect(1).mount(&server).await;
    let fetched = client(&server).fetch(&mut tokens("console", 60), None, true, now()).await.unwrap();
    assert_eq!(fetched.snapshot.plan.as_deref(), Some("Go Plus"));
    assert_eq!(fetched.plan.as_deref(), Some("Go Plus"));
    let labels: Vec<_> = fetched.snapshot.lines.iter().map(|l| l.label().to_string()).collect();
    assert_eq!(labels, ["Session", "Weekly", "Monthly"]);
}

#[tokio::test]
async fn between_plan_reads_the_cached_plan_is_kept_and_status_is_not_called() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/usage")).respond_with(ok(USAGE)).mount(&server).await;
    Mock::given(method("GET")).and(path("/go/status")).respond_with(ok(r#"{"product":"go"}"#)).expect(0).mount(&server).await;
    let fetched = client(&server).fetch(&mut tokens("console", 60), Some("Go".into()), false, now()).await.unwrap();
    assert_eq!(fetched.snapshot.plan.as_deref(), Some("Go"));
}

#[tokio::test]
async fn a_failed_plan_read_keeps_the_usage_and_drops_the_plan() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/usage")).respond_with(ok(USAGE)).mount(&server).await;
    Mock::given(method("GET")).and(path("/go/status")).respond_with(ResponseTemplate::new(500).set_body_string("{}")).mount(&server).await;
    let fetched = client(&server).fetch(&mut tokens("console", 60), Some("Go".into()), true, now()).await.unwrap();
    assert_eq!((fetched.snapshot.plan, fetched.plan), (None, None));
    assert_eq!(fetched.snapshot.lines.len(), 3);
}

#[tokio::test]
async fn the_date_header_is_the_capture_time() {
    let server = MockServer::start().await;
    // Placeholder relative to the server clock; the local `now` is a day off.
    let usage = r#"{"usage":{"rolling":{"percent":0,"resetsAt":"2027-01-15T13:00:00.500Z"},"weekly":{"percent":0,"resetsAt":"2027-01-18T00:00:00Z"},"monthly":{"percent":0,"resetsAt":"2027-02-10T08:00:00Z"}}}"#;
    Mock::given(method("GET")).and(path("/usage")).respond_with(ok(usage).insert_header("date", "Fri, 15 Jan 2027 08:00:00 GMT")).mount(&server).await;
    let fetched = client(&server).fetch(&mut tokens("console", 60), None, false, now()).await.unwrap();
    assert!(matches!(&fetched.snapshot.lines[0], crate::models::MetricLine::Progress { resets_at: None, .. }));
}

#[tokio::test]
async fn an_entitlement_error_means_no_go_subscription() {
    let server = MockServer::start().await;
    let body = r#"{"type":"error","error":{"type":"EntitlementError","message":"x"}}"#;
    Mock::given(method("GET")).and(path("/usage")).respond_with(ResponseTemplate::new(403).set_body_string(body)).mount(&server).await;
    let error = client(&server).fetch(&mut tokens("console", 60), None, false, now()).await.unwrap_err();
    assert_eq!(error, ProviderError::Unavailable("No OpenCode Go subscription on this account.".into()));
}

#[tokio::test]
async fn http_failures_map_to_provider_errors() {
    for (status, expected) in [
        (401, ProviderError::SignInExpired),
        (403, ProviderError::SignInExpired),
        (429, ProviderError::RateLimited { retry_after_secs: None }),
        (500, ProviderError::RequestFailed(500)),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/usage")).respond_with(ResponseTemplate::new(status)).mount(&server).await;
        let mut t = tokens("console", 60);
        t.refresh_token = None;
        assert_eq!(client(&server).fetch(&mut t, None, false, now()).await.unwrap_err(), expected, "{status}");
    }
}

#[tokio::test]
async fn a_due_token_is_refreshed_first_and_the_rotation_is_kept() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/device/token")).and(body_string_contains("grant_type=refresh_token")).and(body_string_contains("client_id=opencode-cli")).and(body_string_contains("refresh_token=r1"))
        .respond_with(ok(&json!({"access_token": "fresh", "refresh_token": "r2", "expires_in": 3600}).to_string())).expect(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/usage")).and(bearer_token("fresh")).respond_with(ok(USAGE)).expect(1).mount(&server).await;
    let mut t = tokens("stale", 1);
    client(&server).fetch(&mut t, None, false, now()).await.unwrap();
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref()), ("fresh", Some("r2")));
    assert_eq!(t.expires_at, Some(now() + Duration::hours(1)));
    assert_eq!(t.extras[ORG_KEY], "wrk_01");
}

#[tokio::test]
async fn a_401_refreshes_once_and_retries_and_a_refused_refresh_expires_the_sign_in() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/usage")).and(bearer_token("old")).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    Mock::given(method("POST")).and(path("/device/token")).respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid_grant"}"#)).mount(&server).await;
    let error = client(&server).fetch(&mut tokens("old", 60), None, false, now()).await.unwrap_err();
    assert_eq!(error, ProviderError::SignInExpired);
}

#[tokio::test]
async fn login_returns_the_email_and_stores_identity_and_workspace() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/device/code")).and(body_string_contains("supports_org_scope=true")).and(body_string_contains("client_id=opencode-cli"))
        .respond_with(ok(r#"{"device_code":"dc","user_code":"UC","expires_in":600,"interval":0,"verification_uri":"https://opencode.ai/device"}"#)).expect(1).mount(&server).await;
    Mock::given(method("POST")).and(path("/device/token"))
        .respond_with(ok(r#"{"access_token":"a","refresh_token":"r","expires_in":3600,"org_id":"wrk_9"}"#)).mount(&server).await;
    Mock::given(method("GET")).and(path("/user")).and(bearer_token("a")).and(header("x-org-id", "wrk_9"))
        .respond_with(ok(r#"{"id":"usr_1","email":"me@example.com"}"#)).expect(1).mount(&server).await;
    let mut shown = None;
    let (label, tokens) = client(&server).login(|code, url| shown = Some((code.to_string(), url.to_string()))).await.unwrap();
    assert_eq!(label, "me@example.com");
    assert_eq!(shown, Some(("UC".into(), "https://opencode.ai/device".into())));
    assert_eq!(tokens.extras[OAuthTokens::IDENTITY_KEY], "opencode:usr_1:wrk_9");
    assert_eq!(tokens.extras[ORG_KEY], "wrk_9");
    assert_eq!(tokens.refresh_token.as_deref(), Some("r"));
    assert!(tokens.expires_at.is_some());
}

#[tokio::test]
async fn login_without_a_workspace_is_an_invalid_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/device/code"))
        .respond_with(ok(r#"{"device_code":"dc","user_code":"UC","expires_in":600,"interval":0,"verification_uri":"https://opencode.ai/device"}"#)).mount(&server).await;
    Mock::given(method("POST")).and(path("/device/token")).respond_with(ok(r#"{"access_token":"a"}"#)).mount(&server).await;
    let error = client(&server).login(|_, _| {}).await.unwrap_err();
    assert!(matches!(error, crate::oauth::OAuthError::InvalidResponse));
}
