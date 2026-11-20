//! Claude provider: browser sign-in, usage read and authorized reads. Never log tokens or response bodies.

pub mod mapper;
pub mod resets;
#[cfg(test)]
mod tests;

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{header, Client, RequestBuilder, Url};
use serde_json::Value;

use crate::hub::transport::{self, Transport};
use crate::models::{OAuthTokens, ProviderSnapshot};
use crate::oauth::{self, error_code, loopback::LoopbackServer, pkce::{random_state, Pkce}, OAuthError, RawResponse, TokenResponse};
use crate::provider::ProviderError;
pub use mapper::Profile;
pub use resets::{claim_outcome, grants_due, ClaimFailure, ClaimOutcome, ResetGrantStatus};

pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const SCOPES: &str = "user:profile user:inference";
/// What a sign-in from before `user:inference` holds; its refresh falls back to this.
const PROFILE_ONLY_SCOPES: &str = "user:profile";
/// Tracks Claude Code: Anthropic's grants check rejects old versions (`ineligible_reason: cli_version`).
const USER_AGENT: &str = "claude-cli/2.1.294 (external, cli)";
const BETA: &str = "oauth-2025-04-20";
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const CLAIM_TIMEOUT: Duration = Duration::from_secs(25);
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);
const PROFILE_MSG: &str = "Claude didn't return your profile. Sign in again.";
const SCOPE_MSG: &str = "This sign-in can't read Claude usage. Remove the account and add it again.";

/// Endpoints, injectable for tests. `Default` is the real service.
#[derive(Debug, Clone)]
pub struct ClaudeConfig {
    pub authorize_url: String,
    pub token_url: String,
    /// Origin of `/api/oauth/{usage,profile}` and `/api/organizations/<org>/reset_rate_limits`.
    pub api_base: String,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            authorize_url: "https://claude.ai/oauth/authorize".into(),
            token_url: "https://platform.claude.com/v1/oauth/token".into(),
            api_base: "https://api.anthropic.com".into(),
        }
    }
}

/// One usage read's result. `grants` is set when the grants block was read and present.
#[derive(Debug)]
pub struct ClaudeFetch {
    pub snapshot: ProviderSnapshot,
    pub grants: Option<ResetGrantStatus>,
    /// From the profile read, for `claim_reset_grant`.
    pub organization_uuid: Option<String>,
}

struct Resp {
    status: u16,
    body: Vec<u8>,
    retry_after: Option<String>,
    rate_limit_exhausted: bool,
}

impl Resp {
    fn rate_limited(&self) -> bool {
        self.status == 429 || (self.status == 403 && (self.rate_limit_exhausted || self.retry_after.is_some()))
    }

    /// 429/rate-limited 403 -> RateLimited; scope refusal -> Unavailable; 401/403 -> SignInExpired.
    fn require_success(&self) -> Result<&Self, ProviderError> {
        if self.rate_limited() {
            let retry_after_secs = self.retry_after.as_ref().and_then(|v| v.trim().parse().ok());
            return Err(ProviderError::RateLimited { retry_after_secs });
        }
        if self.status == 403 && String::from_utf8_lossy(&self.body).contains("scope requirement") {
            return Err(ProviderError::Unavailable(SCOPE_MSG.into()));
        }
        match self.status {
            401 | 403 => Err(ProviderError::SignInExpired),
            200..=299 => Ok(self),
            s => Err(ProviderError::RequestFailed(s)),
        }
    }
}

pub struct ClaudeClient {
    http: Client,
    config: ClaudeConfig,
    transport: Transport,
}

impl Default for ClaudeClient {
    fn default() -> Self {
        Self::new(ClaudeConfig::default())
    }
}

impl ClaudeClient {
    pub fn new(config: ClaudeConfig) -> Self {
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

    /// Browser sign-in on a random loopback port. `open` receives the authorize URL; wrap the call in a
    /// timeout. Returns the account label and the tokens, with the identity stored in the extras.
    pub async fn login(&self, open: impl FnOnce(&Url)) -> Result<(String, OAuthTokens), OAuthError> {
        let pkce = Pkce::generate();
        let state = random_state();
        let server = LoopbackServer::bind(0, "/callback", state.clone()).await.map_err(OAuthError::ListenerFailed)?;
        let redirect = server.redirect_uri();
        let url = oauth::authorize_url(
            &self.config.authorize_url, CLIENT_ID, &redirect, SCOPES, &state, &pkce.challenge, &[("code", "true")],
        )?;
        open(&url);
        let code = server.wait().await?;
        let response = oauth::post_json(
            &self.http,
            &self.config.token_url,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", &code),
                ("state", &state),
                ("redirect_uri", &redirect),
                ("code_verifier", &pkce.verifier),
            ],
        )
        .await?;
        let token = TokenResponse::from_exchange(&response)?;
        let profile = self
            .profile(&token.access_token)
            .await
            .map_err(|_| OAuthError::Failed(PROFILE_MSG.into()))?;
        let extras = [(OAuthTokens::IDENTITY_KEY.to_string(), profile.identity())].into();
        Ok((profile.label(), token.into_tokens(Utc::now(), extras)))
    }

    async fn renew(&self, refresh_token: &str, scope: &str) -> Result<RawResponse, OAuthError> {
        let fields = [("grant_type", "refresh_token"), ("refresh_token", refresh_token), ("client_id", CLIENT_ID), ("scope", scope)];
        oauth::post_json(&self.http, &self.config.token_url, &fields).await
    }

