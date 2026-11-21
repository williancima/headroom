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

/// Codes and links come from the sign-in server: control characters are dropped so they cannot drive the
/// terminal.
fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

fn show(event: LoginEvent) {
    match event {
        LoginEvent::OpenUrl(url) => open_browser(&printable(&url)),
        LoginEvent::DeviceCode { code, url } => {
            eprintln!("Your sign-in code is {}.", printable(&code));
            open_browser(&printable(&url));
        }
        LoginEvent::PortBusy { code, url } => eprintln!("Port 1455 is busy. Visit {} and enter the code: {}", printable(&url), printable(&code)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_text_loses_control_characters() {
        assert_eq!(printable("AB\u{1b}[2J-12\r\n\u{7}\u{9b}31m"), "AB[2J-1231m");
        assert_eq!(printable("https://github.com/login/device"), "https://github.com/login/device");
    }
}
