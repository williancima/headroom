//! `CodexLogin`: loopback browser sign-in on the fixed port, with the device-code flow as fallback.

use std::time::Duration;

use chrono::Utc;
use reqwest::Url;
use serde_json::Value;

use super::{CodexClient, ACCOUNT_ID_KEY, CLIENT_ID, SCOPES};
use crate::models::OAuthTokens;
use crate::oauth::{
    self, jwt_payload,
    loopback::LoopbackServer,
    pkce::{random_state, Pkce},
    poll::{poll, Poll},
    OAuthError, RawResponse, TokenResponse,
};
use crate::openrouter::number;

const DEFAULT_INTERVAL_SECS: f64 = 5.0;
const DEVICE_EXPIRY: Duration = Duration::from_secs(900);

struct DeviceToken {
    authorization_code: String,
    code_verifier: String,
}

/// One device-token poll: 2xx completes; 403/404 and `deviceauth_authorization_pending` wait; `slow_down`
/// backs off; anything else ends the sign-in.
fn device_step(response: &RawResponse) -> Result<Poll<DeviceToken>, OAuthError> {
    let json: Option<Value> = serde_json::from_slice(&response.body).ok();
    let text = |key: &str| json.as_ref().and_then(|j| j.get(key)?.as_str().map(String::from));
    if (200..300).contains(&response.status) {
        return match (text("authorization_code"), text("code_verifier")) {
            (Some(authorization_code), Some(code_verifier)) => Ok(Poll::Complete(DeviceToken { authorization_code, code_verifier })),
            _ => Err(OAuthError::InvalidResponse),
        };
    }
    if response.status == 403 || response.status == 404 {
        return Ok(Poll::Pending);
    }
    let error = text("error").unwrap_or_default();
    Ok(match error.as_str() {
        "deviceauth_authorization_pending" => Poll::Pending,
        "slow_down" => Poll::SlowDown,
        "expired_token" => Poll::Failed("Device sign-in expired.".into()),
        _ => Poll::Failed("Device sign-in failed.".into()),
    })
}

impl CodexClient {
    /// Browser sign-in on `loopback_port`; when it can't be bound, a device-code sign-in instead.
    /// `open` receives the authorize URL; `show_device_code` receives the user code and the page to type
    /// it on. Wrap the call in a timeout. Returns the account label and the tokens, with the identity in
    /// the extras.
    pub async fn login(
        &self,
        open: impl FnOnce(&Url),
        show_device_code: impl FnOnce(&str, &Url),
    ) -> Result<(String, OAuthTokens), OAuthError> {
        let pkce = Pkce::generate();
        let state = random_state();
        let Ok(server) = LoopbackServer::bind(self.config.loopback_port, "/auth/callback", state.clone()).await else {
            return self.device_login(show_device_code).await;
        };
        let redirect = server.redirect_uri();
        let url = oauth::authorize_url(
            &self.config.authorize_url,
            CLIENT_ID,
            &redirect,
            SCOPES,
            &state,
            &pkce.challenge,
            &[("id_token_add_organizations", "true"), ("codex_cli_simplified_flow", "true"), ("originator", "pi")],
        )?;
        open(&url);
        let code = server.wait().await?;
        self.exchange(&code, &pkce.verifier, &redirect).await
    }

    async fn device_login(&self, show_device_code: impl FnOnce(&str, &Url)) -> Result<(String, OAuthTokens), OAuthError> {
        let start = oauth::post_json(&self.http, &self.config.device_usercode_url, &[("client_id", CLIENT_ID)]).await?;
        if !(200..300).contains(&start.status) {
            return Err(OAuthError::RequestFailed(start.status));
        }
        let json: Value = serde_json::from_slice(&start.body).map_err(|_| OAuthError::InvalidResponse)?;
        let text = |key: &str| json.get(key).and_then(Value::as_str).map(String::from).ok_or(OAuthError::InvalidResponse);
        let (device_auth_id, user_code) = (text("device_auth_id")?, text("user_code")?);
        let verify = Url::parse(&self.config.device_verify_url).map_err(|_| OAuthError::InvalidResponse)?;
        show_device_code(&user_code, &verify);
        let interval = number(json.get("interval")).unwrap_or(DEFAULT_INTERVAL_SECS).max(0.0);
        let (http, url) = (&self.http, self.config.device_token_url.as_str());
        let (id, code) = (device_auth_id.as_str(), user_code.as_str());
        let device = poll(Duration::from_secs_f64(interval), DEVICE_EXPIRY, || async move {
            let response = oauth::post_json(http, url, &[("device_auth_id", id), ("user_code", code)]).await?;
            device_step(&response)
        })
        .await?;
        self.exchange(&device.authorization_code, &device.code_verifier, &self.config.device_redirect_uri).await
    }

    async fn exchange(&self, code: &str, verifier: &str, redirect_uri: &str) -> Result<(String, OAuthTokens), OAuthError> {
        let fields = [
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
        ];
        let response = oauth::post_form(&self.http, &self.config.token_url, &fields, &[]).await?;
        let token = TokenResponse::from_exchange(&response)?;
        let account_id = jwt_payload(&token.access_token)
            .and_then(|p| p.get("https://api.openai.com/auth")?.get("chatgpt_account_id")?.as_str().map(String::from));
        let email = token.id_token.as_deref().and_then(jwt_payload).and_then(|p| p.get("email")?.as_str().map(String::from));
        let mut extras = std::collections::HashMap::new();
        if let Some(id) = &account_id {
            extras.insert(ACCOUNT_ID_KEY.to_string(), id.clone());
        }
        if let (Some(id), Some(email)) = (&account_id, &email) {
            extras.insert(OAuthTokens::IDENTITY_KEY.to_string(), format!("{id}/{email}"));
        }
        Ok((email.unwrap_or_else(|| "ChatGPT Account".into()), token.into_tokens(Utc::now(), extras)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(status: u16, body: &str) -> Result<Poll<DeviceToken>, OAuthError> {
        device_step(&RawResponse { status, body: body.as_bytes().to_vec() })
    }

    #[test]
    fn device_poll_responses() {
        assert!(matches!(step(200, r#"{"authorization_code":"c","code_verifier":"v"}"#), Ok(Poll::Complete(t)) if t.authorization_code == "c" && t.code_verifier == "v"));
        assert!(matches!(step(200, "{}"), Err(OAuthError::InvalidResponse)));
        for status in [403, 404] {
            assert!(matches!(step(status, ""), Ok(Poll::Pending)));
        }
        assert!(matches!(step(400, r#"{"error":"deviceauth_authorization_pending"}"#), Ok(Poll::Pending)));
        assert!(matches!(step(400, r#"{"error":"slow_down"}"#), Ok(Poll::SlowDown)));
        assert!(matches!(step(400, r#"{"error":"expired_token"}"#), Ok(Poll::Failed(m)) if m == "Device sign-in expired."));
        assert!(matches!(step(500, "oops"), Ok(Poll::Failed(m)) if m == "Device sign-in failed."));
    }
}
