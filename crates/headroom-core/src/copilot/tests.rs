use chrono::{DateTime, Utc};
use wiremock::matchers::{bearer_token, body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

const USAGE: &str = r#"{"copilot_plan":"enterprise","quota_reset_date":"2026-12-01","quota_snapshots":{"premium_interactions":{"entitlement":300,"remaining":240,"percent_remaining":80}}}"#;
const ORG: &str = r#"{"usageItems":[{"product":"Copilot","grossQuantity":100,"netAmount":1.0}]}"#;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-19T12:00:00Z").unwrap().into()
}

fn ok(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

fn client(server: &MockServer, domain: GitHubDomain) -> CopilotClient {
    CopilotClient::with_config(domain, CopilotConfig { web_base: Some(server.uri()), api_base: Some(server.uri()) })
}

fn tokens(access: &str) -> OAuthTokens {
    OAuthTokens { access_token: access.into(), refresh_token: None, expires_at: None, extras: Default::default() }
}

fn ghe() -> GitHubDomain {
    GitHubDomain::Enterprise("octocorp.ghe.com".into())
}

#[test]
fn the_client_id_is_the_decoded_copilot_id() {
    assert_eq!(CLIENT_ID, "Iv1.b507a08c87ecfe98");
}

#[test]
fn urls_follow_the_domain_unless_overridden() {
    let client = CopilotClient::new(ghe());
    assert_eq!(client.web("/login/device/code"), "https://octocorp.ghe.com/login/device/code");
    assert_eq!(client.api("/copilot_internal/user"), "https://api.octocorp.ghe.com/copilot_internal/user");
    assert_eq!(client.organization_url("ac me").unwrap().as_str(), "https://api.github.com/organizations/ac%20me/settings/billing/ai_credit/usage");
}

#[tokio::test]
async fn the_usage_read_uses_the_token_scheme_and_the_copilot_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/copilot_internal/user")).and(header("authorization", "token tok"))
        .and(header("editor-version", "vscode/1.96.2")).and(header("editor-plugin-version", "copilot-chat/0.26.7"))
        .and(header("user-agent", "GitHubCopilotChat/0.26.7")).and(header("x-github-api-version", "2025-04-01")).and(header("accept", "application/json"))
        .respond_with(ok(USAGE)).expect(1).mount(&server).await;
    let snapshot = client(&server, GitHubDomain::Github).fetch(&mut tokens("tok"), now()).await.unwrap();
    assert_eq!(snapshot.plan.as_deref(), Some("Enterprise"));
    assert_eq!(snapshot.lines.len(), 1);
}

#[tokio::test]
async fn usage_failures_map_to_provider_errors() {
    for (status, retry_after, expected) in [
        (401, None, ProviderError::SignInExpired),
        (403, None, ProviderError::SignInExpired),
        (403, Some("60"), ProviderError::RateLimited { retry_after_secs: Some(60) }),
        (429, None, ProviderError::RateLimited { retry_after_secs: None }),
        (500, None, ProviderError::RequestFailed(500)),
    ] {
        let server = MockServer::start().await;
        let mut response = ResponseTemplate::new(status);
        if let Some(value) = retry_after {
            response = response.insert_header("retry-after", value);
        }
        Mock::given(method("GET")).and(path("/copilot_internal/user")).respond_with(response).mount(&server).await;
        assert_eq!(client(&server, GitHubDomain::Github).fetch(&mut tokens("t"), now()).await.unwrap_err(), expected, "{status} {retry_after:?}");
    }
}

#[tokio::test]
async fn an_organization_read_checks_the_token_with_the_documented_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/organizations/acme/settings/billing/ai_credit/usage")).and(bearer_token("good"))
        .and(header("x-github-api-version", "2022-11-28")).and(header("accept", "application/vnd.github+json")).and(header("user-agent", "Headroom"))
        .respond_with(ok(ORG)).expect(2).mount(&server).await;
    let c = client(&server, GitHubDomain::Github);
    c.validate_organization("acme", "good").await.unwrap();
    let snapshot = c.fetch_organization("acme", "good", now()).await.unwrap();
    assert_eq!((snapshot.plan, snapshot.lines.len()), (None, 2));
}

#[tokio::test]
async fn organization_failures_map_to_provider_errors() {
    for (status, body, expected) in [
        (403, "{}", ProviderError::KeyRejected("GitHub rejected the token.".into())),
        (401, "{}", ProviderError::KeyRejected("GitHub rejected the token.".into())),
        (404, "{}", ProviderError::Unavailable("GitHub has no organization “acme” this token can read.".into())),
        (200, "<html>", ProviderError::InvalidResponse),
        (500, "{}", ProviderError::RequestFailed(500)),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(status).set_body_string(body)).mount(&server).await;
        let c = client(&server, GitHubDomain::Github);
        assert_eq!(c.organization_usage("acme", "t").await.unwrap_err(), expected, "{status}");
    }
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(403)).mount(&server).await;
    let refused = client(&server, GitHubDomain::Github).validate_organization("acme", "t").await.unwrap_err();
    assert_eq!(refused, ProviderError::Unavailable("GitHub rejected the token.".into()), "a refused token on add is unavailable");
}

