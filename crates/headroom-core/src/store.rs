//! Accounts (`$XDG_CONFIG_HOME/headroom/accounts.json`), the layout (`layout.json`: hidden and pinned metrics,
//! metric order, unified providers), the last snapshots
//! (`$XDG_STATE_HOME/headroom/snapshots.json`) and API keys (Secret Service, keyed by account UUID).

use crate::hub::Hub;
use crate::layout::Layout;
use crate::priority::BurningFast;
use crate::models::{Account, Credential, ProviderSnapshot};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const KEYRING_SERVICE: &str = "headroom";

#[derive(Clone)]
pub struct Store {
    accounts: PathBuf,
    layout: PathBuf,
    hubs: PathBuf,
    snapshots: PathBuf,
    provider_state: PathBuf,
    burning_fast: PathBuf,
    settings: PathBuf,
}

/// Secondary-read cadence of one account: when the hourly read last ran and the body it returned
/// (Codex credit list, Antigravity `loadCodeAssist`). Provider JSON, not a credential.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderState {
    #[serde(default)]
    pub last_secondary_read: Option<DateTime<Utc>>,
    #[serde(default)]
    pub cached_body: Option<String>,
    /// The wait after failed refreshes; absent in files written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff: Option<crate::backoff::Backoff>,
    /// The usage-history days already read; absent in files written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<crate::history::Ledger>,
    /// ChatGPT's workspace-credit days, which have a ledger of their own beside the token counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits_history: Option<crate::history::Ledger>,
}

impl Store {
    pub fn at(config_dir: &Path, state_dir: &Path) -> Self {
        Self { accounts: config_dir.join("accounts.json"), layout: config_dir.join("layout.json"), hubs: config_dir.join("hubs.json"), snapshots: state_dir.join("snapshots.json"), provider_state: state_dir.join("provider_state.json"), burning_fast: state_dir.join("burning_fast.json"), settings: config_dir.join("settings.json") }
    }

    pub fn default_paths() -> std::io::Result<Self> {
        let missing = || std::io::Error::other("no config or state directory for this user");
        let config = dirs::config_dir().ok_or_else(missing)?;
        let state = dirs::state_dir().or_else(dirs::data_local_dir).ok_or_else(missing)?;
        Ok(Self::at(&config.join("headroom"), &state.join("headroom")))
    }

    pub fn accounts(&self) -> std::io::Result<Vec<Account>> {
        read_json(&self.accounts)
    }

    pub fn save_accounts(&self, accounts: &[Account]) -> std::io::Result<()> {
        write_json(&self.accounts, &accounts)
    }

    /// The user's customization; a missing file is the default layout.
    pub fn layout(&self) -> std::io::Result<Layout> {
        read_json(&self.layout)
    }

    pub fn save_layout(&self, layout: &Layout) -> std::io::Result<()> {
        write_json(&self.layout, layout)
    }

    /// The connected CLIProxyAPI hubs (without their management keys).
    pub fn hubs(&self) -> std::io::Result<Vec<Hub>> {
        read_json(&self.hubs)
    }

    pub fn save_hubs(&self, hubs: &[Hub]) -> std::io::Result<()> {
        write_json(&self.hubs, &hubs)
    }

    /// Last snapshots keyed by account card id.
    pub fn snapshots(&self) -> std::io::Result<HashMap<String, ProviderSnapshot>> {
        read_json(&self.snapshots)
    }

    pub fn save_snapshots(&self, snapshots: &HashMap<String, ProviderSnapshot>) -> std::io::Result<()> {
        write_json(&self.snapshots, snapshots)
    }

    /// Cadence state keyed by account card id.
    pub fn provider_state(&self) -> std::io::Result<HashMap<String, ProviderState>> {
        read_json(&self.provider_state)
    }

    pub fn save_provider_state(&self, state: &HashMap<String, ProviderState>) -> std::io::Result<()> {
        write_json(&self.provider_state, state)
    }

    /// The user's settings, validated; a missing file is the defaults.
    pub fn settings(&self) -> std::io::Result<crate::settings::Settings> {
        read_json::<crate::settings::Settings>(&self.settings).map(|s| s.validated())
    }

    pub fn save_settings(&self, settings: &crate::settings::Settings) -> std::io::Result<()> {
        write_json(&self.settings, settings)
    }

    pub fn burning_fast(&self) -> std::io::Result<BurningFast> {
        read_json(&self.burning_fast)
    }

    pub fn save_burning_fast(&self, latch: &BurningFast) -> std::io::Result<()> {
        write_json(&self.burning_fast, latch)
    }
}

/// `"key:" + hex(sha256(key))`.
pub fn key_identity(key: &str) -> String {
    let hex: String = Sha256::digest(key.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    format!("key:{hex}")
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> std::io::Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e),
    }
}

fn write_json<T: serde::Serialize + ?Sized>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, path)
}

fn entry(account: Uuid) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, &account.to_string())
}

/// A stored value that is not credential JSON is a phase 1 plain API key.
pub fn parse_credential(raw: &str) -> Credential {
    serde_json::from_str(raw).unwrap_or_else(|_| Credential::ApiKey { key: raw.to_string() })
}

pub fn save_credential(account: Uuid, credential: &Credential) -> keyring::Result<()> {
    let json = serde_json::to_string(credential).map_err(|e| keyring::Error::BadEncoding(e.to_string().into_bytes()))?;
    entry(account)?.set_password(&json)
}

