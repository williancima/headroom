//! Muse Code provider: device-code sign-in, usage read and mapping. The `dca:` token has no refresh token: a 401
//! is `SignInExpired`, with no retry. Never log tokens or response bodies.

mod login;
pub mod mapper;
#[cfg(test)]
mod mapper_tests;
#[cfg(test)]
mod tests;

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{header, Client};

use crate::models::{MetricLine, OAuthTokens, ProviderSnapshot};
use crate::provider::ProviderError;
use mapper::MuseAccount;

pub const CLIENT_ID: &str = "1031625952748946";
const TIMEOUT: Duration = Duration::from_secs(15);

/// Endpoints, injectable for tests. `Default` is the real service.
#[derive(Debug, Clone)]
pub struct MuseConfig {
    pub device_start_url: String,
    pub device_token_url: String,
    pub key_url: String,
}

impl Default for MuseConfig {
    fn default() -> Self {
        Self {
            // The trailing slashes are part of Meta's endpoints.
            device_start_url: "https://auth.meta.com/oidc/device/authorization/".into(),
            device_token_url: "https://auth.meta.com/oidc/device/token/".into(),
            key_url: "https://api.meta.ai/muse-code/key".into(),
        }
    }
}

pub struct MuseClient {
    http: Client,
    config: MuseConfig,
}

impl Default for MuseClient {
    fn default() -> Self {
        Self::new(MuseConfig::default())
    }
}

impl MuseClient {
    pub fn new(config: MuseConfig) -> Self {
        let http = crate::http::client_builder().timeout(TIMEOUT).build().unwrap_or_default();
        Self { http, config }
    }

    /// The request the Muse CLI makes at startup. 401/403 -> SignInExpired; 429 -> RateLimited.
    async fn account(&self, token: &str) -> Result<MuseAccount, ProviderError> {
        let response = self
            .http
            .post(&self.config.key_url)
            .bearer_auth(token)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .header("x-api-version", "1.0.0")
            .body(r#"{"onboard":false}"#)
            .send()
            .await
            .map_err(|_| ProviderError::ConnectionFailed)?;
        let status = response.status().as_u16();
        let retry_after = response.headers().get(header::RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
        let body = response.bytes().await.map_err(|_| ProviderError::ConnectionFailed)?;
        match status {
            401 | 403 => Err(ProviderError::SignInExpired),
            429 => Err(ProviderError::RateLimited { retry_after_secs: retry_after }),
            200..=299 => mapper::parse_account(&body),
            s => Err(ProviderError::RequestFailed(s)),
        }
    }

    /// One key read. `previous` is the account's last snapshot: it supplies the last known windows when
    /// Muse leaves usage out. `tokens` is never changed (nothing refreshes); it is `&mut` only to match
    /// the other providers.
    pub async fn fetch(&self, tokens: &mut OAuthTokens, previous: Option<&ProviderSnapshot>, now: DateTime<Utc>) -> Result<ProviderSnapshot, ProviderError> {
        let account = self.account(&tokens.access_token).await?;
        let last_known: &[MetricLine] = previous.map_or(&[], |s| &s.lines);
        let usage = mapper::map(&account, last_known, now)?;
        Ok(ProviderSnapshot { plan: usage.plan, lines: usage.lines, refreshed_at: now, reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() })
    }
}
