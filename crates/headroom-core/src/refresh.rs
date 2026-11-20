//! Fetches one account of any supported kind, keeping the provider cadence state and rotated sign-ins.

use chrono::{DateTime, Duration, Utc};
use std::collections::HashSet;
use uuid::Uuid;
use crate::amp::AmpClient;
use crate::antigravity::AntigravityClient;
use crate::claude::{grants_due, resets::LINE_LABEL, ClaudeClient};
use crate::codex::{credit_list_due, CodexClient};
use crate::copilot::{CopilotClient, GitHubDomain};
use crate::cursor::CursorClient;
use crate::devin::DevinClient;
use crate::grok::GrokClient;
use crate::history::{Ledger, Run};
use crate::hub::transport::{HubRoute, Transport};
use crate::hub::{HubClient, HubLink, KeyringHubKeys};
use crate::muse::MuseClient;
use crate::opencode::OpenCodeClient;
use crate::ollama::OllamaClient;
use crate::openrouter::{OpenRouterClient, BASE_URL};
use crate::provider::ProviderError;
use crate::store::{self, ProviderState, Store};
use crate::zai::{list_due, ZaiCache, ZaiClient};
use crate::{Account, Credential, OAuthTokens, ProviderKind, ProviderSnapshot};

/// Everything a hub call needs: the client (with its refusal gate), the key vault and the store.
pub struct Hubs {
    pub client: HubClient,
    pub keys: KeyringHubKeys,
    pub store: Store,
}

impl Hubs {
    pub fn new(store: &Store) -> Self {
        Self { client: HubClient::new(HubClient::default_http(), store.clone()), keys: KeyringHubKeys, store: store.clone() }
    }
}

/// Reads each listed hub's login list (all hubs when `only` is `None`) and mirrors it onto the accounts.
/// A failing hub yields one warning line and the rest go on. Returns the card ids that left, and the warnings.
pub async fn sync_hubs(hubs: &Hubs, only: Option<&[Uuid]>) -> std::io::Result<(Vec<String>, Vec<String>)> {
    let before = hubs.store.accounts()?;
    let mut warnings = vec![];
    for hub in hubs.store.hubs()?.iter().filter(|h| only.is_none_or(|ids| ids.contains(&h.id))) {
        if let Err(error) = crate::hub::sync(&hubs.client, &hubs.store, &hubs.keys, hub).await {
            warnings.push(format!("hub {}: {error}", hub.host()));
        }
    }
    let after: HashSet<String> = hubs.store.accounts()?.iter().map(|a| a.card_id()).collect();
    Ok((before.iter().map(|a| a.card_id()).filter(|id| !after.contains(id)).collect(), warnings))
}

/// Forgets the snapshots and cadence state of accounts that left.
pub fn forget(store: &Store, card_ids: &[String]) -> std::io::Result<()> {
    if card_ids.is_empty() {
        return Ok(());
    }
    let mut snapshots = store.snapshots()?;
    let mut states = store.provider_state()?;
    for id in card_ids {
        snapshots.remove(id);
        states.remove(id);
    }
    store.save_snapshots(&snapshots)?;
    store.save_provider_state(&states)?;
    Ok(())
}

/// The outcome of one fetch. `warning` is set when a renewed sign-in could not be saved; the caller shows it.
pub struct Fetched {
    pub snapshot: Result<ProviderSnapshot, ProviderError>,
    pub warning: Option<String>,
}

const MISMATCH: &str = "the stored credential does not match this account. Remove the account and add it again.";

fn mismatch() -> ProviderError {
    ProviderError::Unavailable(MISMATCH.into())
}

/// Keeps the plan and the "Rate Limit Resets" line of the last snapshot when the hourly secondary read
/// was skipped, so the output does not flap between reads.
pub fn carry_over_claude(snapshot: &mut ProviderSnapshot, previous: Option<&ProviderSnapshot>) {
    let Some(previous) = previous else { return };
    if snapshot.plan.is_none() {
        snapshot.plan = previous.plan.clone();
    }
    if snapshot.line(LINE_LABEL).is_none() {
        if let Some(line) = previous.line(LINE_LABEL) {
            snapshot.lines.push(line.clone());
            snapshot.reset_expiries = previous.reset_expiries.clone();
            snapshot.reset_items = previous.reset_items.clone();
        }
    }
}

fn hourly_due(last: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last.is_none_or(|at| now - at >= Duration::hours(1))
}

