mod hubs;
mod login;

use anyhow::{bail, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use headroom_core::copilot::GitHubDomain;
use headroom_core::refresh::{fetch_account, Hubs};
use headroom_core::login::LoginError;
use headroom_core::store::Store;
use headroom_core::view::{PanelModel, PanelView};
use headroom_core::{envelope, ProviderKind};
use std::io::{IsTerminal, Read};
use std::process::ExitCode;

/// Usage limits of your AI coding accounts.
#[derive(Parser)]
#[command(name = "headroom", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add, list or remove accounts.
    Accounts {
        #[command(subcommand)]
        command: AccountsCommand,
    },
    /// Connect, list or remove CLIProxyAPI hubs, whose logins show up as accounts.
    Hubs {
        #[command(subcommand)]
        command: hubs::HubsCommand,
    },
    /// Print usage as JSON. Exit codes: 0 printed, 2 bad argument or no such account, 4 nothing fetched yet.
    Usage {
        /// A provider (such as `openrouter`) or an account id (such as `openrouter@1a2b3c4d`).
        target: Option<String>,
        /// Fetch from the provider first and update the cache.
        #[arg(long)]
        refresh: bool,
    },
    /// Print what the panel shows (top-bar label, Priority and All Usage groups) as JSON, from the saved usage. Never fetches.
    Panel,
}

#[derive(Subcommand)]
enum AccountsCommand {
    /// Add an account. `openrouter`, `ollama` and `zai` read the API key from a hidden prompt, or from stdin
    /// when piped. The others sign in: they open your browser, and the device-code ones (`opencode`, `amp`,
    /// `grok`, `muse`, `copilot`) also print a code to enter there.
    Add {
        /// `claude`, `codex` (or `chatgpt`), `cursor`, `antigravity`, `devin`, `opencode`, `amp`, `grok`,
        /// `muse`, `copilot`, `openrouter`, `ollama` or `zai`.
        provider: String,
        /// Copilot only: a GitHub Enterprise Cloud domain such as `octocorp.ghe.com` (or just `octocorp`).
        #[arg(long)]
        domain: Option<String>,
        /// Copilot only: add an organization's Copilot billing with a pasted token of an owner or billing
        /// manager (hidden prompt, or stdin when piped).
        #[arg(long)]
        organization: Option<String>,
    },
    List,
    Remove {
        /// The account id from `accounts list`.
        id: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("headroom: {error:#}");
            ExitCode::from(1)
        }
    }
}

async fn run(cli: Cli) -> Result<u8> {
    let store = Store::default_paths()?;
    let settings = store.settings().unwrap_or_else(|e| {
        eprintln!("headroom: could not read the settings: {e}");
        Default::default()
    });
    headroom_core::http::set_proxy(settings.parsed_proxy());
    match cli.command {
        Command::Accounts { command } => accounts(&store, command).await,
        Command::Hubs { command } => hubs::run(&store, command).await,
        Command::Usage { target, refresh } => usage(&store, target, refresh, settings.refresh_minutes).await,
        Command::Panel => panel(&store, &settings),
    }
}

async fn accounts(store: &Store, command: AccountsCommand) -> Result<u8> {
    match command {
        AccountsCommand::Add { provider, domain, organization } => {
            let kind = login::parse_kind(&provider);
            if (domain.is_some() || organization.is_some()) && kind != Some(ProviderKind::Copilot) {
                eprintln!("headroom: --domain and --organization only work with `copilot`");
                return Ok(2);
            }
            if domain.is_some() && organization.is_some() {
                eprintln!("headroom: use either --domain or --organization, not both");
                return Ok(2);
            }
            let domain = match domain.as_deref().map(GitHubDomain::parse) {
                None => GitHubDomain::Github,
                Some(Some(domain)) => domain,
                Some(None) => {
                    eprintln!("headroom: {}", headroom_core::copilot::domain::INVALID_MESSAGE);
                    return Ok(2);
                }
            };
            if let Some(organization) = organization {
                let token = read_key("GitHub token")?;
                return login::add_copilot_organization(store, organization.trim(), &token).await;
            }
            match kind {
                Some(kind) if login::is_oauth_kind(kind) => return login::add_oauth(store, kind, domain).await,
                Some(kind @ (ProviderKind::Openrouter | ProviderKind::Ollama | ProviderKind::Zai)) => {
                    return add_api_key(store, kind).await;
                }
                _ => {
                    eprintln!("headroom: cannot add `{provider}`; supported: claude, codex, cursor, antigravity, devin, opencode, amp, grok, muse, copilot, openrouter, ollama, zai");
                    return Ok(2);
                }
            }
        }
        AccountsCommand::List => {
            for account in &store.accounts()? {
                match &account.hub {
                    Some(link) => println!("{}\t{} (hub {})", account.card_id(), account.full_name(), link.host()),
                    None => println!("{}\t{}", account.card_id(), account.full_name()),
                }
            }
        }
        AccountsCommand::Remove { id } => match headroom_core::login::remove_account(store, &id) {
            Ok(_) => {}
            Err(error @ (LoginError::UnknownAccount(_) | LoginError::HubManaged { .. })) => {
                eprintln!("headroom: {error}");
                return Ok(2);
            }
            Err(error) => return Err(error.into()),
        },
    }
    Ok(0)
}

