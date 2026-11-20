//! `DevinLogin`: PKCE in the browser with a loopback callback, then a code exchange that returns the
//! session token. Devin accepts only `http://127.0.0.1:<any port>/callback` as the redirect.

use reqwest::Url;
use serde_json::Value;

use super::{DevinClient, SESSION_TOKEN_PREFIX};
use crate::models::OAuthTokens;
use crate::oauth::{
    self,
    loopback::LoopbackServer,
    pkce::{random_state, Pkce},
    OAuthError,
};

const REFUSED_CODE_MSG: &str = "Devin didn’t accept the sign-in. Click Try Again to sign in again.";
const NO_ACCOUNT_MSG: &str = "Devin signed you in but didn’t return your account. Click Try Again to sign in again.";

impl DevinClient {
    /// Browser sign-in on a random loopback port. `open` receives the authorize URL; wrap the call in a
    /// timeout. Returns the account label (email, else "Devin Account") and the tokens, with
    /// `devin:<email>[/<team id>]` as the identity.
    pub async fn login(&self, open: impl FnOnce(&Url)) -> Result<(String, OAuthTokens), OAuthError> {
        let pkce = Pkce::generate();
        let state = random_state();
        let server = LoopbackServer::bind(0, "/callback", state.clone()).await.map_err(OAuthError::ListenerFailed)?;
        let mut url = Url::parse(&self.config.authorize_url).map_err(|_| OAuthError::InvalidResponse)?;
        url.query_pairs_mut()
            .append_pair("redirect_uri", &server.redirect_uri_for("127.0.0.1"))
            .append_pair("state", &state)
            .append_pair("prompt", "select_account")
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256");
        open(&url);
        let code = server.wait().await?;

        let exchange = oauth::post_json(&self.http, &self.config.token_url, &[("code", &code), ("code_verifier", &pkce.verifier)]).await?;
        if !(200..300).contains(&exchange.status) {
            return Err(OAuthError::Failed(REFUSED_CODE_MSG.into()));
        }
        let json: Value = serde_json::from_slice(&exchange.body).map_err(|_| OAuthError::InvalidResponse)?;
        let token = json.get("token").and_then(Value::as_str).filter(|t| !t.is_empty()).ok_or(OAuthError::InvalidResponse)?;
        let access_token = if token.starts_with(SESSION_TOKEN_PREFIX) { token.to_string() } else { format!("{SESSION_TOKEN_PREFIX}{token}") };
        let mut tokens = OAuthTokens { access_token, refresh_token: None, expires_at: None, extras: Default::default() };

        let (status, body) = self.fetch_user_status(&tokens).await.map_err(|_| OAuthError::Network)?;
        if !(200..300).contains(&status) {
            return Err(OAuthError::Failed(NO_ACCOUNT_MSG.into()));
        }
        let json: Value = serde_json::from_slice(&body).map_err(|_| OAuthError::InvalidResponse)?;
        let user = json.get("userStatus").filter(|u| u.is_object()).ok_or(OAuthError::InvalidResponse)?;
        let text = |key: &str| user.get(key).and_then(Value::as_str).map(String::from);
        let (email, team_id) = (text("email"), text("teamId"));
        if let Some(email) = &email {
            let identity = match team_id {
                Some(team) => format!("devin:{email}/{team}"),
                None => format!("devin:{email}"),
            };
            tokens.extras.insert(OAuthTokens::IDENTITY_KEY.to_string(), identity);
        }
        Ok((email.unwrap_or_else(|| "Devin Account".into()), tokens))
    }
}
