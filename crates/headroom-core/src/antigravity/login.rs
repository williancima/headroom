//! `AntigravityLogin`: Google loopback sign-in on the fixed port, with the userinfo read for the identity.

use std::collections::HashMap;
use std::io::ErrorKind;

use chrono::Utc;
use reqwest::Url;
use serde_json::Value;

use super::{AntigravityClient, CLIENT_ID, CLIENT_SECRET, SCOPES};
use crate::models::OAuthTokens;
use crate::oauth::{
    self,
    loopback::LoopbackServer,
    pkce::{random_state, Pkce},
    OAuthError, TokenResponse,
};

impl AntigravityClient {
    /// Browser sign-in on `loopback_port` (`/oauth-callback`). `open` receives the authorize URL; wrap the
    /// call in a timeout. Returns the account label (email, else "Google Account") and the tokens, with
    /// the Google user id (else the email) as the identity.
    pub async fn login(&self, open: impl FnOnce(&Url)) -> Result<(String, OAuthTokens), OAuthError> {
        let pkce = Pkce::generate();
        let state = random_state();
        let port = self.config.loopback_port;
        let server = LoopbackServer::bind(port, "/oauth-callback", state.clone()).await.map_err(|e| match e.kind() {
            ErrorKind::AddrInUse => OAuthError::Failed(format!("Port {port} is in use.")),
            _ => OAuthError::ListenerFailed(e),
        })?;
        let redirect = server.redirect_uri();
        let url = oauth::authorize_url(
            &self.config.authorize_url,
            CLIENT_ID,
            &redirect,
            SCOPES,
            &state,
            &pkce.challenge,
            &[("access_type", "offline"), ("prompt", "consent")],
        )?;
        open(&url);
        let code = server.wait().await?;
        let fields = [
            ("client_id", CLIENT_ID),
            ("client_secret", CLIENT_SECRET),
            ("code", &code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", &redirect),
            ("code_verifier", &pkce.verifier),
        ];
        let response = oauth::post_form(&self.http, &self.config.token_url, &fields, &[]).await?;
        let token = TokenResponse::from_exchange(&response)?;
        let userinfo = self
            .http
            .get(&self.config.userinfo_url)
            .bearer_auth(&token.access_token)
            .send()
            .await
            .map_err(|_| OAuthError::Network)?;
        let ok = userinfo.status().is_success();
        let body = userinfo.bytes().await.map_err(|_| OAuthError::Network)?;
        let json: Option<Value> = ok.then(|| serde_json::from_slice(&body).ok()).flatten();
        let field = |key: &str| json.as_ref().and_then(|j| j.get(key)?.as_str().map(String::from));
        let (id, email) = (field("id"), field("email"));
        let mut extras = HashMap::new();
        if let Some(identity) = id.or_else(|| email.clone()) {
            extras.insert(OAuthTokens::IDENTITY_KEY.to_string(), identity);
        }
        Ok((email.unwrap_or_else(|| "Google Account".into()), token.into_tokens(Utc::now(), extras)))
    }
}
