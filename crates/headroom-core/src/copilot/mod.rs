//! Copilot provider: device-code sign-in, organization billing, usage read and mapping. The sign-in token has no
//! refresh token and no expiry. Never log tokens or response bodies.

pub mod domain;
mod login;
pub mod mapper;
#[cfg(test)]
mod mapper_tests;
#[cfg(test)]
mod tests;

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{header, Client, RequestBuilder, Url};

use crate::models::{MetricLine, OAuthTokens, ProviderSnapshot};
use crate::provider::ProviderError;
pub use domain::GitHubDomain;

/// The client id of GitHub's Copilot sign-in (`Iv1.` + hex).
pub const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
/// The scope Copilot's own sign-in asks for, known to read `/copilot_internal/user`.
pub const SCOPES: &str = "read:user";
const USER_AGENT: &str = "Headroom";
const API_VERSION: &str = "2022-11-28";
const TIMEOUT: Duration = Duration::from_secs(15);

/// Base-URL overrides for tests. `Default` derives every URL from the domain.
#[derive(Debug, Clone, Default)]
pub struct CopilotConfig {
    /// Replaces `https://<host>` for the device sign-in.
    pub web_base: Option<String>,
    /// Replaces `https://api.<host>` for the user, usage and organization reads.
    pub api_base: Option<String>,
}

struct Resp {
    status: u16,
    body: Vec<u8>,
    retry_after: Option<String>,
    rate_limit_exhausted: bool,
}

impl Resp {
    /// GitHub sends its rate limits as 403 with `x-ratelimit-remaining: 0` or `retry-after`.
    fn rate_limited(&self) -> bool {
        self.status == 429 || (self.status == 403 && (self.rate_limit_exhausted || self.retry_after.is_some()))
    }

    fn auth_failure(&self) -> bool {
        self.status == 401 || (self.status == 403 && !self.rate_limited())
    }

    fn failed(&self) -> ProviderError {
        if self.rate_limited() {
            ProviderError::RateLimited { retry_after_secs: self.retry_after.as_ref().and_then(|v| v.trim().parse().ok()) }
        } else {
            ProviderError::RequestFailed(self.status)
        }
    }
}

pub struct CopilotClient {
    http: Client,
    domain: GitHubDomain,
    config: CopilotConfig,
}

impl CopilotClient {
    pub fn new(domain: GitHubDomain) -> Self {
        Self::with_config(domain, CopilotConfig::default())
    }

    pub fn with_config(domain: GitHubDomain, config: CopilotConfig) -> Self {
        let http = crate::http::client_builder().timeout(TIMEOUT).build().unwrap_or_default();
        Self { http, domain, config }
    }

    fn web(&self, path: &str) -> String {
        self.config.web_base.as_ref().map_or_else(|| self.domain.web_url(path), |b| format!("{}{path}", b.trim_end_matches('/')))
    }

    fn api(&self, path: &str) -> String {
        self.config.api_base.as_ref().map_or_else(|| self.domain.api_url(path), |b| format!("{}{path}", b.trim_end_matches('/')))
    }

    /// Organization billing always lives on api.github.com, whatever the account's domain.
    fn organization_url(&self, organization: &str) -> Result<Url, ProviderError> {
        let base = self.config.api_base.clone().unwrap_or_else(|| "https://api.github.com".into());
        let mut url = Url::parse(&base).map_err(|_| ProviderError::InvalidResponse)?;
        url.path_segments_mut()
            .map_err(|_| ProviderError::InvalidResponse)?
            .extend(["organizations", organization, "settings", "billing", "ai_credit", "usage"]);
        Ok(url)
    }

    async fn send(&self, request: RequestBuilder) -> Result<Resp, ProviderError> {
        let response = request.send().await.map_err(|_| ProviderError::ConnectionFailed)?;
        let status = response.status().as_u16();
        let headers = response.headers();
        let retry_after = headers.get(header::RETRY_AFTER).and_then(|v| v.to_str().ok()).map(String::from);
        let rate_limit_exhausted = headers.get("x-ratelimit-remaining").is_some_and(|v| v == "0");
        let body = response.bytes().await.map_err(|_| ProviderError::ConnectionFailed)?.to_vec();
        Ok(Resp { status, body, retry_after, rate_limit_exhausted })
    }

    /// The Copilot usage read. Mirrors the headers the official Copilot client sends; `Authorization` uses
    /// the `token` scheme (not `Bearer`), which is what `/copilot_internal/user` accepts.
    async fn fetch_usage(&self, token: &str) -> Result<Resp, ProviderError> {
        self.send(
            self.http
                .get(self.api("/copilot_internal/user"))
                .header(header::AUTHORIZATION, format!("token {token}"))
                .header(header::ACCEPT, "application/json")
                .header("Editor-Version", "vscode/1.96.2")
                .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
                .header(header::USER_AGENT, "GitHubCopilotChat/0.26.7")
                .header("X-Github-Api-Version", "2025-04-01"),
        )
        .await
    }

    /// The seat's usage on the account's domain. 401/403 -> SignInExpired. `tokens` is never changed.
    pub async fn fetch(&self, tokens: &mut OAuthTokens, now: DateTime<Utc>) -> Result<ProviderSnapshot, ProviderError> {
        let response = self.fetch_usage(&tokens.access_token).await?;
        if response.auth_failure() {
            return Err(ProviderError::SignInExpired);
        }
        if !(200..300).contains(&response.status) {
            return Err(response.failed());
        }
        let usage = mapper::map_usage(&response.body)?;
        Ok(ProviderSnapshot { plan: usage.plan, lines: usage.lines, refreshed_at: now, reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() })
    }

    /// One organization's Copilot credits and spend this month, read with a token from an owner or billing
    /// manager of the organization: the Copilot sign-in itself cannot read organization billing.
    pub async fn organization_usage(&self, organization: &str, token: &str) -> Result<Vec<MetricLine>, ProviderError> {
        let request = self
            .http
            .get(self.organization_url(organization)?)
            .bearer_auth(token)
            .header(header::ACCEPT, "application/vnd.github+json")
            .header(header::USER_AGENT, USER_AGENT)
            .header("X-GitHub-Api-Version", API_VERSION);
        let response = self.send(request).await?;
        if response.auth_failure() {
            return Err(ProviderError::KeyRejected("GitHub rejected the token.".into()));
        }
        if response.status == 404 {
            return Err(ProviderError::Unavailable(format!("GitHub has no organization “{organization}” this token can read.")));
        }
        if !(200..300).contains(&response.status) {
            return Err(response.failed());
        }
        mapper::org_usage_lines(&response.body).ok_or(ProviderError::InvalidResponse)
    }

    /// The organization account's snapshot.
    pub async fn fetch_organization(&self, organization: &str, token: &str, now: DateTime<Utc>) -> Result<ProviderSnapshot, ProviderError> {
        let lines = self.organization_usage(organization, token).await?;
        Ok(ProviderSnapshot { plan: None, lines, refreshed_at: now, reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() })
    }

    /// Checks a pasted token with one read of the organization's report. A rejected token is reported as
    /// unavailable (nothing to replace yet).
    pub async fn validate_organization(&self, organization: &str, token: &str) -> Result<(), ProviderError> {
        match self.organization_usage(organization, token).await {
            Err(ProviderError::KeyRejected(message)) => Err(ProviderError::Unavailable(message)),
            other => other.map(|_| ()),
        }
    }
}
