use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::oauth::pkce::challenge_for;

const USER_STATUS: &str = r#"{"userStatus":{"email":"rob@example.com","teamId":"devin-team$account-1","planStatus":{"planInfo":{"planName":"Max"},"dailyQuotaRemainingPercent":80,"weeklyQuotaRemainingPercent":25}}}"#;
const STATUS_PATH: &str = "/exa.seat_management_pb.SeatManagementService/GetUserStatus";

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000, 0).unwrap()
}

fn client(server: &MockServer) -> DevinClient {
    let base = server.uri();
    DevinClient::new(DevinConfig {
        authorize_url: format!("{base}/auth/cli/continue"),
        token_url: format!("{base}/auth/cli/token"),
        api_server_url: base,
    })
}

fn json_ok(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

fn tokens(access: &str) -> OAuthTokens {
    OAuthTokens { access_token: access.into(), refresh_token: None, expires_at: None, extras: HashMap::new() }
}

/// Completes the loopback callback the way a browser would and records the authorize URL.
fn browse(seen: Arc<Mutex<Option<Url>>>) -> impl FnOnce(&Url) {
    move |url| {
        *seen.lock().unwrap() = Some(url.clone());
        let q: HashMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let callback = format!("{}?code=fake-authorization-code&state={}", q["redirect_uri"], q["state"]);
        tokio::spawn(async move {
            let _ = reqwest::get(callback).await;
        });
    }
}

use reqwest::Url;

#[tokio::test]
async fn sign_in_runs_the_devin_cli_flow_and_reads_the_user_status_with_the_session_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/cli/token"))
        .respond_with(json_ok(r#"{"token":"eyJ.session.jwt","webapp_host":"app.devin.ai"}"#))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(STATUS_PATH))
        .and(header("content-type", "application/json"))
        .and(header("connect-protocol-version", "1"))
        .and(body_json(json!({"metadata": {
            "apiKey": "devin-session-token$eyJ.session.jwt", "ideName": "devin", "ideVersion": "1.108.2",
            "extensionName": "devin", "extensionVersion": "1.108.2", "locale": "en"
        }})))
        .respond_with(json_ok(USER_STATUS))
        .expect(1)
        .mount(&server)
        .await;
    let seen = Arc::new(Mutex::new(None));
    let (label, t) = client(&server).login(browse(seen.clone())).await.unwrap();

    let url = seen.lock().unwrap().clone().unwrap();
    let q: HashMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    assert_eq!(url.path(), "/auth/cli/continue");
    let keys: Vec<_> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();
    assert_eq!(keys, ["redirect_uri", "state", "prompt", "code_challenge", "code_challenge_method"]);
    let redirect = Url::parse(&q["redirect_uri"]).unwrap();
    assert_eq!((redirect.host_str(), redirect.path(), redirect.scheme()), (Some("127.0.0.1"), "/callback", "http"));
    assert_eq!((q["prompt"].as_str(), q["code_challenge_method"].as_str()), ("select_account", "S256"));

    let requests = server.received_requests().await.unwrap();
    let exchange: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(exchange["code"], "fake-authorization-code");
    assert_eq!(q["code_challenge"], challenge_for(exchange["code_verifier"].as_str().unwrap()));

    assert_eq!(label, "rob@example.com");
    assert_eq!(t.access_token, "devin-session-token$eyJ.session.jwt");
    assert_eq!((t.refresh_token, t.expires_at), (None, None));
    assert_eq!(t.extras, HashMap::from([(OAuthTokens::IDENTITY_KEY.to_string(), "devin:rob@example.com/devin-team$account-1".to_string())]));
}

#[tokio::test]
async fn a_token_that_already_has_the_prefix_is_kept_and_a_missing_email_uses_the_generic_label() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/auth/cli/token")).respond_with(json_ok(r#"{"token":"devin-session-token$abc"}"#)).mount(&server).await;
    Mock::given(method("POST")).and(path(STATUS_PATH)).respond_with(json_ok(r#"{"userStatus":{}}"#)).mount(&server).await;
    let (label, t) = client(&server).login(browse(Arc::default())).await.unwrap();
    assert_eq!(label, "Devin Account");
    assert_eq!(t.access_token, "devin-session-token$abc");
    assert!(t.extras.is_empty());
}

#[tokio::test]
async fn the_identity_has_no_team_suffix_without_a_team_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/auth/cli/token")).respond_with(json_ok(r#"{"token":"t"}"#)).mount(&server).await;
    Mock::given(method("POST")).and(path(STATUS_PATH)).respond_with(json_ok(r#"{"userStatus":{"email":"a@b.c"}}"#)).mount(&server).await;
    let (_, t) = client(&server).login(browse(Arc::default())).await.unwrap();
    assert_eq!(t.extras[OAuthTokens::IDENTITY_KEY], "devin:a@b.c");
}

#[tokio::test]
async fn a_refused_code_exchange_says_so_and_reads_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(401).set_body_string(r#"{"detail":"invalid code"}"#)).mount(&server).await;
    let error = client(&server).login(browse(Arc::default())).await.unwrap_err();
    assert_eq!(error.to_string(), "Devin didn’t accept the sign-in. Click Try Again to sign in again.");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_failed_user_status_read_after_sign_in_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/auth/cli/token")).respond_with(json_ok(r#"{"token":"t"}"#)).mount(&server).await;
    Mock::given(method("POST")).and(path(STATUS_PATH)).respond_with(ResponseTemplate::new(500)).mount(&server).await;
    let error = client(&server).login(browse(Arc::default())).await.unwrap_err();
    assert_eq!(error.to_string(), "Devin signed you in but didn’t return your account. Click Try Again to sign in again.");
}

#[tokio::test]
async fn an_empty_token_is_an_invalid_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(json_ok(r#"{"token":""}"#)).mount(&server).await;
    let error = client(&server).login(browse(Arc::default())).await.unwrap_err();
    assert!(matches!(error, OAuthError::InvalidResponse));
}

use crate::oauth::OAuthError;

#[tokio::test]
async fn a_refresh_reads_usage_from_the_accounts_own_server() {
    let default_server = MockServer::start().await;
    let own_server = MockServer::start().await;
    Mock::given(method("POST")).and(path(STATUS_PATH)).respond_with(json_ok(USER_STATUS)).expect(1).mount(&own_server).await;
    let mut t = tokens("devin-session-token$abc");
    t.extras.insert(API_SERVER_URL_KEY.into(), own_server.uri());
    let snapshot = client(&default_server).fetch(&t, now()).await.unwrap();
    assert_eq!(snapshot.plan.as_deref(), Some("Max"));
    assert_eq!(snapshot.lines.iter().map(|l| l.label()).collect::<Vec<_>>(), ["Daily quota", "Weekly quota"]);
    assert!(default_server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn usage_failures_map_and_nothing_is_retried() {
    let cases = [
        (401, None, ProviderError::SignInExpired),
        (403, None, ProviderError::SignInExpired),
        (403, Some("30"), ProviderError::RateLimited { retry_after_secs: Some(30) }),
        (429, Some("7"), ProviderError::RateLimited { retry_after_secs: Some(7) }),
        (500, None, ProviderError::RequestFailed(500)),
    ];
    for (status, retry_after, expected) in cases {
        let server = MockServer::start().await;
        let mut response = ResponseTemplate::new(status);
        if let Some(value) = retry_after {
            response = response.insert_header("retry-after", value);
        }
        Mock::given(method("POST")).and(path(STATUS_PATH)).respond_with(response).expect(1).mount(&server).await;
        assert_eq!(client(&server).fetch(&tokens("t"), now()).await.unwrap_err(), expected, "{status}");
    }
}
