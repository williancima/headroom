//! `AmpLogin`: Amp's device sign-in, under the Amp CLI's own public WorkOS client and through the page
//! `amp login` opens. The account takes the address Amp's usage text reports for it, so the sign-in
//! proves it can read usage.

use reqwest::Url;

use super::{mapper, AmpClient, CLIENT_ID};
use crate::models::OAuthTokens;
use crate::oauth::device::{device_authorization, DeviceAuthorization};
use crate::oauth::{jwt_expiry, jwt_payload, OAuthError};

impl AmpClient {
    /// Device sign-in. `show_device_code` receives the user code and the page to type it on. Wrap the call
    /// in a timeout. Returns the account's address (else "Amp") and the tokens, with the identity in the
    /// extras.
    pub async fn login(&self, show_device_code: impl FnOnce(&str, &Url)) -> Result<(String, OAuthTokens), OAuthError> {
        let page = Url::parse(&self.config.verification_page).map_err(|_| OAuthError::InvalidResponse)?;
        let params = DeviceAuthorization {
            provider: "Amp",
            start_url: &self.config.device_start_url,
            token_url: &self.config.token_url,
            client_id: CLIENT_ID,
            scope: None,
            start_fields: &[],
            headers: &[],
            verification_page: Some(page),
            denied_message: "Amp device sign-in was denied.",
        };
        let token = device_authorization(&self.http, &params, show_device_code).await?;
        let balance = self.fetch_balance(&token.access_token).await.map_err(|_| OAuthError::Network)?;
        if !(200..300).contains(&balance.status) {
            return Err(OAuthError::RequestFailed(balance.status));
        }
        let mut extras = std::collections::HashMap::new();
        let user_id = jwt_payload(&token.access_token).and_then(|p| p.get("sub")?.as_str().map(String::from));
        if let Some(id) = user_id {
            extras.insert(OAuthTokens::IDENTITY_KEY.to_string(), format!("amp:{id}"));
        }
        let label = mapper::email(&balance.body).unwrap_or_else(|| "Amp".into());
        // The expiry is the JWT's own `exp`: WorkOS reports no `expires_in`.
        let tokens = OAuthTokens { expires_at: jwt_expiry(&token.access_token), access_token: token.access_token, refresh_token: token.refresh_token, extras };
        Ok((label, tokens))
    }
}
