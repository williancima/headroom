//! Keeps a wrong management key to one strike per hub. CLIProxyAPI bans an IP for 30 minutes after five
//! wrong keys, so when the hub rejects the key, or has banned this machine, nothing more goes to that hub
//! with that key. The CLI is one process per run, so the refusal is saved on the hub's record. It holds only
//! the key it was sent with (by hash), so replacing the key opens the gate and a late answer to the old key
//! never stops the new one.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::store::{key_identity, Store};

use super::client::HubRefusal;
use super::model::Hub;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    /// `key_identity` of the refused key, never the key.
    pub key: String,
    pub refusal: HubRefusal,
}

/// The refusal that still holds for `key` on this hub; `None` for a hub not saved yet.
pub(super) fn refusal_of(store: &Store, hub: Uuid, key: &str) -> Option<HubRefusal> {
    let hubs = store.hubs().ok()?;
    let refused = hubs.into_iter().find(|h| h.id == hub)?.refused?;
    (refused.key == key_identity(key)).then_some(refused.refusal)
}

pub(super) fn close(store: &Store, hub: Uuid, key: &str, refusal: HubRefusal) {
    set(store, hub, Some(Refused { key: key_identity(key), refusal }));
}

/// Lets the hub be called again: its key was replaced.
pub fn reopen(store: &Store, hub: Uuid) {
    set(store, hub, None);
}

fn set(store: &Store, hub: Uuid, refused: Option<Refused>) {
    let Ok(mut hubs) = store.hubs() else { return };
    update(&mut hubs, hub, refused);
    let _ = store.save_hubs(&hubs);
}

fn update(hubs: &mut [Hub], hub: Uuid, refused: Option<Refused>) {
    if let Some(h) = hubs.iter_mut().find(|h| h.id == hub) {
        h.refused = refused;
    }
}
