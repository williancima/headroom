//! The terminal side of `headroom accounts add`: prints what core asks the user to do.

use anyhow::Result;
use headroom_core::copilot::GitHubDomain;
use headroom_core::login::{self, LoginError, LoginEvent};
use headroom_core::store::Store;
use headroom_core::{Account, ProviderKind};
use std::process::{Command, Stdio};

pub use headroom_core::login::{is_oauth_kind, parse_kind};

fn open_browser(url: &str) {
    eprintln!("Opening your browser to sign in. If it doesn't open, visit: {url}");
    // Not awaited: the sign-in waits for the callback, not for xdg-open.
    let _ = Command::new("xdg-open").arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

fn show(event: LoginEvent) {
    match event {
        LoginEvent::OpenUrl(url) => open_browser(&url),
        LoginEvent::DeviceCode { code, url } => {
            eprintln!("Your sign-in code is {code}.");
            open_browser(&url);
        }
        LoginEvent::PortBusy { code, url } => eprintln!("Port 1455 is busy. Visit {url} and enter the code: {code}"),
    }
}

fn added(account: Account) -> u8 {
    println!("Added {} ({})", account.full_name(), account.card_id());
    0
}

/// `domain` only matters for Copilot.
pub async fn add_oauth(store: &Store, kind: ProviderKind, domain: GitHubDomain) -> Result<u8> {
    match login::add_oauth(store, kind, domain, show).await {
        Err(LoginError::TimedOut) => {
            eprintln!("Sign-in timed out.");
            Ok(1)
        }
        other => Ok(added(other?)),
    }
}

pub async fn add_copilot_organization(store: &Store, organization: &str, token: &str) -> Result<u8> {
    Ok(added(login::add_copilot_organization(store, organization, token).await?))
}
