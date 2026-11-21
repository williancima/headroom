//! One entry of the hub's `GET /v0/management/auth-files` listing, reduced to what Headroom reads.

use serde::Deserialize;

use crate::models::ProviderKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubLogin {
    pub id: String,
    /// `None` when the hub listed the login without one; such a login can't be called.
    pub auth_index: Option<String>,
    pub provider: String,
    pub label: String,
    pub is_oauth: bool,
    pub is_disabled: bool,
    pub chatgpt_account_id: Option<String>,
}

impl HubLogin {
    /// The provider a login is mirrored as, or `None` for a login Headroom doesn't support yet. Cursor's
    /// hub login holds an API key, not a session, so it is not mirrored.
    pub fn kind(&self) -> Option<ProviderKind> {
        if !self.is_oauth {
            return None;
        }
        match self.provider.as_str() {
            "claude" => Some(ProviderKind::Claude),
            "codex" => Some(ProviderKind::Codex),
            "antigravity" => Some(ProviderKind::Antigravity),
            "devin" => Some(ProviderKind::Devin),
            "xai" => Some(ProviderKind::Grok),
            _ => None,
        }
    }

    /// An enabled login Headroom can't mirror. A file the hub couldn't read lists as provider `unknown`
    /// and isn't a login at all.
    pub fn is_unsupported(&self) -> bool {
        !self.is_disabled && self.kind().is_none() && self.provider != "unknown"
    }

    /// The listing's logins in hub order. An entry without an id is skipped; an undecodable body is `None`.
    pub fn listing(body: &[u8]) -> Option<Vec<Self>> {
        let listing: Listing = serde_json::from_slice(body).ok()?;
        Some(listing.files.into_iter().filter_map(Entry::into_login).collect())
    }
}

#[derive(Deserialize)]
struct Listing {
    files: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: Option<String>,
    auth_index: Option<String>,
    provider: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    email: Option<String>,
    account: Option<String>,
    label: Option<String>,
    name: Option<String>,
    account_type: Option<String>,
    disabled: Option<bool>,
    id_token: Option<IdToken>,
}

#[derive(Deserialize)]
struct IdToken {
    chatgpt_account_id: Option<String>,
}

