//! Cursor provider: browser sign-in, token renewal, usage read and usage history. Never log tokens or response bodies.

mod history;
#[cfg(test)]
mod history_tests;
mod login;
pub mod mapper;
pub mod summary;
#[cfg(test)]
mod mapper_tests;
#[cfg(test)]
mod tests;

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{header, Client, RequestBuilder};
use serde_json::Value;

use crate::models::{MetricLine, OAuthTokens, ProviderSnapshot};
use crate::oauth::{self, jwt_expiry, jwt_payload, TokenResponse};
use crate::provider::ProviderError;
use mapper::{Obj, Unified};

pub const CLIENT_ID: &str = "KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB";
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const TOKEN_TIMEOUT: Duration = Duration::from_secs(15);
/// The access token is a 60-day JWT that is also its refresh token, so the sign-in dies at its `exp`.
/// Refreshing a week early keeps the account alive when the machine is off around expiry.
pub const REFRESH_LEAD: chrono::Duration = chrono::Duration::days(7);

/// Endpoints and the poll timing, injectable for tests. `Default` is the real service.
#[derive(Debug, Clone)]
pub struct CursorConfig {
    pub login_url: String,
    pub poll_url: String,
    pub poll_interval: Duration,
    pub poll_expiry: Duration,
    pub token_url: String,
    /// Origin of the Connect-RPC `aiserver.v1.DashboardService/*` calls.
    pub api_base: String,
    /// Origin of the cookie-authenticated `/api/...` endpoints.
    pub web_base: String,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            login_url: "https://cursor.com/loginDeepControl".into(),
            poll_url: "https://api2.cursor.sh/auth/poll".into(),
            poll_interval: Duration::from_secs(2),
            poll_expiry: Duration::from_secs(300),
            token_url: "https://api2.cursor.sh/oauth/token".into(),
            api_base: "https://api2.cursor.sh".into(),
            web_base: "https://cursor.com".into(),
        }
    }
}

/// One usage read's result.
#[derive(Debug)]
pub struct CursorFetch {
    pub snapshot: ProviderSnapshot,
    /// Display titles replacing a line's default one (`"On-demand"` -> `"Team Spend"`).
    pub line_titles: Vec<(String, String)>,
}

/// The user id and `WorkosCursorSessionToken` cookie value the cursor.com endpoints need, from the
/// access token's `sub` (`auth0|user_x` -> `user_x`).
pub fn session(access_token: &str) -> Option<(String, String)> {
    let payload = jwt_payload(access_token)?;
    let subject = payload.get("sub")?.as_str()?.trim();
    let parts: Vec<&str> = subject.split('|').collect();
    let user_id = parts.get(1).unwrap_or(&parts[0]);
    (!user_id.is_empty()).then(|| (user_id.to_string(), format!("{user_id}%3A%3A{access_token}")))
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

    fn auth_failure(&self) -> bool {
        self.status == 401 || (self.status == 403 && !self.rate_limited())
    }

    fn require_success(&self) -> Result<(), ProviderError> {
        if self.auth_failure() {
            return Err(ProviderError::SignInExpired);
        }
        if self.rate_limited() {
            return Err(ProviderError::RateLimited { retry_after_secs: self.retry_after.as_ref().and_then(|v| v.trim().parse().ok()) });
        }
        match self.status {
            200..=299 => Ok(()),
            s => Err(ProviderError::RequestFailed(s)),
        }
    }

    fn object(&self) -> Option<Obj> {
        match serde_json::from_slice(&self.body) {
            Ok(Value::Object(map)) => Some(map),
            _ => None,
        }
    }
}

pub struct CursorClient {
    http: Client,
    config: CursorConfig,
}

impl Default for CursorClient {
    fn default() -> Self {
        Self::new(CursorConfig::default())
    }
}

impl CursorClient {
    pub fn new(config: CursorConfig) -> Self {
        let http = crate::http::client_builder().timeout(TOKEN_TIMEOUT).build().unwrap_or_default();
        Self { http, config }
    }

