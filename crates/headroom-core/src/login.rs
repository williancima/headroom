//! Adding and removing accounts, shared by the CLI and the panel. Prints nothing: progress goes through
//! [`LoginEvent`] and results come back as values.

use crate::amp::AmpClient;
use crate::antigravity::AntigravityClient;
use crate::claude::ClaudeClient;
use crate::codex::CodexClient;
use crate::copilot::{CopilotClient, GitHubDomain};
use crate::cursor::CursorClient;
use crate::devin::DevinClient;
use crate::grok::GrokClient;
use crate::hub::{self, HubKeys};
use crate::muse::MuseClient;
use crate::oauth::OAuthError;
use crate::ollama::{self, OllamaClient};
use crate::opencode::OpenCodeClient;
use crate::openrouter::{OpenRouterClient, BASE_URL};
use crate::provider::ProviderError;
use crate::refresh::forget;
use crate::store::{self, Store};
use crate::zai::ZaiClient;
use crate::{Account, Credential, OAuthTokens, ProviderKind};
use chrono::Utc;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use uuid::Uuid;

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// What the user has to do during a sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginEvent {
    /// Open this page in a browser.
    OpenUrl(String),
    /// Visit `url` and enter `code`.
    DeviceCode { code: String, url: String },
    /// Codex: its callback port is busy, so the sign-in falls back to a code.
    PortBusy { code: String, url: String },
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("Sign-in timed out.")]
    TimedOut,
    #[error("This account is already added as {0}.")]
    Duplicate(String),
    #[error("this API key is already added")]
    DuplicateKey,
    #[error("no account with id {0}")]
    UnknownAccount(String),
    #[error("{name} is managed by its CLIProxyAPI hub. Remove the hub with `headroom hubs remove {hub_id}`.")]
    HubManaged { name: String, hub_id: Uuid },
    #[error("{0} does not use browser sign-in.")]
    NotSignIn(String),
    #[error(transparent)]
    OAuth(#[from] OAuthError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// The Secret Service refused; the text says what was being done.
    #[error("{0}: {1}")]
    Keyring(&'static str, keyring::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

type Login<'a> = Pin<Box<dyn Future<Output = Result<(String, OAuthTokens), OAuthError>> + Send + 'a>>;
type Events<'a> = &'a (dyn Fn(LoginEvent) + Send + Sync);

/// A provider name as typed on the command line; `chatgpt` is the Codex provider's display name.
pub fn parse_kind(name: &str) -> Option<ProviderKind> {
    ProviderKind::parse(if name == "chatgpt" { "codex" } else { name })
}

pub fn is_oauth_kind(kind: ProviderKind) -> bool {
    matches!(
        kind,
        ProviderKind::Claude | ProviderKind::Codex | ProviderKind::Cursor | ProviderKind::Antigravity | ProviderKind::Devin | ProviderKind::Opencode | ProviderKind::Amp | ProviderKind::Grok | ProviderKind::Muse | ProviderKind::Copilot
    )
}

/// The same provider and the same identity. A sign-in without an
/// identity is never a duplicate. A re-login is rejected, not merged.
pub fn find_duplicate<'a>(accounts: &'a [Account], kind: ProviderKind, identity: Option<&str>) -> Option<&'a Account> {
    let identity = identity?;
    accounts.iter().find(|a| a.kind == kind && a.identity.as_deref() == Some(identity))
}

fn sign_in<'a>(kind: ProviderKind, domain: GitHubDomain, on: Events<'a>) -> Login<'a> {
    let open = move |u: &reqwest::Url| on(LoginEvent::OpenUrl(u.as_str().into()));
    let code = move |c: &str, u: &reqwest::Url| on(LoginEvent::DeviceCode { code: c.into(), url: u.to_string() });
    match kind {
        ProviderKind::Claude => Box::pin(async move { ClaudeClient::default().login(open).await }),
        ProviderKind::Codex => Box::pin(async move {
            let busy = move |c: &str, u: &reqwest::Url| on(LoginEvent::PortBusy { code: c.into(), url: u.to_string() });
            CodexClient::default().login(open, busy).await
        }),
        ProviderKind::Cursor => Box::pin(async move { CursorClient::default().login(open).await }),
        ProviderKind::Antigravity => Box::pin(async move { AntigravityClient::default().login(open).await }),
        ProviderKind::Devin => Box::pin(async move { DevinClient::default().login(open).await }),
        ProviderKind::Opencode => Box::pin(async move { OpenCodeClient::default().login(code).await }),
        ProviderKind::Amp => Box::pin(async move { AmpClient::default().login(code).await }),
        ProviderKind::Grok => Box::pin(async move { GrokClient::default().login(code).await }),
        ProviderKind::Muse => Box::pin(async move { MuseClient::default().login(code).await }),
        ProviderKind::Copilot => Box::pin(async move { CopilotClient::new(domain).login(code).await }),
        other => Box::pin(async move { Err(OAuthError::Failed(format!("{} does not use browser sign-in.", other.display_name()))) }),
    }
}

