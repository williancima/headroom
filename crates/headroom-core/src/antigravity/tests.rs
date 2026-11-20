use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use reqwest::Url;
use serde_json::json;
use wiremock::matchers::{body_json, body_string, header, method, path, query_param};
use wiremock::{Mock, MockBuilder, MockServer, ResponseTemplate};

use super::*;
use crate::models::MetricLine;

const LOAD: &str = r#"{"cloudaicompanionProject":"fake-project-123","currentTier":{"id":"free-tier","name":"Antigravity Starter Quota"},"paidTier":{"id":"free-tier","name":"Antigravity Starter Quota"}}"#;
const SUBSCRIPTION_REQUIRED: &str = r#"{"error":{"code":403,"details":[{"reason":"SUBSCRIPTION_REQUIRED"}]}}"#;
const QUOTA: &str = "/v1internal:retrieveUserQuotaSummary";
const CODE_ASSIST: &str = "/v1internal:loadCodeAssist";

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-19T12:00:00Z").unwrap().into()
}

fn tokens(access: &str) -> OAuthTokens {
    OAuthTokens {
        access_token: access.into(),
        refresh_token: Some("r1".into()),
        expires_at: Some(now() + Duration::hours(1)),
        extras: HashMap::from([("keep".into(), "me".into())]),
    }
}

fn config(server: &MockServer) -> AntigravityConfig {
    let base = server.uri();
    AntigravityConfig {
        authorize_url: format!("{base}/authorize"),
        token_url: format!("{base}/token"),
        userinfo_url: format!("{base}/userinfo?alt=json"),
        loopback_port: 0,
        production_url: format!("{base}/prod"),
        daily_url: format!("{base}/daily"),
    }
}

fn client(server: &MockServer) -> AntigravityClient {
    AntigravityClient::new(config(server))
}

