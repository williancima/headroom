//! Generic device-code sign-in.
//! Never log tokens or response bodies.

use super::poll::{poll, Poll};
use super::{error_code, post_form, OAuthError, RawResponse, TokenResponse};
use reqwest::{Client, Url};
use serde::Deserialize;
use std::time::Duration;

const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
const DEFAULT_INTERVAL_SECS: f64 = 5.0;

pub struct DeviceAuthorization<'a> {
    /// Display name used in failure messages.
    pub provider: &'a str,
    pub start_url: &'a str,
    pub token_url: &'a str,
    pub client_id: &'a str,
    /// Left out of the start request when `None`.
    pub scope: Option<&'a str>,
    pub start_fields: &'a [(&'a str, &'a str)],
    /// Sent on both the start and the token requests.
    pub headers: &'a [(&'a str, &'a str)],
    /// Preferred verification page; gets `?user_code=` appended.
    pub verification_page: Option<Url>,
    pub denied_message: &'a str,
}

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: Option<String>,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: Option<f64>,
}

impl DeviceCode {
    /// The first candidate that is https with a host: page, `verification_uri_complete`, `verification_uri`.
    fn verification_url(&self, params: &DeviceAuthorization) -> Option<Url> {
        let base = Url::parse(params.start_url).ok()?;
        let page = params.verification_page.clone().map(|mut u| {
            u.query_pairs_mut().append_pair("user_code", &self.user_code);
            u
        });
        let relative = [&self.verification_uri_complete, &self.verification_uri]
            .into_iter()
            .filter_map(|u| base.join(u.as_deref()?).ok());
        page.into_iter().chain(relative).find(|u| u.scheme() == "https" && u.host_str().is_some_and(|h| !h.is_empty()))
    }
}

/// Starts the device flow, hands `(user_code, verification_url)` to `prompt`, and polls for the token.
/// Callers read provider-specific fields (such as `org_id`) from the returned [`TokenResponse`].
pub async fn device_authorization(
    client: &Client,
    params: &DeviceAuthorization<'_>,
    prompt: impl FnOnce(&str, &Url),
) -> Result<TokenResponse, OAuthError> {
    let mut fields = vec![("client_id", params.client_id)];
    fields.extend(params.scope.map(|s| ("scope", s)));
    fields.extend_from_slice(params.start_fields);
    let start = post_form(client, params.start_url, &fields, params.headers).await?;
    if !(200..300).contains(&start.status) {
        return Err(OAuthError::RequestFailed(start.status));
    }
    let device: DeviceCode = serde_json::from_slice(&start.body).map_err(|_| OAuthError::InvalidResponse)?;
    let url = device.verification_url(params).ok_or(OAuthError::InvalidResponse)?;
    prompt(&device.user_code, &url);
    let interval = Duration::from_secs_f64(device.interval.unwrap_or(DEFAULT_INTERVAL_SECS).max(0.0));
    let token_fields = [("client_id", params.client_id), ("device_code", device.device_code.as_str()), ("grant_type", GRANT_TYPE)];
    poll(interval, Duration::from_secs(device.expires_in), || async {
        let response = post_form(client, params.token_url, &token_fields, params.headers).await?;
        Ok(step(params, &response))
    })
    .await
}

