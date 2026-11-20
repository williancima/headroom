//! The two management endpoints Headroom calls on a CLIProxyAPI hub: the login listing, and `api-call`,
//! which sends a request upstream with the hub's own token in place of `$TOKEN$`. Every call goes through
//! the hub's key gate. Neither the key nor a body is ever logged.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::{redirect, Client, Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::provider::ProviderError;
use crate::store::Store;

use super::gate;
use super::login::HubLogin;
use super::model::{allows_key, host_of, HubLink};

const MANAGEMENT_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A request a provider client built, to be sent through the hub as the linked login.
#[derive(Debug, Clone)]
pub struct UpstreamRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,
    pub timeout: Duration,
}

/// An answer from the hub, or (from `call`) the upstream answer inside the `api-call` envelope. Header
/// names are lowercase and repeated values are joined with `, `.
#[derive(Debug, Clone, PartialEq)]
pub struct HubReply {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl HubReply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_lowercase()).map(String::as_str)
    }
}

/// A management answer that isn't a success, sorted by what the user can do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HubRefusal {
    KeyRejected,
    Banned,
    RemoteManagementOff,
    ManagementOff,
    LoginMissing,
    NoToken,
    UpstreamUnreachable,
    Failed(u16),
}

impl HubRefusal {
    /// `None` for a 2xx answer.
    pub fn of(status: u16, body: &[u8]) -> Option<Self> {
        if (200..300).contains(&status) {
            return None;
        }
        let message = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(str::to_lowercase))
            .unwrap_or_default();
        Some(match status {
            401 => Self::KeyRejected,
            403 if message.starts_with("ip banned") => Self::Banned,
            403 if message.contains("remote management disabled") => Self::RemoteManagementOff,
            404 => Self::ManagementOff,
            400 if message.contains("auth credential not found") => Self::LoginMissing,
            400 if message.contains("token") => Self::NoToken,
            502 => Self::UpstreamUnreachable,
            s => Self::Failed(s),
        })
    }

    /// A refusal that a repeat would turn into another strike toward the hub's IP ban.
    pub fn closes_gate(self) -> bool {
        matches!(self, Self::KeyRejected | Self::Banned)
    }

    pub fn error(self, host: &str, upstream: Option<&str>) -> ProviderError {
        let unavailable = |text: String| ProviderError::Unavailable(format!("The CLIProxyAPI hub at {host} {text}"));
        match self {
            Self::KeyRejected => unavailable("rejected the management key. Replace it with `headroom hubs key`.".into()),
            Self::Banned => unavailable(
                "blocked this machine after too many wrong keys. Wait 30 minutes, then replace the management key with `headroom hubs key`.".into(),
            ),
            Self::RemoteManagementOff => unavailable("only allows management from its own machine. Turn on allow-remote in its config.".into()),
            Self::ManagementOff => unavailable("has its management API turned off. Set a management key in its config.".into()),
            Self::LoginMissing => ProviderError::Unavailable(format!("This login is no longer on the CLIProxyAPI hub at {host}.")),
            Self::NoToken => unavailable("has no working sign-in for this login. Sign it in again on the hub.".into()),
            Self::UpstreamUnreachable => {
                ProviderError::Unreachable(format!("The CLIProxyAPI hub at {host} couldn’t reach {}.", upstream.unwrap_or("the provider")))
            }
            Self::Failed(status) => unavailable(format!("answered with an error (HTTP {status}).")),
        }
    }
}

#[derive(Clone)]
pub struct HubClient {
    http: Client,
    store: Store,
    /// One call at a time per hub, so a refused key is saved before the next call waiting behind it looks.
    locks: Arc<Mutex<HashMap<Uuid, Arc<AsyncMutex<()>>>>>,
    login_missing: Arc<Mutex<HashSet<Uuid>>>,
}

impl HubClient {
    /// `http` must not follow redirects: the management key would go with them. Use `default_http`.
    pub fn new(http: Client, store: Store) -> Self {
        Self { http, store, locks: Default::default(), login_missing: Default::default() }
    }

    pub fn default_http() -> Client {
        crate::http::client_builder().redirect(redirect::Policy::none()).build().expect("reqwest client")
    }

    /// Hubs that answered that a login is gone since the last call: the caller reads their list again.
    pub fn take_login_missing(&self) -> Vec<Uuid> {
        self.login_missing.lock().unwrap().drain().collect()
    }

    pub async fn logins(&self, hub: Uuid, url: &str, key: &str) -> Result<Vec<HubLogin>, ProviderError> {
        let reply = self.management(Method::GET, "auth-files", hub, url, key, None, MANAGEMENT_TIMEOUT, None).await?;
        HubLogin::listing(&reply.body).ok_or_else(|| {
            ProviderError::Unavailable(format!("The CLIProxyAPI hub at {} sent a login list Headroom can't read.", host_of(url)))
        })
    }

