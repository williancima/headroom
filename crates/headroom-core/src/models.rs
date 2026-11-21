use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Claude,
    Codex,
    Cursor,
    Amp,
    Antigravity,
    Copilot,
    Devin,
    Grok,
    Muse,
    Ollama,
    Opencode,
    Openrouter,
    Zai,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 13] = [
        Self::Claude,
        Self::Codex,
        Self::Cursor,
        Self::Amp,
        Self::Antigravity,
        Self::Copilot,
        Self::Devin,
        Self::Grok,
        Self::Muse,
        Self::Ollama,
        Self::Opencode,
        Self::Openrouter,
        Self::Zai,
    ];

    /// The raw id: `claude`, `codex`, ... `openrouter`, `zai`.
    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Amp => "amp",
            Self::Antigravity => "antigravity",
            Self::Copilot => "copilot",
            Self::Devin => "devin",
            Self::Grok => "grok",
            Self::Muse => "muse",
            Self::Ollama => "ollama",
            Self::Opencode => "opencode",
            Self::Openrouter => "openrouter",
            Self::Zai => "zai",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "ChatGPT",
            Self::Cursor => "Cursor",
            Self::Amp => "Amp",
            Self::Antigravity => "Antigravity",
            Self::Copilot => "Copilot",
            Self::Devin => "Devin",
            Self::Grok => "Grok",
            Self::Muse => "Muse Code",
            Self::Ollama => "Ollama",
            Self::Opencode => "OpenCode",
            Self::Openrouter => "OpenRouter",
            Self::Zai => "Z.ai",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: Uuid,
    pub kind: ProviderKind,
    pub label: String,
    pub added_at: DateTime<Utc>,
    /// `"key:" + hex(sha256(key))`, used to reject duplicate keys.
    #[serde(default)]
    pub identity: Option<String>,
    /// A Copilot organization account carries the login whose billing its token reads; every other
    /// account has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    /// A Copilot account signed in on GitHub Enterprise Cloud carries its GHE.com domain; a github.com
    /// account, like every other account, has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// An account mirrored from a CLIProxyAPI hub says where its login lives; every other account has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub: Option<crate::hub::HubLink>,
}

impl Account {
    pub fn new(kind: ProviderKind, label: &str) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind,
            label: label.to_string(),
            added_at: Utc::now(),
            identity: None,
            organization: None,
            domain: None,
            hub: None,
        }
    }

    /// `"{kind}@{first 8 hex of uuid}"`.
    pub fn card_id(&self) -> String {
        format!("{}@{}", self.kind.id(), &self.id.simple().to_string()[..8])
    }

    /// Display name, plus ` · label` unless the label is empty or repeats the name.
    pub fn full_name(&self) -> String {
        let name = self.kind.display_name();
        // Labels come from providers and hubs; a control character must never reach a terminal.
        let label: String = self.label.chars().map(|c| if c.is_control() { '\u{FFFD}' } else { c }).collect();
        if label.is_empty() || label.to_lowercase() == name.to_lowercase() {
            name.to_string()
        } else {
            format!("{name} · {label}")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProgressFormat {
    Percent,
    Dollars,
    Currency(String),
    Count(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricValue {
    pub number: f64,
    pub currency_code: Option<String>,
    /// A unit shown after the number ("tokens", "BYOK", "in 30 days"); only history rows set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MetricLine {
    Values {
        label: String,
        values: Vec<MetricValue>,
    },
    Progress {
        label: String,
        used: f64,
        limit: f64,
        format: ProgressFormat,
        resets_at: Option<DateTime<Utc>>,
        period_ms: Option<i64>,
    },
    Badge {
        label: String,
        text: String,
    },
}

impl MetricLine {
    pub fn label(&self) -> &str {
        match self {
            Self::Values { label, .. } | Self::Progress { label, .. } | Self::Badge { label, .. } => label,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSnapshot {
    pub plan: Option<String>,
    pub lines: Vec<MetricLine>,
    pub refreshed_at: DateTime<Utc>,
    /// Expiry of each "Rate Limit Resets" credit (Claude, ChatGPT); exported as `expiresAt`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reset_expiries: Vec<DateTime<Utc>>,
    /// One item per reset the account holds (Claude, ChatGPT, Z.ai), for the panels' Use buttons.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reset_items: Vec<crate::claim::ResetItem>,
    /// A note shown instead of metrics (a Grok team account has no personal quota); exported as `notice`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// The usage-history breakdowns, keyed by the label of the history line they open from.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub breakdowns: HashMap<String, crate::history::Breakdown>,
}

impl ProviderSnapshot {
    pub fn line(&self, label: &str) -> Option<&MetricLine> {
        self.lines.iter().find(|line| line.label() == label)
    }
}

/// ISO 8601 UTC with milliseconds, as the envelope prints dates.
pub fn iso(date: DateTime<Utc>) -> String {
    date.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub extras: HashMap<String, String>,
}

impl OAuthTokens {
    /// Extras key holding the account identity used for duplicate detection.
    pub const IDENTITY_KEY: &'static str = "account_identity";
    pub const REFRESH_MARGIN_SECS: i64 = 5 * 60;

    /// False without a refresh token or an expiry; true within the 5-minute margin.
    pub fn needs_refresh(&self, now: DateTime<Utc>) -> bool {
        match (&self.refresh_token, self.expires_at) {
            (Some(_), Some(exp)) => exp <= now + chrono::Duration::seconds(Self::REFRESH_MARGIN_SECS),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Credential {
    #[serde(rename = "oauth")]
    OAuth(OAuthTokens),
    ApiKey { key: String },
}

#[cfg(test)]
mod account_tests {
    use super::*;

    #[test]
    fn an_account_without_organization_or_domain_loads_and_round_trips_unchanged() {
        let json = serde_json::json!({
            "id": "6f1c1f0e-8a52-4a52-9f43-6a4a9a3f0b11", "kind": "claude", "label": "Work",
            "added_at": "2026-01-01T00:00:00Z", "identity": null
        });
        let account: Account = serde_json::from_value(json).unwrap();
        assert_eq!((account.organization.as_deref(), account.domain.as_deref()), (None, None));
        let out = serde_json::to_value(&account).unwrap();
        assert!(out.get("organization").is_none() && out.get("domain").is_none());
    }

    #[test]
    fn full_name_never_prints_a_control_character() {
        let account = Account::new(ProviderKind::Claude, "a\u{1b}[2Kb");
        assert_eq!(account.full_name(), "Claude · a\u{FFFD}[2Kb");
    }
}