/// Adds an API-key account (`openrouter`, `ollama`, `zai`): reads the key, then core checks and saves it.
async fn add_api_key(store: &Store, kind: ProviderKind) -> Result<u8> {
    let key = read_key(&format!("{} API key", kind.display_name()))?;
    let account = headroom_core::login::add_api_key(store, kind, &key).await?;
    println!("Added {} ({})", account.full_name(), account.card_id());
    Ok(0)
}

/// Reads a secret from a hidden prompt on a terminal, else from stdin.
fn read_key(label: &str) -> Result<String> {
    let key = if std::io::stdin().is_terminal() {
        rpassword::prompt_password(format!("{label}: "))?
    } else {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        text
    };
    let key = key.trim().to_string();
    if key.is_empty() {
        bail!("no {} given", label.to_lowercase());
    }
    Ok(key)
}

fn panel(store: &Store, settings: &headroom_core::settings::Settings) -> Result<u8> {
    let model = PanelModel::build(store.accounts()?, store.hubs()?, store.snapshots()?, &store.provider_state()?, &store.burning_fast()?, &store.layout()?, &settings.priority(), Utc::now());
    println!("{}", serde_json::to_string_pretty(&PanelView::new(&model))?);
    Ok(0)
}

async fn usage(store: &Store, target: Option<String>, refresh: bool, refresh_minutes: u32) -> Result<u8> {
    let hubs = Hubs::new(store);
    let mut snapshots = store.snapshots()?;
    if refresh {
        // The hubs own their login lists, so read them before any usage.
        hubs::forget(store, &hubs::sync(&hubs, None).await?)?;
        snapshots = store.snapshots()?;
    }
    let mut accounts = store.accounts()?;
    if let Some(target) = &target {
        let kind = login::parse_kind(target);
        accounts.retain(|a| Some(a.kind) == kind || a.card_id() == *target);
        if accounts.is_empty() {
            eprintln!("headroom: no account matches `{target}`");
            return Ok(2);
        }
    }
    if refresh {
        let mut states = store.provider_state()?;
        for account in &accounts {
            let card_id = account.card_id();
            let state = states.entry(card_id.clone()).or_default();
            // An explicit refresh, like the app's Refresh button: history rereads today and yesterday at once.
            let fetched = fetch_account(account, &hubs, snapshots.get(&card_id), state, Utc::now(), true).await;
            if let Some(warning) = fetched.warning {
                eprintln!("headroom: {warning}");
            }
            match fetched.snapshot {
                Ok(snapshot) => {
                    snapshots.insert(card_id, snapshot);
                }
                Err(message) => eprintln!("headroom: {}: {message}", account.full_name()),
            }
        }
        store.save_provider_state(&states)?;
        store.save_snapshots(&snapshots)?;
        // A hub that said a login is gone is read again at once; the account leaves with it.
        let missing = hubs.client.take_login_missing();
        if !missing.is_empty() {
            let gone = hubs::sync(&hubs, Some(&missing)).await?;
            accounts.retain(|a| !gone.contains(&a.card_id()));
            for id in &gone {
                snapshots.remove(id);
            }
            hubs::forget(store, &gone)?;
        }
    }
    let envelope = envelope::encode(&accounts, &snapshots, Utc::now(), i64::from(refresh_minutes));
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    let fetched = accounts.iter().any(|a| snapshots.contains_key(&a.card_id()));
    Ok(if fetched { 0 } else { 4 })
}
