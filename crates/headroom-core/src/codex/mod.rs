//! ChatGPT/Codex provider: browser sign-in, usage read, mapping and reset-credit claims.
//! Never log tokens or response bodies.

mod history;
#[cfg(test)]
mod history_tests;
mod login;
pub mod mapper;
pub mod resets;
#[cfg(test)]
mod tests;

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{header, Client, RequestBuilder};

use crate::claim::ResetItem;
use crate::claude::{ClaimFailure, ClaimOutcome};
use crate::hub::transport::{self, Transport};
use crate::models::{OAuthTokens, ProviderSnapshot};
use crate::oauth::{self, jwt_expiry, RawResponse, TokenResponse};
use crate::provider::ProviderError;
pub use mapper::{Headers, Usage};
pub use resets::{claim_outcome, credit_list_due};

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const SCOPES: &str = "openid profile email offline_access";
/// Extras key holding the `ChatGPT-Account-Id` header value.
pub const ACCOUNT_ID_KEY: &str = "chatgpt_account_id";
const USER_AGENT: &str = "Headroom";
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const CLAIM_TIMEOUT: Duration = Duration::from_secs(15);
const TOKEN_TIMEOUT: Duration = Duration::from_secs(15);

/// Endpoints and the loopback port, injectable for tests. `Default` is the real service.
#[derive(Debug, Clone)]
pub struct CodexConfig {
    pub authorize_url: String,
    pub token_url: String,
    /// Fixed because OpenAI registers the redirect; when it is taken, sign-in falls back to a device code.
    pub loopback_port: u16,
    pub device_usercode_url: String,
    pub device_token_url: String,
    /// Where the user types the device code.
    pub device_verify_url: String,
    pub device_redirect_uri: String,
    /// Origin of `/wham/usage` and `/wham/rate-limit-reset-credits[/consume]`.
    pub api_base: String,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            authorize_url: "https://auth.openai.com/oauth/authorize".into(),
            token_url: "https://auth.openai.com/oauth/token".into(),
            loopback_port: 1455,
            device_usercode_url: "https://auth.openai.com/api/accounts/deviceauth/usercode".into(),
            device_token_url: "https://auth.openai.com/api/accounts/deviceauth/token".into(),
            device_verify_url: "https://auth.openai.com/codex/device".into(),
            device_redirect_uri: "https://auth.openai.com/deviceauth/callback".into(),
            api_base: "https://chatgpt.com/backend-api".into(),
        }
    }
}

/// One usage read's result.
#[derive(Debug)]
pub struct CodexFetch {
    pub snapshot: ProviderSnapshot,
    /// Expiry of each "Rate Limit Resets" credit, for `claim_reset_credit`.
    pub reset_expiries: Vec<DateTime<Utc>>,
    /// The reset-credit list behind this snapshot: the fresh read, else the one passed in. The caller
    /// keeps it for the next pass and records when it was last read (`credit_list_due`).
    pub credit_list: Option<Vec<u8>>,
    /// The `plan_type` of the usage read, which picks the workspace-credits report for history.
    pub plan_type: Option<String>,
}

struct Resp {
    status: u16,
    body: Vec<u8>,
    retry_after: Option<String>,
    rate_limit_exhausted: bool,
    codex: Headers,
}

impl Resp {
    fn rate_limited(&self) -> bool {
        self.status == 429 || (self.status == 403 && (self.rate_limit_exhausted || self.retry_after.is_some()))
    }

    fn auth_failure(&self) -> bool {
        self.status == 401 || (self.status == 403 && !self.rate_limited())
    }

    /// 401/403 -> SignInExpired; 429 or a rate-limited 403 -> RateLimited; other failures -> RequestFailed.
    fn require_success(&self) -> Result<&Self, ProviderError> {
        if self.auth_failure() {
            return Err(ProviderError::SignInExpired);
        }
        if self.rate_limited() {
            let retry_after_secs = self.retry_after.as_ref().and_then(|v| v.trim().parse().ok());
            return Err(ProviderError::RateLimited { retry_after_secs });
        }
        match self.status {
            200..=299 => Ok(self),
            s => Err(ProviderError::RequestFailed(s)),
        }
    }
}

pub struct CodexClient {
    http: Client,
    config: CodexConfig,
    transport: Transport,
}

impl Default for CodexClient {
    fn default() -> Self {
        Self::new(CodexConfig::default())
    }
}

