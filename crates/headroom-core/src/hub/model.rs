//! A CLIProxyAPI hub and the link from a mirrored account to its login.
//! Addresses are kept as normalized strings (no trailing slash), so they serialize without extra features.

use chrono::{DateTime, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::OAuthTokens;
use crate::provider::ProviderError;

use super::client::HubRefusal;
use super::gate::Refused;
use super::login::HubLogin;

/// The placeholder the hub swaps for the login's own token inside an `api-call`.
pub const TOKEN_PLACEHOLDER: &str = "$TOKEN$";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hub {
    pub id: Uuid,
    pub url: String,
    pub added_at: DateTime<Utc>,
    /// Set after the hub rejected the key or banned this machine; nothing is sent with that key again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<Refused>,
}

impl Hub {
    pub fn new(url: String) -> Self {
        Self { id: Uuid::new_v4(), url, added_at: Utc::now(), refused: None }
    }

    pub fn host(&self) -> String {
        host_of(&self.url)
    }
}

/// `host` or `host:port`; the whole text when it is not an address.
pub fn host_of(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else { return url.to_string() };
    let Some(host) = parsed.host_str() else { return url.to_string() };
    parsed.port().map_or_else(|| host.to_string(), |port| format!("{host}:{port}"))
}

/// The hub's base URL from what the user typed: the scheme defaults to https, and the query, the fragment,
/// credentials and a pasted panel or API path are dropped. A path the hub sits under behind a reverse proxy
/// is kept.
pub fn base_url(input: &str) -> Option<String> {
    let mut text = input.trim().to_string();
    if text.is_empty() {
        return None;
    }
    if !text.contains("://") {
        text = format!("https://{text}");
    }
    let mut url = Url::parse(&text).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let mut path = url.path().to_string();
    for suffix in ["/", "/management.html", "/v0/management", "/v8/management", "/v1"] {
        while path.to_lowercase().ends_with(suffix) {
            path.truncate(path.len() - suffix.len());
        }
    }
    url.set_path(&path);
    Some(url.as_str().trim_end_matches('/').to_string())
}

/// Whether the management key may go to `url` at all: over https anywhere, and over http only to this
/// machine, a host on the local network (a private or link-local address, or a `.local` name), or a
/// Tailscale peer (a `.ts.net` name or an address in 100.64.0.0/10 or fd7a:115c:a1e0::/48), whose traffic
/// WireGuard encrypts.
pub fn allows_key(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else { return false };
    if parsed.scheme() != "http" {
        return parsed.scheme() == "https";
    }
    let Some(host) = parsed.host_str() else { return false };
    let host = host.to_lowercase();
    let host = host.trim_matches(['[', ']']);
    if host == "localhost" || host.ends_with(".local") || host.ends_with(".ts.net") {
        return true;
    }
    if host.contains(':') {
        return host == "::1" || host.starts_with("fe80:") || host.starts_with("fc") || host.starts_with("fd");
    }
    let parts: Vec<&str> = host.split('.').collect();
    let octets: Vec<u8> = parts.iter().filter_map(|p| p.parse().ok()).collect();
    if parts.len() != 4 || octets.len() != 4 {
        return false;
    }
    matches!((octets[0], octets[1]), (127, _) | (10, _) | (192, 168) | (169, 254) | (172, 16..=31) | (100, 64..=127))
}

/// Where a mirrored account's login lives on its hub. `login_id` is the hub's stable id for the login (its
/// file name); `auth_index` is what `api-call` takes, and it changes when the hub renames the file.
/// `chatgpt_account_id` is the ChatGPT workspace a Codex login is signed in to, which can change under the
/// same file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubLink {
    pub hub_id: Uuid,
    pub hub_url: String,
    pub login_id: String,
    pub auth_index: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chatgpt_account_id: Option<String>,
}

impl HubLink {
    /// `None` for a login the hub listed without an auth index, which can't be called.
    pub fn new(hub: &Hub, login: &HubLogin) -> Option<Self> {
        Some(Self {
            hub_id: hub.id,
            hub_url: hub.url.clone(),
            login_id: login.id.clone(),
            auth_index: login.auth_index.clone()?,
            chatgpt_account_id: login.chatgpt_account_id.clone(),
        })
    }

    pub fn host(&self) -> String {
        host_of(&self.hub_url)
    }

    /// The account's sign-in: the hub's placeholder, which the hub swaps for its own token. No refresh
    /// token and no expiry, so nothing ever tries to renew it. A ChatGPT login also carries its workspace.
    pub fn tokens(&self) -> OAuthTokens {
        let extras = self.chatgpt_account_id.iter().map(|id| ("chatgpt_account_id".to_string(), id.clone())).collect();
        OAuthTokens { access_token: TOKEN_PLACEHOLDER.into(), refresh_token: None, expires_at: None, extras }
    }

    /// What a hub account's provider refusal means: the hub has no working token for this login.
    pub fn refusal(&self, error: ProviderError) -> ProviderError {
        match error {
            ProviderError::SignInExpired => HubRefusal::NoToken.error(&self.host(), None),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_is_cleaned_from_what_the_user_pastes() {
        let cases = [
            ("127.0.0.1:8317", Some("https://127.0.0.1:8317")),
            ("hub.example.com", Some("https://hub.example.com")),
            (" http://mini.local:8317/ ", Some("http://mini.local:8317")),
            ("http://192.168.1.20:8317/management.html#/usage", Some("http://192.168.1.20:8317")),
            ("https://hub.example.com/v0/management?x=1", Some("https://hub.example.com")),
            ("https://hub.example.com/cpa/v1", Some("https://hub.example.com/cpa")),
            ("ftp://hub", None),
            ("", None),
        ];
        for (input, expected) in cases {
            assert_eq!(base_url(input).as_deref(), expected, "{input}");
        }
        assert_eq!(host_of("http://mini.local:8317"), "mini.local:8317");
    }

    #[test]
    fn the_key_goes_over_http_only_to_this_machine_the_local_network_or_tailscale() {
        let allowed = [
            "http://127.0.0.1:8317", "http://localhost:8317", "http://10.0.0.5", "http://172.20.1.1", "http://169.254.1.1",
            "http://mini.local:8317", "http://[::1]:8317", "https://hub.example.com",
            "http://mini.tail2a0317.ts.net:8317", "http://MINI.TAIL2A0317.TS.NET", "http://100.64.0.0", "http://100.100.20.5:8317",
            "http://100.127.255.255", "http://[fd7a:115c:a1e0::1]:8317", "http://[fd7a:115c:a1e0:ab12::5]",
        ];
        let refused = [
            "http://hub.example.com", "http://10.0.0.1.attacker.example", "http://172.32.0.1", "http://8.8.8.8", "ftp://mini.local",
            "http://100.63.255.255", "http://100.128.0.0", "http://ts.net", "http://evilts.net", "http://mini.ts.net.attacker.example",
            "http://[2001:db8::1]:8317", "http://100.64.0.1.attacker.example",
        ];
        for url in allowed {
            assert!(allows_key(url), "{url}");
        }
        for url in refused {
            assert!(!allows_key(url), "{url}");
        }
    }
}