/// Signs in (5 minutes at most) and saves the account. `domain` only matters for Copilot; github.com saves none.
pub async fn add_oauth(
    store: &Store,
    kind: ProviderKind,
    domain: GitHubDomain,
    on_event: impl Fn(LoginEvent) + Send + Sync,
) -> Result<Account, LoginError> {
    if !is_oauth_kind(kind) {
        return Err(LoginError::NotSignIn(kind.display_name().into()));
    }
    let result = tokio::time::timeout(SIGN_IN_TIMEOUT, sign_in(kind, domain.clone(), &on_event)).await.map_err(|_| LoginError::TimedOut)?;
    let (label, tokens) = result?;
    // Read after the sign-in: the app may have synced hubs meanwhile, and saving an older list would undo that.
    let mut accounts = store.accounts()?;
    let identity = tokens.extras.get(OAuthTokens::IDENTITY_KEY).cloned();
    if let Some(existing) = find_duplicate(&accounts, kind, identity.as_deref()) {
        return Err(LoginError::Duplicate(existing.label.clone()));
    }
    let mut account = Account::new(kind, &label);
    account.identity = identity;
    account.domain = domain.saved().map(String::from).filter(|_| kind == ProviderKind::Copilot);
    store::save_credential(account.id, &Credential::OAuth(tokens))
        .map_err(|e| LoginError::Keyring("could not store the sign-in in the Secret Service", e))?;
    insert_in_order(&mut accounts, account.clone());
    store.save_accounts(&accounts)?;
    Ok(account)
}

/// Adds an organization's Copilot billing as its own account, checked with one read of the report.
/// One account per organization, compared case-insensitively.
pub async fn add_copilot_organization(store: &Store, organization: &str, token: &str) -> Result<Account, LoginError> {
    CopilotClient::new(GitHubDomain::Github).validate_organization(organization, token).await?;
    let mut accounts = store.accounts()?;
    if let Some(existing) = find_organization_duplicate(&accounts, organization) {
        return Err(LoginError::Duplicate(existing.label.clone()));
    }
    let mut account = Account::new(ProviderKind::Copilot, organization);
    account.organization = Some(organization.to_string());
    store::save_api_key(account.id, token).map_err(|e| LoginError::Keyring("could not store the token in the Secret Service", e))?;
    insert_in_order(&mut accounts, account.clone());
    store.save_accounts(&accounts)?;
    Ok(account)
}

/// AGENTS.md order: a new account goes right after its provider's last account; a provider's first goes last.
fn insert_in_order(accounts: &mut Vec<Account>, account: Account) {
    let at = accounts.iter().rposition(|a| a.kind == account.kind).map_or(accounts.len(), |i| i + 1);
    accounts.insert(at, account);
}

fn find_organization_duplicate<'a>(accounts: &'a [Account], organization: &str) -> Option<&'a Account> {
    accounts.iter().find(|a| a.kind == ProviderKind::Copilot && a.organization.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(organization)))
}

