//! OAuth building blocks: authorize URLs, token requests and JWT parsing.
//! Never log tokens or response bodies.

pub mod device;
pub mod loopback;
pub mod pkce;
pub mod poll;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration, Utc};
use reqwest::{header, Client, Url};
use serde::Deserialize;
use serde_json::Value;

use crate::models::OAuthTokens;

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("Sign-in was cancelled.")]
    Cancelled,
    #[error("The sign-in service returned an invalid response.")]
    InvalidResponse,
    #[error("Sign-in failed with HTTP {0}.")]
    RequestFailed(u16),
    #[error("The sign-in response did not match this request.")]
    InvalidState,
    #[error("The authorization code was invalid.")]
    InvalidCode,
    #[error("Could not start the sign-in callback: {0}")]
    ListenerFailed(#[source] std::io::Error),
    #[error("The sign-in callback request was invalid.")]
    InvalidCallback,
    #[error("Authorization failed: {0}")]
    AuthorizationFailed(String),
    /// The refresh token was refused; the user must sign in again.
    #[error("Sign-in expired. Sign in again.")]
    SignInExpired,
    #[error("Connection failed. Check your internet connection and try again.")]
    Network,
    #[error("{0}")]
    Failed(String),
}

/// `authorize_url?client_id&response_type=code&redirect_uri&scope&code_challenge&code_challenge_method=S256&state` + extras.
pub fn authorize_url(
    base: &str,
    client_id: &str,
    redirect_uri: &str,
    scopes: &str,
    state: &str,
    challenge: &str,
    extra: &[(&str, &str)],
) -> Result<Url, OAuthError> {
    let mut url = Url::parse(base).map_err(|_| OAuthError::InvalidResponse)?;
    let pairs = [
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("scope", scopes),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ];
    // Spaces as %20; `query_pairs_mut` would send `+`.
    let query: Vec<String> = pairs.iter().chain(extra).map(|(k, v)| format!("{k}={}", form_encode(v))).collect();
    url.set_query(Some(&query.join("&")));
    Ok(url)
}

pub struct RawResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// RFC 3986 unreserved characters pass through; everything else (space included) is `%XX`.
fn form_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Body of `application/x-www-form-urlencoded`, `key=value` pairs sorted and joined with `&`.
pub fn form_body(fields: &[(&str, &str)]) -> String {
    let mut pairs: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={}", form_encode(v))).collect();
    pairs.sort();
    pairs.join("&")
}

async fn send(request: reqwest::RequestBuilder) -> Result<RawResponse, OAuthError> {
    let response = request.send().await.map_err(|_| OAuthError::Network)?;
    let status = response.status().as_u16();
    let body = response.bytes().await.map_err(|_| OAuthError::Network)?.to_vec();
    Ok(RawResponse { status, body })
}

pub async fn post_form(
    client: &Client,
    url: &str,
    fields: &[(&str, &str)],
    headers: &[(&str, &str)],
) -> Result<RawResponse, OAuthError> {
    let mut req = client
        .post(url)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ACCEPT, "application/json")
        .body(form_body(fields));
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    send(req).await
}

pub async fn post_json(client: &Client, url: &str, fields: &[(&str, &str)]) -> Result<RawResponse, OAuthError> {
    let map: serde_json::Map<String, Value> = fields.iter().map(|(k, v)| (k.to_string(), Value::from(*v))).collect();
    send(client.post(url).json(&map)).await
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<f64>,
    pub id_token: Option<String>,
    /// The workspace an OpenCode Console sign-in is scoped to.
    pub org_id: Option<String>,
}

impl TokenResponse {
    pub fn expires_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.expires_in.map(|s| now + Duration::milliseconds((s * 1000.0) as i64))
    }

    pub fn into_tokens(self, now: DateTime<Utc>, extras: std::collections::HashMap<String, String>) -> OAuthTokens {
        OAuthTokens { expires_at: self.expires_at(now), access_token: self.access_token, refresh_token: self.refresh_token, extras }
    }

    fn decode(body: &[u8]) -> Result<Self, OAuthError> {
        match serde_json::from_slice::<Self>(body) {
            Ok(t) if !t.access_token.is_empty() => Ok(t),
            _ => Err(OAuthError::InvalidResponse),
        }
    }

    /// Code exchange: any non-2xx is a failed request.
    pub fn from_exchange(response: &RawResponse) -> Result<Self, OAuthError> {
        if !(200..300).contains(&response.status) {
            return Err(OAuthError::RequestFailed(response.status));
        }
        Self::decode(&response.body)
    }

    /// Renewal: only a refusal of the refresh token itself is `SignInExpired`; anything else is transient.
    pub fn from_renewal(response: &RawResponse) -> Result<Self, OAuthError> {
        const REFUSALS: [&str; 4] = ["invalid_grant", "refresh_token_expired", "refresh_token_reused", "refresh_token_invalidated"];
        if [400, 401, 403].contains(&response.status)
            && error_code(&response.body).is_some_and(|c| REFUSALS.contains(&c.as_str()))
        {
            return Err(OAuthError::SignInExpired);
        }
        Self::from_exchange(response)
    }
}

/// RFC 6749 sends `error` as a string; OpenAI also sends `error.code` or a top-level `code`.
pub fn error_code(body: &[u8]) -> Option<String> {
    let json: Value = serde_json::from_slice(body).ok()?;
    let from = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_string);
    from(json.get("error"))
        .or_else(|| from(json.get("error").and_then(|e| e.get("code"))))
        .or_else(|| from(json.get("code")))
}