impl CodexClient {
    pub fn new(config: CodexConfig) -> Self {
        let http = crate::http::client_builder().timeout(TOKEN_TIMEOUT).build().unwrap_or_default();
        Self { http, config, transport: Transport::Direct }
    }

    /// Sends every usage-read request through `transport` (a hub account's login lives on its hub).
    pub fn with_transport(mut self, transport: Transport) -> Self {
        self.transport = transport;
        self
    }

    fn api(&self, path: &str) -> String {
        format!("{}{path}", self.config.api_base.trim_end_matches('/'))
    }

    /// Form POST of `grant_type`, `client_id`, `refresh_token`, no scope. The expiry is the access
    /// token's JWT `exp` (none when it has none). Extras are kept.
    pub async fn refresh(&self, tokens: &OAuthTokens) -> Result<OAuthTokens, ProviderError> {
        let refresh_token = tokens.refresh_token.as_deref().ok_or(ProviderError::SignInExpired)?;
        let fields = [("grant_type", "refresh_token"), ("client_id", CLIENT_ID), ("refresh_token", refresh_token)];
        let response: RawResponse = oauth::post_form(&self.http, &self.config.token_url, &fields, &[]).await?;
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
        let reply = transport::send(&self.transport, request).await?;
        let text = |name: &str| reply.header(name).map(String::from);
        let codex = Headers {
            primary_percent: text("x-codex-primary-used-percent"),
            secondary_percent: text("x-codex-secondary-used-percent"),
            has_credits: text("x-codex-credits-has-credits"),
            balance: text("x-codex-credits-balance"),
        };
        let retry_after = text(header::RETRY_AFTER.as_str());
        let rate_limit_exhausted = reply.header("x-ratelimit-remaining") == Some("0");
        Ok(Resp { status: reply.status, body: reply.body, retry_after, rate_limit_exhausted, codex })
    }

    /// `Authorization`, `Accept`, `User-Agent` and, when the account has one, `ChatGPT-Account-Id`.
    fn wham(&self, request: RequestBuilder, token: &str, account_id: Option<&str>) -> RequestBuilder {
        let request = request.bearer_auth(token).header(header::ACCEPT, "application/json").header(header::USER_AGENT, USER_AGENT);
        match account_id {
            Some(id) => request.header("ChatGPT-Account-Id", id),
            None => request,
        }
    }

