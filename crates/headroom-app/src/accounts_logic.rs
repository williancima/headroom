//! What the Accounts screen and the add flow show, as plain data so it can be tested without GTK.

use headroom_core::copilot::domain::INVALID_MESSAGE;
use headroom_core::copilot::GitHubDomain;
use headroom_core::hub::Hub;
use headroom_core::login::LoginError;
use headroom_core::{Account, ProviderKind};

/// A row of the provider picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Provider(ProviderKind),
    Hub,
}

impl Choice {
    pub fn title(self) -> &'static str {
        match self {
            Self::Provider(kind) => kind.display_name(),
            Self::Hub => "CLIProxyAPI",
        }
    }
}

/// Every provider in catalog order, then the hub.
pub fn picker() -> Vec<Choice> {
    ProviderKind::ALL.into_iter().map(Choice::Provider).chain([Choice::Hub]).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRow {
    pub card_id: String,
    pub title: String,
    pub subtitle: String,
    /// Hub accounts leave with their hub.
    pub removable: bool,
}

/// Accounts in saved order.
pub fn account_rows(accounts: &[Account]) -> Vec<AccountRow> {
    accounts
        .iter()
        .map(|a| {
            // The title already starts with the provider name.
            let (subtitle, removable) = match &a.hub {
                Some(link) => (format!("Via {}. Removed with its hub.", link.host()), false),
                None => (String::new(), true),
            };
            AccountRow { card_id: a.card_id(), title: a.full_name(), subtitle, removable }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubRow {
    pub id: uuid::Uuid,
    pub host: String,
    pub subtitle: String,
    pub accounts: usize,
}

pub fn hub_rows(hubs: &[Hub], accounts: &[Account]) -> Vec<HubRow> {
    hubs.iter()
        .map(|h| {
            let count = accounts.iter().filter(|a| a.hub.as_ref().is_some_and(|l| l.hub_id == h.id)).count();
            let subtitle = if h.refused.is_some() {
                "The hub refused its key. Replace it with `headroom hubs key` in a terminal.".to_string()
            } else {
                count_text(count)
            };
            HubRow { id: h.id, host: h.host(), subtitle, accounts: count }
        })
        .collect()
}

pub fn count_text(count: usize) -> String {
    format!("{count} account{}", if count == 1 { "" } else { "s" })
}

/// The hub removal confirmation text.
pub fn hub_removal_body(row: &HubRow) -> String {
    format!("Its {} and its management key are deleted from this computer. Nothing changes on the hub.", count_text(row.accounts))
}

/// Blank is github.com; anything else must be a GHE.com address.
pub fn parse_domain(input: &str) -> Result<GitHubDomain, &'static str> {
    GitHubDomain::parse(input).ok_or(INVALID_MESSAGE)
}

/// The text a failed add or remove shows. The CLI hint in `HubManaged` does not apply in the panel.
pub fn login_error_text(error: &LoginError) -> String {
    match error {
        LoginError::HubManaged { name, .. } => format!("{name} is managed by its hub. Remove the hub to remove this account."),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use headroom_core::hub::{HubLink, HubLogin};
    use uuid::Uuid;

    fn hub_account(hub: &Hub) -> Account {
        let mut a = Account::new(ProviderKind::Claude, "work");
        let login = HubLogin {
            id: "1".into(),
            auth_index: Some("i".into()),
            provider: "claude".into(),
            label: String::new(),
            is_oauth: true,
            is_disabled: false,
            chatgpt_account_id: None,
        };
        a.hub = HubLink::new(hub, &login);
        a
    }

    #[test]
    fn the_picker_lists_the_providers_in_catalog_order_then_the_hub() {
        let picker = picker();
        assert_eq!(picker.len(), 14);
        assert_eq!(picker[0], Choice::Provider(ProviderKind::Claude));
        assert_eq!(picker[1].title(), "ChatGPT");
        assert_eq!(picker[5], Choice::Provider(ProviderKind::Copilot));
        assert_eq!(picker[12], Choice::Provider(ProviderKind::Zai));
        assert_eq!(picker[13].title(), "CLIProxyAPI");
    }

    #[test]
    fn hub_accounts_cannot_be_removed_and_say_where_they_come_from() {
        let hub = Hub::new("https://hub.example.com".into());
        let rows = account_rows(&[Account::new(ProviderKind::Zai, ""), hub_account(&hub)]);
        assert!(rows[0].removable);
        assert_eq!((rows[0].title.as_str(), rows[0].subtitle.as_str()), ("Z.ai", ""));
        assert!(!rows[1].removable);
        assert_eq!(rows[1].subtitle, "Via hub.example.com. Removed with its hub.");
    }

    #[test]
    fn a_hub_row_counts_its_accounts_and_notes_a_refused_key() {
        let mut hub = Hub::new("https://hub.example.com".into());
        let accounts = [hub_account(&hub), hub_account(&hub), Account::new(ProviderKind::Zai, "")];
        let rows = hub_rows(std::slice::from_ref(&hub), &accounts);
        assert_eq!((rows[0].host.as_str(), rows[0].subtitle.as_str()), ("hub.example.com", "2 accounts"));
        assert_eq!(count_text(1), "1 account");

        hub.refused = Some(headroom_core::hub::Refused { key: "k".into(), refusal: headroom_core::hub::HubRefusal::KeyRejected });
        assert!(hub_rows(&[hub], &accounts)[0].subtitle.contains("headroom hubs key"));
    }

    #[test]
    fn a_hub_managed_error_does_not_name_the_cli() {
        let text = login_error_text(&LoginError::HubManaged { name: "Claude".into(), hub_id: Uuid::new_v4() });
        assert_eq!(text, "Claude is managed by its hub. Remove the hub to remove this account.");
        assert_eq!(login_error_text(&LoginError::DuplicateKey), "this API key is already added");
    }

    #[test]
    fn copilot_domains_are_validated() {
        assert_eq!(parse_domain(""), Ok(GitHubDomain::Github));
        assert_eq!(parse_domain("octocorp"), Ok(GitHubDomain::Enterprise("octocorp.ghe.com".into())));
        assert_eq!(parse_domain("github.example.org"), Err(INVALID_MESSAGE));
    }
}