/// Decodes a JWT payload (no signature check). Padding is tolerated.
pub fn jwt_payload(token: &str) -> Option<Value> {
    let segment = token.split('.').filter(|s| !s.is_empty()).nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(segment.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn jwt_expiry(token: &str) -> Option<DateTime<Utc>> {
    let exp = jwt_payload(token)?.get("exp")?.as_f64()?;
    DateTime::from_timestamp_millis((exp * 1000.0) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn raw(status: u16, body: &str) -> RawResponse {
        RawResponse { status, body: body.as_bytes().to_vec() }
    }

    #[test]
    fn authorize_url_has_the_expected_query() {
        let url = authorize_url("https://a.test/authorize", "cid", "http://localhost:1/callback", "a b", "st", "ch", &[("originator", "x")]).unwrap();
        let q: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let keys: Vec<&str> = q.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["client_id", "response_type", "redirect_uri", "scope", "code_challenge", "code_challenge_method", "state", "originator"]);
        assert_eq!(q[1].1, "code");
        assert_eq!(q[3].1, "a b");
        assert_eq!(q[5].1, "S256");
    }

    #[test]
    fn form_body_is_sorted_and_percent_encoded() {
        assert_eq!(form_body(&[("z", "1"), ("a", "x y+/")]), "a=x%20y%2B%2F&z=1");
    }

    #[test]
    fn token_response_reads_the_known_keys() {
        let t = TokenResponse::from_exchange(&raw(200, r#"{"access_token":"a","refresh_token":"r","expires_in":3600,"id_token":"i","org_id":"o"}"#)).unwrap();
        assert_eq!((t.refresh_token.as_deref(), t.id_token.as_deref(), t.org_id.as_deref()), (Some("r"), Some("i"), Some("o")));
        let now = Utc::now();
        assert_eq!(t.expires_at(now), Some(now + Duration::seconds(3600)));
        let bare = TokenResponse::from_exchange(&raw(200, r#"{"access_token":"a"}"#)).unwrap();
        assert_eq!(bare.expires_at(now), None);
        assert!(matches!(TokenResponse::from_exchange(&raw(200, r#"{"access_token":""}"#)), Err(OAuthError::InvalidResponse)));
        assert!(matches!(TokenResponse::from_exchange(&raw(500, "")), Err(OAuthError::RequestFailed(500))));
    }

    #[test]
    fn renewal_maps_only_refusals_to_sign_in_expired() {
        for code in ["invalid_grant", "refresh_token_expired", "refresh_token_reused", "refresh_token_invalidated"] {
            for status in [400, 401, 403] {
                let body = format!(r#"{{"error":"{code}"}}"#);
                assert!(matches!(TokenResponse::from_renewal(&raw(status, &body)), Err(OAuthError::SignInExpired)), "{status} {code}");
            }
        }
        assert!(matches!(TokenResponse::from_renewal(&raw(401, r#"{"error":{"code":"refresh_token_expired"}}"#)), Err(OAuthError::SignInExpired)));
        assert!(matches!(TokenResponse::from_renewal(&raw(400, r#"{"code":"invalid_grant"}"#)), Err(OAuthError::SignInExpired)));
        assert!(matches!(TokenResponse::from_renewal(&raw(400, r#"{"error":"invalid_scope"}"#)), Err(OAuthError::RequestFailed(400))));
        assert!(matches!(TokenResponse::from_renewal(&raw(500, r#"{"error":"invalid_grant"}"#)), Err(OAuthError::RequestFailed(500))));
        assert!(TokenResponse::from_renewal(&raw(200, r#"{"access_token":"n"}"#)).is_ok());
    }

    #[tokio::test]
    async fn post_helpers_hit_the_server_and_map_renewal() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/token")).and(body_string("grant_type=refresh_token&refresh_token=r%201"))
            .respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid_grant"}"#))
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/json")).and(body_string(r#"{"k":"v"}"#))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"access_token":"a"}"#))
            .mount(&server).await;
        let client = Client::new();
        let r = post_form(&client, &format!("{}/token", server.uri()), &[("refresh_token", "r 1"), ("grant_type", "refresh_token")], &[]).await.unwrap();
        assert!(matches!(TokenResponse::from_renewal(&r), Err(OAuthError::SignInExpired)));
        let r = post_json(&client, &format!("{}/json", server.uri()), &[("k", "v")]).await.unwrap();
        assert_eq!(TokenResponse::from_exchange(&r).unwrap().access_token, "a");
    }

    #[test]
    fn jwt_payload_and_expiry() {
        // {"alg":"none"} . {"exp":1800000000,"sub":"u"}
        let token = "eyJhbGciOiJub25lIn0.eyJleHAiOjE4MDAwMDAwMDAsInN1YiI6InUifQ.sig";
        assert_eq!(jwt_payload(token).unwrap()["sub"], "u");
        assert_eq!(jwt_expiry(token).unwrap().timestamp(), 1_800_000_000);
        let padded = "eyJhbGciOiJub25lIn0.eyJleHAiOjE4MDAwMDAwMDAsInN1YiI6InUifQ==.sig";
        assert!(jwt_payload(padded).is_some());
        assert!(jwt_payload("nodots").is_none());
        assert!(jwt_expiry("a.e30.c").is_none());
    }
}
