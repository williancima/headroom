//! `headroom hubs`: CLIProxyAPI hubs. The hub owns its list of logins; Headroom mirrors it as accounts.

use anyhow::{bail, Result};
use clap::Subcommand;
use headroom_core::hub::{self, Hub};
use headroom_core::refresh::{self, Hubs};
use headroom_core::store::Store;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum HubsCommand {
    /// Connect a hub, such as `hub.example.com:8317`. The management key comes from a hidden prompt, or from
    /// stdin when piped. It is tested before anything is saved.
    Add {
        address: String,
    },
    List,
    /// Replace a hub's management key (tested before it is saved; a refused key keeps the old one).
    Key {
        /// The hub id from `hubs list`, or a unique prefix of it.
        id: String,
    },
    /// Remove a hub with its accounts and key from this machine. Nothing changes on the hub.
    Remove {
        id: String,
    },
}

/// Reads the hubs' login lists and prints one line for each hub that failed. Returns the card ids that left.
pub async fn sync(hubs: &Hubs, only: Option<&[Uuid]>) -> Result<Vec<String>> {
    let (gone, warnings) = refresh::sync_hubs(hubs, only).await?;
    for warning in warnings {
        eprintln!("headroom: {warning}");
    }
    Ok(gone)
}

/// A hub from its full id or a unique prefix of it.
fn find(hubs: &[Hub], id: &str) -> Result<Hub> {
    let id = id.trim().to_lowercase();
    let mut found = hubs.iter().filter(|h| !id.is_empty() && h.id.to_string().starts_with(&id));
    match (found.next(), found.next()) {
        (Some(hub), None) => Ok(hub.clone()),
        (Some(_), Some(_)) => bail!("`{id}` matches more than one hub; use more of the id"),
        _ => bail!("no hub with id {id}"),
    }
}

pub use headroom_core::refresh::forget;

pub async fn run(store: &Store, command: HubsCommand) -> Result<u8> {
    let hubs = Hubs::new(store);
    match command {
        HubsCommand::Add { address } => {
            let key = crate::read_key("CLIProxyAPI management key")?;
            let (hub, synced) = hub::connect(&hubs.client, store, &hubs.keys, &address, &key).await?;
            let added: Vec<String> =
                store.accounts()?.iter().filter(|a| a.hub.as_ref().is_some_and(|l| l.hub_id == hub.id)).map(|a| a.card_id()).collect();
            println!("Connected {} ({})", hub.host(), hub.id);
            println!("{} accounts added: {}", added.len(), added.join(", "));
            if synced.unsupported > 0 {
                println!("{} logins on the hub are not supported yet", synced.unsupported);
            }
        }
        HubsCommand::List => {
            for hub in store.hubs()? {
                let marker = if hub.refused.is_some() { "\tblocked: replace the key with `headroom hubs key`" } else { "" };
                println!("{}\t{}{marker}", hub.id, hub.host());
            }
        }
        HubsCommand::Key { id } => {
            let hub = find(&store.hubs()?, &id)?;
            let key = crate::read_key("CLIProxyAPI management key")?;
            let synced = hub::replace_key(&hubs.client, store, &hubs.keys, hub.id, &key).await?;
            println!("Replaced the management key of {}", hub.host());
            if synced.unsupported > 0 {
                println!("{} logins on the hub are not supported yet", synced.unsupported);
            }
        }
        HubsCommand::Remove { id } => {
            let hub = find(&store.hubs()?, &id)?;
            let gone = headroom_core::login::remove_hub(store, &hubs.keys, hub.id)?;
            println!("Removed {} and its {} accounts", hub.host(), gone.len());
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hub_is_found_by_its_id_or_a_unique_prefix() {
        let (one, two) = (Hub::new("https://a.example".into()), Hub::new("https://b.example".into()));
        let list = [one.clone(), two];
        assert_eq!(find(&list, &one.id.to_string()).unwrap().id, one.id);
        assert_eq!(find(&list, &one.id.to_string()[..12]).unwrap().id, one.id);
        assert!(find(&list, "").is_err() && find(&list, "zzzz").is_err());
    }
}