fn json_ok(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

fn post(prefix: &str, rpc: &str) -> MockBuilder {
    Mock::given(method("POST")).and(path(format!("{prefix}{rpc}")))
}

fn bucket(id: &str, fraction: Option<f64>, reset: Option<&str>) -> String {
    let mut fields = vec![format!(r#""bucketId":"{id}""#)];
    fields.extend(fraction.map(|f| format!(r#""remainingFraction":{f}"#)));
    fields.extend(reset.map(|r| format!(r#""resetTime":"{r}""#)));
    format!("{{{}}}", fields.join(","))
}

fn summary(buckets: &[String]) -> String {
    format!(r#"{{"groups":[{{"displayName":"fake","buckets":[{}]}}]}}"#, buckets.join(","))
}

fn daily_summary() -> String {
    summary(&[
        bucket("gemini-5h", Some(0.99965), Some("2027-03-02T14:00:00Z")),
        bucket("gemini-weekly", Some(0.9742), Some("2027-03-08T09:00:00Z")),
        bucket("3p-5h", Some(0.5), Some("2027-03-02T15:00:00Z")),
        bucket("3p-weekly", Some(0.4), Some("2027-03-08T09:00:00Z")),
    ])
}

fn production_summary() -> String {
    summary(&[
        bucket("gemini-5h", Some(1.0), Some("2027-03-02T19:00:00Z")),
        bucket("gemini-weekly", Some(1.0), Some("2027-03-09T00:00:00Z")),
        bucket("3p-5h", Some(0.9), Some("2027-03-02T19:00:00Z")),
        bucket("3p-weekly", Some(0.8), Some("2027-03-09T00:00:00Z")),
    ])
}

fn used_and_reset(lines: &[MetricLine], label: &str) -> (f64, Option<DateTime<Utc>>) {
    match lines.iter().find(|l| l.label() == label) {
        Some(MetricLine::Progress { used, resets_at, .. }) => (*used, *resets_at),
        other => panic!("missing {label}: {other:?}"),
    }
}

fn at(text: &str) -> Option<DateTime<Utc>> {
    Some(DateTime::parse_from_rfc3339(text).unwrap().into())
}

async fn fetch(server: &MockServer, t: &mut OAuthTokens) -> Result<AntigravityFetch, ProviderError> {
    client(server).fetch(t, None, false, now()).await
}

fn expected_user_agent() -> String {
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "amd64" };
    format!("antigravity/hub/2.24.0 (aidev_client; os_type=darwin; arch={arch})")
}

#[tokio::test]
async fn a_complete_daily_summary_answers_without_asking_production() {
    let server = MockServer::start().await;
    post("/daily", QUOTA)
        .and(header("authorization", "Bearer access"))
        .and(header("accept", "application/json"))
        .and(header("content-type", "application/json"))
        .and(header("user-agent", expected_user_agent().as_str()))
        .and(body_json(json!({"project": "fake-project"})))
        .respond_with(json_ok(&daily_summary()))
        .expect(1)
        .mount(&server)
        .await;
    post("/prod", QUOTA).respond_with(json_ok(&production_summary())).expect(0).mount(&server).await;
    post("/prod", CODE_ASSIST).respond_with(json_ok(r#"{"cloudaicompanionProject":"fake-project"}"#)).mount(&server).await;
    let fetched = fetch(&server, &mut tokens("access")).await.unwrap();
    let (session, reset) = used_and_reset(&fetched.snapshot.lines, "Session");
    assert!((session - 0.035).abs() < 0.001);
    assert_eq!(reset, at("2027-03-02T14:00:00Z"));
    assert!((used_and_reset(&fetched.snapshot.lines, "Weekly").0 - 2.58).abs() < 1e-4);
    assert_eq!(fetched.snapshot.lines.iter().map(MetricLine::label).collect::<Vec<_>>(), ["Session", "Weekly", "Claude", "Claude Weekly"]);
}

#[tokio::test]
async fn daily_wins_per_bucket_and_production_fills_omissions() {
    let server = MockServer::start().await;
    let daily = summary(&[bucket("3p-5h", Some(0.6), Some("2027-03-02T10:00:00Z"))]);
    post("/daily", QUOTA).respond_with(json_ok(&daily)).expect(1).mount(&server).await;
    post("/prod", QUOTA).respond_with(json_ok(&production_summary())).expect(1).mount(&server).await;
    post("/prod", CODE_ASSIST).respond_with(ResponseTemplate::new(503)).mount(&server).await;
    post("/daily", CODE_ASSIST).respond_with(ResponseTemplate::new(503)).mount(&server).await;
    let lines = fetch(&server, &mut tokens("access")).await.unwrap().snapshot.lines;
    for label in ["Session", "Weekly"] {
        assert!(used_and_reset(&lines, label).0.abs() < 1e-4, "{label} comes from production");
    }
    let (claude, reset) = used_and_reset(&lines, "Claude");
    assert!((claude - 40.0).abs() < 1e-3);
    assert_eq!(reset, at("2027-03-02T10:00:00Z"));
    assert!((used_and_reset(&lines, "Claude Weekly").0 - 20.0).abs() < 1e-3);
}

#[tokio::test]
async fn an_unusable_daily_fraction_fills_from_production() {
    let server = MockServer::start().await;
    let daily = summary(&[bucket("gemini-5h", None, None), bucket("3p-5h", Some(0.5), Some("2027-03-02T10:00:00Z"))]);
    post("/daily", QUOTA).respond_with(json_ok(&daily)).mount(&server).await;
    post("/prod", QUOTA).respond_with(json_ok(&production_summary())).mount(&server).await;
    let lines = fetch(&server, &mut tokens("access")).await.unwrap().snapshot.lines;
    let (used, reset) = used_and_reset(&lines, "Session");
    assert!(used.abs() < 1e-4);
    assert_eq!(reset, at("2027-03-02T19:00:00Z"));
}

#[tokio::test]
async fn a_daily_server_error_or_transport_error_falls_back_to_production() {
    for transport in [false, true] {
        let server = MockServer::start().await;
        if !transport {
            post("/daily", QUOTA).respond_with(ResponseTemplate::new(503).set_body_string("{}")).expect(1).mount(&server).await;
        }
        post("/prod", QUOTA).respond_with(json_ok(&production_summary())).expect(1).mount(&server).await;
        let mut config = config(&server);
        if transport {
            config.daily_url = "http://127.0.0.1:1".into();
        }
        let fetched = AntigravityClient::new(config).fetch(&mut tokens("access"), Some(LOAD.into()), false, now()).await.unwrap();
        assert_eq!(fetched.snapshot.lines.len(), 4);
    }
}

#[tokio::test]
async fn a_daily_401_still_succeeds_on_production() {
    let server = MockServer::start().await;
    post("/daily", QUOTA).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    post("/prod", QUOTA).respond_with(json_ok(&production_summary())).mount(&server).await;
    assert_eq!(fetch(&server, &mut tokens("access")).await.unwrap().snapshot.lines.len(), 4);
}

#[tokio::test]
async fn the_quota_request_names_the_accounts_project_and_the_plan_comes_from_load_code_assist() {
    let server = MockServer::start().await;
    post("/prod", CODE_ASSIST).and(body_string(LOAD_CODE_ASSIST_PAYLOAD)).respond_with(json_ok(LOAD)).expect(1).mount(&server).await;
    let quota = summary(&[bucket("gemini-weekly", Some(0.75), None), bucket("3p-weekly", Some(0.5), None)]);
    post("/daily", QUOTA).and(body_json(json!({"project": "fake-project-123"}))).respond_with(json_ok(&quota)).expect(1).mount(&server).await;
    post("/prod", QUOTA).respond_with(ResponseTemplate::new(403).set_body_string(SUBSCRIPTION_REQUIRED)).mount(&server).await;
    let fetched = fetch(&server, &mut tokens("access")).await.unwrap();
    assert_eq!(fetched.snapshot.lines.iter().map(MetricLine::label).collect::<Vec<_>>(), ["Weekly", "Claude Weekly"]);
    assert_eq!(fetched.snapshot.plan.as_deref(), Some("Antigravity Starter Quota"));
    assert_eq!(fetched.code_assist.as_deref(), Some(LOAD.as_bytes()));
}

#[tokio::test]
async fn load_code_assist_asks_production_first_then_daily() {
    let server = MockServer::start().await;
    post("/prod", CODE_ASSIST).and(header("user-agent", expected_user_agent().as_str())).respond_with(ResponseTemplate::new(503)).expect(1).mount(&server).await;
    post("/daily", CODE_ASSIST).and(header("authorization", "Bearer access")).respond_with(json_ok(LOAD)).expect(1).mount(&server).await;
    assert_eq!(client(&server).load_code_assist("access").await.unwrap(), LOAD.as_bytes());
    let hosts: Vec<String> = server.received_requests().await.unwrap().iter().map(|r| r.url.path().to_string()).collect();
    assert_eq!(hosts, ["/prod/v1internal:loadCodeAssist", "/daily/v1internal:loadCodeAssist"]);
}

#[tokio::test]
async fn load_code_assist_maps_auth_failures_and_falls_through_to_unavailable() {
    for status in [401, 403] {
        let server = MockServer::start().await;
        post("/prod", CODE_ASSIST).respond_with(ResponseTemplate::new(status)).expect(1).mount(&server).await;
        post("/daily", CODE_ASSIST).respond_with(json_ok(LOAD)).expect(0).mount(&server).await;
        assert_eq!(client(&server).load_code_assist("access").await.unwrap_err(), ProviderError::SignInExpired, "{status}");
    }
    let server = MockServer::start().await;
    assert_eq!(client(&server).load_code_assist("access").await.unwrap_err(), ProviderError::Unavailable(UNAVAILABLE_MSG.into()));
}

#[tokio::test]
async fn quota_failures_map_to_friendly_errors() {
    let cases = [
        (403, SUBSCRIPTION_REQUIRED, Some(LOAD), ProviderError::Unavailable(NO_SUBSCRIPTION_MSG.into())),
        (403, SUBSCRIPTION_REQUIRED, None, ProviderError::Unavailable(NO_PROJECT_MSG.into())),
        (403, r#"{"error":{"status":"PERMISSION_DENIED"}}"#, Some(LOAD), ProviderError::SignInExpired),
        (500, "", Some(LOAD), ProviderError::Unavailable(UNAVAILABLE_MSG.into())),
    ];
    for (status, body, code_assist, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path_regex_quota()).respond_with(ResponseTemplate::new(status).set_body_string(body)).mount(&server).await;
        let result = client(&server).fetch(&mut tokens("access"), code_assist.map(Into::into), false, now()).await;
        // Without a cached body `fetch` reads loadCodeAssist, which these mocks answer 404 -> no project.
        assert_eq!(result.unwrap_err(), expected, "{status} {body}");
    }
}

fn path_regex_quota() -> wiremock::matchers::PathRegexMatcher {
    wiremock::matchers::path_regex(r"/v1internal:retrieveUserQuotaSummary$")
}

#[tokio::test]
async fn both_bases_rate_limited_keep_their_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path_regex_quota()).respond_with(ResponseTemplate::new(429).insert_header("retry-after", "120")).mount(&server).await;
    let error = client(&server).fetch(&mut tokens("access"), Some(LOAD.into()), false, now()).await.unwrap_err();
    assert_eq!(error, ProviderError::RateLimited { retry_after_secs: Some(120) });
}

#[tokio::test]
async fn every_base_offline_is_connection_failed() {
    let server = MockServer::start().await;
    let mut config = config(&server);
    config.daily_url = "http://127.0.0.1:1".into();
    config.production_url = "http://127.0.0.1:1".into();
    let error = AntigravityClient::new(config).fetch(&mut tokens("access"), Some(LOAD.into()), false, now()).await.unwrap_err();
    assert_eq!(error, ProviderError::ConnectionFailed);
}

fn quota_with(bearer: &str) -> MockBuilder {
    post("/daily", QUOTA).and(header("authorization", format!("Bearer {bearer}").as_str()))
}

fn token_ok() -> ResponseTemplate {
    json_ok(r#"{"access_token":"new-access","expires_in":120}"#)
}

#[tokio::test]
async fn refresh_is_a_form_post_with_the_client_secret_and_keeps_the_old_refresh_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string(format!("client_id={CLIENT_ID}&client_secret={CLIENT_SECRET}&grant_type=refresh_token&refresh_token=r%201")))
        .respond_with(json_ok(r#"{"access_token":"new","refresh_token":"ignored"}"#))
        .expect(1)
        .mount(&server)
        .await;
    let mut t = tokens("old");
    t.refresh_token = Some("r 1".into());
    let renewed = client(&server).refresh(&t, now()).await.unwrap();
    assert_eq!(renewed.access_token, "new");
    assert_eq!(renewed.refresh_token.as_deref(), Some("r 1"));
    assert_eq!(renewed.expires_at, Some(now() + Duration::seconds(3600)), "expiry defaults to an hour");
    assert_eq!(renewed.extras, t.extras);
}

#[tokio::test]
async fn a_refused_refresh_token_is_sign_in_expired() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/token")).respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid_grant"}"#)).mount(&server).await;
    assert_eq!(client(&server).refresh(&tokens("old"), now()).await.unwrap_err(), ProviderError::SignInExpired);
}

#[tokio::test]
async fn an_unauthorized_quota_read_renews_once_and_retries_and_keeps_the_renewal() {
    let server = MockServer::start().await;
    quota_with("old").respond_with(ResponseTemplate::new(401)).mount(&server).await;
    post("/prod", QUOTA).and(header("authorization", "Bearer old")).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    Mock::given(method("POST")).and(path("/token")).respond_with(token_ok()).expect(1).mount(&server).await;
    quota_with("new-access").respond_with(json_ok(&daily_summary())).expect(1).mount(&server).await;
    let mut t = tokens("old");
    let fetched = client(&server).fetch(&mut t, Some(LOAD.into()), false, now()).await.unwrap();
    assert_eq!(fetched.snapshot.lines.len(), 4);
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref()), ("new-access", Some("r1")));
    assert_eq!(t.extras["keep"], "me");
}

#[tokio::test]
async fn a_401_that_survives_the_retry_is_sign_in_expired_and_the_renewal_stays_in_place() {
    let server = MockServer::start().await;
    post("/daily", QUOTA).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    post("/prod", QUOTA).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    Mock::given(method("POST")).and(path("/token")).respond_with(token_ok()).expect(1).mount(&server).await;
    let mut t = tokens("old");
    let error = client(&server).fetch(&mut t, Some(LOAD.into()), false, now()).await.unwrap_err();
    assert_eq!(error, ProviderError::SignInExpired);
    assert_eq!(t.access_token, "new-access", "renewal kept on Err");
    assert!(t.expires_at.unwrap() < now() + Duration::hours(1));

    let server = MockServer::start().await;
    post("/daily", QUOTA).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    post("/prod", QUOTA).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    let mut t = tokens("old");
    t.refresh_token = None;
    assert_eq!(client(&server).fetch(&mut t, Some(LOAD.into()), false, now()).await.unwrap_err(), ProviderError::SignInExpired);
}

#[tokio::test]
async fn a_token_inside_the_margin_is_refreshed_first_and_a_failed_early_refresh_keeps_an_unexpired_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/token")).respond_with(token_ok()).expect(1).mount(&server).await;
    quota_with("new-access").respond_with(json_ok(&daily_summary())).expect(1).mount(&server).await;
    let mut t = tokens("old");
    t.expires_at = Some(now() + Duration::minutes(2));
    client(&server).fetch(&mut t, Some(LOAD.into()), false, now()).await.unwrap();
    assert_eq!(t.access_token, "new-access");

    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/token")).respond_with(ResponseTemplate::new(500)).mount(&server).await;
    quota_with("old").respond_with(json_ok(&daily_summary())).expect(1).mount(&server).await;
    let mut t = tokens("old");
    t.expires_at = Some(now() + Duration::minutes(2));
    client(&server).fetch(&mut t, Some(LOAD.into()), false, now()).await.unwrap();
    assert_eq!(t.access_token, "old");
}

#[tokio::test]
async fn login_runs_the_loopback_flow_and_reads_the_identity() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(wiremock::matchers::body_string_contains("client_secret="))
        .and(wiremock::matchers::body_string_contains("code=the-code"))
        .and(wiremock::matchers::body_string_contains("code_verifier="))
        .and(wiremock::matchers::body_string_contains("grant_type=authorization_code"))
        .respond_with(json_ok(r#"{"access_token":"at","refresh_token":"rt","expires_in":3600}"#))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .and(query_param("alt", "json"))
        .and(header("authorization", "Bearer at"))
        .respond_with(json_ok(r#"{"id":"1234","email":"me@example.com"}"#))
        .mount(&server)
        .await;
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let mut cfg = config(&server);
    cfg.loopback_port = port;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None::<Url>));
    let seen_in = seen.clone();
    let (label, t) = AntigravityClient::new(cfg)
        .login(move |url| {
            *seen_in.lock().unwrap() = Some(url.clone());
            let q: HashMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
            let callback = format!("{}?code=the-code&state={}", q["redirect_uri"], q["state"]);
            tokio::spawn(async move {
                let _ = reqwest::get(callback).await;
            });
        })
        .await
        .unwrap();
    assert_eq!(label, "me@example.com");
    assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref()), ("at", Some("rt")));
    assert_eq!(t.extras[OAuthTokens::IDENTITY_KEY], "1234");
    let url = seen.lock().unwrap().clone().unwrap();
    let q: HashMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    assert_eq!(q["redirect_uri"], format!("http://localhost:{port}/oauth-callback"));
    assert_eq!((q["client_id"].as_str(), q["access_type"].as_str(), q["prompt"].as_str()), (CLIENT_ID, "offline", "consent"));
    assert_eq!(q["scope"], SCOPES);
    assert_eq!(q["code_challenge_method"], "S256");
}

#[tokio::test]
async fn login_without_userinfo_falls_back_to_the_generic_label_and_a_busy_port_is_a_clear_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/token")).respond_with(json_ok(r#"{"access_token":"at"}"#)).mount(&server).await;
    Mock::given(method("GET")).and(path("/userinfo")).respond_with(ResponseTemplate::new(500)).mount(&server).await;
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    let mut cfg = config(&server);
    cfg.loopback_port = port;
    let error = AntigravityClient::new(cfg.clone()).login(|_| {}).await.unwrap_err();
    assert_eq!(error.to_string(), format!("Port {port} is in use."));
    drop(busy);
    let (label, t) = AntigravityClient::new(cfg)
        .login(|url| {
            let q: HashMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
            let callback = format!("{}?code=c&state={}", q["redirect_uri"], q["state"]);
            tokio::spawn(async move {
                let _ = reqwest::get(callback).await;
            });
        })
        .await
        .unwrap();
    assert_eq!(label, "Google Account");
    assert!(!t.extras.contains_key(OAuthTokens::IDENTITY_KEY));
}