    /// JSON POST of `grant_type`, `client_id`, `refresh_token`. The answer carries only `access_token`,
    /// which is also the next refresh token; `shouldLogout` ends the session. The expiry is the new
    /// token's JWT `exp`. Extras are kept.
    pub async fn refresh(&self, tokens: &OAuthTokens) -> Result<OAuthTokens, ProviderError> {
        let refresh_token = tokens.refresh_token.as_deref().ok_or(ProviderError::SignInExpired)?;
        let fields = [("grant_type", "refresh_token"), ("client_id", CLIENT_ID), ("refresh_token", refresh_token)];
        let response = oauth::post_json(&self.http, &self.config.token_url, &fields).await?;
        let logout = serde_json::from_slice::<Value>(&response.body).ok().and_then(|j| j.get("shouldLogout")?.as_bool());
        if logout == Some(true) {
            return Err(ProviderError::SignInExpired);
        }
        let granted = TokenResponse::from_renewal(&response)?;
        let mut renewed = tokens.clone();
        renewed.expires_at = jwt_expiry(&granted.access_token);
        renewed.refresh_token = Some(granted.refresh_token.unwrap_or_else(|| granted.access_token.clone()));
        renewed.access_token = granted.access_token;
        Ok(renewed)
    }

    /// Gives a token without an expiry the one from its JWT, then refreshes when within the lead of it.
    /// An early refresh that fails keeps the token while it is still valid. `tokens` is updated in place.
    async fn ensure_fresh(&self, tokens: &mut OAuthTokens, now: DateTime<Utc>) -> Result<(), ProviderError> {
        if tokens.expires_at.is_none() {
            tokens.expires_at = jwt_expiry(&tokens.access_token);
        }
        let Some(expires_at) = tokens.expires_at else { return Ok(()) };
        if expires_at - REFRESH_LEAD <= now {
            match self.refresh(tokens).await {
                Ok(renewed) => *tokens = renewed,
                Err(_) if expires_at > now => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    async fn send(&self, request: RequestBuilder) -> Result<Resp, ProviderError> {
        let response = request.timeout(READ_TIMEOUT).send().await.map_err(|_| ProviderError::ConnectionFailed)?;
        let status = response.status().as_u16();
        let headers = response.headers();
        let retry_after = headers.get(header::RETRY_AFTER).and_then(|v| v.to_str().ok()).map(String::from);
        let rate_limit_exhausted = headers.get("x-ratelimit-remaining").is_some_and(|v| v == "0");
        let body = response.bytes().await.map_err(|_| ProviderError::ConnectionFailed)?.to_vec();
        Ok(Resp { status, body, retry_after, rate_limit_exhausted })
    }

    /// `POST {api_base}/aiserver.v1.DashboardService/<name>` with the Connect headers and an empty body.
    async fn connect(&self, name: &str, token: &str) -> Result<Resp, ProviderError> {
        let url = format!("{}/aiserver.v1.DashboardService/{name}", self.config.api_base.trim_end_matches('/'));
        let request = self
            .http
            .post(url)
            .bearer_auth(token)
            .header(header::CONTENT_TYPE, "application/json")
            .header("Connect-Protocol-Version", "1")
            .body("{}");
        self.send(request).await
    }

    /// `GET {web_base}{path}` with the session cookie; `None` when the token has no user id.
    async fn web_get(&self, path: &str, token: &str) -> Result<Option<Resp>, ProviderError> {
        let Some((_, cookie)) = session(token) else { return Ok(None) };
        let url = format!("{}{path}", self.config.web_base.trim_end_matches('/'));
        let request = self.http.get(url).header(header::COOKIE, format!("WorkosCursorSessionToken={cookie}"));
        self.send(request).await.map(Some)
    }

    /// An optional read is best-effort: a failure, a non-2xx answer or a non-object body is `None`.
    fn optional(response: Result<Option<Resp>, ProviderError>) -> Option<Obj> {
        response.ok().flatten().filter(|r| (200..300).contains(&r.status)).and_then(|r| r.object())
    }

    async fn optional_connect(&self, name: &str, token: &str) -> Option<Obj> {
        Self::optional(self.connect(name, token).await.map(Some))
    }

    async fn request_based_usage(&self, token: &str) -> Option<Obj> {
        let (user_id, _) = session(token)?;
        Self::optional(self.web_get(&format!("/api/usage?user={user_id}"), token).await)
    }

    /// Adds the Today, Yesterday and Last 30 Days rows after the usage lines. A failed history read never
    /// fails the card.
    pub async fn add_history(&self, fetched: &mut CursorFetch, token: &str, now: DateTime<Utc>, run: crate::history::Run<'_>) {
        let output = history::history(self, token, now, run).await;
        let at = fetched.snapshot.lines.len();
        output.splice(&mut fetched.snapshot, at);
    }

    /// The account's usage lines. `GetCurrentPeriodUsage` is required (a 401 renews the sign-in once and
    /// retries once); the plan, summary, credit and Grok Bot reads are best-effort. Plan and membership
    /// are read on every call; the hourly cache for them belongs to the caller.
    ///
    /// `tokens` is updated in place (refresh), also when this returns `Err`. The caller must persist it
    /// whenever it differs from its copy, on Ok and on Err.
    pub async fn fetch(&self, tokens: &mut OAuthTokens, now: DateTime<Utc>) -> Result<CursorFetch, ProviderError> {
        self.ensure_fresh(tokens, now).await?;
        let mut response = self.connect("GetCurrentPeriodUsage", &tokens.access_token).await?;
        if response.auth_failure() {
            *tokens = self.refresh(tokens).await?;
            response = self.connect("GetCurrentPeriodUsage", &tokens.access_token).await?;
        }
        response.require_success()?;
        let usage = response.object().ok_or(ProviderError::InvalidResponse)?;
        let (plan, lines, line_titles) = self.map(&usage, &tokens.access_token).await?;
        Ok(CursorFetch { snapshot: ProviderSnapshot { plan, lines, refreshed_at: now, reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() }, line_titles })
    }

    async fn map(&self, usage: &Obj, token: &str) -> Result<(Option<String>, Vec<MetricLine>, Vec<(String, String)>), ProviderError> {
        let plan = self.optional_connect("GetPlanInfo", token).await;
        let plan_name = plan
            .as_ref()
            .and_then(|p| mapper::obj(p, "planInfo")?.get("planName")?.as_str())
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string);
        let plan_name = plan_name.as_deref();
        let is_merged = plan.as_ref().and_then(|p| p.get("isMerged")).and_then(Value::as_bool) == Some(true);
        let summary_body = Self::optional(self.web_get("/api/usage-summary", token).await);
        let membership = mapper::membership_type(summary_body.as_ref());
        let is_org_seat = membership.as_deref().is_some_and(mapper::is_org_membership);
        let display_plan = mapper::resolved_plan_name(plan_name, membership.as_deref());
        let facts = mapper::PlanFacts::new(usage);
        let (fallback, message) = mapper::should_use_request_based_fallback(&facts, if is_org_seat { membership.as_deref() } else { plan_name });

        let generic = if !fallback && facts.should_try_generic_request_fallback() {
            let requests = self.request_based_usage(token).await;
            requests.and_then(|r| summary::map(None, Some(&r), display_plan.as_deref(), "").ok())
        } else {
            None
        };
        let mut mapped = if fallback {
            let requests = self.request_based_usage(token).await;
            summary::map(summary_body.as_ref(), requests.as_ref(), display_plan.as_deref(), message)?
        } else if let Some(mapped) = generic {
            mapped
        } else if is_merged {
            let unified = Unified {
                credit_balance: self.optional_connect("GetComplatCreditBalance", token).await,
                usage_status: Self::optional(self.web_get("/api/dashboard/v2/usage-status?scope=user", token).await),
            };
            let mut mapped = mapper::map_usage(usage, plan_name, None, 0.0, Some(&unified))?;
            mapped.plan = mapper::resolved_plan_label(plan_name, membership.as_deref());
            mapped
        } else {
            let stripe = Self::optional(self.web_get("/api/auth/stripe", token).await);
            let grants = self.optional_connect("GetCreditGrantsBalance", token).await;
            let mut mapped = mapper::map_usage(usage, plan_name, grants.as_ref(), mapper::stripe_balance_cents(stripe.as_ref()), None)?;
            mapped.plan = mapper::resolved_plan_label(plan_name, membership.as_deref());
            let needs_on_demand = mapped.lines.iter().all(|l| l.label() != "On-demand");
            let wants_summary = if is_org_seat { needs_on_demand } else { facts.is_team_by_shape() };
            if let Some(body) = summary_body.as_ref().filter(|b| wants_summary && summary::has_usable_summary_payload(b)) {
                summary::append_missing_on_demand(body, &mut mapped.lines);
                let membership = mapper::membership_type(Some(body)).or(membership);
                mapped.plan = mapper::resolved_plan_label(plan_name, membership.as_deref()).or(mapped.plan);
            }
            mapped
        };

        if !is_merged {
            if let Some(line) = self.optional_connect("GetSandUsageStatus", token).await.as_ref().and_then(mapper::map_grok_bot_usage) {
                mapped.lines.push(line);
            }
        }
        Ok((mapped.plan, mapped.lines, mapped.line_titles))
    }
}
