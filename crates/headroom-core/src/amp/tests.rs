use std::collections::HashMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use wiremock::matchers::{bearer_token, body_json, body_string, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::mapper_tests::{balance, MEGAWATT};
use super::*;

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_794_290_000, 0).unwrap()
}

fn jwt(payload: serde_json::Value) -> String {
    format!("eyJhbGciOiJub25lIn0.{}.signature", URL_SAFE_NO_PAD.encode(payload.to_string()))
}

fn tokens(access: &str, expires_in_secs: i64) -> OAuthTokens {
    OAuthTokens {
        access_token: access.into(),
        refresh_token: Some("refresh-1".into()),
        expires_at: Some(now() + Duration::seconds(expires_in_secs)),
        extras: HashMap::from([("keep".into(), "me".into())]),
    }
}

fn client(server: &MockServer) -> AmpClient {
    let base = server.uri();
    AmpClient::new(AmpConfig {
        device_start_url: format!("{base}/authorize/device"),
        token_url: format!("{base}/authenticate"),
        verification_page: "https://ampcode.com/auth/cli/device".into(),
        balance_url: format!("{base}/api/internal?userDisplayBalanceInfo"),
    })
}

fn ok(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

#[tokio::test]
async fn the_balance_read_sends_the_documented_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/internal")).and(bearer_token("amp-access")).and(header("content-type", "application/json"))
        .and(body_string(r#"{"method":"userDisplayBalanceInfo","params":{}}"#))
        .respond_with(ok(MEGAWATT)).expect(1).mount(&server).await;
    let snapshot = client(&server).fetch(&mut tokens("amp-access", 3600), now()).await.unwrap();
    assert_eq!(snapshot.plan.as_deref(), Some("Megawatt"));
    assert_eq!(snapshot.lines.len(), 3);
}

#[tokio::test]
async fn an_expiring_token_is_renewed_first_and_the_rotation_is_kept() {
    let renewed = jwt(json!({"sub": "user_1", "exp": now().timestamp() + 300}));
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/authenticate"))
        .and(body_json(json!({"client_id": CLIENT_ID, "grant_type": "refresh_token", "refresh_token": "refresh-1"})))
        .respond_with(ok(&json!({"user": {"id": "user_1"}, "access_token": renewed, "refresh_token": "refresh-2"}).to_string())).expect(1).mount(&server).await;
    Mock::given(method("POST")).and(path("/api/internal")).and(bearer_token(renewed.as_str())).respond_with(ok(MEGAWATT)).expect(1).mount(&server).await;
    let mut t = tokens("amp-old", 60);
    client(&server).fetch(&mut t, now()).await.unwrap();
    assert_eq!(t.refresh_token.as_deref(), Some("refresh-2"), "WorkOS rotates the refresh token on every renewal");
    assert_eq!(t.expires_at, Some(now() + Duration::seconds(300)));
    assert_eq!(t.extras["keep"], "me");
}

#[tokio::test]
async fn a_refused_renewal_asks_to_sign_in_again() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/authenticate"))
        .respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid_grant","error_description":"Invalid refresh token."}"#)).mount(&server).await;
    let error = client(&server).fetch(&mut tokens("amp-old", -1), now()).await.unwrap_err();
    assert_eq!(error, ProviderError::SignInExpired);
}

#[tokio::test]
async fn a_401_renews_once_and_retries_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/internal")).and(bearer_token("old")).respond_with(ResponseTemplate::new(401)).expect(1).mount(&server).await;
    Mock::given(method("POST")).and(path("/authenticate"))
        .respond_with(ok(r#"{"access_token":"new","refresh_token":"refresh-2"}"#)).expect(1).mount(&server).await;
    Mock::given(method("POST")).and(path("/api/internal")).and(bearer_token("new")).respond_with(ok(MEGAWATT)).expect(1).mount(&server).await;
    let mut t = tokens("old", 3600);
    client(&server).fetch(&mut t, now()).await.unwrap();
    assert_eq!(t.access_token, "new");
}

#[tokio::test]
async fn http_failures_and_unreadable_text_map_to_provider_errors() {
    for (status, expected) in [(403, ProviderError::SignInExpired), (429, ProviderError::RateLimited { retry_after_secs: None }), (500, ProviderError::RequestFailed(500))] {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/api/internal")).respond_with(ResponseTemplate::new(status)).mount(&server).await;
        let mut t = tokens("a", 3600);
        t.refresh_token = None;
        assert_eq!(client(&server).fetch(&mut t, now()).await.unwrap_err(), expected, "{status}");
    }
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/internal")).respond_with(ok(&balance("Amp is down"))).mount(&server).await;
    assert_eq!(client(&server).fetch(&mut tokens("a", 3600), now()).await.unwrap_err(), ProviderError::InvalidResponse);
}

#[tokio::test]
async fn login_proves_it_can_read_usage_and_takes_the_email_and_jwt_identity() {
    let access = jwt(json!({"sub": "user_9", "exp": 1_900_000_000}));
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/authorize/device")).and(body_string(format!("client_id={CLIENT_ID}")))
        .respond_with(ok(r#"{"device_code":"dc","user_code":"UC","expires_in":600,"interval":0,"verification_uri":"https://auth.test/x"}"#)).expect(1).mount(&server).await;
    Mock::given(method("POST")).and(path("/authenticate"))
        .respond_with(ok(&json!({"access_token": access, "refresh_token": "r"}).to_string())).mount(&server).await;
    Mock::given(method("POST")).and(path("/api/internal")).and(bearer_token(access.as_str())).respond_with(ok(MEGAWATT)).expect(1).mount(&server).await;
    let mut shown = None;
    let (label, tokens) = client(&server).login(|code, url| shown = Some((code.to_string(), url.to_string()))).await.unwrap();
    assert_eq!(label, "amp@example.com");
    assert_eq!(shown, Some(("UC".into(), "https://ampcode.com/auth/cli/device?user_code=UC".into())));
    assert_eq!(tokens.extras[OAuthTokens::IDENTITY_KEY], "amp:user_9");
    assert_eq!(tokens.refresh_token.as_deref(), Some("r"));
    assert_eq!(tokens.expires_at, DateTime::from_timestamp(1_900_000_000, 0), "the expiry is the JWT's exp");
}

#[tokio::test]
async fn login_fails_when_the_balance_cannot_be_read() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/authorize/device"))
        .respond_with(ok(r#"{"device_code":"dc","user_code":"UC","expires_in":600,"interval":0}"#)).mount(&server).await;
    Mock::given(method("POST")).and(path("/authenticate")).respond_with(ok(r#"{"access_token":"a"}"#)).mount(&server).await;
    Mock::given(method("POST")).and(path("/api/internal")).respond_with(ResponseTemplate::new(500)).mount(&server).await;
    let error = client(&server).login(|_, _| {}).await.unwrap_err();
    assert!(matches!(error, crate::oauth::OAuthError::RequestFailed(500)));
}
