use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::models::ProviderKind;
use crate::provider::ProviderError;
use crate::store::Store;

const LISTING: &str = r#"{"files":[{"id":"a","auth_index":"index-a","provider":"claude","account_type":"oauth","email":"a@example.com"},
{"id":"k","auth_index":"index-k","provider":"kimi","account_type":"oauth"}]}"#;

struct Rig {
    dir: std::path::PathBuf,
    store: Store,
    client: HubClient,
    keys: MemoryHubKeys,
    server: MockServer,
}

impl Rig {
    async fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("headroom-hub-{}", Uuid::new_v4()));
        let store = Store::at(&dir.join("config"), &dir.join("state"));
        let client = HubClient::new(HubClient::default_http(), store.clone());
        Self { dir, store, client, keys: MemoryHubKeys::default(), server: MockServer::start().await }
    }

    fn url(&self) -> String {
        self.server.uri()
    }

    async fn connect(&self, key: &str) -> Result<(Hub, Synced), ProviderError> {
        connect(&self.client, &self.store, &self.keys, &self.url(), key).await
    }

    async fn serve_listing(&self, key: &str) {
        Mock::given(method("GET"))
            .and(path("/v0/management/auth-files"))
            .and(header("authorization", format!("Bearer {key}").as_str()))
            .and(header("accept", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(LISTING))
            .mount(&self.server)
            .await;
    }

    async fn requests(&self) -> usize {
        self.server.received_requests().await.unwrap().len()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn link(hub: &Hub) -> HubLink {
    HubLink { hub_id: hub.id, hub_url: hub.url.clone(), login_id: "a".into(), auth_index: "index-a".into(), chatgpt_account_id: None }
}

fn upstream() -> UpstreamRequest {
    UpstreamRequest {
        method: "GET".into(),
        url: "https://api.anthropic.com/api/oauth/usage".into(),
        headers: BTreeMap::from([("Authorization".into(), "Bearer $TOKEN$".into())]),
        body: Some(br#"{"q":1}"#.to_vec()),
        timeout: Duration::from_secs(15),
    }
}

fn message(error: ProviderError) -> String {
    error.to_string()
}

#[tokio::test]
async fn connecting_saves_the_key_and_the_hub_and_mirrors_the_supported_logins() {
    let rig = Rig::new().await;
    rig.serve_listing("mgmt-key").await;
    let (hub, synced) = rig.connect(" mgmt-key ").await.unwrap();

    assert_eq!(synced, Synced { unsupported: 1 });
    assert_eq!(rig.store.hubs().unwrap(), vec![hub.clone()]);
    let accounts = rig.store.accounts().unwrap();
    assert_eq!((accounts.len(), accounts[0].kind, accounts[0].label.as_str()), (1, ProviderKind::Claude, "a@example.com"));
    assert_eq!(accounts[0].hub, Some(link(&hub)));
    assert_eq!(rig.keys.load(hub.id).unwrap(), Some(HubKey { key: "mgmt-key".into(), url: hub.url.clone() }));

    let again = rig.connect("mgmt-key").await.unwrap_err();
    assert!(message(again).ends_with("is already connected."));
}

#[tokio::test]
async fn a_wrong_address_or_key_saves_nothing() {
    let rig = Rig::new().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(401)).mount(&rig.server).await;
    let error = rig.connect("wrong").await.unwrap_err();
    assert!(message(error).contains("rejected the management key"));
    assert!(rig.store.hubs().unwrap().is_empty() && rig.store.accounts().unwrap().is_empty());

    assert_eq!(message(rig.connect("  ").await.unwrap_err()), "Enter the hub’s management key.");
    let bad = connect(&rig.client, &rig.store, &rig.keys, "ftp://hub", "k").await.unwrap_err();
    assert!(message(bad).starts_with("Enter the hub’s address"));
}

#[tokio::test]
async fn plain_http_to_a_public_name_is_refused_before_anything_is_sent() {
    let rig = Rig::new().await;
    let error = connect(&rig.client, &rig.store, &rig.keys, "http://hub.example.com:8317", "k").await.unwrap_err();
    assert_eq!(
        message(error),
        "Use https for hub.example.com:8317. Headroom uses http only for a hub on this machine, your local network, or Tailscale."
    );
    assert!(rig.store.hubs().unwrap().is_empty());
}

#[tokio::test]
async fn an_https_hub_that_answers_only_over_http_says_to_use_http_and_the_probe_carries_no_key() {
    let rig = Rig::new().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_string("CLIProxyAPI Server")).mount(&rig.server).await;
    let https = rig.url().replacen("http://", "https://", 1);
    let error = connect(&rig.client, &rig.store, &rig.keys, &https, "mgmt-key").await.unwrap_err();
    assert_eq!(message(error), format!("This hub doesn’t use https. Try {}.", rig.url()));
    let seen = rig.server.received_requests().await.unwrap();
    assert!(seen.iter().all(|r| r.headers.get("authorization").is_none()), "the http check carries no key");
    assert_eq!(seen.len(), 1);

    let down = connect(&rig.client, &rig.store, &rig.keys, "https://10.0.0.9:1", "k").await.unwrap_err();
    assert_eq!(message(down), "Can’t reach the CLIProxyAPI hub at 10.0.0.9:1.");
    assert!(rig.store.hubs().unwrap().is_empty());
}

#[tokio::test]
async fn a_call_wraps_the_request_in_the_api_call_envelope_and_unwraps_the_answer() {
    let rig = Rig::new().await;
    let hub = Hub::new(rig.url());
    let envelope = json!({"status_code": 429, "header": {"Retry-After": ["30"], "X-Multi": ["a", "b"], "Empty": []}, "body": "{\"e\":1}"});
    Mock::given(method("POST"))
        .and(path("/v0/management/api-call"))
        .and(header("authorization", "Bearer mgmt-key"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({
            "auth_index": "index-a", "method": "GET", "url": "https://api.anthropic.com/api/oauth/usage",
            "header": {"Authorization": "Bearer $TOKEN$"}, "data": "{\"q\":1}"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope))
        .mount(&rig.server)
        .await;

    let reply = rig.client.call(&upstream(), &link(&hub), "mgmt-key", "Claude").await.unwrap();
    assert_eq!(reply.status, 429, "the upstream status is returned as is");
    assert_eq!((reply.header("retry-after"), reply.header("X-Multi"), reply.header("empty")), (Some("30"), Some("a, b"), None));
    assert_eq!(reply.body, br#"{"e":1}"#);
}

#[tokio::test]
async fn an_unreadable_envelope_is_an_invalid_response() {
    let rig = Rig::new().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_string("<html>")).mount(&rig.server).await;
    let error = rig.client.call(&upstream(), &link(&Hub::new(rig.url())), "k", "Claude").await.unwrap_err();
    assert_eq!(error, ProviderError::InvalidResponse);
    assert!(unwrap(br#"{"status_code":200}"#).is_some_and(|r| r.body.is_empty()));
}

#[tokio::test]
async fn every_hub_answer_reads_as_an_account_specific_message() {
    let rig = Rig::new().await;
    let hub = Hub::new(rig.url());
    let host = hub.host();
    let cases: [(u16, &str, String); 8] = [
        (401, "", format!("The CLIProxyAPI hub at {host} rejected the management key. Replace it with `headroom hubs key`.")),
        (403, r#"{"error":"IP banned due to too many failed attempts"}"#, format!("The CLIProxyAPI hub at {host} blocked this machine after too many wrong keys. Wait 30 minutes, then replace the management key with `headroom hubs key`.")),
        (403, r#"{"error":"remote management disabled"}"#, format!("The CLIProxyAPI hub at {host} only allows management from its own machine. Turn on allow-remote in its config.")),
        (404, "", format!("The CLIProxyAPI hub at {host} has its management API turned off. Set a management key in its config.")),
        (400, r#"{"error":"auth credential not found"}"#, format!("This login is no longer on the CLIProxyAPI hub at {host}.")),
        (400, r#"{"error":"no token"}"#, format!("The CLIProxyAPI hub at {host} has no working sign-in for this login. Sign it in again on the hub.")),
        (502, "", format!("The CLIProxyAPI hub at {host} couldn’t reach Claude.")),
        (500, "", format!("The CLIProxyAPI hub at {host} answered with an error (HTTP 500).")),
    ];
    for (status, body, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(status).set_body_string(body)).mount(&server).await;
        let hub = Hub::new(server.uri());
        let error = rig.client.call(&upstream(), &link(&hub), "k", "Claude").await.unwrap_err();
        assert_eq!(error.to_string().replace(&hub.host(), &host), expected, "{status} {body}");
    }
    assert!(matches!(HubRefusal::UpstreamUnreachable.error("h", None), ProviderError::Unreachable(m) if m.ends_with("couldn’t reach the provider.")));
}

#[tokio::test]
async fn a_wrong_management_key_reaches_the_hub_once_until_the_key_is_replaced() {
    let rig = Rig::new().await;
    Mock::given(method("GET")).and(header("authorization", "Bearer old")).respond_with(ResponseTemplate::new(401)).mount(&rig.server).await;
    let mut hub = Hub::new(rig.url());
    rig.store.save_hubs(std::slice::from_ref(&hub)).unwrap();
    let first = rig.client.logins(hub.id, &hub.url, "old").await.unwrap_err();
    let second = rig.client.logins(hub.id, &hub.url, "old").await.unwrap_err();
    let from_a_new_process = HubClient::new(HubClient::default_http(), rig.store.clone());
    let third = from_a_new_process.logins(hub.id, &hub.url, "old").await.unwrap_err();

    assert_eq!(rig.requests().await, 1, "the refused key is sent once, and the refusal survives a new run");
    assert!(first == second && second == third && first.to_string().contains("rejected the management key"));
    hub = rig.store.hubs().unwrap().remove(0);
    let refused = hub.refused.expect("saved");
    assert!(refused.key.starts_with("key:") && !refused.key.contains("old") && refused.refusal == HubRefusal::KeyRejected);

    rig.serve_listing("new").await;
    assert!(rig.client.logins(hub.id, &hub.url, "new").await.is_ok(), "a refusal holds only the key it was sent with");
}

#[tokio::test]
async fn an_ip_ban_stops_calls_too() {
    let rig = Rig::new().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(403).set_body_string(r#"{"error":"IP banned for 30m"}"#)).mount(&rig.server).await;
    let hub = Hub::new(rig.url());
    rig.store.save_hubs(std::slice::from_ref(&hub)).unwrap();
    for _ in 0..3 {
        let error = rig.client.call(&upstream(), &link(&hub), "k", "Claude").await.unwrap_err();
        assert!(error.to_string().contains("blocked this machine"));
    }
    assert_eq!(rig.requests().await, 1);
}

#[tokio::test]
async fn replacing_the_key_tests_it_first_and_a_refused_key_keeps_the_old_one() {
    let rig = Rig::new().await;
    rig.serve_listing("good").await;
    let (hub, _) = rig.connect("good").await.unwrap();
    // The hub bans the saved key later.
    gate::close(&rig.store, hub.id, "good", HubRefusal::KeyRejected);
    let blocked = sync(&rig.client, &rig.store, &rig.keys, &hub).await.unwrap_err();
    assert!(blocked.to_string().contains("rejected the management key"));

    Mock::given(method("GET")).and(header("authorization", "Bearer bad")).respond_with(ResponseTemplate::new(401)).mount(&rig.server).await;
    assert!(replace_key(&rig.client, &rig.store, &rig.keys, hub.id, "bad").await.is_err());
    assert_eq!(rig.keys.load(hub.id).unwrap().unwrap().key, "good", "the old key stays");
    assert!(rig.store.hubs().unwrap()[0].refused.is_some(), "and the gate stays closed");

    rig.serve_listing("better").await;
    let synced = replace_key(&rig.client, &rig.store, &rig.keys, hub.id, "better").await.unwrap();
    assert_eq!(synced, Synced { unsupported: 1 });
    assert_eq!(rig.keys.load(hub.id).unwrap().unwrap().key, "better");
    assert!(rig.store.hubs().unwrap()[0].refused.is_none(), "the gate is open again");
    assert!(sync(&rig.client, &rig.store, &rig.keys, &rig.store.hubs().unwrap()[0]).await.is_ok());
}

#[tokio::test]
async fn a_rewritten_hub_address_never_gets_the_key() {
    let rig = Rig::new().await;
    rig.serve_listing("mgmt-key").await;
    let (mut hub, _) = rig.connect("mgmt-key").await.unwrap();
    let before = rig.requests().await;
    hub.url = "https://attacker.example".into();
    let error = sync(&rig.client, &rig.store, &rig.keys, &hub).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "The CLIProxyAPI hub at attacker.example isn’t the address its management key was saved for. Remove the hub and connect it again."
    );
    assert_eq!(rig.requests().await, before);
}

#[tokio::test]
async fn the_key_is_never_sent_along_a_redirect() {
    let rig = Rig::new().await;
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_string(LISTING)).mount(&elsewhere).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", format!("{}/v0/management/auth-files", elsewhere.uri()).as_str()))
        .mount(&rig.server)
        .await;
    let error = rig.client.logins(Uuid::new_v4(), &rig.url(), "mgmt-key").await.unwrap_err();
    assert!(error.to_string().contains("answered with an error (HTTP 302)"), "{error}");
    assert!(elsewhere.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_login_the_hub_says_is_gone_is_reported_for_a_relist() {
    let rig = Rig::new().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":"auth credential not found"}"#)).mount(&rig.server).await;
    let hub = Hub::new(rig.url());
    assert!(rig.client.call(&upstream(), &link(&hub), "k", "Claude").await.is_err());
    assert_eq!(rig.client.take_login_missing(), vec![hub.id]);
    assert!(rig.client.take_login_missing().is_empty());
}

#[tokio::test]
async fn removing_a_hub_removes_its_accounts_and_key_only() {
    let rig = Rig::new().await;
    rig.serve_listing("mgmt-key").await;
    let (hub, _) = rig.connect("mgmt-key").await.unwrap();
    let mut accounts = rig.store.accounts().unwrap();
    let direct = crate::models::Account::new(ProviderKind::Codex, "Direct");
    accounts.push(direct.clone());
    rig.store.save_accounts(&accounts).unwrap();

    remove(&rig.store, &rig.keys, hub.id).unwrap();

    assert_eq!(rig.store.accounts().unwrap(), vec![direct]);
    assert!(rig.store.hubs().unwrap().is_empty());
    assert_eq!(rig.keys.load(hub.id).unwrap(), None);
}

#[tokio::test]
async fn a_hub_with_its_accounts_survives_a_relaunch() {
    let rig = Rig::new().await;
    rig.serve_listing("mgmt-key").await;
    let (hub, _) = rig.connect("mgmt-key").await.unwrap();
    let relaunched = Store::at(&rig.dir.join("config"), &rig.dir.join("state"));
    assert_eq!(relaunched.hubs().unwrap(), vec![hub.clone()]);
    assert_eq!(relaunched.accounts().unwrap()[0].hub, Some(link(&hub)));
}

#[test]
fn an_account_without_a_hub_loads_and_round_trips_unchanged() {
    let json = json!({"id": "6f1c1f0e-8a52-4a52-9f43-6a4a9a3f0b11", "kind": "claude", "label": "Work", "added_at": "2026-01-01T00:00:00Z", "identity": null});
    let account: crate::models::Account = serde_json::from_value(json).unwrap();
    assert!(account.hub.is_none());
    assert!(serde_json::to_value(&account).unwrap().get("hub").is_none());
}

#[tokio::test]
async fn a_failed_listing_keeps_the_accounts_and_the_next_sync_drops_a_login_turned_off_on_the_hub() {
    let rig = Rig::new().await;
    rig.serve_listing("mgmt-key").await;
    let (hub, _) = rig.connect("mgmt-key").await.unwrap();
    assert_eq!(rig.store.accounts().unwrap().len(), 1);

    rig.server.reset().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(500)).mount(&rig.server).await;
    let error = sync(&rig.client, &rig.store, &rig.keys, &hub).await.unwrap_err();
    assert!(error.to_string().contains("answered with an error (HTTP 500)"));
    assert_eq!(rig.store.accounts().unwrap().len(), 1, "a failed listing changes nothing");

    rig.server.reset().await;
    let off = r#"{"files":[{"id":"a","auth_index":"index-a","provider":"claude","account_type":"oauth","disabled":true}]}"#;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_string(off)).mount(&rig.server).await;
    sync(&rig.client, &rig.store, &rig.keys, &hub).await.unwrap();
    assert!(rig.store.accounts().unwrap().is_empty(), "the next read drops the login turned off on the hub");
}
