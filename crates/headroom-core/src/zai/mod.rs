//! Z.ai provider: API-key usage read, mapping and reset claims. The reset logic lives here; the CLI claim command waits for
//! phase 4. Never log keys or response bodies.

pub mod mapper;
pub mod resets;
#[cfg(test)]
mod tests;

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{header, Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::claude::{ClaimFailure, ClaimOutcome};
use crate::models::ProviderSnapshot;
use crate::provider::{KeyResponse, ProviderError};
pub use resets::{list_due, Reset, ResetWindow};

pub const BASE_URL: &str = "https://api.z.ai";
const TIMEOUT: Duration = Duration::from_secs(15);
const REJECTED: &str = "Z.ai rejected the API key.";

/// The hourly secondary reads, kept between passes: the plan and the banked resets (`None` when the read
/// failed). Stored as JSON in `ProviderState::cached_body`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ZaiCache {
    pub plan: Option<String>,
    pub resets: Option<Vec<Reset>>,
}

impl ZaiCache {
    pub fn decode(text: Option<&str>) -> Option<Self> {
        serde_json::from_str(text?).ok()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

#[derive(Debug)]
pub struct ZaiFetch {
    pub snapshot: ProviderSnapshot,
    /// The secondary reads behind this snapshot: fresh when `read_secondary` was set, else the cache passed in.
    pub cache: ZaiCache,
}

pub struct ZaiClient {
    http: Client,
    base_url: String,
}

impl Default for ZaiClient {
    fn default() -> Self {
        Self::new(BASE_URL)
    }
}

impl ZaiClient {
    pub fn new(base_url: &str) -> Self {
        let http = crate::http::client_builder().timeout(TIMEOUT).build().unwrap_or_default();
        Self { http, base_url: base_url.trim_end_matches('/').to_string() }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    async fn send(&self, request: RequestBuilder, api_key: &str) -> Result<KeyResponse, ProviderError> {
        let request = request.bearer_auth(api_key).header(header::ACCEPT, "application/json");
        KeyResponse::read(request.send().await.map_err(|_| ProviderError::ConnectionFailed)?).await
    }

    async fn get(&self, path: &str, api_key: &str) -> Result<KeyResponse, ProviderError> {
        self.send(self.http.get(self.url(path)), api_key).await
    }

    /// The available, unexpired resets, earliest first.
    pub async fn read_resets(&self, api_key: &str, now: DateTime<Utc>) -> Result<Vec<Reset>, ProviderError> {
        let response = self.get("/api/biz/customer-package-reset/list?targetType=PERSONAL", api_key).await?;
        resets::parse_resets(&response.require_success("Z.ai")?.body, now)
    }

    /// The quota read, plus the plan and the reset list when `read_secondary` is set (each best-effort: a
    /// failure costs only its row). Resets that expired since the last read are dropped.
    pub async fn fetch(&self, api_key: &str, cached: Option<ZaiCache>, read_secondary: bool, now: DateTime<Utc>) -> Result<ZaiFetch, ProviderError> {
        let quota = self.get("/api/monitor/usage/quota/limit", api_key).await?;
        let quota = quota.require_success("Z.ai")?;
        let mut cache = cached.unwrap_or_default();
        if read_secondary {
            let subscription = self.get("/api/biz/subscription/list", api_key).await;
            cache.plan = subscription.ok().filter(KeyResponse::is_success).and_then(|r| mapper::plan_name(&r.body));
        }
        let mut lines = mapper::map_quota(&quota.body).map_err(|e| match e {
            ProviderError::SignInExpired => ProviderError::KeyRejected(REJECTED.into()),
            other => other,
        })?;
        if read_secondary {
            cache.resets = match self.read_resets(api_key, now).await {
                Ok(list) => Some(list),
                Err(error) => {
                    eprintln!("headroom: Z.ai reset list failed; retrying in an hour: {error}");
                    None
                }
            };
        }
        let live: Option<Vec<Reset>> = cache.resets.as_ref().map(|r| r.iter().filter(|r| r.expires_at > now).cloned().collect());
        let (mut reset_expiries, mut reset_items) = (vec![], vec![]);
        if let Some(live) = &live {
            lines.push(resets::line(live));
            reset_expiries = live.iter().map(|r| r.expires_at).collect();
            reset_items = resets::items(live);
        }
        let snapshot = ProviderSnapshot { plan: cache.plan.clone(), lines, refreshed_at: now, reset_expiries, reset_items, notice: None, breakdowns: Default::default() };
        Ok(ZaiFetch { snapshot, cache })
    }

    /// Spends exactly `reset`, and only while it is still its window's earliest. Anything else is
    /// `NoCredit`, and the next refresh shows the list to confirm again. A request whose answer never
    /// arrives is not retried (`Err(ConnectionFailed)`).
    pub async fn claim_reset(&self, api_key: &str, reset: &Reset, request_id: &str, now: DateTime<Utc>) -> Result<ClaimOutcome, ProviderError> {
        let current = match self.read_resets(api_key, now).await {
            Ok(list) => list,
            Err(ProviderError::KeyRejected(_) | ProviderError::SignInExpired) => return Ok(ClaimOutcome::Failed(ClaimFailure::SignInExpired)),
            Err(ProviderError::ConnectionFailed) => return Err(ProviderError::ConnectionFailed),
            Err(_) => return Ok(ClaimOutcome::Failed(ClaimFailure::Refused)),
        };
        if current.iter().find(|r| r.window == reset.window) != Some(reset) {
            return Ok(ClaimOutcome::NoCredit);
        }
        let payload = json!({
            "targetType": "PERSONAL",
            "resetType": reset.window.reset_type(),
            "recordId": reset.record_id,
            "requestId": request_id,
            "grantType": reset.grant_type,
        });
        let request = self.http.post(self.url("/api/biz/customer-package-reset/use")).json(&payload);
        let response = self.send(request, api_key).await?;
        let auth_failure = matches!(response.require_success("Z.ai"), Err(ProviderError::KeyRejected(_)));
        Ok(resets::claim_outcome(response.status, auth_failure, &response.body))
    }
}
