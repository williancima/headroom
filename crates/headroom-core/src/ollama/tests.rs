use chrono::{DateTime, Utc};
use wiremock::matchers::{bearer_token, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::mapper::*;
use super::*;
use crate::models::{MetricLine, ProgressFormat};

const ACCOUNT: &str = r#"{"ID":"u1","Email":"free@example.com","Name":"free","Plan":"free"}"#;
/// A free ollama.com account: no allowance, no purchased credits.
const FREE_BALANCE: &str = r#"{"included":{"balance_usd":0,"allowance_usd":0,"period":{"from":"2026-10-14T00:00:00Z","until":"2026-11-14T00:00:00Z"}},"purchased":{"balance_usd":0}}"#;
const BALANCE: &str = r#"{"included":{"balance_usd":72.5,"allowance_usd":100,"period":{"from":"2026-09-15T09:30:00Z","until":"2026-10-15T09:30:00Z"}},"purchased":{"balance_usd":25}}"#;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-19T12:00:00Z").unwrap().into()
}

fn labels(lines: &[MetricLine]) -> Vec<&str> {
    lines.iter().map(MetricLine::label).collect()
}

fn ok(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

#[test]
fn a_free_account_has_a_zero_purchased_balance_and_no_monthly_row() {
    let lines = balance_lines(FREE_BALANCE.as_bytes()).unwrap();
    assert_eq!(labels(&lines), ["Purchased Credits"], "a $0 allowance is no monthly allowance");
}

#[test]
fn a_balance_body_without_an_included_object_is_invalid() {
    for body in [r#"{"purchased":{"balance_usd":0}}"#, r#"{"ok":true}"#, "[]", "not json", r#"{"included":{}}"#] {
        assert_eq!(balance_lines(body.as_bytes()).unwrap_err(), ProviderError::InvalidResponse, "{body}");
    }
}

#[test]
fn monthly_and_purchased_lines() {
    let lines = balance_lines(BALANCE.as_bytes()).unwrap();
    let period = (DateTime::parse_from_rfc3339("2026-10-15T09:30:00Z").unwrap() - DateTime::parse_from_rfc3339("2026-09-15T09:30:00Z").unwrap()).num_milliseconds();
    assert!(matches!(&lines[0], MetricLine::Progress { used, limit, format: ProgressFormat::Dollars, period_ms: Some(p), .. } if (*used, *limit, *p) == (27.5, 100.0, period)));
    assert_eq!(labels(&lines), ["Monthly", "Purchased Credits"]);
}

#[test]
fn a_zero_balance_with_a_full_allowance_shows_zero_purchased() {
    let lines = balance_lines(br#"{"included":{"balance_usd":100,"allowance_usd":100},"purchased":{"balance_usd":0}}"#).unwrap();
    assert!(matches!(&lines[1], MetricLine::Values { values, .. } if values[0].number == 0.0));
}

#[test]
fn legacy_windows_use_remaining_percent_and_keep_a_period_only_with_a_reset() {
    let body = br#"{"included":{"session":{"remaining_percent":75,"resets_at":"2026-11-19T15:00:00Z"},"weekly":{"remaining_percent":120}}}"#;
    let lines = balance_lines(body).unwrap();
    assert!(matches!(&lines[0], MetricLine::Progress { used, period_ms: Some(18_000_000), resets_at: Some(_), .. } if *used == 25.0));
    assert!(matches!(&lines[1], MetricLine::Progress { used, period_ms: None, resets_at: None, .. } if *used == 0.0));
}

#[test]
fn plan_and_email_accept_both_key_spellings() {
    for account in [r#"{"Email":" dev@example.com ","Name":"dev","Plan":"pro"}"#, r#"{"email":"dev@example.com","name":"dev","plan":"pro"}"#] {
        assert_eq!((plan(account.as_bytes()).as_deref(), email(account.as_bytes()).as_deref()), (Some("Pro"), Some("dev@example.com")));
    }
    for none in [r#"{"Name":"dev","Plan":"free"}"#, r#"{"Email":""}"#, "{}", "not json"] {
        assert_eq!(email(none.as_bytes()), None, "{none}");
    }
}

#[tokio::test]
async fn fetch_reads_balance_and_the_account_once_when_due() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/balance")).and(bearer_token("key")).and(header("accept", "application/json")).respond_with(ok(FREE_BALANCE)).expect(1).mount(&server).await;
    Mock::given(method("POST")).and(path("/api/me")).and(bearer_token("key")).respond_with(ok(ACCOUNT)).expect(1).mount(&server).await;
    let fetched = OllamaClient::new(&server.uri()).fetch("key", None, true, now()).await.unwrap();
    assert_eq!(fetched.snapshot.plan.as_deref(), Some("Free"));
    assert_eq!(fetched.account_body.as_deref().and_then(|b| email(b.as_bytes())).as_deref(), Some("free@example.com"));
}

#[tokio::test]
async fn between_reads_the_cached_account_keeps_the_plan_and_a_failed_read_drops_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/balance")).respond_with(ok(BALANCE)).mount(&server).await;
    Mock::given(method("POST")).and(path("/api/me")).respond_with(ResponseTemplate::new(401)).expect(1).mount(&server).await;
    let client = OllamaClient::new(&server.uri());
    let cached = client.fetch("key", Some(ACCOUNT.into()), false, now()).await.unwrap();
    assert_eq!(cached.snapshot.plan.as_deref(), Some("Free"));
    let failed = client.fetch("key", Some(ACCOUNT.into()), true, now()).await.unwrap();
    assert_eq!((failed.snapshot.plan, failed.account_body), (None, None));
    assert_eq!(failed.snapshot.lines.len(), 2);
}

#[tokio::test]
async fn http_failures_map_to_provider_errors() {
    for (status, expected) in [
        (401, ProviderError::KeyRejected("Ollama rejected the API key.".into())),
        (403, ProviderError::KeyRejected("Ollama rejected the API key.".into())),
        (429, ProviderError::RateLimited { retry_after_secs: None }),
        (500, ProviderError::RequestFailed(500)),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(status)).mount(&server).await;
        assert_eq!(OllamaClient::new(&server.uri()).fetch("k", None, true, now()).await.unwrap_err(), expected, "{status}");
    }
}
