//! Antigravity provider: browser sign-in, quota read and mapping. Never log tokens or response bodies.

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
use crate::oauth::{self, RawResponse, TokenResponse};
use crate::provider::ProviderError;

/// Google "installed application" client the Antigravity app ships with. Public by design: Google does
/// not treat an installed-app secret as confidential, and the refresh grant requires it.
pub const CLIENT_ID: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
pub const CLIENT_SECRET: &str = "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";
pub const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email";
pub const UNAVAILABLE_MSG: &str = "Antigravity usage is temporarily unavailable. Try again shortly.";
const NO_PROJECT_MSG: &str = "Antigravity couldn't find a project with quota access for this account. Try again shortly.";
const NO_SUBSCRIPTION_MSG: &str = "Google reports no Antigravity quota subscription for this project.";
const LOAD_CODE_ASSIST_PATH: &str = "/v1internal:loadCodeAssist";
const QUOTA_SUMMARY_PATH: &str = "/v1internal:retrieveUserQuotaSummary";
const LOAD_CODE_ASSIST_PAYLOAD: &str = r#"{"metadata":{"ideType":"ANTIGRAVITY","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}}"#;
const TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_EXPIRY_SECS: f64 = 3600.0;

/// The hub client identity exposes the separate Argon quota; `os_type` is a literal there.
fn user_agent() -> String {
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "amd64" };
    format!("antigravity/hub/2.24.0 (aidev_client; os_type=darwin; arch={arch})")
}

/// Endpoints and the loopback port, injectable for tests. `Default` is the real service.
#[derive(Debug, Clone)]
pub struct AntigravityConfig {
    pub authorize_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    /// Fixed because Google registers the redirect; when it is taken, sign-in fails.
    pub loopback_port: u16,
    /// Plan and project lookups try this first.
    pub production_url: String,
    /// Has the active counters, so the quota summary tries it first.
    pub daily_url: String,
}

impl Default for AntigravityConfig {
    fn default() -> Self {
        Self {
            authorize_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            userinfo_url: "https://www.googleapis.com/oauth2/v1/userinfo?alt=json".into(),
            loopback_port: 51121,
            production_url: "https://cloudcode-pa.googleapis.com".into(),
            daily_url: "https://daily-cloudcode-pa.googleapis.com".into(),
        }
    }
}

/// One usage read's result.
#[derive(Debug)]
pub struct AntigravityFetch {
    pub snapshot: ProviderSnapshot,
    /// The `loadCodeAssist` body behind this snapshot: the fresh read, else the one passed in. The caller
    /// keeps it for the next pass (it holds the plan and the project).
    pub code_assist: Option<Vec<u8>>,
}

struct Resp {
    status: u16,
    body: Vec<u8>,
    retry_after: Option<String>,
    rate_limit_exhausted: bool,
}

impl Resp {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

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

    /// A 403 whose error details name `SUBSCRIPTION_REQUIRED` (free-tier account without a project).
    fn requires_subscription(&self) -> bool {
        let Ok(json) = serde_json::from_slice::<serde_json::Value>(&self.body) else { return false };
        self.status == 403
            && json["error"]["details"]
                .as_array()
                .is_some_and(|details| details.iter().any(|d| d["reason"].as_str() == Some("SUBSCRIPTION_REQUIRED")))
    }
}

/// The quota read answered 401: the access token was refused, so a renewed sign-in may read it.
enum QuotaFailure {
    TokenRefused,
    Failed(ProviderError),
}

impl From<ProviderError> for QuotaFailure {
    fn from(error: ProviderError) -> Self {
        Self::Failed(error)
    }
}

pub struct AntigravityClient {
    http: Client,
    config: AntigravityConfig,
    transport: Transport,
}

impl Default for AntigravityClient {
    fn default() -> Self {
        Self::new(AntigravityConfig::default())
    }
}

impl AntigravityClient {
    pub fn new(config: AntigravityConfig) -> Self {
        let http = crate::http::client_builder().timeout(TIMEOUT).build().unwrap_or_default();
        Self { http, config, transport: Transport::Direct }
    }

    /// Sends every usage-read request through `transport` (a hub account's login lives on its hub).
    pub fn with_transport(mut self, transport: Transport) -> Self {
        self.transport = transport;
        self
    }

    /// Form POST of client id/secret, `refresh_token` and `grant_type`. The expiry defaults to an hour
    /// and the old refresh token is kept (Google sends no new one). Extras are kept.
    pub async fn refresh(&self, tokens: &OAuthTokens, now: DateTime<Utc>) -> Result<OAuthTokens, ProviderError> {
        let refresh_token = tokens.refresh_token.as_deref().ok_or(ProviderError::SignInExpired)?;
        let fields = [
            ("client_id", CLIENT_ID),
            ("client_secret", CLIENT_SECRET),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ];
        let response: RawResponse = oauth::post_form(&self.http, &self.config.token_url, &fields, &[]).await?;
        let granted = TokenResponse::from_renewal(&response)?;
        let mut renewed = tokens.clone();
        renewed.expires_at = Some(now + chrono::Duration::milliseconds((granted.expires_in.unwrap_or(DEFAULT_EXPIRY_SECS) * 1000.0) as i64));
        renewed.access_token = granted.access_token;
        Ok(renewed)
    }

    async fn send(&self, request: RequestBuilder) -> Result<Resp, ProviderError> {
        let reply = transport::send(&self.transport, request).await?;
        let retry_after = reply.header("retry-after").map(String::from);
        let rate_limit_exhausted = reply.header("x-ratelimit-remaining") == Some("0");
        Ok(Resp { status: reply.status, body: reply.body, retry_after, rate_limit_exhausted })
    }

