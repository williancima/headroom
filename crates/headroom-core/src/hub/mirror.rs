//! Makes a hub's mirrored accounts match its login list. A new login is added at the bottom of the accounts, a login missing from the list
//! or disabled on the hub is removed, and a renamed login (new auth index) or a ChatGPT login on another
//! workspace is relinked and keeps its account. A login listed without an auth index keeps its account as
//! it is. Applying the same listing twice changes nothing the second time.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::models::{Account, ProviderKind};

use super::login::HubLogin;
use super::model::{Hub, HubLink};

#[derive(Debug, Default, PartialEq)]
pub struct Reconciliation {
    pub added: Vec<(ProviderKind, String, HubLink)>,
    pub removed: Vec<Uuid>,
    pub relinked: HashMap<Uuid, HubLink>,
}

impl Reconciliation {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.relinked.is_empty()
    }
}

pub fn reconcile(accounts: &[Account], hub: &Hub, logins: &[HubLogin]) -> Reconciliation {
    let mut listed: HashMap<&str, &HubLogin> = HashMap::new();
    for login in logins {
        listed.entry(&login.id).or_insert(login);
    }
    let mut plan = Reconciliation::default();
    let mut mirrored: HashSet<&str> = HashSet::new();
    for account in accounts {
        let Some(link) = account.hub.as_ref().filter(|l| l.hub_id == hub.id) else { continue };
        let kept = listed
            .get(link.login_id.as_str())
            .filter(|login| !login.is_disabled && login.kind() == Some(account.kind) && mirrored.insert(login.id.as_str()));
        let Some(login) = kept else {
            plan.removed.push(account.id);
            continue;
        };
        if let Some(current) = HubLink::new(hub, login).filter(|current| current != link) {
            plan.relinked.insert(account.id, current);
        }
    }
    for login in logins {
        let Some(kind) = login.kind().filter(|_| !login.is_disabled) else { continue };
        let Some(link) = HubLink::new(hub, login) else { continue };
        if mirrored.insert(login.id.as_str()) {
            plan.added.push((kind, login.label.clone(), link));
        }
    }
    plan
}

/// The account list after mirroring `logins` of `hub`.
pub fn mirror(accounts: &[Account], hub: &Hub, logins: &[HubLogin]) -> Vec<Account> {
    let plan = reconcile(accounts, hub, logins);
    let mut next: Vec<Account> = accounts.iter().filter(|a| !plan.removed.contains(&a.id)).cloned().collect();
    for account in &mut next {
        if let Some(link) = plan.relinked.get(&account.id) {
            account.hub = Some(link.clone());
        }
    }
    for (kind, label, link) in plan.added {
        // A card id keeps only 8 hex digits of the id; two cards with one id would collide, so draw again.
        let mut account = Account::new(kind, &account_name(&label, kind));
        while next.iter().any(|a| a.card_id() == account.card_id()) {
            account = Account::new(kind, &account.label);
        }
        account.hub = Some(link);
        next.push(account);
    }
    next
}

