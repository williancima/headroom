//! Management keys, in a Secret Service collection of their own so a hub id can never load or delete an
//! account's sign-in. A key is stored with the address it was saved for and goes only there.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::provider::ProviderError;

use super::model::host_of;

pub const KEYRING_SERVICE: &str = "headroom-cliproxy-hubs";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubKey {
    pub key: String,
    pub url: String,
}

pub trait HubKeys: Send + Sync {
    fn load(&self, hub: Uuid) -> Result<Option<HubKey>, ProviderError>;
    fn save(&self, hub: Uuid, key: &HubKey) -> Result<(), ProviderError>;
    fn delete(&self, hub: Uuid) -> Result<(), ProviderError>;
}

impl dyn HubKeys + '_ {
    /// The hub's management key, only for the address it was saved for.
    pub fn key_for(&self, hub: Uuid, url: &str) -> Result<String, ProviderError> {
        let host = host_of(url);
        let Some(stored) = self.load(hub)? else {
            return Err(ProviderError::Unavailable(format!(
                "The management key for the CLIProxyAPI hub at {host} is missing. Replace it with `headroom hubs key`."
            )));
        };
        if stored.url != url {
            return Err(ProviderError::Unavailable(format!(
                "The CLIProxyAPI hub at {host} isn’t the address its management key was saved for. Remove the hub and connect it again."
            )));
        }
        Ok(stored.key)
    }
}

pub struct KeyringHubKeys;

impl KeyringHubKeys {
    fn entry(hub: Uuid) -> Result<keyring::Entry, ProviderError> {
        keyring::Entry::new(KEYRING_SERVICE, &hub.to_string()).map_err(unavailable)
    }
}

fn unavailable(error: keyring::Error) -> ProviderError {
    ProviderError::Unavailable(format!("The system keyring refused the hub's management key: {error}"))
}

impl HubKeys for KeyringHubKeys {
    fn load(&self, hub: Uuid) -> Result<Option<HubKey>, ProviderError> {
        match Self::entry(hub)?.get_password() {
            Ok(raw) => serde_json::from_str(&raw).map(Some).map_err(|_| {
                ProviderError::Unavailable("The stored management key can't be read. Replace it with `headroom hubs key`.".into())
            }),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn save(&self, hub: Uuid, key: &HubKey) -> Result<(), ProviderError> {
        let json = serde_json::to_string(key).map_err(|_| ProviderError::InvalidResponse)?;
        Self::entry(hub)?.set_password(&json).map_err(unavailable)
    }

    fn delete(&self, hub: Uuid) -> Result<(), ProviderError> {
        match Self::entry(hub)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(unavailable(e)),
        }
    }
}

/// Keys held in memory, for tests.
#[derive(Default)]
pub struct MemoryHubKeys(Mutex<HashMap<Uuid, HubKey>>);

impl HubKeys for MemoryHubKeys {
    fn load(&self, hub: Uuid) -> Result<Option<HubKey>, ProviderError> {
        Ok(self.0.lock().unwrap().get(&hub).cloned())
    }

    fn save(&self, hub: Uuid, key: &HubKey) -> Result<(), ProviderError> {
        self.0.lock().unwrap().insert(hub, key.clone());
        Ok(())
    }

    fn delete(&self, hub: Uuid) -> Result<(), ProviderError> {
        self.0.lock().unwrap().remove(&hub);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_goes_only_to_the_address_it_was_saved_for() {
        let keys = MemoryHubKeys::default();
        let keys: &dyn HubKeys = &keys;
        let hub = Uuid::new_v4();
        let saved = HubKey { key: "mgmt-key".into(), url: "http://mini.local:8317".into() };
        assert!(keys.key_for(hub, &saved.url).unwrap_err().to_string().contains("is missing"));
        keys.save(hub, &saved).unwrap();
        assert_eq!(keys.key_for(hub, "http://mini.local:8317").unwrap(), "mgmt-key");
        assert_eq!(
            keys.key_for(hub, "https://attacker.example").unwrap_err().to_string(),
            "The CLIProxyAPI hub at attacker.example isn’t the address its management key was saved for. Remove the hub and connect it again."
        );
    }
}
