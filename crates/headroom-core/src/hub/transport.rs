//! How a provider client sends a request: straight to the provider, or, for an account mirrored from a hub,
//! wrapped in the hub's `api-call`, where the hub puts the login's own token in place of `$TOKEN$`. The
//! providers build their requests unchanged and only swap their `send` for this.

use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::RequestBuilder;

use crate::models::ProviderKind;
use crate::provider::ProviderError;
use crate::store::Store;

use super::client::{read, HubClient, HubReply, UpstreamRequest};
use super::keys::HubKeys;
use super::model::{host_of, HubLink};

/// What a provider reads from an answer: status, lowercase headers, body.
pub type Reply = HubReply;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Default)]
pub enum Transport {
    #[default]
    Direct,
    Hub(Box<HubRoute>),
}

/// A hub account's way out: the hub client, its link, the management key, and the provider's name for
/// messages ("Claude", "ChatGPT"). The key is only ever obtained through `HubRoute::new`.
#[derive(Clone)]
pub struct HubRoute {
    client: HubClient,
    link: HubLink,
    key: String,
    upstream: String,
}

impl HubRoute {
    /// Fails, with the same message as a missing key, unless the link names a hub saved under that id and
    /// address, and then takes the key only for that address. A forged or orphaned link gets no key.
    pub fn new(client: &HubClient, store: &Store, keys: &dyn HubKeys, link: &HubLink, kind: ProviderKind) -> Result<Self, ProviderError> {
        let saved = store.hubs().map_err(|e| ProviderError::Unavailable(e.to_string()))?;
        if !saved.iter().any(|h| h.id == link.hub_id && h.url == link.hub_url) {
            return Err(ProviderError::Unavailable(format!(
                "The management key for the CLIProxyAPI hub at {} is missing. Replace it with `headroom hubs key`.",
                host_of(&link.hub_url)
            )));
        }
        let key = keys.key_for(link.hub_id, &link.hub_url)?;
        Ok(Self { client: client.clone(), link: link.clone(), key, upstream: kind.display_name().to_string() })
    }
}

pub async fn send(transport: &Transport, request: RequestBuilder) -> Result<Reply, ProviderError> {
    match transport {
        Transport::Direct => read(request.send().await.map_err(|_| ProviderError::ConnectionFailed)?).await,
        Transport::Hub(route) => {
            let request = request.build().map_err(|_| ProviderError::InvalidResponse)?;
            let headers: BTreeMap<String, String> = request
                .headers()
                .iter()
                .filter_map(|(name, value)| Some((name.as_str().to_string(), value.to_str().ok()?.to_string())))
                .collect();
            let upstream = UpstreamRequest {
                method: request.method().as_str().to_string(),
                url: request.url().to_string(),
                headers,
                body: request.body().and_then(|b| b.as_bytes()).map(<[u8]>::to_vec),
                timeout: request.timeout().copied().unwrap_or(DEFAULT_TIMEOUT),
            };
            route_call(route, &upstream).await
        }
    }
}

async fn route_call(route: &HubRoute, request: &UpstreamRequest) -> Result<Reply, ProviderError> {
    route.client.call(request, &route.link, &route.key, &route.upstream).await
}