impl Entry {
    fn into_login(self) -> Option<HubLogin> {
        let id = self.id.filter(|id| !id.is_empty())?;
        // The hub is a separate server: drop control characters so its strings can't drive a terminal.
        let clean = |v: &str| v.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string();
        let label = [&self.email, &self.account, &self.label, &self.name]
            .into_iter()
            .filter_map(|v| v.as_deref().map(clean))
            .find(|v| !v.is_empty())
            .unwrap_or_else(|| clean(&id));
        Some(HubLogin {
            auth_index: self.auth_index.filter(|i| !i.is_empty()),
            provider: self.provider.or(self.kind).unwrap_or_default().to_lowercase(),
            label,
            is_oauth: self.account_type.unwrap_or_else(|| "oauth".into()).to_lowercase() == "oauth",
            is_disabled: self.disabled.unwrap_or(false),
            chatgpt_account_id: self.id_token.and_then(|t| t.chatgpt_account_id),
            id,
        })
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::HubLogin;

    /// `GET /v0/management/auth-files` as CLIProxyAPI v8.0.47 answered it, with dummy logins.
    pub const RECORDED_LISTING: &str = r#"{"files":[
    {"account":"alice@example.com","account_type":"oauth","auth_index":"b799da203df01af5","disabled":false,"email":"alice@example.com","id":"claude-alice@example.com.json","label":"alice@example.com","name":"claude-alice@example.com.json","provider":"claude","status":"active","status_message":"","type":"claude","unavailable":false},
    {"account_type":"oauth","auth_index":"b9e9b8780e7ae25b","disabled":false,"id":"claude-apikey-file.json","label":"claude","name":"claude-apikey-file.json","provider":"claude","status":"active","status_message":"","type":"claude","unavailable":false},
    {"account":"disabled@example.com","account_type":"oauth","auth_index":"c62b935696257364","disabled":true,"email":"disabled@example.com","id":"claude-disabled@example.com.json","label":"disabled@example.com","name":"claude-disabled@example.com.json","provider":"claude","status":"disabled","status_message":"","type":"claude","unavailable":false},
    {"account":"notoken@example.com","account_type":"oauth","auth_index":"4af9debc50b6ca56","disabled":false,"email":"notoken@example.com","id":"claude-notoken@example.com.json","label":"notoken@example.com","name":"claude-notoken@example.com.json","provider":"claude","status":"active","status_message":"","type":"claude","unavailable":false},
    {"account":"codex-user@example.com","account_type":"oauth","auth_index":"2eff6fcd14d4e964","disabled":false,"email":"codex-user@example.com","id":"codex-codex-user@example.com-plus.json","id_token":{"chatgpt_account_id":"acct-dummy-0001","plan_type":"plus"},"label":"codex-user@example.com","name":"codex-codex-user@example.com-plus.json","provider":"codex","status":"active","status_message":"","type":"codex","unavailable":false},
    {"account":"kim@example.com","account_type":"oauth","auth_index":"e1c935aa6b063328","disabled":false,"email":"kim@example.com","id":"kimi-kim@example.com.json","label":"kim@example.com","name":"kimi-kim@example.com.json","provider":"kimi","status":"active","status_message":"","type":"kimi","unavailable":false}
    ]}"#;

    pub fn login(id: &str, provider: &str, index: Option<&str>, disabled: bool) -> HubLogin {
        HubLogin {
            id: id.into(),
            auth_index: Some(index.map_or_else(|| format!("index-{id}"), String::from)),
            provider: provider.into(),
            label: id.into(),
            is_oauth: true,
            is_disabled: disabled,
            chatgpt_account_id: (provider == "codex").then(|| format!("acct-{id}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hub_provider_with_a_runtime_is_mirrored_and_cursor_is_not() {
        let providers = ["antigravity", "devin", "xai", "cursor", "gemini"];
        let files: Vec<String> =
            providers.iter().map(|p| format!(r#"{{"id":"{p}","auth_index":"{p}-index","provider":"{p}","account_type":"oauth"}}"#)).collect();
        let logins = HubLogin::listing(format!(r#"{{"files":[{}]}}"#, files.join(",")).as_bytes()).unwrap();
        let kinds: Vec<_> = logins.iter().map(HubLogin::kind).collect();
        assert_eq!(kinds, [Some(ProviderKind::Antigravity), Some(ProviderKind::Devin), Some(ProviderKind::Grok), None, None]);
    }

    #[test]
    fn an_api_key_entry_or_an_uncallable_entry_reads_as_such_and_html_is_unreadable() {
        let body = br#"{"files":[{"id":"a","auth_index":"1","provider":"claude","account_type":"api_key"},{"id":"b","provider":"codex"},{"id":"c","auth_index":"3","provider":"codex"},{"provider":"codex"}]}"#;
        let logins = HubLogin::listing(body).unwrap();
        assert_eq!(logins.iter().map(HubLogin::kind).collect::<Vec<_>>(), [None, Some(ProviderKind::Codex), Some(ProviderKind::Codex)]);
        assert_eq!(logins[1].auth_index, None);
        assert!(HubLogin::listing(b"<html>").is_none());
    }

    #[test]
    fn the_recorded_listing_reads_labels_disabled_and_unsupported() {
        let logins = HubLogin::listing(fixtures::RECORDED_LISTING.as_bytes()).unwrap();
        let labels: Vec<_> = logins.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(labels, ["alice@example.com", "claude", "disabled@example.com", "notoken@example.com", "codex-user@example.com", "kim@example.com"]);
        assert_eq!(logins[4].chatgpt_account_id.as_deref(), Some("acct-dummy-0001"));
        assert_eq!(logins.iter().filter(|l| l.is_unsupported()).count(), 1, "kimi counts, the disabled login does not");
    }

    #[test]
    fn control_characters_in_a_hub_label_are_dropped() {
        let body = br#"{"files":[{"id":"c.json","provider":"claude","email":"a\u001b]0;x\u0007\u001b[2K\rb"}]}"#;
        let logins = HubLogin::listing(body).unwrap();
        assert_eq!(logins[0].label, "a]0;x[2Kb");
    }

    #[test]
    fn an_unreadable_file_is_not_a_login_to_count() {
        let logins = HubLogin::listing(br#"{"files":[{"id":"x","auth_index":"1","provider":"unknown"},{"id":"catalog.json","provider":"local-models","account_type":"oauth"}]}"#).unwrap();
        assert_eq!(logins.iter().filter(|l| l.is_unsupported()).count(), 1);
    }
}
