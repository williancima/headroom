//! `GrokLogin`: xAI's device sign-in under the Grok CLI's own public client.

use chrono::Utc;
use reqwest::Url;
use std::collections::HashMap;

use super::{GrokClient, CLIENT_ID, SCOPES};
use crate::models::OAuthTokens;
use crate::oauth::device::{device_authorization, DeviceAuthorization};
use crate::oauth::{jwt_payload, OAuthError};

impl GrokClient {
    /// Device sign-in. `show_device_code` receives the user code and the page to type it on. Wrap the call
    /// in a timeout. The label is the access token's `email` claim (else "Grok Account"); the identity is
    /// its `sub`, else its `email`.
    pub async fn login(&self, show_device_code: impl FnOnce(&str, &Url)) -> Result<(String, OAuthTokens), OAuthError> {
        let params = DeviceAuthorization {
            provider: "xAI",
            start_url: &self.config.device_start_url,
            token_url: &self.config.token_url,
            client_id: CLIENT_ID,
            scope: Some(SCOPES),
            start_fields: &[],
            headers: &[],
            verification_page: None,
            denied_message: "xAI device sign-in failed.",
        };
        let token = device_authorization(&self.http, &params, show_device_code).await?;
        let claims = jwt_payload(&token.access_token);
        let claim = |name: &str| claims.as_ref().and_then(|c| c.get(name)?.as_str().map(String::from));
        let email = claim("email");
        let mut extras = HashMap::new();
        if let Some(identity) = claim("sub").or_else(|| email.clone()) {
            extras.insert(OAuthTokens::IDENTITY_KEY.to_string(), identity);
        }
        Ok((email.unwrap_or_else(|| "Grok Account".into()), token.into_tokens(Utc::now(), extras)))
    }
}
