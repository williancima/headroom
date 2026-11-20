//! `CursorLogin`: a browser deep link, then polling `/auth/poll` for the tokens (no loopback callback).

use reqwest::Url;
use serde_json::Value;

use super::{session, CursorClient};
use crate::models::OAuthTokens;
use crate::oauth::{
    jwt_expiry,
    pkce::Pkce,
    poll::{poll, Poll},
    OAuthError, RawResponse,
};

struct Granted {
    access_token: String,
    refresh_token: String,
}

/// 2xx with both tokens completes; 404 and 202 wait; anything else ends the sign-in.
fn poll_step(response: &RawResponse) -> Poll<Granted> {
    if (200..300).contains(&response.status) {
        let json: Option<Value> = serde_json::from_slice(&response.body).ok();
        let text = |key: &str| json.as_ref().and_then(|j| j.get(key)?.as_str()).filter(|t| !t.is_empty()).map(String::from);
        if let (Some(access_token), Some(refresh_token)) = (text("accessToken"), text("refreshToken")) {
            return Poll::Complete(Granted { access_token, refresh_token });
        }
    }
    if response.status == 404 || response.status == 202 {
        return Poll::Pending;
    }
    Poll::Failed("Cursor sign-in failed.".into())
}

/// The profile email, else its name, from `/api/auth/me`.
fn profile_label(body: &[u8]) -> Option<String> {
    let json: Value = serde_json::from_slice(body).ok()?;
    ["email", "name"].iter().filter_map(|k| json.get(*k)?.as_str()).find(|s| !s.is_empty()).map(String::from)
}

impl CursorClient {
    /// Opens `login_url?challenge&uuid&mode=login&redirectTarget=cli` through `open`, then polls until the
    /// user approves. Wrap the call in a timeout for cancelling. Returns the account label (profile email,
    /// else name, else "Cursor Account") and the tokens, with `cursor:<user id>` as the identity.
    pub async fn login(&self, open: impl FnOnce(&Url)) -> Result<(String, OAuthTokens), OAuthError> {
        let pkce = Pkce::generate();
        let uuid = uuid::Uuid::new_v4().to_string().to_uppercase();
        let mut url = Url::parse(&self.config.login_url).map_err(|_| OAuthError::InvalidResponse)?;
        url.query_pairs_mut()
            .append_pair("challenge", &pkce.challenge)
            .append_pair("uuid", &uuid)
            .append_pair("mode", "login")
            .append_pair("redirectTarget", "cli");
        open(&url);

        let mut poll_url = Url::parse(&self.config.poll_url).map_err(|_| OAuthError::InvalidResponse)?;
        poll_url.query_pairs_mut().append_pair("uuid", &uuid).append_pair("verifier", &pkce.verifier);
        let http = &self.http;
        let granted = poll(self.config.poll_interval, self.config.poll_expiry, || {
            let request = http.get(poll_url.clone());
            async move {
                let response = request.send().await.map_err(|_| OAuthError::Network)?;
                let status = response.status().as_u16();
                let body = response.bytes().await.map_err(|_| OAuthError::Network)?.to_vec();
                Ok(poll_step(&RawResponse { status, body }))
            }
        })
        .await?;

        // The profile read is best-effort: any failure names the account "Cursor Account".
        let label = match self.web_get("/api/auth/me", &granted.access_token).await {
            Ok(Some(r)) if (200..300).contains(&r.status) => profile_label(&r.body),
            _ => None,
        };
        let mut extras = std::collections::HashMap::new();
        if let Some((user_id, _)) = session(&granted.access_token) {
            extras.insert(OAuthTokens::IDENTITY_KEY.to_string(), format!("cursor:{user_id}"));
        }
        let tokens = OAuthTokens {
            expires_at: jwt_expiry(&granted.access_token),
            access_token: granted.access_token,
            refresh_token: Some(granted.refresh_token),
            extras,
        };
        Ok((label.unwrap_or_else(|| "Cursor Account".into()), tokens))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(status: u16, body: &str) -> Poll<Granted> {
        poll_step(&RawResponse { status, body: body.as_bytes().to_vec() })
    }

    #[test]
    fn poll_responses() {
        assert!(matches!(step(200, r#"{"accessToken":"a","refreshToken":"r","authId":"x"}"#), Poll::Complete(g) if g.access_token == "a" && g.refresh_token == "r"));
        for status in [404, 202] {
            assert!(matches!(step(status, ""), Poll::Pending));
        }
        for (status, body) in [(200, "{}"), (200, r#"{"accessToken":"a","refreshToken":""}"#), (500, ""), (401, "")] {
            assert!(matches!(step(status, body), Poll::Failed(m) if m == "Cursor sign-in failed."), "{status} {body}");
        }
    }

    #[test]
    fn label_prefers_email_then_name() {
        assert_eq!(profile_label(br#"{"email":"a@b.c","name":"N"}"#).as_deref(), Some("a@b.c"));
        assert_eq!(profile_label(br#"{"email":"","name":"N"}"#).as_deref(), Some("N"));
        assert_eq!(profile_label(b"{}"), None);
    }
}