/// The label, or `<Provider> Account` when it is empty or repeats the provider's name.
fn account_name(label: &str, kind: ProviderKind) -> String {
    let name = label.trim();
    let display = kind.display_name();
    if name.is_empty() || name.to_lowercase() == display.to_lowercase() {
        format!("{display} Account")
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::super::login::fixtures::{login, RECORDED_LISTING};
    use super::*;

    fn hub() -> Hub {
        Hub::new("http://mini.local:8317".into())
    }

    fn labels(accounts: &[Account]) -> Vec<&str> {
        accounts.iter().map(|a| a.label.as_str()).collect()
    }

    fn login_ids(accounts: &[Account]) -> Vec<Option<&str>> {
        accounts.iter().map(|a| a.hub.as_ref().map(|l| l.login_id.as_str())).collect()
    }

    #[test]
    fn connecting_mirrors_every_supported_login_as_an_ordinary_account() {
        let hub = hub();
        let logins = HubLogin::listing(RECORDED_LISTING.as_bytes()).unwrap();
        let accounts = mirror(&[], &hub, &logins);
        let kinds: Vec<_> = accounts.iter().map(|a| a.kind).collect();
        assert_eq!(kinds, [ProviderKind::Claude, ProviderKind::Claude, ProviderKind::Claude, ProviderKind::Codex]);
        assert_eq!(
            labels(&accounts),
            ["alice@example.com", "Claude Account", "notoken@example.com", "codex-user@example.com"],
            "hub order is kept, a disabled login gets no account, and a login named like its provider gets the default name"
        );
        assert_eq!(accounts[0].hub.as_ref().unwrap().auth_index, "b799da203df01af5");
        assert!(accounts.iter().all(|a| a.hub.as_ref().is_some_and(|l| l.hub_id == hub.id && l.hub_url == hub.url)));
        assert_eq!(accounts[3].hub.as_ref().unwrap().chatgpt_account_id.as_deref(), Some("acct-dummy-0001"));
    }

    #[test]
    fn mirroring_the_same_listing_twice_changes_nothing() {
        let hub = hub();
        let logins = HubLogin::listing(RECORDED_LISTING.as_bytes()).unwrap();
        let once = mirror(&[], &hub, &logins);
        assert!(reconcile(&once, &hub, &logins).is_empty());
        assert_eq!(mirror(&once, &hub, &logins), once);
    }

    #[test]
    fn the_mirror_follows_logins_added_removed_renamed_and_disabled_on_the_hub() {
        let hub = hub();
        let mut accounts = vec![Account::new(ProviderKind::Claude, "Direct")];
        accounts = mirror(&accounts, &hub, &[login("a", "claude", None, false), login("b", "codex", None, false), login("c", "claude", None, false)]);
        let (b, c) = (accounts[2].id, accounts[3].id);
        accounts[3].label = "Renamed Here".into();

        let next = [login("b", "codex", Some("moved"), false), login("c", "claude", None, true), login("d", "claude", None, false)];
        accounts = mirror(&accounts, &hub, &next);
        assert_eq!(labels(&accounts), ["Direct", "b", "d"], "gone and disabled logins disappear, and a new one is added last");
        assert_eq!(accounts[1].id, b);
        assert_eq!(accounts[1].hub.as_ref().unwrap().auth_index, "moved", "a renamed hub file is relinked, not re-added");

        let back = [login("b", "codex", Some("moved"), false), login("c", "claude", None, false), login("d", "claude", None, false)];
        accounts = mirror(&accounts, &hub, &back);
        assert_eq!(labels(&accounts), ["Direct", "b", "d", "c"], "a login enabled again comes back as a new account, last");
        assert_ne!(accounts[3].id, c);
    }

    #[test]
    fn a_chatgpt_workspace_switch_relinks_and_keeps_the_account_and_its_name() {
        let hub = hub();
        let mut accounts = mirror(&[], &hub, &[login("b", "codex", None, false)]);
        accounts[0].label = "My Codex".into();
        let mut switched = login("b", "codex", None, false);
        switched.chatgpt_account_id = Some("other-workspace".into());
        let next = mirror(&accounts, &hub, &[switched]);
        assert_eq!(next[0].id, accounts[0].id);
        assert_eq!(next[0].label, "My Codex");
        assert_eq!(next[0].hub.as_ref().unwrap().chatgpt_account_id.as_deref(), Some("other-workspace"));
    }

    #[test]
    fn a_login_missing_from_a_listing_is_removed_and_an_uncallable_one_keeps_its_account() {
        let hub = hub();
        let accounts = mirror(&[], &hub, &[login("a", "claude", None, false), login("b", "codex", None, false)]);
        let mut uncallable = login("a", "claude", None, false);
        uncallable.auth_index = None;
        assert_eq!(mirror(&accounts, &hub, &[uncallable, login("b", "codex", None, false)]), accounts);

        let only_a = mirror(&accounts, &hub, &[login("a", "claude", None, false)]);
        assert_eq!(only_a.iter().map(|a| a.id).collect::<Vec<_>>(), [accounts[0].id]);
        assert!(mirror(&only_a, &hub, &[]).is_empty(), "a hub that lists no logins has none");
    }

    #[test]
    fn an_api_key_or_uncallable_entry_is_not_mirrored() {
        let hub = hub();
        let body = br#"{"files":[{"id":"a","auth_index":"1","provider":"claude","account_type":"api_key"},{"id":"b","provider":"codex"},{"id":"c","auth_index":"3","provider":"codex"}]}"#;
        let accounts = mirror(&[], &hub, &HubLogin::listing(body).unwrap());
        assert_eq!(login_ids(&accounts), [Some("c")]);
    }

    #[test]
    fn another_hubs_and_direct_accounts_are_left_alone_and_a_kind_change_replaces_the_account() {
        let (one, two) = (hub(), hub());
        let direct = Account::new(ProviderKind::Codex, "Direct");
        let mut accounts = mirror(std::slice::from_ref(&direct), &one, &[login("a", "claude", None, false)]);
        accounts = mirror(&accounts, &two, &[login("a", "claude", None, false)]);
        assert_eq!(accounts.len(), 3);
        accounts = mirror(&accounts, &one, &[]);
        assert_eq!(accounts.iter().filter(|a| a.hub.is_none()).count(), 1);
        assert_eq!(accounts.len(), 2);
        let swapped = mirror(&accounts, &two, &[login("a", "codex", None, false)]);
        assert_eq!(swapped.iter().filter_map(|a| a.hub.as_ref()).count(), 1);
        assert_eq!(swapped[1].kind, ProviderKind::Codex);
        assert_ne!(swapped[1].id, accounts[1].id);
    }

    #[test]
    fn a_forged_record_that_points_at_another_hub_is_not_this_hubs_account() {
        let (real, forged) = (hub(), hub());
        let mut account = Account::new(ProviderKind::Claude, "Linked");
        account.hub = HubLink::new(&forged, &login("forged", "claude", None, false));
        let accounts = mirror(&[account.clone()], &real, &[]);
        assert_eq!(accounts, [account]);
    }
}
