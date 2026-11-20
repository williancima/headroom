//! `OpenCodeLogin`: OpenCode Console's device sign-in under the OpenCode CLI's own public client. The token
//! is scoped to the workspace picked on the confirm page; each workspace is its own account.

use chrono::Utc;
use reqwest::{header, Url};
use serde::Deserialize;
use std::collections::HashMap;

use super::{OpenCodeClient, CLIENT_ID, ORG_KEY};
use crate::models::OAuthTokens;
use crate::oauth::device::{device_authorization, DeviceAuthorization};
use crate::oauth::OAuthError;

#[derive(Deserialize)]
struct User {
    id: String,
    email: String,
}

impl OpenCodeClient {
    /// Device sign-in. `show_device_code` receives the user code and the page to type it on. Wrap the call
    /// in a timeout. Returns the member's email and the tokens, with the identity and workspace in the extras.
    pub async fn login(&self, show_device_code: impl FnOnce(&str, &Url)) -> Result<(String, OAuthTokens), OAuthError> {
        let params = DeviceAuthorization {
            provider: "OpenCode",
            start_url: &self.config.device_start_url,
            token_url: &self.config.token_url,
            client_id: CLIENT_ID,
            scope: None,
            start_fields: &[("supports_org_scope", "true")],
            headers: &[],
            verification_page: None,
            denied_message: "OpenCode device sign-in was denied.",
        };
        let token = device_authorization(&self.http, &params, show_device_code).await?;
        let org_id = token.org_id.clone().ok_or(OAuthError::InvalidResponse)?;
        let response = self
            .http
            .get(&self.config.user_url)
            .bearer_auth(&token.access_token)
            .header(header::ACCEPT, "application/json")
            .header("x-org-id", &org_id)
            .send()
            .await
            .map_err(|_| OAuthError::Network)?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(|_| OAuthError::Network)?;
        if !(200..300).contains(&status) {
            return Err(OAuthError::RequestFailed(status));
        }
        let user: User = serde_json::from_slice(&body).map_err(|_| OAuthError::InvalidResponse)?;
        let extras = HashMap::from([
            (OAuthTokens::IDENTITY_KEY.to_string(), format!("opencode:{}:{org_id}", user.id)),
            (ORG_KEY.to_string(), org_id),
        ]);
        Ok((user.email, token.into_tokens(Utc::now(), extras)))
    }
}