pub fn load_credential(account: Uuid) -> keyring::Result<Credential> {
    entry(account)?.get_password().map(|raw| parse_credential(&raw))
}

pub fn save_api_key(account: Uuid, key: &str) -> keyring::Result<()> {
    save_credential(account, &Credential::ApiKey { key: key.to_string() })
}

/// The API key of an API-key account; an OAuth account has none (`NoEntry`).
pub fn load_api_key(account: Uuid) -> keyring::Result<String> {
    match load_credential(account)? {
        Credential::ApiKey { key } => Ok(key),
        Credential::OAuth(_) => Err(keyring::Error::NoEntry),
    }
}

pub fn delete_credential(account: Uuid) -> keyring::Result<()> {
    entry(account)?.delete_credential()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ProviderKind;

    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!("headroom-test-{}", Uuid::new_v4()))
    }

    #[test]
    fn missing_files_read_empty_and_round_trip() {
        let dir = temp();
        let store = Store::at(&dir.join("config"), &dir.join("state"));
        assert!(store.accounts().unwrap().is_empty());
        assert!(store.snapshots().unwrap().is_empty());

        let mut account = Account::new(ProviderKind::Openrouter, "OpenRouter Account");
        account.identity = Some(key_identity("k"));
        store.save_accounts(&[account.clone()]).unwrap();
        assert_eq!(store.accounts().unwrap(), vec![account.clone()]);

        let snapshot = ProviderSnapshot { plan: None, lines: vec![], refreshed_at: chrono::Utc::now(), reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() };
        let cache = HashMap::from([(account.card_id(), snapshot)]);
        store.save_snapshots(&cache).unwrap();
        assert_eq!(store.snapshots().unwrap(), cache);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn provider_state_reads_empty_and_round_trips() {
        let dir = temp();
        let store = Store::at(&dir.join("config"), &dir.join("state"));
        assert!(store.provider_state().unwrap().is_empty());
        let state = HashMap::from([(
            "codex@1a2b3c4d".to_string(),
            ProviderState { last_secondary_read: Some("2026-11-19T12:00:00Z".parse().unwrap()), cached_body: Some("{}".into()), backoff: None, history: None, credits_history: None },
        )]);
        store.save_provider_state(&state).unwrap();
        assert_eq!(store.provider_state().unwrap(), state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn burning_fast_reads_empty_and_round_trips() {
        let dir = temp();
        let store = Store::at(&dir.join("config"), &dir.join("state"));
        assert_eq!(store.burning_fast().unwrap(), BurningFast::default());
        let latch = BurningFast { latched: HashMap::from([("claude@1a2b3c4d:Weekly".to_string(), "2026-11-19T12:00:00Z".parse().unwrap())]) };
        store.save_burning_fast(&latch).unwrap();
        assert_eq!(store.burning_fast().unwrap(), latch);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn provider_state_without_a_backoff_still_loads() {
        let old: ProviderState = serde_json::from_str(r#"{"last_secondary_read":"2026-11-19T12:00:00Z","cached_body":"{}"}"#).unwrap();
        assert_eq!(old.backoff, None);
        let empty: ProviderState = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, ProviderState::default());
    }

    #[test]
    fn identity_is_prefixed_sha256_hex() {
        assert_eq!(
            key_identity("abc"),
            "key:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn card_id_uses_the_first_eight_hex_digits() {
        let mut account = Account::new(ProviderKind::Openrouter, "x");
        account.id = "1a2b3c4d-5e6f-4000-8000-000000000000".parse().unwrap();
        assert_eq!(account.card_id(), "openrouter@1a2b3c4d");
    }

    #[test]
    fn credential_json_round_trips_and_legacy_strings_load_as_keys() {
        use crate::models::OAuthTokens;
        let tokens = OAuthTokens {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: Some("2026-11-19T12:00:00Z".parse().unwrap()),
            extras: HashMap::from([("account_identity".into(), "id".into())]),
        };
        let oauth = Credential::OAuth(tokens);
        let json = serde_json::to_string(&oauth).unwrap();
        assert!(json.contains(r#""type":"oauth""#) && json.contains(r#""accessToken":"a""#) && json.contains(r#""expiresAt""#), "{json}");
        assert_eq!(parse_credential(&json), oauth);

        let key = Credential::ApiKey { key: "sk-1".into() };
        assert_eq!(serde_json::to_string(&key).unwrap(), r#"{"type":"apiKey","key":"sk-1"}"#);
        assert_eq!(parse_credential(&serde_json::to_string(&key).unwrap()), key);
        assert_eq!(parse_credential("sk-or-v1-abc"), Credential::ApiKey { key: "sk-or-v1-abc".into() });
        assert_eq!(parse_credential("12345"), Credential::ApiKey { key: "12345".into() });
    }

    #[test]
    fn needs_refresh_uses_a_five_minute_margin() {
        use crate::models::OAuthTokens;
        let now = chrono::Utc::now();
        let t = |refresh: Option<&str>, exp: Option<i64>| OAuthTokens {
            access_token: "a".into(),
            refresh_token: refresh.map(Into::into),
            expires_at: exp.map(|s| now + chrono::Duration::seconds(s)),
            extras: HashMap::new(),
        };
        assert!(t(Some("r"), Some(120)).needs_refresh(now));
        assert!(!t(Some("r"), Some(3600)).needs_refresh(now));
        assert!(!t(None, Some(1)).needs_refresh(now));
        assert!(!t(Some("r"), None).needs_refresh(now));
    }
}
