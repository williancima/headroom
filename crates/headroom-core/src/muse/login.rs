//! `MuseLogin`: Meta's device sign-in under the Muse CLI's own public client. It sends no scope (the
//! device endpoint refuses `openid`) and returns a `dca:` token with no refresh token and no expiry; the
//! account takes the email Muse reports for it.

use reqwest::Url;
use std::collections::HashMap;

use super::{MuseClient, CLIENT_ID};
use crate::models::OAuthTokens;
use crate::oauth::device::{device_authorization, DeviceAuthorization};
use crate::oauth::OAuthError;
use crate::provider::ProviderError;

impl MuseClient {
    /// Device sign-in. `show_device_code` receives the user code and the page to type it on. Wrap the call
    /// in a timeout. The label is the account's email, else "Muse Code".
    pub async fn login(&self, show_device_code: impl FnOnce(&str, &Url)) -> Result<(String, OAuthTokens), OAuthError> {
        let params = DeviceAuthorization {
            provider: "Meta",
            start_url: &self.config.device_start_url,
            token_url: &self.config.device_token_url,
            client_id: CLIENT_ID,
            scope: None,
            start_fields: &[],
            headers: &[],
            verification_page: None,
            denied_message: "Meta device sign-in was denied.",
        };
        let token = device_authorization(&self.http, &params, show_device_code).await?;
        let account = self.account(&token.access_token).await.map_err(|e| match e {
            ProviderError::SignInExpired => OAuthError::SignInExpired,
            ProviderError::ConnectionFailed => OAuthError::Network,
            ProviderError::InvalidResponse => OAuthError::InvalidResponse,
            ProviderError::RequestFailed(status) => OAuthError::RequestFailed(status),
            other => OAuthError::Failed(other.to_string()),
        })?;
        let email = account.email.map(|e| e.trim().to_string()).filter(|e| !e.is_empty());
        let extras = email.iter().map(|e| (OAuthTokens::IDENTITY_KEY.to_string(), format!("muse:{e}"))).collect::<HashMap<_, _>>();
        let tokens = OAuthTokens { access_token: token.access_token, refresh_token: None, expires_at: None, extras };
        Ok((email.unwrap_or_else(|| "Muse Code".into()), tokens))
    }
}