/// The token is decoded on any status before it looks at `error`.
fn step(params: &DeviceAuthorization, response: &RawResponse) -> Poll<TokenResponse> {
    if let Ok(token) = TokenResponse::decode(&response.body) {
        return Poll::Complete(token);
    }
    match error_code(&response.body).as_deref() {
        Some("authorization_pending") => Poll::Pending,
        Some("slow_down") => Poll::SlowDown,
        Some("expired_token") => Poll::Failed(format!("{} device sign-in expired.", params.provider)),
        Some("access_denied") => Poll::Failed(params.denied_message.to_string()),
        _ => Poll::Failed(format!("{} device sign-in failed.", params.provider)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use wiremock::matchers::{body_string, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn params<'a>(start: &'a str, token: &'a str) -> DeviceAuthorization<'a> {
        DeviceAuthorization {
            provider: "Acme",
            start_url: start,
            token_url: token,
            client_id: "cid",
            scope: Some("openid"),
            start_fields: &[],
            headers: &[],
            verification_page: None,
            denied_message: "Denied by user.",
        }
    }

    fn start_json(extra: &str) -> String {
        format!(r#"{{"device_code":"dc","user_code":"UC-1","expires_in":600,"interval":0{extra}}}"#)
    }

    async fn mount_start(server: &MockServer, body: String) {
        Mock::given(method("POST")).and(path("/start")).respond_with(ResponseTemplate::new(200).set_body_string(body)).mount(server).await;
    }

    async fn mount_token(server: &MockServer, status: u16, body: &str) {
        Mock::given(method("POST")).and(path("/token")).respond_with(ResponseTemplate::new(status).set_body_string(body.to_string())).mount(server).await;
    }

    /// Runs the flow, returning the result and the prompted `(code, url)`.
    async fn run(p: &DeviceAuthorization<'_>) -> (Result<TokenResponse, OAuthError>, Option<(String, String)>) {
        let shown = RefCell::new(None);
        let r = device_authorization(&Client::new(), p, |c, u| *shown.borrow_mut() = Some((c.to_string(), u.to_string()))).await;
        (r, shown.into_inner())
    }

    fn urls(server: &MockServer) -> (String, String) {
        (format!("{}/start", server.uri()), format!("{}/token", server.uri()))
    }

    #[tokio::test(start_paused = true)]
    async fn happy_path_sends_scope_and_returns_the_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/start")).and(body_string("client_id=cid&scope=openid"))
            .respond_with(ResponseTemplate::new(200).set_body_string(start_json(r#","verification_uri":"https://v.test/device""#)))
            .expect(1).mount(&server).await;
        Mock::given(method("POST")).and(path("/token"))
            .and(body_string("client_id=cid&device_code=dc&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"access_token":"a","org_id":"org"}"#))
            .expect(1).mount(&server).await;
        let (s, t) = urls(&server);
        let (r, shown) = run(&params(&s, &t)).await;
        let token = r.unwrap();
        assert_eq!((token.access_token.as_str(), token.org_id.as_deref()), ("a", Some("org")));
        assert_eq!(shown, Some(("UC-1".into(), "https://v.test/device".into())));
    }

    #[tokio::test(start_paused = true)]
    async fn scope_is_omitted_and_start_fields_and_headers_are_sent() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/start")).and(header("user-agent", "Headroom"))
            .and(body_string("client_id=cid&supports_org_scope=true"))
            .respond_with(ResponseTemplate::new(200).set_body_string(start_json(r#","verification_uri":"https://v.test/d""#)))
            .expect(1).mount(&server).await;
        Mock::given(method("POST")).and(path("/token")).and(header("user-agent", "Headroom"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"access_token":"a"}"#))
            .expect(1).mount(&server).await;
        let (s, t) = urls(&server);
        let mut p = params(&s, &t);
        p.scope = None;
        p.start_fields = &[("supports_org_scope", "true")];
        p.headers = &[("User-Agent", "Headroom")];
        assert!(run(&p).await.0.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn verification_url_follows_the_candidate_order() {
        let server = MockServer::start().await;
        mount_token(&server, 200, r#"{"access_token":"a"}"#).await;
        let (s, t) = urls(&server);
        let both = r#","verification_uri":"https://v.test/plain","verification_uri_complete":"https://v.test/complete?c=1""#;
        let cases = [
            (both, Some("https://page.test/go"), "https://page.test/go?user_code=UC-1"),
            (both, None, "https://v.test/complete?c=1"),
            (r#","verification_uri":"https://v.test/plain""#, None, "https://v.test/plain"),
            // Resolved against the start URL, which is http here, so it is rejected and the next one wins.
            (r#","verification_uri_complete":"/rel","verification_uri":"https://v.test/plain""#, None, "https://v.test/plain"),
            (r#","verification_uri_complete":"http://v.test/insecure","verification_uri":"https://v.test/plain""#, None, "https://v.test/plain"),
        ];
        for (extra, page, expected) in cases {
            server.reset().await;
            mount_token(&server, 200, r#"{"access_token":"a"}"#).await;
            mount_start(&server, start_json(extra)).await;
            let mut p = params(&s, &t);
            p.verification_page = page.map(|u| Url::parse(u).unwrap());
            assert_eq!(run(&p).await.1.unwrap().1, expected, "{extra}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_relative_uri_resolves_against_the_start_url_and_non_https_is_invalid() {
        let base = Url::parse("https://auth.test/device/start").unwrap();
        let device: DeviceCode = serde_json::from_str(&start_json(r#","verification_uri":"/activate""#)).unwrap();
        let server = MockServer::start().await;
        let p = params(base.as_str(), "https://auth.test/token");
        assert_eq!(device.verification_url(&p).unwrap().as_str(), "https://auth.test/activate");
        let (s, t) = urls(&server);
        mount_start(&server, start_json(r#","verification_uri":"http://v.test/plain""#)).await;
        assert!(matches!(run(&params(&s, &t)).await.0, Err(OAuthError::InvalidResponse)));
    }

    #[tokio::test(start_paused = true)]
    async fn pending_then_slow_down_then_complete() {
        let server = MockServer::start().await;
        mount_start(&server, start_json(r#","verification_uri":"https://v.test/d""#)).await;
        for (status, body) in [(400, r#"{"error":"authorization_pending"}"#), (400, r#"{"error":"slow_down"}"#)] {
            Mock::given(method("POST")).and(path("/token")).respond_with(ResponseTemplate::new(status).set_body_string(body))
                .up_to_n_times(1).mount(&server).await;
        }
        mount_token(&server, 200, r#"{"access_token":"a"}"#).await;
        let (s, t) = urls(&server);
        assert_eq!(run(&params(&s, &t)).await.0.unwrap().access_token, "a");
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_errors_map_to_messages() {
        let server = MockServer::start().await;
        mount_start(&server, start_json(r#","verification_uri":"https://v.test/d""#)).await;
        let (s, t) = urls(&server);
        for (body, message) in [
            (r#"{"error":"expired_token"}"#, "Acme device sign-in expired."),
            (r#"{"error":"access_denied"}"#, "Denied by user."),
            (r#"{"error":"boom"}"#, "Acme device sign-in failed."),
            ("not json", "Acme device sign-in failed."),
        ] {
            server.reset().await;
            mount_start(&server, start_json(r#","verification_uri":"https://v.test/d""#)).await;
            mount_token(&server, 400, body).await;
            assert_eq!(run(&params(&s, &t)).await.0.unwrap_err().to_string(), message, "{body}");
        }
    }
}
