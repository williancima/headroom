//! OpenCode Go provider: device-code sign-in, usage read and mapping
//! (sign-in only: Headroom has no OpenCode API-key accounts).
//! Never log tokens or response bodies.

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
use crate::oauth::{self, RawResponse, TokenResponse};
use crate::provider::ProviderError;

pub const CLIENT_ID: &str = "opencode-cli";
/// Extras key holding the workspace the sign-in chose; every read names it.
pub const ORG_KEY: &str = "org_id";
const TIMEOUT: Duration = Duration::from_secs(15);

/// Endpoints, injectable for tests. `Default` is the real service.
#[derive(Debug, Clone)]
pub struct OpenCodeConfig {
    pub device_start_url: String,
    pub token_url: String,
    pub user_url: String,
    pub usage_url: String,
    /// The Go subscription as OpenCode Console's own Go page reads it; it names the plan as `product`.
    pub go_status_url: String,
}

impl Default for OpenCodeConfig {
    fn default() -> Self {
        Self {
            device_start_url: "https://opencode.ai/console/auth/device/code".into(),
            token_url: "https://opencode.ai/console/auth/device/token".into(),
            user_url: "https://opencode.ai/console/api/user".into(),
            usage_url: "https://opencode.ai/zen/go/v1/usage".into(),
            go_status_url: "https://opencode.ai/console/api/go/status".into(),
        }
    }
}

#[derive(Debug)]
pub struct OpenCodeFetch {
    pub snapshot: ProviderSnapshot,
    /// The plan behind this snapshot: the fresh read when `read_plan` was set (`None` when it failed),
    /// else the one passed in. The caller keeps it and records when it was last read.
    pub plan: Option<String>,
}

struct Resp {
    status: u16,
    body: Vec<u8>,
    retry_after: Option<String>,
    /// The `Date` header, the server's clock for the placeholder-reset comparison.
    date: Option<DateTime<Utc>>,
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

pub struct OpenCodeClient {
    http: Client,
    config: OpenCodeConfig,
}

impl Default for OpenCodeClient {
    fn default() -> Self {
        Self::new(OpenCodeConfig::default())
    }
}

impl OpenCodeClient {
    pub fn new(config: OpenCodeConfig) -> Self {
        let http = crate::http::client_builder().timeout(TIMEOUT).build().unwrap_or_default();
        Self { http, config }
    }

    /// Form POST of `client_id`, `grant_type`, `refresh_token`. Extras are kept.
    pub async fn refresh(&self, tokens: &OAuthTokens, now: DateTime<Utc>) -> Result<OAuthTokens, ProviderError> {
        let refresh_token = tokens.refresh_token.as_deref().ok_or(ProviderError::SignInExpired)?;
        let fields = [("client_id", CLIENT_ID), ("grant_type", "refresh_token"), ("refresh_token", refresh_token)];
        let response: RawResponse = oauth::post_form(&self.http, &self.config.token_url, &fields, &[]).await?;
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
        let response = request.send().await.map_err(|_| ProviderError::ConnectionFailed)?;
        let status = response.status().as_u16();
        let headers = response.headers();
        let retry_after = headers.get(header::RETRY_AFTER).and_then(|v| v.to_str().ok()).map(String::from);
        let date = headers
            .get(header::DATE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| DateTime::parse_from_rfc2822(v).ok())
            .map(Into::into);
        let body = response.bytes().await.map_err(|_| ProviderError::ConnectionFailed)?.to_vec();
        Ok(Resp { status, body, retry_after, date })
    }

    fn get(&self, url: &str, token: &str, org_header: &str, org: Option<&str>) -> RequestBuilder {
        let request = self.http.get(url).bearer_auth(token).header(header::ACCEPT, "application/json");
        match org {
            Some(org) => request.header(org_header, org),
            None => request,
        }
    }

    /// Refreshes first when due (an early refresh that fails keeps the unexpired token), sends, and on a
    /// 401 with a refresh token renews once and retries once. `tokens` is updated in place as soon as a
    /// refresh succeeds, so the rotated refresh token survives a failing request after it.
    async fn authorized(
        &self,
        tokens: &mut OAuthTokens,
        now: DateTime<Utc>,
        build: impl Fn(&str) -> RequestBuilder,
    ) -> Result<Resp, ProviderError> {
        if tokens.needs_refresh(now) {
            match self.refresh(tokens, now).await {
                Ok(renewed) => *tokens = renewed,
                Err(_) if tokens.expires_at.is_some_and(|at| at > now) => {}
                Err(e) => return Err(e),
            }
        }
        let mut response = self.send(build(&tokens.access_token)).await?;
        if response.status == 401 && tokens.refresh_token.is_some() {
            *tokens = self.refresh(tokens, now).await?;
            response = self.send(build(&tokens.access_token)).await?;
        }
        Ok(response)
    }

    /// The Go usage read, plus the plan from the Go status when `read_plan` is set. A failed plan read
    /// keeps the usage and records no plan (`plan: None`); the caller retries at its next due time.
    ///
    /// `tokens` is updated in place (refresh, rotated refresh token), also when this returns `Err`.
    /// The caller must persist it whenever it differs from its copy, on Ok and on Err.
    pub async fn fetch(
        &self,
        tokens: &mut OAuthTokens,
        cached_plan: Option<String>,
        read_plan: bool,
        now: DateTime<Utc>,
    ) -> Result<OpenCodeFetch, ProviderError> {
        let org = tokens.extras.get(ORG_KEY).filter(|o| !o.is_empty()).cloned();
        let response = self.authorized(tokens, now, |t| self.get(&self.config.usage_url, t, "x-opencode-org-id", org.as_deref())).await?;
        if response.status == 403 && mapper::error_type(&response.body).as_deref() == Some("EntitlementError") {
            return Err(ProviderError::Unavailable("No OpenCode Go subscription on this account.".into()));
        }
        response.require_success()?;
        let lines = mapper::meter_lines(&response.body, response.date.unwrap_or(now))?;
        let plan = if read_plan { self.read_plan(&tokens.access_token, org.as_deref()).await } else { cached_plan };
        let snapshot = ProviderSnapshot { plan: plan.clone(), lines, refreshed_at: now, reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() };
        Ok(OpenCodeFetch { snapshot, plan })
    }

    async fn read_plan(&self, access_token: &str, org: Option<&str>) -> Option<String> {
        let result = async {
            let status = self.send(self.get(&self.config.go_status_url, access_token, "x-org-id", org)).await?;
            mapper::plan(&status.require_success()?.body)
        }
        .await;
        if let Err(error) = &result {
            eprintln!("headroom: OpenCode plan lookup failed; retrying in an hour: {error}");
        }
        result.ok()
    }
}
