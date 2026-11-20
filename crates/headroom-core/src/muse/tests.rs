use serde_json::json;
use wiremock::matchers::{bearer_token, body_string, body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::mapper_tests::{active, body, percent, start, KEY};
use super::*;

fn client(server: &MockServer) -> MuseClient {
    let base = server.uri();
    MuseClient::new(MuseConfig {
        device_start_url: format!("{base}/oidc/device/authorization/"),
        device_token_url: format!("{base}/oidc/device/token/"),
        key_url: format!("{base}/muse-code/key"),
    })
}

fn ok(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

fn tokens() -> OAuthTokens {
    OAuthTokens { access_token: "dca:token".into(), refresh_token: None, expires_at: None, extras: Default::default() }
}

#[tokio::test]
async fn one_key_read_gives_the_plan_and_windows_with_the_documented_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/muse-code/key")).and(bearer_token("dca:token")).and(header("x-api-version", "1.0.0"))
        .and(header("content-type", "application/json")).and(body_string(r#"{"onboard":false}"#))
        .respond_with(ok(&active())).expect(1).mount(&server).await;
    let mut t = tokens();
    let snapshot = client(&server).fetch(&mut t, None, start()).await.unwrap();
    assert_eq!(snapshot.plan.as_deref(), Some("Power Usage"));
    assert_eq!(snapshot.lines.len(), 2);
    assert_eq!(t, tokens(), "nothing refreshes");
    assert!(!serde_json::to_string(&snapshot).unwrap().contains("SYNTHETIC-inference-key"));
}

#[tokio::test]
async fn an_idle_account_keeps_the_previous_snapshots_values() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/muse-code/key")).respond_with(ok(&body(true, None))).mount(&server).await;
    let previous = ProviderSnapshot {
        plan: Some("Power Usage".into()),
        lines: vec![percent("Session", 42.0, Some(start() + chrono::Duration::hours(3)), 18_000_000)],
        refreshed_at: start(),
        reset_expiries: vec![], reset_items: vec![],
        notice: None,
        breakdowns: Default::default(),
    };
    let snapshot = client(&server).fetch(&mut tokens(), Some(&previous), start()).await.unwrap();
    assert_eq!(snapshot.lines[0], previous.lines[0]);
    assert!(matches!(&snapshot.lines[1], MetricLine::Progress { used, .. } if *used == 0.0));
}

#[tokio::test]
async fn http_failures_map_to_provider_errors_without_the_key() {
    for (status, expected) in [
        (401, ProviderError::SignInExpired),
        (403, ProviderError::SignInExpired),
        (500, ProviderError::RequestFailed(500)),
        (429, ProviderError::RateLimited { retry_after_secs: Some(120) }),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/muse-code/key"))
            .respond_with(ResponseTemplate::new(status).insert_header("retry-after", "120").set_body_string(active())).mount(&server).await;
        let error = client(&server).fetch(&mut tokens(), None, start()).await.unwrap_err();
        assert_eq!(error, expected, "{status}");
        assert!(!error.to_string().contains(KEY));
    }
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/muse-code/key")).respond_with(ok(&format!(r#"{{"api_key":"{KEY}","is_subs_active":"yes"}}"#))).mount(&server).await;
    let error = client(&server).fetch(&mut tokens(), None, start()).await.unwrap_err();
    assert_eq!(error, ProviderError::InvalidResponse);
}

async fn mount_device(server: &MockServer, email: Option<&str>) {
    Mock::given(method("POST")).and(path("/oidc/device/authorization/")).and(body_string(format!("client_id={CLIENT_ID}")))
        .respond_with(ok(r#"{"device_code":"d","user_code":"META-1","verification_uri":"https://meta.test/activate","expires_in":600,"interval":0}"#)).expect(1).mount(server).await;
    Mock::given(method("POST")).and(path("/oidc/device/token/")).and(body_string_contains("device_code=d"))
        .respond_with(ok(r#"{"access_token":"dca:abc"}"#)).mount(server).await;
    let key = match email {
        Some(e) => json!({"user_email": e, "is_subs_active": true}),
        None => json!({"is_subs_active": true}),
    };
    Mock::given(method("POST")).and(path("/muse-code/key")).and(bearer_token("dca:abc")).respond_with(ok(&key.to_string())).expect(1).mount(server).await;
}

#[tokio::test]
async fn login_sends_no_scope_and_names_the_account_by_email() {
    let server = MockServer::start().await;
    mount_device(&server, Some(" muse@example.com ")).await;
    let mut shown = None;
    let (label, tokens) = client(&server).login(|c, u| shown = Some((c.to_string(), u.to_string()))).await.unwrap();
    assert_eq!(label, "muse@example.com");
    assert_eq!(shown, Some(("META-1".into(), "https://meta.test/activate".into())));
    assert_eq!(tokens.extras[OAuthTokens::IDENTITY_KEY], "muse:muse@example.com");
    assert_eq!((tokens.access_token.as_str(), tokens.refresh_token, tokens.expires_at), ("dca:abc", None, None));
}

#[tokio::test]
async fn login_without_an_email_is_named_muse_code_with_no_identity() {
    let server = MockServer::start().await;
    mount_device(&server, None).await;
    let (label, tokens) = client(&server).login(|_, _| {}).await.unwrap();
    assert_eq!(label, "Muse Code");
    assert!(tokens.extras.is_empty());
}