    /// The http address of an https hub that answers there instead, asked without the key: `None` when
    /// nothing answers, or when the key couldn't go to that address over http anyway.
    pub async fn plain_http_address(&self, url: &str) -> Option<String> {
        let mut plain = Url::parse(url).ok()?;
        if plain.scheme() != "https" || plain.set_scheme("http").is_err() {
            return None;
        }
        let plain = plain.as_str().trim_end_matches('/').to_string();
        if !allows_key(&plain) {
            return None;
        }
        let response = self.http.get(&plain).timeout(PROBE_TIMEOUT).send().await.ok()?;
        (response.status().as_u16() == 200).then_some(plain)
    }

    /// Sends `request` through the hub as the linked login and returns the upstream answer, so a provider
    /// client reads it as if it had called the provider itself.
    pub async fn call(&self, request: &UpstreamRequest, link: &HubLink, key: &str, upstream: &str) -> Result<HubReply, ProviderError> {
        let mut payload = json!({
            "auth_index": link.auth_index,
            "method": request.method,
            "url": request.url,
            "header": request.headers,
        });
        if let Some(body) = &request.body {
            payload["data"] = Value::String(String::from_utf8_lossy(body).into_owned());
        }
        let body = serde_json::to_vec(&payload).map_err(|_| ProviderError::InvalidResponse)?;
        let reply = self
            .management(Method::POST, "api-call", link.hub_id, &link.hub_url, key, Some(body), request.timeout + Duration::from_secs(10), Some(upstream))
            .await?;
        unwrap(&reply.body).ok_or(ProviderError::InvalidResponse)
    }

    #[allow(clippy::too_many_arguments)]
    async fn management(
        &self,
        method: Method,
        path: &str,
        hub: Uuid,
        url: &str,
        key: &str,
        body: Option<Vec<u8>>,
        timeout: Duration,
        upstream: Option<&str>,
    ) -> Result<HubReply, ProviderError> {
        let host = host_of(url);
        if !allows_key(url) {
            return Err(ProviderError::Unavailable(format!(
                "Use https for {host}. Headroom uses http only for a hub on this machine, your local network, or Tailscale."
            )));
        }
        let mut request = self
            .http
            .request(method, format!("{}/v0/management/{path}", url.trim_end_matches('/')))
            .header("Authorization", format!("Bearer {key}"))
            .header("Accept", "application/json")
            .timeout(timeout);
        if let Some(body) = body {
            request = request.header("Content-Type", "application/json").body(body);
        }
        let lock = self.locks.lock().unwrap().entry(hub).or_default().clone();
        let _turn = lock.lock().await;
        if let Some(refusal) = gate::refusal_of(&self.store, hub, key) {
            return Err(refusal.error(&host, None));
        }
        let response = request.send().await.map_err(|_| ProviderError::Unreachable(format!("Can’t reach the CLIProxyAPI hub at {host}.")))?;
        let reply = read(response).await?;
        let Some(refusal) = HubRefusal::of(reply.status, &reply.body) else { return Ok(reply) };
        if refusal.closes_gate() {
            gate::close(&self.store, hub, key, refusal);
        }
        if refusal == HubRefusal::LoginMissing {
            self.login_missing.lock().unwrap().insert(hub);
        }
        Err(refusal.error(&host, upstream))
    }
}

pub(super) async fn read(response: reqwest::Response) -> Result<HubReply, ProviderError> {
    let status = response.status().as_u16();
    let mut headers: HashMap<String, String> = HashMap::new();
    for (name, value) in response.headers() {
        if let Ok(value) = value.to_str() {
            headers.entry(name.as_str().to_lowercase()).and_modify(|v| *v = format!("{v}, {value}")).or_insert_with(|| value.to_string());
        }
    }
    let body = response.bytes().await.map_err(|_| ProviderError::ConnectionFailed)?.to_vec();
    Ok(HubReply { status, headers, body })
}

#[derive(Deserialize)]
struct Envelope {
    status_code: u16,
    header: Option<HashMap<String, Vec<String>>>,
    body: Option<String>,
}

/// The upstream response inside an `api-call` envelope: `{status_code, header: {name: [value]}, body}`.
pub fn unwrap(body: &[u8]) -> Option<HubReply> {
    let envelope: Envelope = serde_json::from_slice(body).ok()?;
    let headers = envelope
        .header
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, values)| !values.is_empty())
        .map(|(name, values)| (name.to_lowercase(), values.join(", ")))
        .collect();
    Some(HubReply { status: envelope.status_code, headers, body: envelope.body.unwrap_or_default().into_bytes() })
}
