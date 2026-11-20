//! A mirrored account's unchanged provider clients read usage through the hub's `api-call`. The upstream
//! answer comes inside the envelope, so the hub is the only server these tests run.

use std::collections::HashMap;

use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::keys::{HubKey, HubKeys, MemoryHubKeys};
use super::transport::{HubRoute, Transport};
use super::*;
use crate::antigravity::AntigravityClient;
use crate::claude::ClaudeClient;
use crate::codex::CodexClient;
use crate::devin::DevinClient;
use crate::grok::GrokClient;
use crate::models::ProviderKind;
use crate::provider::ProviderError;
use crate::store::Store;

const CLAUDE_USAGE: &str = r#"{"five_hour":{"utilization":42,"resets_at":"2099-01-01T00:00:00.000Z"}}"#;
const CLAUDE_PROFILE: &str = r#"{"account":{"uuid":"acc-1"},"organization":{"uuid":"6a9f1c2e-1b4d-4c3a-9e8f-0123456789ab","organization_type":"claude_max","rate_limit_tier":"default_claude_max_20x"}}"#;
const ANTIGRAVITY_SUMMARY: &str = r#"{"groups":[{"displayName":"Gemini","buckets":[{"bucketId":"gemini-weekly","remainingFraction":0.75,"resetTime":"2099-01-01T00:00:00Z","window":"weekly"}]}]}"#;
const DEVIN_STATUS: &str = r#"{"userStatus":{"planStatus":{"planInfo":{"planName":"Max"},"weeklyQuotaRemainingPercent":40}}}"#;
const GROK_CREDITS: &str = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2099-01-01T00:00:00Z","end":"2099-01-08T00:00:00Z"},"creditUsagePercent":25}}"#;

type Answer = (u16, String, Vec<(&'static str, &'static str)>);

struct Rig {
    dir: std::path::PathBuf,
    store: Store,
    client: HubClient,
    keys: MemoryHubKeys,
    server: MockServer,
    hub: Hub,
}

impl Rig {
    async fn new(upstream: impl Fn(&str) -> Answer + Send + Sync + 'static) -> Self {
        let dir = std::env::temp_dir().join(format!("headroom-transport-{}", Uuid::new_v4()));
        let store = Store::at(&dir.join("config"), &dir.join("state"));
        let server = MockServer::start().await;
        let hub = Hub::new(server.uri());
        store.save_hubs(std::slice::from_ref(&hub)).unwrap();
        let keys = MemoryHubKeys::default();
        keys.save(hub.id, &HubKey { key: "mgmt-key".into(), url: hub.url.clone() }).unwrap();
        Mock::given(method("POST"))
            .and(path("/v0/management/api-call"))
            .respond_with(move |request: &Request| {
                let call: Value = serde_json::from_slice(&request.body).unwrap();
                let (status, body, headers) = upstream(call["url"].as_str().unwrap());
                let headers: HashMap<&str, Vec<&str>> = headers.into_iter().map(|(k, v)| (k, vec![v])).collect();
                ResponseTemplate::new(200).set_body_json(json!({"status_code": status, "header": headers, "body": body}))
            })
            .mount(&server)
            .await;
        let client = HubClient::new(HubClient::default_http(), store.clone());
        Self { dir, store, client, keys, server, hub }
    }

    fn link(&self, login: &str, kind: ProviderKind) -> HubLink {
        let mut link = HubLink::new(&self.hub, &super::login::fixtures::login(login, kind.id(), None, false)).unwrap();
        link.chatgpt_account_id = (kind == ProviderKind::Codex).then(|| format!("acct-{login}"));
        link
    }

    fn transport(&self, link: &HubLink, kind: ProviderKind) -> Transport {
        Transport::Hub(Box::new(HubRoute::new(&self.client, &self.store, &self.keys, link, kind).unwrap()))
    }