    /// The reset-credit calls also send the Codex desktop client's headers, which the endpoint expects.
    fn reset_credit(&self, request: RequestBuilder, token: &str, account_id: Option<&str>) -> RequestBuilder {
        self.wham(request, token, account_id).header("OpenAI-Beta", "codex-1").header("originator", "Codex Desktop")
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
            match self.refresh(tokens).await {
                Ok(renewed) => *tokens = renewed,
                Err(_) if tokens.expires_at.is_some_and(|at| at > now) => {}
                Err(e) => return Err(e),
            }
        }
        let mut response = self.send(build(&tokens.access_token)).await?;
        if response.status == 401 && tokens.refresh_token.is_some() {
            *tokens = self.refresh(tokens).await?;
            response = self.send(build(&tokens.access_token)).await?;
        }
        Ok(response)
    }

    /// `GET /wham/usage`, plus the reset-credit list when `read_credit_list` is set (best-effort: a
    /// failure keeps `cached_credit_list`). Callers set it when `credit_list_due`.
    ///
    /// `tokens` is updated in place (refresh, rotated refresh token), also when this returns `Err`.
    /// The caller must persist it whenever it differs from its copy, on Ok and on Err.
    pub async fn fetch(
        &self,
        tokens: &mut OAuthTokens,
        cached_credit_list: Option<Vec<u8>>,
        read_credit_list: bool,
        now: DateTime<Utc>,
    ) -> Result<CodexFetch, ProviderError> {
        let account_id = tokens.extras.get(ACCOUNT_ID_KEY).filter(|id| !id.is_empty()).cloned();
        let url = self.api("/wham/usage");
        let response = self.authorized(tokens, now, |t| self.wham(self.http.get(&url), t, account_id.as_deref()).timeout(READ_TIMEOUT)).await?;
        response.require_success()?;
        let mut credit_list = cached_credit_list;
        if read_credit_list {
            let list_url = self.api("/wham/rate-limit-reset-credits");
            let request = self.reset_credit(self.http.get(list_url), &tokens.access_token, account_id.as_deref()).timeout(READ_TIMEOUT);
            if let Ok(list) = self.send(request).await {
                if (200..300).contains(&list.status) {
                    credit_list = Some(list.body);
                }
            }
        }
        let usage = mapper::map_usage(&response.body, &response.codex, credit_list.as_deref(), now)?;
        let plan_type = serde_json::from_slice::<serde_json::Value>(&response.body).ok().and_then(|v| Some(v.get("plan_type")?.as_str()?.to_string()));
        Ok(CodexFetch {
            snapshot: ProviderSnapshot { plan: usage.plan, lines: usage.lines, refreshed_at: now, reset_expiries: usage.reset_expiries.clone(), reset_items: ResetItem::codex(&usage.reset_expiries), notice: None, breakdowns: Default::default() },
            reset_expiries: usage.reset_expiries,
            credit_list,
            plan_type,
        })
    }

    /// Adds the Today, Yesterday and Last 30 Days rows after the usage lines. A direct account only: a hub
    /// account has no history. The workspace-credits report of a team or enterprise plan has its own ledger.
    /// A failed history read never fails the card.
    pub async fn add_history(&self, fetched: &mut CodexFetch, tokens: &OAuthTokens, now: DateTime<Utc>, counts: crate::history::Run<'_>, credits: &mut crate::history::Ledger) {
        let account_id = tokens.extras.get(ACCOUNT_ID_KEY).filter(|id| !id.is_empty());
        let output = history::history(self, &tokens.access_token, account_id.map(String::as_str), fetched.plan_type.as_deref(), now, counts, credits).await;
        let at = fetched.snapshot.lines.len();
        output.splice(&mut fetched.snapshot, at);
    }

    /// One of the analytics reports the Codex Analytics view reads, at `path` under `wham/`; `query` is
    /// already encoded.
    async fn report(&self, path: &str, query: &str, token: &str, account_id: Option<&str>) -> Result<Resp, ProviderError> {
        let url = self.api(&format!("/wham/{path}?{query}"));
        self.send(self.wham(self.http.get(url), token, account_id).timeout(READ_TIMEOUT)).await
    }

    /// Spends the reset credit expiring at `expiry`. Lists the credits first and matches by expiry, unless
    /// `matched_credit_id` already holds the credit for this `redeem_request_id`: keep it across retries so
    /// a replay after a lost answer skips matching and the server's `already_redeemed` proves the claim
    /// landed. `tokens` is updated in place on a refresh, also on `Err`; persist it whenever it changed.
    pub async fn claim_reset_credit(
        &self,
        tokens: &mut OAuthTokens,
        expiry: DateTime<Utc>,
        redeem_request_id: &str,
        matched_credit_id: &mut Option<String>,
        now: DateTime<Utc>,
    ) -> Result<ClaimOutcome, ProviderError> {
        let account_id = tokens.extras.get(ACCOUNT_ID_KEY).filter(|id| !id.is_empty()).cloned();
        let account_id = account_id.as_deref();
        if matched_credit_id.is_none() {
            let url = self.api("/wham/rate-limit-reset-credits");
            let list = self.authorized(tokens, now, |t| self.reset_credit(self.http.get(&url), t, account_id).timeout(READ_TIMEOUT)).await?;
            if list.auth_failure() {
                return Ok(ClaimOutcome::Failed(ClaimFailure::SignInExpired));
            }
            let body = (200..300).contains(&list.status).then(|| serde_json::from_slice(&list.body).ok()).flatten();
            let Some(body) = body else { return Ok(ClaimOutcome::Failed(ClaimFailure::Refused)) };
            let Some(id) = resets::credit_id(&body, expiry) else { return Ok(ClaimOutcome::NoCredit) };
            *matched_credit_id = Some(id);
        }
        let credit_id = matched_credit_id.as_deref().unwrap_or_default();
        let url = self.api("/wham/rate-limit-reset-credits/consume");
        let payload = serde_json::json!({"redeem_request_id": redeem_request_id, "credit_id": credit_id});
        let response = self
            .authorized(tokens, now, |t| self.reset_credit(self.http.post(&url), t, account_id).json(&payload).timeout(CLAIM_TIMEOUT))
            .await?;
        Ok(claim_outcome(response.status, response.auth_failure(), &response.body))
    }
}