/// Adds an API-key account (`openrouter`, `ollama`, `zai`): rejects a duplicate, checks the key with one read
/// (the whole fetch for Ollama and Z.ai), and names the account.
pub async fn add_api_key(store: &Store, kind: ProviderKind, key: &str) -> Result<Account, LoginError> {
    let identity = store::key_identity(key);
    if store.accounts()?.iter().any(|a| a.identity.as_deref() == Some(&identity)) {
        return Err(LoginError::DuplicateKey);
    }
    let now = Utc::now();
    let label = match kind {
        ProviderKind::Ollama => {
            let fetched = OllamaClient::default().fetch(key, None, true, now).await?;
            let email = fetched.account_body.as_deref().and_then(|b| ollama::mapper::email(b.as_bytes()));
            email.unwrap_or_else(|| kind.display_name().into())
        }
        ProviderKind::Zai => {
            ZaiClient::default().fetch(key, None, true, now).await?;
            kind.display_name().into()
        }
        ProviderKind::Openrouter => {
            OpenRouterClient::new(BASE_URL).key(key, now).await?;
            "OpenRouter Account".into()
        }
        other => return Err(LoginError::NotSignIn(other.display_name().into())),
    };
    let mut account = Account::new(kind, &label);
    account.identity = Some(identity);
    let mut accounts = store.accounts()?;
    store::save_api_key(account.id, key).map_err(|e| LoginError::Keyring("could not store the key in the Secret Service", e))?;
    insert_in_order(&mut accounts, account.clone());
    store.save_accounts(&accounts)?;
    Ok(account)
}

/// Removes an account, its credential, snapshot and cadence state. Hub accounts leave with their hub.
pub fn remove_account(store: &Store, card_id: &str) -> Result<Account, LoginError> {
    remove_with(store, card_id, store::delete_credential)
}

fn remove_with(store: &Store, card_id: &str, delete: impl Fn(Uuid) -> keyring::Result<()>) -> Result<Account, LoginError> {
    let mut accounts = store.accounts()?;
    let index = accounts.iter().position(|a| a.card_id() == card_id).ok_or_else(|| LoginError::UnknownAccount(card_id.into()))?;
    if let Some(link) = &accounts[index].hub {
        return Err(LoginError::HubManaged { name: accounts[index].full_name(), hub_id: link.hub_id });
    }
    let account = accounts.remove(index);
    // A credential that is already gone is fine.
    match delete(account.id) {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(e) => return Err(LoginError::Keyring("could not delete the credential from the Secret Service", e)),
    }
    store.save_accounts(&accounts)?;
    forget(store, &[account.card_id()])?;
    Ok(account)
}

