//! Grok provider: device-code sign-in, usage read and mapping. Never log tokens or response bodies.
//!
//! Request building (`GrokClient`) and mapping (`mapper`, pure functions over bodies) are separate.

mod login;
pub mod mapper;
#[cfg(test)]
mod mapper_tests;
#[cfg(test)]
mod tests;

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{header, Client, RequestBuilder};

use crate::hub::transport::{self, Transport};
use crate::models::{OAuthTokens, ProviderSnapshot};
use crate::oauth::{self, TokenResponse};
use crate::provider::ProviderError;

pub const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const SCOPES: &str = "openid email offline_access grok-cli:access";
const TOKEN_AUTH: &str = "xai-grok-cli";
const USER_AGENT: &str = "Headroom";
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const TOKEN_TIMEOUT: Duration = Duration::from_secs(15);

/// Endpoints, injectable for tests. `Default` is the real service.
#[derive(Debug, Clone)]
pub struct GrokConfig {
    pub device_start_url: String,
    pub token_url: String,
    pub settings_url: String,
    pub credits_url: String,
}

impl Default for GrokConfig {
    fn default() -> Self {
        Self {
            device_start_url: "https://auth.x.ai/oauth2/device/code".into(),
            token_url: "https://auth.x.ai/oauth2/token".into(),
            settings_url: "https://cli-chat-proxy.grok.com/v1/settings".into(),
            credits_url: "https://cli-chat-proxy.grok.com/v1/billing?format=credits".into(),
        }
    }
}

#[derive(Debug)]
pub struct GrokFetch {
    pub snapshot: ProviderSnapshot,
    /// The plan behind this snapshot: the fresh read when `read_plan` was set (`None` when it failed),
    /// else the one passed in. The caller keeps it and records when it was last read.
    pub plan: Option<String>,
}

struct Resp {
    status: u16,
    body: Vec<u8>,
    retry_after: Option<String>,
}

impl Resp {
    /// 401/403 -> SignInExpired; 429 -> RateLimited; other failures -> RequestFailed.
    fn require_success(&self) -> Result<&Self, ProviderError> {
        match self.status {
            401 | 403 => Err(ProviderError::SignInExpired),
            429 => Err(ProviderError::RateLimited { retry_after_secs: self.retry_after.as_ref().and_then(|v| v.trim().parse().ok()) }),
            200..=299 => Ok(self),
            s => Err(ProviderError::RequestFailed(s)),
        }
    }
}

pub struct GrokClient {
    http: Client,
    config: GrokConfig,
    transport: Transport,
}

impl Default for GrokClient {
    fn default() -> Self {
        Self::new(GrokConfig::default())
    }
}

impl GrokClient {
    pub fn new(config: GrokConfig) -> Self {
        let http = crate::http::client_builder().timeout(TOKEN_TIMEOUT).build().unwrap_or_default();
        Self { http, config, transport: Transport::Direct }
    }

    /// Sends every usage-read request through `transport` (a hub account's login lives on its hub).
    pub fn with_transport(mut self, transport: Transport) -> Self {
        self.transport = transport;
        self
    }

    /// Form POST of `grant_type`, `client_id`, `refresh_token`. The expiry is `expires_in` from now.
    /// Extras are kept.
    pub async fn refresh(&self, tokens: &OAuthTokens, now: DateTime<Utc>) -> Result<OAuthTokens, ProviderError> {
        let refresh_token = tokens.refresh_token.as_deref().ok_or(ProviderError::SignInExpired)?;
        let fields = [("grant_type", "refresh_token"), ("client_id", CLIENT_ID), ("refresh_token", refresh_token)];
        let response = oauth::post_form(&self.http, &self.config.token_url, &fields, &[]).await?;
        let granted = TokenResponse::from_renewal(&response)?;
        let mut renewed = tokens.clone();
        renewed.expires_at = granted.expires_at(now);
        renewed.access_token = granted.access_token;
        if granted.refresh_token.is_some() {
            renewed.refresh_token = granted.refresh_token;
        }
        Ok(renewed)
    }

    async fn send(&self, request: RequestBuilder) -> Result<Resp, ProviderError> {
        let reply = transport::send(&self.transport, request).await?;
        let retry_after = reply.header("retry-after").map(String::from);
        Ok(Resp { status: reply.status, body: reply.body, retry_after })
    }

    fn get(&self, url: &str, access_token: &str) -> RequestBuilder {
        self.http
            .get(url)
            .bearer_auth(access_token.trim())
            .header("X-XAI-Token-Auth", TOKEN_AUTH)
            .header(header::ACCEPT, "application/json")
            .header(header::USER_AGENT, USER_AGENT)
            .timeout(READ_TIMEOUT)
    }

    /// Refreshes first when due (an early refresh that fails keeps the unexpired token), sends, and on a
    /// 401 with a refresh token renews once and retries once. `tokens` is updated in place as soon as a
    /// refresh succeeds, so the rotated refresh token survives a failing request after it.
    async fn authorized(&self, tokens: &mut OAuthTokens, now: DateTime<Utc>, url: &str) -> Result<Resp, ProviderError> {
        if tokens.needs_refresh(now) {
            match self.refresh(tokens, now).await {
                Ok(renewed) => *tokens = renewed,
                Err(_) if tokens.expires_at.is_some_and(|at| at > now) => {}
                Err(e) => return Err(e),
            }
        }
        let mut response = self.send(self.get(url, &tokens.access_token)).await?;
        if response.status == 401 && tokens.refresh_token.is_some() {
            *tokens = self.refresh(tokens, now).await?;
            response = self.send(self.get(url, &tokens.access_token)).await?;
        }
        Ok(response)
    }

    /// The credits read, plus the plan from the settings when `read_plan` is set. A failed plan read keeps
    /// the usage and records no plan (`plan: None`); the caller retries at its next due time. A team
    /// principal's refusal is a notice, not an error.
    ///
    /// `tokens` is updated in place (refresh, rotated refresh token), also when this returns `Err`.
    /// The caller must persist it whenever it differs from its copy, on Ok and on Err.
    pub async fn fetch(
        &self,
        tokens: &mut OAuthTokens,
        cached_plan: Option<String>,
        read_plan: bool,
        now: DateTime<Utc>,
    ) -> Result<GrokFetch, ProviderError> {
        let response = self.authorized(tokens, now, &self.config.credits_url).await?;
        let team = mapper::is_team_refusal(response.status, &response.body);
        let lines = if team { vec![] } else { mapper::map_credits_config(&response.require_success()?.body)? };
        let plan = if read_plan { self.read_plan(&tokens.access_token).await } else { cached_plan };
        let snapshot = ProviderSnapshot { plan: plan.clone(), lines, refreshed_at: now, reset_expiries: vec![], reset_items: vec![], notice: team.then(|| mapper::TEAM_NOTICE.to_string()), breakdowns: Default::default() };
        Ok(GrokFetch { snapshot, plan })
    }

    async fn read_plan(&self, access_token: &str) -> Option<String> {
        let response = self.send(self.get(&self.config.settings_url, access_token)).await.ok()?;
        (200..300).contains(&response.status).then(|| mapper::plan_name(&response.body)).flatten()
    }
}