    fn cloud_code(&self, base: &str, path: &str, token: &str, payload: String) -> RequestBuilder {
        self.http
            .post(format!("{}{path}", base.trim_end_matches('/')))
            .header(header::ACCEPT, "application/json")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::USER_AGENT, user_agent())
            .body(payload)
            .timeout(TIMEOUT)
    }

    /// `loadCodeAssist` with Antigravity's client metadata (without it the plan can't be read). Production
    /// first, then daily: a 401/403 is an expired sign-in; other failures fall through to the next base and
    /// finally "unavailable".
    pub async fn load_code_assist(&self, token: &str) -> Result<Vec<u8>, ProviderError> {
        for base in [&self.config.production_url, &self.config.daily_url] {
            let request = self.cloud_code(base, LOAD_CODE_ASSIST_PATH, token, LOAD_CODE_ASSIST_PAYLOAD.into());
            let Ok(response) = self.send(request).await else { continue };
            if response.auth_failure() {
                return Err(ProviderError::SignInExpired);
            }
            if response.ok() {
                return Ok(response.body);
            }
        }
        Err(ProviderError::Unavailable(UNAVAILABLE_MSG.into()))
    }

    /// `retrieveUserQuotaSummary`, daily first. A complete daily answer stops there; otherwise production
    /// answers too, and the bodies are returned in that order for `parse_quota_summaries` to merge.
    async fn quota_summary(&self, token: &str, project: Option<&str>) -> Result<Vec<Vec<u8>>, QuotaFailure> {
        let payload = project.map_or_else(|| "{}".to_string(), |p| serde_json::json!({ "project": p }).to_string());
        let mut responses = Vec::new();
        let mut transport_error = None;
        for (index, base) in [&self.config.daily_url, &self.config.production_url].into_iter().enumerate() {
            let request = self.cloud_code(base, QUOTA_SUMMARY_PATH, token, payload.clone());
            match self.send(request).await {
                Ok(response) => responses.push(response),
                Err(error) => {
                    transport_error = Some(error);
                    continue;
                }
            }
            let first = &responses[responses.len() - 1];
            if index == 0 && first.ok() && mapper::is_complete_summary(&first.body) {
                break;
            }
        }
        if responses.is_empty() {
            return Err(transport_error.unwrap_or(ProviderError::Unavailable(UNAVAILABLE_MSG.into())).into());
        }
        if responses.iter().any(Resp::ok) {
            return Ok(responses.into_iter().filter(Resp::ok).map(|r| r.body).collect());
        }
        if responses.iter().any(|r| r.status == 401) {
            return Err(QuotaFailure::TokenRefused);
        }
        if responses.iter().any(Resp::requires_subscription) {
            let message = if project.is_none() { NO_PROJECT_MSG } else { NO_SUBSCRIPTION_MSG };
            return Err(ProviderError::Unavailable(message.into()).into());
        }
        if responses.iter().any(Resp::auth_failure) {
            return Err(ProviderError::SignInExpired.into());
        }
        if let Some(limited) = responses.iter().find(|r| r.status == 429) {
            return Err(limited.failed().into());
        }
        Err(ProviderError::Unavailable(UNAVAILABLE_MSG.into()).into())
    }

    /// One usage pass for `token`. The plan body is read when missing or `read_code_assist` is set
    /// (best-effort: a failure keeps `code_assist`).
    async fn usage(&self, token: &str, code_assist: &mut Option<Vec<u8>>, read_code_assist: bool) -> Result<Vec<crate::models::MetricLine>, QuotaFailure> {
        if read_code_assist || code_assist.is_none() {
            if let Ok(body) = self.load_code_assist(token).await {
                *code_assist = Some(body);
            }
        }
        let project = code_assist.as_deref().and_then(mapper::parse_project);
        let bodies = self.quota_summary(token, project.as_deref()).await?;
        mapper::parse_quota_summaries(&bodies).ok_or_else(|| ProviderError::InvalidResponse.into())
    }

    /// Refreshes first when due (an early refresh that fails keeps the unexpired token), reads, and on a
    /// refused token renews once and retries once; a second refusal is `SignInExpired`.
    ///
    /// `tokens` is updated in place as soon as a refresh succeeds, also when this returns `Err`. The
    /// caller must persist it whenever it differs from its copy, on Ok and on Err.
    pub async fn fetch(
        &self,
        tokens: &mut OAuthTokens,
        cached_code_assist: Option<Vec<u8>>,
        read_code_assist: bool,
        now: DateTime<Utc>,
    ) -> Result<AntigravityFetch, ProviderError> {
        if tokens.needs_refresh(now) {
            match self.refresh(tokens, now).await {
                Ok(renewed) => *tokens = renewed,
                Err(_) if tokens.expires_at.is_some_and(|at| at > now) => {}
                Err(e) => return Err(e),
            }
        }
        let mut code_assist = cached_code_assist;
        let lines = match self.usage(&tokens.access_token, &mut code_assist, read_code_assist).await {
            Ok(lines) => lines,
            Err(QuotaFailure::Failed(error)) => return Err(error),
            Err(QuotaFailure::TokenRefused) => {
                if tokens.refresh_token.is_none() {
                    return Err(ProviderError::SignInExpired);
                }
                *tokens = self.refresh(tokens, now).await?;
                match self.usage(&tokens.access_token, &mut code_assist, false).await {
                    Ok(lines) => lines,
                    Err(QuotaFailure::TokenRefused) => return Err(ProviderError::SignInExpired),
                    Err(QuotaFailure::Failed(error)) => return Err(error),
                }
            }
        };
        let plan = code_assist.as_deref().and_then(mapper::parse_plan);
        Ok(AntigravityFetch { snapshot: ProviderSnapshot { plan, lines, refreshed_at: now, reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() }, code_assist })
    }
}
