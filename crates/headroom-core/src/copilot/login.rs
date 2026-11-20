//! `CopilotLogin`: GitHub's device sign-in under Copilot's own client, on github.com or a GHE.com domain.

use reqwest::{header, Url};
use serde::Deserialize;
use std::collections::HashMap;

use super::{CopilotClient, GitHubDomain, API_VERSION, CLIENT_ID, SCOPES, USER_AGENT};
use crate::models::OAuthTokens;
use crate::oauth::device::{device_authorization, DeviceAuthorization};
use crate::oauth::OAuthError;
use crate::provider::ProviderError;

#[derive(Deserialize)]
struct User {
    id: Option<u64>,
    login: String,
}

impl CopilotClient {
    /// Device sign-in. `show_device_code` receives the user code and the page to type it on. Wrap the call
    /// in a timeout. The label is the GitHub login. On GHE.com a seat check (one usage read) comes first, and
    /// a connection error says the domain could not be reached. The tokens have no refresh token or expiry.
    pub async fn login(&self, show_device_code: impl FnOnce(&str, &Url)) -> Result<(String, OAuthTokens), OAuthError> {
        match self.sign_in(show_device_code).await {
            Err(OAuthError::Network) if matches!(self.domain, GitHubDomain::Enterprise(_)) => {
                let host = self.domain.host();
                Err(OAuthError::Failed(format!("Couldn’t reach {host}. Check the address and your connection.")))
            }
            other => other,
        }
    }

    async fn sign_in(&self, show_device_code: impl FnOnce(&str, &Url)) -> Result<(String, OAuthTokens), OAuthError> {
        let (start_url, token_url) = (self.web("/login/device/code"), self.web("/login/oauth/access_token"));
        let params = DeviceAuthorization {
            provider: "GitHub",
            start_url: &start_url,
            token_url: &token_url,
            client_id: CLIENT_ID,
            scope: Some(SCOPES),
            start_fields: &[],
            headers: &[("User-Agent", USER_AGENT)],
            verification_page: None,
            denied_message: "GitHub device sign-in was denied.",
        };
        let token = device_authorization(&self.http, &params, show_device_code).await?;
        let response = self
            .http
            .get(self.api("/user"))
            .bearer_auth(&token.access_token)
            .header(header::ACCEPT, "application/vnd.github+json")
            .header(header::USER_AGENT, USER_AGENT)
            .header("X-GitHub-Api-Version", API_VERSION)
            .send()
            .await
            .map_err(|_| OAuthError::Network)?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(|_| OAuthError::Network)?;
        if !(200..300).contains(&status) {
            return Err(OAuthError::RequestFailed(status));
        }
        let user: User = serde_json::from_slice(&body).map_err(|_| OAuthError::InvalidResponse)?;
        let user_id = user.id.map_or_else(|| user.login.clone(), |id| id.to_string());
        let mut identity = format!("github:{user_id}");
        if let GitHubDomain::Enterprise(host) = &self.domain {
            self.require_seat(&token.access_token, &user.login).await?;
            // The same user id on two domains is two accounts.
            identity = format!("github:{host}:{user_id}");
        }
        let extras = HashMap::from([(OAuthTokens::IDENTITY_KEY.to_string(), identity)]);
        Ok((user.login, OAuthTokens { access_token: token.access_token, refresh_token: None, expires_at: None, extras }))
    }

    /// An enterprise sign-in reads usage once, so an account without a Copilot seat is never added.
    async fn require_seat(&self, token: &str, login: &str) -> Result<(), OAuthError> {
        let usage = self.fetch_usage(token).await.map_err(|e| match e {
            ProviderError::ConnectionFailed => OAuthError::Network,
            other => OAuthError::Failed(other.to_string()),
        })?;
        if (200..300).contains(&usage.status) {
            return Ok(());
        }
        if usage.auth_failure() || usage.status == 404 {
            let host = self.domain.host();
            return Err(OAuthError::Failed(format!("{login} has no Copilot seat on {host}. Ask your GitHub admin for one, then try again.")));
        }
        Err(OAuthError::RequestFailed(usage.status))
    }
}
