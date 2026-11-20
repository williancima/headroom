//! Ollama provider: API-key usage read, mapping and history. Never log keys or response bodies.

mod history;
pub mod mapper;
#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod tests;

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{header, Client, Method};

use crate::history::Run;
use crate::models::ProviderSnapshot;
use crate::provider::{KeyResponse, ProviderError};

pub const BASE_URL: &str = "https://ollama.com";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub struct OllamaFetch {
    pub snapshot: ProviderSnapshot,
    /// The account body behind this snapshot (it names the plan and the email): the fresh read when
    /// `read_account` was set (`None` when it failed), else the one passed in. The caller keeps it and
    /// records when it was last read.
    pub account_body: Option<String>,
}

pub struct OllamaClient {
    http: Client,
    base_url: String,
}

impl Default for OllamaClient {
    fn default() -> Self {
        Self::new(BASE_URL)
    }
}

impl OllamaClient {
    pub fn new(base_url: &str) -> Self {
        let http = crate::http::client_builder().timeout(TIMEOUT).build().unwrap_or_default();
        Self { http, base_url: base_url.trim_end_matches('/').to_string() }
    }

    async fn send(&self, method: Method, path: &str, api_key: &str) -> Result<KeyResponse, ProviderError> {
        let request = self.http.request(method, format!("{}{path}", self.base_url)).bearer_auth(api_key).header(header::ACCEPT, "application/json");
        KeyResponse::read(request.send().await.map_err(|_| ProviderError::ConnectionFailed)?).await
    }

    /// `GET /api/usage`: `range` is 7d or 30d, whole UTC days back from today plus today so far.
    async fn usage(&self, api_key: &str, range: &str) -> Result<KeyResponse, ProviderError> {
        self.send(Method::GET, &format!("/api/usage?range={range}&group_by=model"), api_key).await
    }

    /// The balance read, plus the account body (plan and email) when `read_account` is set. A failed account
    /// read keeps the usage and records no body; the caller retries at its next due time.
    pub async fn fetch(
        &self,
        api_key: &str,
        cached_account: Option<String>,
        read_account: bool,
        now: DateTime<Utc>,
    ) -> Result<OllamaFetch, ProviderError> {
        let balance = self.send(Method::GET, "/api/balance", api_key).await?;
        let lines = mapper::balance_lines(&balance.require_success("Ollama")?.body)?;
        let account_body = if read_account {
            self.send(Method::POST, "/api/me", api_key).await.ok().filter(KeyResponse::is_success).map(|r| String::from_utf8_lossy(&r.body).into_owned())
        } else {
            cached_account
        };
        let plan = account_body.as_deref().and_then(|b| mapper::plan(b.as_bytes()));
        let snapshot = ProviderSnapshot { plan, lines, refreshed_at: now, reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() };
        Ok(OllamaFetch { snapshot, account_body })
    }

    /// `fetch`, then the history rows after the balance lines. A failed history read never fails the card.
    pub async fn fetch_with_history(
        &self,
        api_key: &str,
        cached_account: Option<String>,
        read_account: bool,
        now: DateTime<Utc>,
        run: Run<'_>,
    ) -> Result<OllamaFetch, ProviderError> {
        let mut fetched = self.fetch(api_key, cached_account, read_account, now).await?;
        let at = fetched.snapshot.lines.len();
        history::history(self, api_key, now, run).await.splice(&mut fetched.snapshot, at);
        Ok(fetched)
    }
}