/// Removes a hub with its accounts and key. Returns the card ids of the accounts that left.
pub fn remove_hub(store: &Store, keys: &dyn HubKeys, hub_id: Uuid) -> Result<Vec<String>, LoginError> {
    let gone: Vec<String> =
        store.accounts()?.iter().filter(|a| a.hub.as_ref().is_some_and(|l| l.hub_id == hub_id)).map(|a| a.card_id()).collect();
    hub::remove(store, keys, hub_id)?;
    forget(store, &gone)?;
    Ok(gone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::{Hub, HubLink};
    use crate::ProviderSnapshot;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn account(kind: ProviderKind, label: &str, identity: Option<&str>) -> Account {
        let mut a = Account::new(kind, label);
        a.identity = identity.map(String::from);
        a
    }

    fn temp_store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("headroom-test-{}", Uuid::new_v4()));
        (Store::at(&dir.join("config"), &dir.join("state")), dir)
    }

    fn snapshot() -> ProviderSnapshot {
        ProviderSnapshot { plan: None, lines: vec![], refreshed_at: Utc::now(), reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() }
    }

    #[test]
    fn a_new_account_goes_after_its_providers_accounts_and_a_new_provider_goes_last() {
        let mut accounts = vec![account(ProviderKind::Openrouter, "A", None), account(ProviderKind::Claude, "B", None)];
        insert_in_order(&mut accounts, account(ProviderKind::Openrouter, "C", None));
        insert_in_order(&mut accounts, account(ProviderKind::Amp, "D", None));
        let labels: Vec<&str> = accounts.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(labels, ["A", "C", "B", "D"]);
    }

    #[test]
    fn duplicates_need_the_same_provider_and_identity() {
        let accounts = [account(ProviderKind::Claude, "Work", Some("acct/org"))];
        assert_eq!(find_duplicate(&accounts, ProviderKind::Claude, Some("acct/org")).unwrap().label, "Work");
        assert!(find_duplicate(&accounts, ProviderKind::Claude, Some("acct/other-org")).is_none());
        assert!(find_duplicate(&accounts, ProviderKind::Codex, Some("acct/org")).is_none());
    }

    #[test]
    fn a_sign_in_without_an_identity_is_always_added() {
        let accounts = [account(ProviderKind::Claude, "Old", None)];
        assert!(find_duplicate(&accounts, ProviderKind::Claude, None).is_none());
    }

    #[test]
    fn an_organization_is_a_duplicate_ignoring_case_and_only_among_copilot_organizations() {
        let mut org = Account::new(ProviderKind::Copilot, "Acme");
        org.organization = Some("Acme".into());
        let personal = Account::new(ProviderKind::Copilot, "robin");
        let accounts = [personal, org];
        assert_eq!(find_organization_duplicate(&accounts, "acme").unwrap().label, "Acme");
        assert!(find_organization_duplicate(&accounts, "beta").is_none());
        assert!(find_organization_duplicate(&accounts[..1], "acme").is_none());
    }

    #[test]
    fn chatgpt_is_the_codex_provider() {
        assert_eq!(parse_kind("chatgpt"), Some(ProviderKind::Codex));
        assert_eq!(parse_kind("claude"), Some(ProviderKind::Claude));
    }

    #[test]
    fn the_sign_in_future_is_send() {
        fn is_send<T: Send>(_: &T) {}
        let store = Store::at(&PathBuf::from("/nonexistent/c"), &PathBuf::from("/nonexistent/s"));
        is_send(&add_oauth(&store, ProviderKind::Codex, GitHubDomain::Github, |_| {}));
        is_send(&add_copilot_organization(&store, "acme", "t"));
        is_send(&add_api_key(&store, ProviderKind::Zai, "k"));
    }

    #[tokio::test]
    async fn an_api_key_already_added_is_refused_before_any_network_call() {
        let (store, dir) = temp_store();
        store.save_accounts(&[account(ProviderKind::Zai, "Z.ai", Some(&store::key_identity("k1")))]).unwrap();
        let error = add_api_key(&store, ProviderKind::Zai, "k1").await.unwrap_err();
        assert!(matches!(error, LoginError::DuplicateKey));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_provider_without_sign_in_is_refused() {
        let (store, dir) = temp_store();
        let error = add_oauth(&store, ProviderKind::Zai, GitHubDomain::Github, |_| {}).await.unwrap_err();
        assert!(matches!(error, LoginError::NotSignIn(_)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn removing_an_account_drops_its_snapshot_and_state() {
        let (store, dir) = temp_store();
        let (keep, gone) = (account(ProviderKind::Zai, "Keep", None), account(ProviderKind::Zai, "Gone", None));
        store.save_accounts(&[keep.clone(), gone.clone()]).unwrap();
        let cards = [keep.card_id(), gone.card_id()];
        store.save_snapshots(&cards.iter().map(|id| (id.clone(), snapshot())).collect()).unwrap();
        store.save_provider_state(&cards.iter().map(|id| (id.clone(), Default::default())).collect::<HashMap<_, _>>()).unwrap();

        let removed = remove_with(&store, &gone.card_id(), |id| {
            assert_eq!(id, gone.id);
            Err(keyring::Error::NoEntry)
        })
        .unwrap();
        assert_eq!(removed.id, gone.id);
        assert_eq!(store.accounts().unwrap(), vec![keep.clone()]);
        assert_eq!(store.snapshots().unwrap().keys().collect::<Vec<_>>(), vec![&keep.card_id()]);
        assert_eq!(store.provider_state().unwrap().keys().collect::<Vec<_>>(), vec![&keep.card_id()]);
        assert!(matches!(remove_with(&store, "nope@00000000", |_| Ok(())), Err(LoginError::UnknownAccount(_))));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_hub_account_is_not_removed_alone() {
        let (store, dir) = temp_store();
        let hub = Hub::new("https://a.example".into());
        let mut mirrored = account(ProviderKind::Claude, "Hub login", None);
        mirrored.hub = Some(HubLink {
            hub_id: hub.id,
            hub_url: hub.url.clone(),
            login_id: "l".into(),
            auth_index: "0".into(),
            chatgpt_account_id: None,
        });
        store.save_accounts(&[mirrored.clone()]).unwrap();
        let error = remove_with(&store, &mirrored.card_id(), |_| panic!("must not touch the credential")).unwrap_err();
        assert!(matches!(error, LoginError::HubManaged { hub_id, .. } if hub_id == hub.id));
        assert_eq!(store.accounts().unwrap().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
