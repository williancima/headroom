use super::*;
use crate::oauth::OAuthError;
use reqwest::Url;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use serde_json::json;
use std::collections::HashMap;

/// A loopback port that is free right now.
async fn free_port() -> u16 {
    tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port()
}

/// Drives the loopback with a real GET, as the browser would after the user approves.
fn approve(url: &Url) {
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    let target = format!("{}?code=abc&state={}", query["redirect_uri"], query["state"]);
    tokio::spawn(async move {
        let _ = reqwest::get(target).await;
    });
}

fn no_device_code(_: &str, _: &Url) {
    panic!("the loopback was free, so no device code should be shown");
}

#[tokio::test]
async fn login_on_the_loopback_maps_the_account_claims() {
    let server = MockServer::start().await;
    let access = jwt(json!({"https://api.openai.com/auth": {"chatgpt_account_id": "chatgpt-account"}}));
    let id_token = jwt(json!({"email": "codex@example.com"}));
    let body = json!({"access_token": access, "refresh_token": "codex-refresh", "id_token": id_token, "expires_in": 3600});
    mount(
        &server,
        token_post()
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string_contains_all(&["grant_type=authorization_code", &format!("client_id={CLIENT_ID}"), "code=abc", "redirect_uri=http%3A%2F%2Flocalhost%3A"]))
            .respond_with(json_ok(&body.to_string()))
            .expect(1),
    )
    .await;
    let port = free_port().await;
    let mut opened = None;
    let before = Utc::now();
    let (label, tokens) = CodexClient::new(config(&server, port))
        .login(
            |url| {
                opened = Some(url.clone());
                approve(url);
            },
            no_device_code,
        )
        .await
        .unwrap();
    assert_eq!(label, "codex@example.com");
    assert_eq!(tokens.extras[ACCOUNT_ID_KEY], "chatgpt-account");
    assert_eq!(tokens.extras[OAuthTokens::IDENTITY_KEY], "chatgpt-account/codex@example.com");
    assert_eq!(tokens.refresh_token.as_deref(), Some("codex-refresh"));
    let expires_at = tokens.expires_at.unwrap();
    assert!(expires_at >= before + Duration::seconds(3600) && expires_at <= Utc::now() + Duration::seconds(3600), "login expiry is expires_in");
    let q: HashMap<_, _> = opened.unwrap().query_pairs().into_owned().collect();
    assert_eq!((q["originator"].as_str(), q["scope"].as_str(), q["client_id"].as_str()), ("pi", SCOPES, CLIENT_ID));
    assert_eq!((q["id_token_add_organizations"].as_str(), q["codex_cli_simplified_flow"].as_str()), ("true", "true"));
    assert_eq!(q["redirect_uri"], format!("http://localhost:{port}/auth/callback"));
    // The code verifier sent matches the challenge in the URL.
    let sent = String::from_utf8(server.received_requests().await.unwrap()[0].body.clone()).unwrap();
    let verifier = sent.split('&').find_map(|p| p.strip_prefix("code_verifier=")).unwrap();
    assert_eq!(crate::oauth::pkce::challenge_for(verifier), q["code_challenge"]);
}

/// `body_string_contains` for each piece; the form body is sorted, so pieces are matched individually.
fn body_string_contains_all(parts: &[&str]) -> impl wiremock::Match {
    let parts: Vec<String> = parts.iter().map(|p| p.to_string()).collect();
    move |request: &wiremock::Request| {
        let body = String::from_utf8_lossy(&request.body);
        parts.iter().all(|p| body.contains(p.as_str()))
    }
}

#[tokio::test]
async fn login_without_claims_uses_the_default_label_and_no_identity() {
    let server = MockServer::start().await;
    mount(&server, token_post().respond_with(json_ok(r#"{"access_token":"opaque"}"#))).await;
    let port = free_port().await;
    let (label, tokens) = CodexClient::new(config(&server, port)).login(approve, no_device_code).await.unwrap();
    assert_eq!(label, "ChatGPT Account");
    assert!(tokens.extras.is_empty() && tokens.expires_at.is_none());
}

#[tokio::test]
async fn a_refused_exchange_fails_the_login() {
    let server = MockServer::start().await;
    mount(&server, token_post().respond_with(ResponseTemplate::new(400))).await;
    let port = free_port().await;
    let err = CodexClient::new(config(&server, port)).login(approve, no_device_code).await.unwrap_err();
    assert!(matches!(err, OAuthError::RequestFailed(400)));
}

// Real time: the mock server's socket I/O does not mix with paused time (client timeouts would fire).
// The slow-down back-off timing is covered by `oauth::poll` with paused time and `device_poll_responses`.
#[tokio::test]
async fn a_taken_port_falls_back_to_the_device_code_and_polls_until_approved() {
    let server = MockServer::start().await;
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = taken.local_addr().unwrap().port();
    mount(
        &server,
        Mock::given(method("POST"))
            .and(path("/device/usercode"))
            .and(body_partial_json(json!({"client_id": CLIENT_ID})))
            .respond_with(json_ok(r#"{"device_auth_id":"dev-1","user_code":"ABCD-1234","interval":1}"#))
            .expect(1),
    )
    .await;
    let poll_body = json!({"device_auth_id": "dev-1", "user_code": "ABCD-1234"});
    let poll = || Mock::given(method("POST")).and(path("/device/token")).and(body_partial_json(&poll_body));
    mount(&server, poll().respond_with(ResponseTemplate::new(403)).up_to_n_times(1)).await;
    mount(&server, poll().respond_with(json_ok(r#"{"authorization_code":"dev-code","code_verifier":"dev-verifier"}"#)).expect(1)).await;
    mount(
        &server,
        token_post()
            .and(body_string_contains_all(&["code=dev-code", "code_verifier=dev-verifier", "redirect_uri=http%3A%2F%2F"]))
            .and(body_string_contains_all(&["%2Fdeviceauth%2Fcallback"]))
            .respond_with(json_ok(r#"{"access_token":"opaque","refresh_token":"r"}"#))
            .expect(1),
    )
    .await;
    let mut shown = None;
    let (label, tokens) = CodexClient::new(config(&server, port))
        .login(|_| panic!("the browser must not open when the port is taken"), |code, url| shown = Some((code.to_string(), url.clone())))
        .await
        .unwrap();
    let (code, url) = shown.unwrap();
    assert_eq!((code.as_str(), url.path()), ("ABCD-1234", "/codex/device"));
    assert_eq!((label.as_str(), tokens.access_token.as_str()), ("ChatGPT Account", "opaque"));
    drop(taken);
}

#[tokio::test]
async fn an_expired_device_code_ends_the_sign_in() {
    let server = MockServer::start().await;
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = taken.local_addr().unwrap().port();
    mount(&server, Mock::given(method("POST")).and(path("/device/usercode")).respond_with(json_ok(r#"{"device_auth_id":"d","user_code":"U"}"#))).await;
    mount(&server, Mock::given(method("POST")).and(path("/device/token")).respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":"expired_token"}"#))).await;
    let err = CodexClient::new(config(&server, port)).login(|_| {}, |_, _| {}).await.unwrap_err();
    assert!(matches!(err, OAuthError::Failed(ref m) if m == "Device sign-in expired."), "{err:?}");
}
