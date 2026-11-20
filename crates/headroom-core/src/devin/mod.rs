//! Devin provider: browser sign-in, usage read and mapping. The session token has no refresh token: a 401/403 is `SignInExpired`, with no retry.
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
use serde_json::json;

use crate::hub::transport::{self, Transport};
use crate::models::{OAuthTokens, ProviderSnapshot};
use crate::provider::ProviderError;

pub const SESSION_TOKEN_PREFIX: &str = "devin-session-token$";
/// Extras key holding the account's own API server (default: `DevinConfig::api_server_url`).
pub const API_SERVER_URL_KEY: &str = "api_server_url";
const CLOUD_COMPAT_VERSION: &str = "1.108.2";
const USER_STATUS_PATH: &str = "/exa.seat_management_pb.SeatManagementService/GetUserStatus";
const TIMEOUT: Duration = Duration::from_secs(15);

/// Endpoints, injectable for tests. `Default` is the real service.
#[derive(Debug, Clone)]
pub struct DevinConfig {
    pub authorize_url: String,
    pub token_url: String,
    /// Used when the account's tokens name no `api_server_url`.
    pub api_server_url: String,
}

impl Default for DevinConfig {
    fn default() -> Self {
        Self {
            authorize_url: "https://app.devin.ai/auth/cli/continue".into(),
            token_url: "https://api.devin.ai/auth/cli/token".into(),
            api_server_url: "https://server.codeium.com".into(),
        }
    }
}

pub struct DevinClient {
    http: Client,
    config: DevinConfig,
    transport: Transport,
}

impl Default for DevinClient {
    fn default() -> Self {
        Self::new(DevinConfig::default())
    }
}

impl DevinClient {
    pub fn new(config: DevinConfig) -> Self {
        let http = crate::http::client_builder().timeout(TIMEOUT).build().unwrap_or_default();
        Self { http, config, transport: Transport::Direct }
    }

    /// Sends every usage-read request through `transport` (a hub account's login lives on its hub).
    pub fn with_transport(mut self, transport: Transport) -> Self {
        self.transport = transport;
        self
    }

    /// `POST <api server>/exa.seat_management_pb.SeatManagementService/GetUserStatus`; the session token
    /// goes in the body as `metadata.apiKey`.
    fn user_status_request(&self, tokens: &OAuthTokens) -> RequestBuilder {
        let base = tokens.extras.get(API_SERVER_URL_KEY).filter(|u| !u.is_empty()).unwrap_or(&self.config.api_server_url);
        let body = json!({"metadata": {
            "apiKey": tokens.access_token,
            "ideName": "devin",
            "ideVersion": CLOUD_COMPAT_VERSION,
            "extensionName": "devin",
            "extensionVersion": CLOUD_COMPAT_VERSION,
            "locale": "en",
        }});
        self.http
            .post(format!("{}{USER_STATUS_PATH}", base.trim_end_matches('/')))
            .header(header::CONTENT_TYPE, "application/json")
            .header("Connect-Protocol-Version", "1")
            .body(body.to_string())
    }

    /// Returns the status and body of the user-status read.
    async fn fetch_user_status(&self, tokens: &OAuthTokens) -> Result<(u16, Vec<u8>), ProviderError> {
        let reply = transport::send(&self.transport, self.user_status_request(tokens)).await?;
        Ok((reply.status, reply.body))
    }

    /// One usage read. 401, and a 403 that is not a rate limit, are `SignInExpired`; 429 and a
    /// rate-limited 403 are `RateLimited`; other failures are `RequestFailed`. Tokens are never changed.
    pub async fn fetch(&self, tokens: &OAuthTokens, now: DateTime<Utc>) -> Result<ProviderSnapshot, ProviderError> {
        let reply = transport::send(&self.transport, self.user_status_request(tokens)).await?;
        let status = reply.status;
        let retry_after = reply.header("retry-after").map(String::from);
        let exhausted = reply.header("x-ratelimit-remaining") == Some("0");
        let body = reply.body;
        let rate_limited = status == 429 || (status == 403 && (exhausted || retry_after.is_some()));
        match status {
            _ if status == 401 || (status == 403 && !rate_limited) => Err(ProviderError::SignInExpired),
            200..=299 => mapper::map_usage(&body, now),
            _ if rate_limited => Err(ProviderError::RateLimited { retry_after_secs: retry_after.and_then(|v| v.trim().parse().ok()) }),
            s => Err(ProviderError::RequestFailed(s)),
        }
    }
}
