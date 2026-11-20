//! Amp provider: device-code sign-in, balance read and mapping. Never log tokens or response bodies.

mod login;
pub mod mapper;
#[cfg(test)]
mod mapper_tests;
#[cfg(test)]
mod tests;

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{header, Client, RequestBuilder};

use crate::models::{OAuthTokens, ProviderSnapshot};
use crate::oauth::{self, jwt_expiry, TokenResponse};
use crate::provider::ProviderError;

pub const CLIENT_ID: &str = "client_01JNSEYM3V0J5AXK4YXNXRXTGP";
const BALANCE_BODY: &str = r#"{"method":"userDisplayBalanceInfo","params":{}}"#;
const TIMEOUT: Duration = Duration::from_secs(15);

/// Endpoints, injectable for tests. `Default` is the real service.
#[derive(Debug, Clone)]
pub struct AmpConfig {
    pub device_start_url: String,
    /// Device-code polling and refresh both go here.
    pub token_url: String,
    /// The page the sign-in opens; it gets `?user_code=`.
    pub verification_page: String,
    pub balance_url: String,
}

impl Default for AmpConfig {
    fn default() -> Self {
        Self {
            device_start_url: "https://authapi.ampcode.com/user_management/authorize/device".into(),
            token_url: "https://authapi.ampcode.com/user_management/authenticate".into(),
            verification_page: "https://ampcode.com/auth/cli/device".into(),
            balance_url: "https://ampcode.com/api/internal?userDisplayBalanceInfo".into(),
        }
    }
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

pub struct AmpClient {
    http: Client,
    config: AmpConfig,
}

impl Default for AmpClient {
    fn default() -> Self {
        Self::new(AmpConfig::default())
    }
}

impl AmpClient {
    pub fn new(config: AmpConfig) -> Self {
        let http = crate::http::client_builder().timeout(TIMEOUT).build().unwrap_or_default();
        Self { http, config }
    }

    /// JSON POST of `client_id`, `grant_type`, `refresh_token`. WorkOS rotates the refresh token on every
    /// renewal and reports no `expires_in`, so the expiry is the access token's JWT `exp`. Extras are kept.
    pub async fn refresh(&self, tokens: &OAuthTokens) -> Result<OAuthTokens, ProviderError> {
        let refresh_token = tokens.refresh_token.as_deref().ok_or(ProviderError::SignInExpired)?;
        let fields = [("client_id", CLIENT_ID), ("grant_type", "refresh_token"), ("refresh_token", refresh_token)];
        let response = oauth::post_json(&self.http, &self.config.token_url, &fields).await?;
        let granted = TokenResponse::from_renewal(&response)?;
        let mut renewed = tokens.clone();
        renewed.expires_at = jwt_expiry(&granted.access_token);
        renewed.access_token = granted.access_token;
        if granted.refresh_token.is_some() {
            renewed.refresh_token = granted.refresh_token;
        }
        Ok(renewed)
    }

    async fn send(&self, request: RequestBuilder) -> Result<Resp, ProviderError> {
        let response = request.send().await.map_err(|_| ProviderError::ConnectionFailed)?;
        let status = response.status().as_u16();
        let retry_after = response.headers().get(header::RETRY_AFTER).and_then(|v| v.to_str().ok()).map(String::from);
        let body = response.bytes().await.map_err(|_| ProviderError::ConnectionFailed)?.to_vec();
        Ok(Resp { status, body, retry_after })
    }

    fn balance_request(&self, access_token: &str) -> RequestBuilder {
        self.http
            .post(&self.config.balance_url)
            .bearer_auth(access_token)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .body(BALANCE_BODY)
    }

    async fn fetch_balance(&self, access_token: &str) -> Result<Resp, ProviderError> {
        self.send(self.balance_request(access_token)).await
    }

    /// Refreshes first when due (an early refresh that fails keeps the unexpired token), sends, and on a
    /// 401 with a refresh token renews once and retries once. `tokens` is updated in place as soon as a
    /// refresh succeeds, so the rotated refresh token survives a failing request after it.
    async fn authorized(&self, tokens: &mut OAuthTokens, now: DateTime<Utc>) -> Result<Resp, ProviderError> {
        if tokens.needs_refresh(now) {
            match self.refresh(tokens).await {
                Ok(renewed) => *tokens = renewed,
                Err(_) if tokens.expires_at.is_some_and(|at| at > now) => {}
                Err(e) => return Err(e),
            }
        }
        let mut response = self.fetch_balance(&tokens.access_token).await?;
        if response.status == 401 && tokens.refresh_token.is_some() {
            *tokens = self.refresh(tokens).await?;
            response = self.fetch_balance(&tokens.access_token).await?;
        }
        Ok(response)
    }

    /// The balance read mapped to a snapshot.
    ///
    /// `tokens` is updated in place (refresh, rotated refresh token), also when this returns `Err`.
    /// The caller must persist it whenever it differs from its copy, on Ok and on Err.
    pub async fn fetch(&self, tokens: &mut OAuthTokens, now: DateTime<Utc>) -> Result<ProviderSnapshot, ProviderError> {
        let response = self.authorized(tokens, now).await?;
        let usage = mapper::map(&response.require_success()?.body)?;
        Ok(ProviderSnapshot { plan: usage.plan, lines: usage.lines, refreshed_at: now, reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() })
    }
}
