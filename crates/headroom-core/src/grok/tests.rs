use std::collections::HashMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use wiremock::matchers::{bearer_token, body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

const CREDITS: &str = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-06-30T21:36:52Z","end":"2026-07-07T21:36:52Z"},"creditUsagePercent":40}}"#;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-19T12:00:00Z").unwrap().into()
}

fn jwt(payload: serde_json::Value) -> String {
    format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(payload.to_string()))
}

fn tokens(access: &str, expires_in_min: i64) -> OAuthTokens {
    OAuthTokens {
        access_token: access.into(),
        refresh_token: Some("r1".into()),
        expires_at: Some(now() + Duration::minutes(expires_in_min)),
        extras: HashMap::from([("keep".into(), "me".into())]),
    }
}

fn client(server: &MockServer) -> GrokClient {
    let base = server.uri();
    GrokClient::new(GrokConfig {
        device_start_url: format!("{base}/oauth2/device/code"),
        token_url: format!("{base}/oauth2/token"),
        settings_url: format!("{base}/v1/settings"),
        credits_url: format!("{base}/v1/billing?format=credits"),
    })
}

fn ok(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

#[tokio::test]
async fn usage_and_plan_are_read_with_the_grok_cli_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/billing")).and(query_param("format", "credits")).and(bearer_token("grok"))
        .and(header("x-xai-token-auth", "xai-grok-cli")).and(header("user-agent", "Headroom")).and(header("accept", "application/json"))
        .respond_with(ok(CREDITS)).expect(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/v1/settings")).and(bearer_token("grok")).and(header("x-xai-token-auth", "xai-grok-cli"))
        .respond_with(ok(r#"{"subscription_tier_display":"SuperGrok"}"#)).expect(1).mount(&server).await;
    let fetched = client(&server).fetch(&mut tokens("grok", 60), None, true, now()).await.unwrap();
    assert_eq!(fetched.snapshot.plan.as_deref(), Some("SuperGrok"));
    assert_eq!(fetched.plan.as_deref(), Some("SuperGrok"));
    assert_eq!(fetched.snapshot.lines.len(), 1);
}

#[tokio::test]
async fn between_plan_reads_the_cached_plan_is_kept_and_a_failed_read_drops_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/billing")).respond_with(ok(CREDITS)).mount(&server).await;
    Mock::given(method("GET")).and(path("/v1/settings")).respond_with(ResponseTemplate::new(500)).expect(1).mount(&server).await;
    let c = client(&server);
    let cached = c.fetch(&mut tokens("grok", 60), Some("Old".into()), false, now()).await.unwrap();
    assert_eq!(cached.snapshot.plan.as_deref(), Some("Old"));
    let failed = c.fetch(&mut tokens("grok", 60), Some("Old".into()), true, now()).await.unwrap();
    assert_eq!((failed.snapshot.plan, failed.plan), (None, None));
}

#[tokio::test]
async fn a_team_principal_gets_a_notice_instead_of_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/billing"))
        .respond_with(ResponseTemplate::new(412).set_body_string(r#"{"error":"no personal team"}"#)).mount(&server).await;
    let fetched = client(&server).fetch(&mut tokens("grok", 60), None, false, now()).await.unwrap();
    assert!(fetched.snapshot.lines.is_empty());
    assert_eq!(fetched.snapshot.notice.as_deref(), Some(mapper::TEAM_NOTICE));
}

#[tokio::test]
async fn http_failures_map_to_provider_errors() {
    for (status, expected) in [
        (401, ProviderError::SignInExpired),
        (403, ProviderError::SignInExpired),
        (412, ProviderError::RequestFailed(412)),
        (429, ProviderError::RateLimited { retry_after_secs: None }),
        (500, ProviderError::RequestFailed(500)),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/v1/billing")).respond_with(ResponseTemplate::new(status).set_body_string(r#"{"error":"precondition failed"}"#)).mount(&server).await;
        let mut t = tokens("grok", 60);
        t.refresh_token = None;
        assert_eq!(client(&server).fetch(&mut t, None, false, now()).await.unwrap_err(), expected, "{status}");
    }
}

#[tokio::test]
async fn a_due_token_is_refreshed_first_and_the_rotation_is_kept() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/oauth2/token"))
        .and(body_string_contains("grant_type=refresh_token")).and(body_string_contains(format!("client_id={CLIENT_ID}"))).and(body_string_contains("refresh_token=r1"))
        .respond_with(ok(&json!({"access_token": "fresh", "refresh_token": "r2", "expires_in": 3600}).to_string())).expect(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/v1/billing")).and(bearer_token("fresh")).respond_with(ok(CREDITS)).expect(1).mount(&server).await;
    let mut t = tokens("stale", 1);
    client(&server).fetch(&mut t, None, false, now()).await.unwrap();
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref()), ("fresh", Some("r2")));
    assert_eq!(t.expires_at, Some(now() + Duration::hours(1)));
    assert_eq!(t.extras["keep"], "me");
}

#[tokio::test]
async fn a_401_renews_once_and_retries_and_a_refused_renewal_expires_the_sign_in() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/billing")).and(bearer_token("old")).respond_with(ResponseTemplate::new(401)).expect(1).mount(&server).await;
    Mock::given(method("POST")).and(path("/oauth2/token")).respond_with(ok(r#"{"access_token":"new","refresh_token":"r2"}"#)).expect(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/v1/billing")).and(bearer_token("new")).respond_with(ok(CREDITS)).expect(1).mount(&server).await;
    let mut t = tokens("old", 60);
    client(&server).fetch(&mut t, None, false, now()).await.unwrap();
    assert_eq!(t.access_token, "new");

    let refused = MockServer::start().await;
    Mock::given(method("POST")).and(path("/oauth2/token")).respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid_grant"}"#)).mount(&refused).await;
    let error = client(&refused).fetch(&mut tokens("old", -1), None, false, now()).await.unwrap_err();
    assert_eq!(error, ProviderError::SignInExpired);
}

#[tokio::test]
async fn login_maps_the_email_and_identity_from_the_token() {
    let access = jwt(json!({"email": "grok@example.com", "sub": "xai-user"}));
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/oauth2/device/code"))
        .and(body_string_contains(format!("client_id={CLIENT_ID}"))).and(body_string_contains("scope=openid%20email%20offline_access%20grok-cli%3Aaccess"))
        .respond_with(ok(r#"{"device_code":"device","user_code":"GROK-CODE","verification_uri":"https://auth.x.ai/activate","expires_in":600,"interval":1}"#)).expect(1).mount(&server).await;
    Mock::given(method("POST")).and(path("/oauth2/token")).and(body_string_contains("device_code=device"))
        .respond_with(ok(&json!({"access_token": access, "refresh_token": "grok-refresh", "expires_in": 3600}).to_string())).mount(&server).await;
    let mut shown = None;
    let (label, tokens) = client(&server).login(|c, u| shown = Some((c.to_string(), u.to_string()))).await.unwrap();
    assert_eq!(label, "grok@example.com");
    assert_eq!(shown, Some(("GROK-CODE".into(), "https://auth.x.ai/activate".into())));
    assert_eq!(tokens.extras[OAuthTokens::IDENTITY_KEY], "xai-user");
    assert_eq!(tokens.refresh_token.as_deref(), Some("grok-refresh"));
}

#[tokio::test]
async fn login_without_claims_is_a_generic_account_with_no_identity() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/oauth2/device/code"))
        .respond_with(ok(r#"{"device_code":"d","user_code":"C","verification_uri":"https://auth.x.ai/activate","expires_in":600,"interval":0}"#)).mount(&server).await;
    Mock::given(method("POST")).and(path("/oauth2/token")).respond_with(ok(r#"{"access_token":"opaque"}"#)).mount(&server).await;
    let (label, tokens) = client(&server).login(|_, _| {}).await.unwrap();
    assert_eq!(label, "Grok Account");
    assert!(tokens.extras.is_empty());
}