    /// Refresh with the `invalid_scope` -> `user:profile` retry. Extras are kept.
    pub async fn refresh(&self, tokens: &OAuthTokens, now: DateTime<Utc>) -> Result<OAuthTokens, ProviderError> {
        let refresh_token = tokens.refresh_token.as_deref().ok_or(ProviderError::SignInExpired)?;
        let mut response: RawResponse = self.renew(refresh_token, SCOPES).await?;
        if response.status == 400 && error_code(&response.body).as_deref() == Some("invalid_scope") {
            response = self.renew(refresh_token, PROFILE_ONLY_SCOPES).await?;
        }
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
        let rate_limit_exhausted = reply.header("x-ratelimit-remaining") == Some("0");
        Ok(Resp { status: reply.status, body: reply.body, retry_after, rate_limit_exhausted })
    }

    /// The headers Claude Code sends; Anthropic decides grant eligibility by client surface.
    fn claude_code(&self, request: RequestBuilder, token: &str) -> RequestBuilder {
        request
            .bearer_auth(token.trim())
            .header(header::ACCEPT, "application/json")
            .header(header::CONTENT_TYPE, "application/json")
            .header("anthropic-beta", BETA)
            .header(header::USER_AGENT, USER_AGENT)
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

    /// `GET /api/oauth/profile`.
    pub async fn profile(&self, access_token: &str) -> Result<Profile, ProviderError> {
        let request = self
            .http
            .get(self.api("/api/oauth/profile"))
            .bearer_auth(access_token.trim())
            .header(header::ACCEPT, "application/json")
            .header("anthropic-beta", BETA)
            .timeout(READ_TIMEOUT);
        Profile::parse(&self.send(request).await?.require_success()?.body)
    }

    /// `GET /api/oauth/usage?cedar_ember=1&skip_spend=1`, the request Claude Code makes to find a reset to offer.
    async fn read_grants(&self, token: &str) -> Result<Option<ResetGrantStatus>, ProviderError> {
        let request = self.claude_code(self.http.get(self.api("/api/oauth/usage?cedar_ember=1&skip_spend=1")), token).timeout(READ_TIMEOUT);
        let body: Value = serde_json::from_slice(&self.send(request).await?.require_success()?.body).map_err(|_| ProviderError::InvalidResponse)?;
        Ok(ResetGrantStatus::parse(body.get("cedar_ember")))
    }

    /// Usage lines, plus, when `secondary` is set, the plan (profile) and the reset grants. Both are
    /// best-effort: a failure only costs that part. Callers set `secondary` on the hourly cadence
    /// (see `grants_due`). A 429 on the usage read is `RateLimited`.
    ///
    /// `tokens` is updated in place (refresh, rotated refresh token, identity extra), also when this
    /// returns `Err`. The caller must persist it whenever it differs from its copy, on Ok and on Err.
    pub async fn fetch(&self, tokens: &mut OAuthTokens, secondary: bool, now: DateTime<Utc>) -> Result<ClaudeFetch, ProviderError> {
        let current = tokens;
        let response = self
            .authorized(current, now, |t| self.claude_code(self.http.get(self.api("/api/oauth/usage")), t).timeout(READ_TIMEOUT))
            .await?;
        let mut lines = mapper::map_usage(&response.require_success()?.body)?;
        let (mut plan, mut grants, mut organization_uuid) = (None, None, None);
        if secondary {
            if let Ok(Some(status)) = self.read_grants(&current.access_token).await {
                lines.push(status.line(now));
                grants = Some(status);
            }
            if let Ok(profile) = self.profile(&current.access_token).await {
                plan = profile.plan();
                organization_uuid = profile.organization.as_ref().map(|o| o.uuid.clone()).filter(|u| uuid::Uuid::parse_str(u).is_ok());
                if !current.extras.contains_key(OAuthTokens::IDENTITY_KEY) {
                    current.extras.insert(OAuthTokens::IDENTITY_KEY.into(), profile.identity());
                }
            }
        }
        let reset_expiries = grants.as_ref().map(|g| g.expiries(now)).unwrap_or_default();
        let reset_items = grants.as_ref().map(|g| g.items(now)).unwrap_or_default();
        Ok(ClaudeFetch {
            snapshot: ProviderSnapshot { plan, lines, refreshed_at: now, reset_expiries, reset_items, notice: None, breakdowns: Default::default() },
            grants,
            organization_uuid,
        })
    }

    /// Spends a reset from `grant_id`. Resend an unsettled claim under its original `request_id` so
    /// Anthropic answers `already_used` instead of spending another. A malformed id is refused without a
    /// request. `tokens` is updated in place on a refresh, also on `Err`; persist it whenever it changed.
    pub async fn claim_reset_grant(
        &self,
        tokens: &mut OAuthTokens,
        organization_uuid: &str,
        grant_id: &str,
        request_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ClaimOutcome, ProviderError> {
        let ok = |s: &str, max: usize, upper: bool| {
            (1..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-' || (upper && b.is_ascii_uppercase()))
        };
        if !ok(grant_id, 40, false) || !ok(request_id, 64, true) || uuid::Uuid::parse_str(organization_uuid).is_err() {
            return Ok(ClaimOutcome::Failed(ClaimFailure::Refused));
        }
        let url = self.api(&format!("/api/organizations/{organization_uuid}/reset_rate_limits"));
        let body = serde_json::json!({"program": "cedar_ember", "grant_id": grant_id, "request_id": request_id});
        let response = self
            .authorized(tokens, now, |t| self.claude_code(self.http.post(&url), t).json(&body).timeout(CLAIM_TIMEOUT))
            .await?;
        // A renewable sign-in was already renewed and retried, so a 401 that survives is a scope refusal.
        Ok(if response.status == 401 && tokens.refresh_token.is_some() {
            ClaimOutcome::Failed(ClaimFailure::NotPermitted)
        } else {
            claim_outcome(response.status, response.rate_limited(), &response.body)
        })
    }
}