    /// The `api-call` payloads the hub received, in order.
    async fn calls(&self) -> Vec<Value> {
        self.server.received_requests().await.unwrap().iter().map(|r| serde_json::from_slice(&r.body).unwrap()).collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn ok(body: &str) -> Answer {
    (200, body.into(), vec![])
}

#[tokio::test]
async fn claude_and_chatgpt_read_usage_through_the_hub_with_their_clients_unchanged() {
    let rig = Rig::new(|url| if url.ends_with("/api/oauth/usage") { ok(CLAUDE_USAGE) } else { ok(r#"{"rate_limit":{}}"#) }).await;
    let (claude_link, codex_link) = (rig.link("a", ProviderKind::Claude), rig.link("b", ProviderKind::Codex));

    let mut tokens = claude_link.tokens();
    let claude = ClaudeClient::default().with_transport(rig.transport(&claude_link, ProviderKind::Claude));
    let snapshot = claude.fetch(&mut tokens, false, Utc::now()).await.unwrap().snapshot;
    assert!(!snapshot.lines.is_empty(), "the Claude mapper read the tunnelled answer");
    let mut tokens = codex_link.tokens();
    CodexClient::default().with_transport(rig.transport(&codex_link, ProviderKind::Codex)).fetch(&mut tokens, None, false, Utc::now()).await.unwrap();

    let calls = rig.calls().await;
    let requests = rig.server.received_requests().await.unwrap();
    assert!(requests.iter().all(|r| r.url.path() == "/v0/management/api-call" && r.method.as_str() == "POST"));
    assert!(requests.iter().all(|r| r.headers.get("authorization").unwrap().to_str().unwrap() == "Bearer mgmt-key"));
    assert_eq!(calls.len(), 2);
    assert_eq!((calls[0]["auth_index"].as_str(), calls[0]["method"].as_str()), (Some("index-a"), Some("GET")));
    assert_eq!(calls[0]["url"], "https://api.anthropic.com/api/oauth/usage");
    assert_eq!(calls[0]["header"]["authorization"], "Bearer $TOKEN$", "the hub puts its own token in");
    assert_eq!(calls[1]["auth_index"], "index-b");
    assert_eq!(calls[1]["header"]["authorization"], "Bearer $TOKEN$");
    assert_eq!(calls[1]["header"]["chatgpt-account-id"], "acct-b");
}

#[tokio::test]
async fn antigravity_devin_and_grok_read_usage_through_the_hub_with_their_clients_unchanged() {
    let rig = Rig::new(|url| {
        if url.ends_with("loadCodeAssist") {
            ok(r#"{"cloudaicompanionProject":"p-1","currentTier":{"id":"free-tier","name":"Free"}}"#)
        } else if url.ends_with("retrieveUserQuotaSummary") {
            ok(ANTIGRAVITY_SUMMARY)
        } else if url.ends_with("GetUserStatus") {
            ok(DEVIN_STATUS)
        } else if url.contains("billing") {
            ok(GROK_CREDITS)
        } else {
            ok("{}")
        }
    })
    .await;
    let (g, d, x) = (rig.link("g", ProviderKind::Antigravity), rig.link("d", ProviderKind::Devin), rig.link("x", ProviderKind::Grok));

    let antigravity = AntigravityClient::default().with_transport(rig.transport(&g, ProviderKind::Antigravity));
    assert!(!antigravity.fetch(&mut g.tokens(), None, true, Utc::now()).await.unwrap().snapshot.lines.is_empty());
    let devin = DevinClient::default().with_transport(rig.transport(&d, ProviderKind::Devin));
    assert!(!devin.fetch(&d.tokens(), Utc::now()).await.unwrap().lines.is_empty());
    let grok = GrokClient::default().with_transport(rig.transport(&x, ProviderKind::Grok));
    assert!(!grok.fetch(&mut x.tokens(), None, false, Utc::now()).await.unwrap().snapshot.lines.is_empty());

    let calls = rig.calls().await;
    let quota = calls.iter().find(|c| c["url"].as_str().unwrap().ends_with("retrieveUserQuotaSummary")).unwrap();
    assert_eq!(quota["auth_index"], "index-g");
    assert_eq!(quota["header"]["authorization"], "Bearer $TOKEN$");
    assert!(quota["data"].as_str().unwrap().contains("p-1"), "the quota read names the project from the first call");
    let devin = calls.iter().find(|c| c["url"].as_str().unwrap().ends_with("GetUserStatus")).unwrap();
    assert_eq!(devin["auth_index"], "index-d");
    assert!(devin["data"].as_str().unwrap().contains(r#""apiKey":"$TOKEN$""#), "Devin's key goes in the body, where the hub substitutes it too");
    let grok = calls.iter().find(|c| c["url"].as_str().unwrap().contains("billing")).unwrap();
    assert_eq!((grok["auth_index"].as_str(), grok["header"]["authorization"].as_str()), (Some("index-x"), Some("Bearer $TOKEN$")));
}

#[tokio::test]
async fn an_upstream_rate_limit_keeps_its_retry_after() {
    let rig = Rig::new(|_| (429, "{}".into(), vec![("Retry-After", "120")])).await;
    let link = rig.link("b", ProviderKind::Codex);
    let codex = CodexClient::default().with_transport(rig.transport(&link, ProviderKind::Codex));
    let error = codex.fetch(&mut link.tokens(), None, false, Utc::now()).await.unwrap_err();
    assert_eq!(error, ProviderError::RateLimited { retry_after_secs: Some(120) });
}

#[tokio::test]
async fn a_hub_claude_account_reads_its_plan_with_its_first_refresh() {
    let rig = Rig::new(|url| ok(if url.ends_with("/api/oauth/profile") { CLAUDE_PROFILE } else { CLAUDE_USAGE })).await;
    let link = rig.link("a", ProviderKind::Claude);
    let claude = ClaudeClient::default().with_transport(rig.transport(&link, ProviderKind::Claude));
    let fetched = claude.fetch(&mut link.tokens(), true, Utc::now()).await.unwrap();
    assert_eq!(fetched.snapshot.plan.as_deref(), Some("Ultra"));
    let profile: Vec<_> = rig.calls().await.into_iter().filter(|c| c["url"].as_str().unwrap().ends_with("/api/oauth/profile")).collect();
    assert_eq!(profile.len(), 1);
    assert_eq!((profile[0]["auth_index"].as_str(), profile[0]["header"]["authorization"].as_str()), (Some("index-a"), Some("Bearer $TOKEN$")));
}

#[tokio::test]
async fn a_chatgpt_workspace_switched_on_the_hub_is_sent_with_the_next_read() {
    let rig = Rig::new(|_| ok(r#"{"rate_limit":{}}"#)).await;
    let mut link = rig.link("b", ProviderKind::Codex);
    for workspace in ["acct-old", "acct-new"] {
        link.chatgpt_account_id = Some(workspace.into());
        let codex = CodexClient::default().with_transport(rig.transport(&link, ProviderKind::Codex));
        codex.fetch(&mut link.tokens(), None, false, Utc::now()).await.unwrap();
    }
    let sent: Vec<_> = rig.calls().await.iter().map(|c| c["header"]["chatgpt-account-id"].as_str().unwrap().to_string()).collect();
    assert_eq!(sent, ["acct-old", "acct-new"]);
}

#[tokio::test]
async fn a_direct_account_still_calls_the_provider_directly_and_sends_nothing_to_a_hub() {
    let rig = Rig::new(|_| ok("{}")).await;
    let upstream = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/oauth/usage")).respond_with(ResponseTemplate::new(200).set_body_string(CLAUDE_USAGE)).mount(&upstream).await;
    let config = crate::claude::ClaudeConfig { api_base: upstream.uri(), ..Default::default() };
    let mut tokens = crate::models::OAuthTokens { access_token: "real".into(), refresh_token: None, expires_at: None, extras: Default::default() };
    ClaudeClient::new(config).fetch(&mut tokens, false, Utc::now()).await.unwrap();
    assert_eq!(upstream.received_requests().await.unwrap().len(), 1);
    assert!(rig.calls().await.is_empty());
}

#[tokio::test]
async fn a_forged_or_orphaned_link_gets_no_key() {
    let rig = Rig::new(|_| ok("{}")).await;
    let forged_url = HubLink { hub_url: "https://attacker.example".into(), ..rig.link("a", ProviderKind::Claude) };
    let orphan = HubLink { hub_id: Uuid::new_v4(), ..rig.link("a", ProviderKind::Claude) };
    // A forged hub record reusing a direct account's id: the keyring has nothing under it.
    let forged_hub = Hub { id: Uuid::new_v4(), url: "https://attacker.example".into(), ..rig.hub.clone() };
    let mut saved = rig.store.hubs().unwrap();
    saved.push(forged_hub.clone());
    rig.store.save_hubs(&saved).unwrap();
    let forged_id = HubLink { hub_id: forged_hub.id, hub_url: forged_hub.url.clone(), ..rig.link("a", ProviderKind::Claude) };

    for (link, expected) in [
        (&forged_url, "https://attacker.example"),
        (&orphan, &rig.hub.url),
        (&forged_id, "https://attacker.example"),
    ] {
        let error = HubRoute::new(&rig.client, &rig.store, &rig.keys, link, ProviderKind::Claude).err().expect("no key").to_string();
        assert!(error.contains("is missing"), "{expected}: {error}");
    }
    assert!(rig.calls().await.is_empty());
}

#[tokio::test]
async fn a_rewritten_link_for_a_saved_hub_id_never_gets_the_key() {
    let rig = Rig::new(|_| ok("{}")).await;
    let rewritten = HubLink { hub_url: "https://attacker.example".into(), ..rig.link("a", ProviderKind::Claude) };
    let mut saved = rig.hub.clone();
    saved.url = "https://attacker.example".into();
    rig.store.save_hubs(&[saved]).unwrap();
    let error = HubRoute::new(&rig.client, &rig.store, &rig.keys, &rewritten, ProviderKind::Claude).err().expect("no key");
    assert_eq!(
        error.to_string(),
        "The CLIProxyAPI hub at attacker.example isn’t the address its management key was saved for. Remove the hub and connect it again."
    );
}

#[tokio::test]
async fn an_upstream_refusal_of_the_hubs_token_reads_as_no_working_sign_in() {
    let rig = Rig::new(|_| (401, r#"{"error":"unauthorized"}"#.into(), vec![])).await;
    let link = rig.link("a", ProviderKind::Claude);
    let claude = ClaudeClient::default().with_transport(rig.transport(&link, ProviderKind::Claude));
    let error = link.refusal(claude.fetch(&mut link.tokens(), false, Utc::now()).await.unwrap_err());
    assert_eq!(
        error.to_string(),
        format!("The CLIProxyAPI hub at {} has no working sign-in for this login. Sign it in again on the hub.", link.host())
    );
}

#[tokio::test]
async fn a_hub_account_never_renews_and_a_401_is_not_retried() {
    let rig = Rig::new(|_| (401, "{}".into(), vec![])).await;
    let link = rig.link("a", ProviderKind::Claude);
    let tokens = link.tokens();
    assert!(!tokens.needs_refresh(Utc::now()) && tokens.refresh_token.is_none() && tokens.expires_at.is_none());
    let claude = ClaudeClient::default().with_transport(rig.transport(&link, ProviderKind::Claude));
    let mut current = tokens.clone();
    assert!(claude.fetch(&mut current, false, Utc::now()).await.is_err());
    assert_eq!(current, tokens);
    assert_eq!(rig.calls().await.len(), 1, "one read, no retry");
}

#[tokio::test]
async fn a_hub_answer_that_a_login_is_gone_is_reported_for_a_relist() {
    let rig = Rig::new(|_| ok("{}")).await;
    rig.server.reset().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":"auth credential not found for auth_index"}"#)).mount(&rig.server).await;
    let link = rig.link("a", ProviderKind::Claude);
    let claude = ClaudeClient::default().with_transport(rig.transport(&link, ProviderKind::Claude));
    let error = claude.fetch(&mut link.tokens(), false, Utc::now()).await.unwrap_err();
    assert_eq!(error.to_string(), format!("This login is no longer on the CLIProxyAPI hub at {}.", link.host()));
    assert_eq!(rig.client.take_login_missing(), vec![rig.hub.id]);
}