#[tokio::test]
async fn a_rate_limited_organization_read_is_a_rate_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(403).insert_header("retry-after", "60")).mount(&server).await;
    let error = client(&server, GitHubDomain::Github).organization_usage("acme", "good").await.unwrap_err();
    assert_eq!(error, ProviderError::RateLimited { retry_after_secs: Some(60) });
}

async fn mount_github(server: &MockServer, usage_status: u16, user: &str) {
    let verify = "https://github.test/login/device";
    Mock::given(method("POST")).and(path("/login/device/code")).and(header("user-agent", "Headroom"))
        .and(body_string_contains(format!("client_id={CLIENT_ID}"))).and(body_string_contains("scope=read%3Auser"))
        .respond_with(ok(&format!(r#"{{"device_code":"device","user_code":"ABCD-EFGH","verification_uri":"{verify}","expires_in":600,"interval":0}}"#))).mount(server).await;
    Mock::given(method("POST")).and(path("/login/oauth/access_token")).and(header("user-agent", "Headroom")).and(header("accept", "application/json"))
        .and(body_string_contains("device_code=device")).respond_with(ok(r#"{"access_token":"copilot-access"}"#)).mount(server).await;
    Mock::given(method("GET")).and(path("/user")).and(bearer_token("copilot-access")).and(header("accept", "application/vnd.github+json"))
        .respond_with(ok(user)).mount(server).await;
    Mock::given(method("GET")).and(path("/copilot_internal/user")).respond_with(ResponseTemplate::new(usage_status).set_body_string(USAGE)).mount(server).await;
}

#[tokio::test]
async fn a_github_com_sign_in_names_the_account_and_reads_no_usage() {
    let server = MockServer::start().await;
    mount_github(&server, 404, r#"{"id":42,"login":"robin"}"#).await;
    // `expect(0)` for the seat check is implicit: a 404 there would fail an enterprise login, not this one.
    let mut shown = None;
    let (label, t) = client(&server, GitHubDomain::Github).login(|c, u| shown = Some((c.to_string(), u.to_string()))).await.unwrap();
    assert_eq!(label, "robin");
    assert_eq!(t.extras[OAuthTokens::IDENTITY_KEY], "github:42");
    assert_eq!((t.refresh_token, t.expires_at), (None, None));
    assert_eq!(shown.unwrap().0, "ABCD-EFGH");
    let requests = server.received_requests().await.unwrap();
    assert!(requests.iter().all(|r| r.url.path() != "/copilot_internal/user"));
}

#[tokio::test]
async fn a_profile_without_an_id_is_identified_by_its_login() {
    let server = MockServer::start().await;
    mount_github(&server, 200, r#"{"login":"copilot-user"}"#).await;
    let (label, t) = client(&server, GitHubDomain::Github).login(|_, _| {}).await.unwrap();
    assert_eq!((label.as_str(), t.extras[OAuthTokens::IDENTITY_KEY].as_str()), ("copilot-user", "github:copilot-user"));
}

#[tokio::test]
async fn an_enterprise_sign_in_checks_the_seat_once_and_scopes_the_identity_to_the_host() {
    let server = MockServer::start().await;
    mount_github(&server, 200, r#"{"id":42,"login":"robin"}"#).await;
    let (label, t) = client(&server, ghe()).login(|_, _| {}).await.unwrap();
    assert_eq!(label, "robin");
    assert_eq!(t.extras[OAuthTokens::IDENTITY_KEY], "github:octocorp.ghe.com:42");
    let seat_checks = server.received_requests().await.unwrap().iter().filter(|r| r.url.path() == "/copilot_internal/user").count();
    assert_eq!(seat_checks, 1);
}

#[tokio::test]
async fn an_enterprise_account_without_a_seat_is_never_added() {
    for status in [404, 401, 403] {
        let server = MockServer::start().await;
        mount_github(&server, status, r#"{"id":42,"login":"robin"}"#).await;
        let error = client(&server, ghe()).login(|_, _| {}).await.unwrap_err();
        assert_eq!(error.to_string(), "robin has no Copilot seat on octocorp.ghe.com. Ask your GitHub admin for one, then try again.", "{status}");
    }
    let server = MockServer::start().await;
    mount_github(&server, 500, r#"{"id":42,"login":"robin"}"#).await;
    assert!(matches!(client(&server, ghe()).login(|_, _| {}).await.unwrap_err(), crate::oauth::OAuthError::RequestFailed(500)));
}

#[tokio::test]
async fn an_unreachable_enterprise_domain_says_so_and_github_com_keeps_the_plain_error() {
    // A closed port: nothing listens, so the first request cannot connect.
    let dead = CopilotConfig { web_base: Some("http://127.0.0.1:1".into()), api_base: Some("http://127.0.0.1:1".into()) };
    let error = CopilotClient::with_config(GitHubDomain::Enterprise("octocrop.ghe.com".into()), dead.clone()).login(|_, _| {}).await.unwrap_err();
    assert_eq!(error.to_string(), "Couldn’t reach octocrop.ghe.com. Check the address and your connection.");
    let plain = CopilotClient::with_config(GitHubDomain::Github, dead).login(|_, _| {}).await.unwrap_err();
    assert!(matches!(plain, crate::oauth::OAuthError::Network));
}
