//! Connecting, re-keying, syncing and removing a hub. The caller (the CLI) does the prompting and the printing.

use uuid::Uuid;

use crate::models::Account;
use crate::provider::ProviderError;
use crate::store::Store;

use super::client::HubClient;
use super::gate;
use super::keys::{HubKey, HubKeys};
use super::login::HubLogin;
use super::mirror::mirror;
use super::model::{base_url, host_of, Hub};

/// What a sync found that Headroom can't mirror (enabled logins of providers it doesn't support yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Synced {
    pub unsupported: usize,
}

fn failed(error: impl std::fmt::Display) -> ProviderError {
    ProviderError::Unavailable(error.to_string())
}

/// Connects a hub once its login list answers to the key, so a wrong address or key saves nothing. Then
/// mirrors its logins.
pub async fn connect(
    client: &HubClient,
    store: &Store,
    keys: &dyn HubKeys,
    input: &str,
    key: &str,
) -> Result<(Hub, Synced), ProviderError> {
    let Some(url) = base_url(input) else {
        return Err(ProviderError::Unavailable("Enter the hub’s address, like https://hub.example.com or http://192.168.1.20:8317.".into()));
    };
    let key = key.trim();
    if key.is_empty() {
        return Err(ProviderError::Unavailable("Enter the hub’s management key.".into()));
    }
    let mut hubs = store.hubs().map_err(failed)?;
    if hubs.iter().any(|h| h.url == url) {
        return Err(ProviderError::Unavailable(format!("The hub at {} is already connected.", host_of(&url))));
    }
    let hub = Hub::new(url.clone());
    let logins = match client.logins(hub.id, &url, key).await {
        Ok(logins) => logins,
        Err(ProviderError::Unreachable(message)) => {
            return Err(match client.plain_http_address(&url).await {
                Some(plain) => ProviderError::Unavailable(format!("This hub doesn’t use https. Try {plain}.")),
                None => ProviderError::Unreachable(message),
            });
        }
        Err(e) => return Err(e),
    };
    keys.save(hub.id, &HubKey { key: key.to_string(), url })?;
    hubs.push(hub.clone());
    if let Err(e) = store.save_hubs(&hubs) {
        let _ = keys.delete(hub.id);
        return Err(failed(e));
    }
    let synced = record(store, &hub, &logins)?;
    Ok((hub, synced))
}

/// Saves a new management key only once the hub accepts it (a refused key keeps the old one), then lets
/// the hub be called again and mirrors its logins.
pub async fn replace_key(client: &HubClient, store: &Store, keys: &dyn HubKeys, hub: Uuid, key: &str) -> Result<Synced, ProviderError> {
    let hub = store.hubs().map_err(failed)?.into_iter().find(|h| h.id == hub).ok_or_else(|| failed("That hub isn't connected."))?;
    let key = key.trim();
    if key.is_empty() {
        return Err(ProviderError::Unavailable("Enter the hub’s management key.".into()));
    }
    // A throwaway id keeps the old key's refusal from blocking the test of the new one.
    let logins = client.logins(Uuid::new_v4(), &hub.url, key).await?;
    keys.save(hub.id, &HubKey { key: key.to_string(), url: hub.url.clone() })?;
    gate::reopen(store, hub.id);
    record(store, &hub, &logins)
}

/// Mirrors a hub's login list onto the saved accounts. Returns what the hub lists that isn't mirrored.
pub fn record(store: &Store, hub: &Hub, logins: &[HubLogin]) -> Result<Synced, ProviderError> {
    let accounts: Vec<Account> = store.accounts().map_err(failed)?;
    let next = mirror(&accounts, hub, logins);
    if next != accounts {
        store.save_accounts(&next).map_err(failed)?;
    }
    Ok(Synced { unsupported: logins.iter().filter(|l| l.is_unsupported()).count() })
}

/// Reads the hub's login list with its saved key and mirrors it.
pub async fn sync(client: &HubClient, store: &Store, keys: &dyn HubKeys, hub: &Hub) -> Result<Synced, ProviderError> {
    let key = keys.key_for(hub.id, &hub.url)?;
    let logins = client.logins(hub.id, &hub.url, &key).await?;
    record(store, hub, &logins)
}

/// Removes a hub with every account mirrored from it and its management key. Nothing changes on the hub.
/// The key goes first: if the keyring refuses, nothing else is touched.
pub fn remove(store: &Store, keys: &dyn HubKeys, hub: Uuid) -> Result<(), ProviderError> {
    keys.delete(hub)?;
    let mut accounts = store.accounts().map_err(failed)?;
    accounts.retain(|a| a.hub.as_ref().is_none_or(|l| l.hub_id != hub));
    store.save_accounts(&accounts).map_err(failed)?;
    let mut hubs = store.hubs().map_err(failed)?;
    hubs.retain(|h| h.id != hub);
    store.save_hubs(&hubs).map_err(failed)
}