fn text(body: Option<Vec<u8>>) -> Option<String> {
    body.map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Fetches one account. Tokens that changed (a refresh rotates them) are saved on Ok and on Err. `forced` is
/// an explicit refresh: the history rows read today and yesterday again at once instead of waiting out the
/// source's interval.
pub async fn fetch_account(
    account: &Account,
    hubs: &Hubs,
    previous: Option<&ProviderSnapshot>,
    state: &mut ProviderState,
    now: DateTime<Utc>,
    forced: bool,
) -> Fetched {
    let mut warning = None;
    let snapshot = fetch_inner(account, hubs, previous, state, now, forced, &mut warning).await;
    Fetched { snapshot, warning }
}

async fn fetch_inner(
    account: &Account,
    hubs: &Hubs,
    previous: Option<&ProviderSnapshot>,
    state: &mut ProviderState,
    now: DateTime<Utc>,
    forced: bool,
    warning: &mut Option<String>,
) -> Result<ProviderSnapshot, ProviderError> {
    if let Some(link) = &account.hub {
        return fetch_hub_account(account, link, hubs, previous, state, now).await;
    }
    let credential = store::load_credential(account.id).map_err(|e| ProviderError::Unavailable(format!("could not read the credential from the Secret Service: {e}")))?;
    match (account.kind, credential) {
        (ProviderKind::Openrouter, Credential::ApiKey { key }) => {
            let run = Run { ledger: state.history.get_or_insert_with(Ledger::default), forced, account: &account.card_id() };
            OpenRouterClient::new(BASE_URL).fetch(&key, now, run).await
        }
        (ProviderKind::Ollama, Credential::ApiKey { key }) => {
            let read = hourly_due(state.last_secondary_read, now);
            let run = Run { ledger: state.history.get_or_insert_with(Ledger::default), forced, account: &account.card_id() };
            let fetched = OllamaClient::default().fetch_with_history(&key, state.cached_body.clone(), read, now, run).await?;
            if read {
                state.last_secondary_read = Some(now);
            }
            state.cached_body = fetched.account_body;
            Ok(fetched.snapshot)
        }
        (ProviderKind::Zai, Credential::ApiKey { key }) => {
            // The plan and the reset list share one hourly cadence, kept as JSON in `cached_body`.
            let read = list_due(state.last_secondary_read, now);
            let cached = ZaiCache::decode(state.cached_body.as_deref());
            let fetched = ZaiClient::default().fetch(&key, cached, read, now).await?;
            if read {
                state.last_secondary_read = Some(now);
            }
            state.cached_body = Some(fetched.cache.encode());
            Ok(fetched.snapshot)
        }
        (ProviderKind::Openrouter | ProviderKind::Ollama | ProviderKind::Zai, _) => Err(mismatch()),
        (ProviderKind::Copilot, Credential::ApiKey { key }) => match &account.organization {
            Some(organization) => {
                CopilotClient::new(GitHubDomain::Github).fetch_organization(organization, &key, now).await
            }
            None => Err(mismatch()),
        },
        (ProviderKind::Copilot, Credential::OAuth(_)) if account.organization.is_some() => Err(mismatch()),
        (_, Credential::OAuth(tokens)) => {
            let mut current = tokens.clone();
            let result = fetch_oauth(account, &mut current, Transport::Direct, previous, state, now, forced).await;
            if current != tokens {
                if let Err(error) = store::save_credential(account.id, &Credential::OAuth(current)) {
                    *warning = Some(format!("{}: could not save the renewed sign-in: {error}", account.full_name()));
                }
            }
            result
        }
        _ => Err(mismatch()),
    }
}

/// A hub account has no stored credential: its placeholder token never refreshes, and nothing is saved.
/// The hub's key comes only through `HubRoute::new`, which checks the link against the saved hub.
async fn fetch_hub_account(
    account: &Account,
    link: &HubLink,
    hubs: &Hubs,
    previous: Option<&ProviderSnapshot>,
    state: &mut ProviderState,
    now: DateTime<Utc>,
) -> Result<ProviderSnapshot, ProviderError> {
    if !matches!(
        account.kind,
        ProviderKind::Claude | ProviderKind::Codex | ProviderKind::Antigravity | ProviderKind::Devin | ProviderKind::Grok
    ) {
        return Err(mismatch());
    }
    let route = HubRoute::new(&hubs.client, &hubs.store, &hubs.keys, link, account.kind)?;
    let mut tokens = link.tokens();
    fetch_oauth(account, &mut tokens, Transport::Hub(Box::new(route)), previous, state, now, false).await.map_err(|e| link.refusal(e))
}

async fn fetch_oauth(
    account: &Account,
    tokens: &mut OAuthTokens,
    transport: Transport,
    previous: Option<&ProviderSnapshot>,
    state: &mut ProviderState,
    now: DateTime<Utc>,
    forced: bool,
) -> Result<ProviderSnapshot, ProviderError> {
    // A hub account has no history: its login lives on the hub.
    let history = account.hub.is_none();
    match account.kind {
        ProviderKind::Claude => {
            // A hub account reads its plan with its first usage read, so it shows as soon as it appears.
            let secondary = grants_due(state.last_secondary_read, now, false)
                || (account.hub.is_some() && previous.is_none_or(|p| p.plan.is_none()));
            let mut snapshot = ClaudeClient::default().with_transport(transport).fetch(tokens, secondary, now).await?.snapshot;
            if secondary {
                state.last_secondary_read = Some(now);
            } else {
                carry_over_claude(&mut snapshot, previous);
            }
            Ok(snapshot)
        }
        ProviderKind::Codex => {
            let read = credit_list_due(state.last_secondary_read, now);
            let cached = state.cached_body.clone().map(String::into_bytes);
            let client = CodexClient::default().with_transport(transport);
            let mut fetched = client.fetch(tokens, cached, read, now).await?;
            if history {
                let counts = Run { ledger: state.history.get_or_insert_with(Ledger::default), forced, account: &account.card_id() };
                client.add_history(&mut fetched, tokens, now, counts, state.credits_history.get_or_insert_with(Ledger::default)).await;
            }
            if read {
                state.last_secondary_read = Some(now);
            }
            state.cached_body = text(fetched.credit_list);
            Ok(fetched.snapshot)
        }
        ProviderKind::Antigravity => {
            let read = hourly_due(state.last_secondary_read, now);
            let cached = state.cached_body.clone().map(String::into_bytes);
            let fetched = AntigravityClient::default().with_transport(transport).fetch(tokens, cached, read, now).await?;
            if read {
                state.last_secondary_read = Some(now);
            }
            state.cached_body = text(fetched.code_assist);
            Ok(fetched.snapshot)
        }
        ProviderKind::Cursor => {
            let client = CursorClient::default();
            let mut fetched = client.fetch(tokens, now).await?;
            if history {
                let run = Run { ledger: state.history.get_or_insert_with(Ledger::default), forced, account: &account.card_id() };
                client.add_history(&mut fetched, &tokens.access_token, now, run).await;
            }
            Ok(fetched.snapshot)
        }
        ProviderKind::Devin => DevinClient::default().with_transport(transport).fetch(tokens, now).await,
        ProviderKind::Amp => AmpClient::default().fetch(tokens, now).await,
        ProviderKind::Copilot => CopilotClient::new(GitHubDomain::from_saved(account.domain.as_deref())).fetch(tokens, now).await,
        ProviderKind::Muse => MuseClient::default().fetch(tokens, previous, now).await,
        ProviderKind::Grok => {
            let read = hourly_due(state.last_secondary_read, now);
            let fetched = GrokClient::default().with_transport(transport).fetch(tokens, state.cached_body.clone(), read, now).await?;
            if read {
                state.last_secondary_read = Some(now);
            }
            state.cached_body = fetched.plan;
            Ok(fetched.snapshot)
        }
        ProviderKind::Opencode => {
            let read = hourly_due(state.last_secondary_read, now);
            let fetched = OpenCodeClient::default().fetch(tokens, state.cached_body.clone(), read, now).await?;
            if read {
                state.last_secondary_read = Some(now);
            }
            state.cached_body = fetched.plan;
            Ok(fetched.snapshot)
        }
        // API-key providers never reach here: `fetch_account` routes them by credential.
        ProviderKind::Openrouter | ProviderKind::Ollama | ProviderKind::Zai => Err(mismatch()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MetricLine, MetricValue};

    fn snap(plan: Option<&str>, resets: Option<f64>) -> ProviderSnapshot {
        let mut lines = vec![MetricLine::Badge { label: "x".into(), text: "y".into() }];
        let mut reset_expiries = vec![];
        let mut reset_items = vec![];
        if let Some(number) = resets {
            lines.push(MetricLine::Values { label: LINE_LABEL.into(), values: vec![MetricValue { number, currency_code: None, label: None }] });
            reset_expiries.push("2026-12-01T00:00:00Z".parse().unwrap());
            reset_items = crate::claim::ResetItem::codex(&reset_expiries);
        }
        ProviderSnapshot { plan: plan.map(String::from), lines, refreshed_at: Utc::now(), reset_expiries, reset_items, notice: None, breakdowns: Default::default() }
    }

    #[test]
    fn carries_the_plan_and_resets_from_the_previous_snapshot() {
        let mut fresh = snap(None, None);
        carry_over_claude(&mut fresh, Some(&snap(Some("Max"), Some(2.0))));
        assert_eq!(fresh.plan.as_deref(), Some("Max"));
        assert_eq!(fresh.line(LINE_LABEL), snap(None, Some(2.0)).line(LINE_LABEL));
        assert_eq!(fresh.reset_expiries.len(), 1);
        assert_eq!(fresh.reset_items.len(), 1);
    }

    #[test]
    fn carry_over_keeps_fresh_values_and_tolerates_no_previous() {
        let mut fresh = snap(Some("Pro"), Some(1.0));
        carry_over_claude(&mut fresh, Some(&snap(Some("Max"), Some(2.0))));
        assert_eq!(fresh.plan.as_deref(), Some("Pro"));
        assert_eq!(fresh.lines.len(), 2);
        let mut bare = snap(None, None);
        carry_over_claude(&mut bare, None);
        assert_eq!(bare.plan, None);
        carry_over_claude(&mut bare, Some(&snap(None, None)));
        assert_eq!(bare.lines.len(), 1);
    }

    #[test]
    fn hourly_cadence() {
        let now = Utc::now();
        assert!(hourly_due(None, now));
        assert!(!hourly_due(Some(now - Duration::minutes(59)), now));
        assert!(hourly_due(Some(now - Duration::minutes(60)), now));
    }
}
