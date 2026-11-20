//! Reset items and claiming one.
//! The panels list `ResetItem`s and send back the chosen item's `ResetRef`. Never log credentials or bodies.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::claude::resets::ResetKind;
use crate::claude::{ClaimFailure, ClaimOutcome, ClaudeClient};
use crate::codex::CodexClient;
use crate::priority::duration;
use crate::provider::ProviderError;
use crate::store::{self, Store};
use crate::zai::{Reset, ZaiClient};
use crate::{Credential, OAuthTokens, ProviderKind};

/// The record a claim spends: Claude's grant, ChatGPT's credit with that expiry, or Z.ai's exact record.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResetRef {
    ClaudeGrant(String),
    CodexCredit(DateTime<Utc>),
    Zai(Reset),
}

impl ResetRef {
    /// Orders two resets that share an expiry and a kind, so the list keeps its order across refreshes.
    fn tie_breaker(&self) -> (i64, &str) {
        match self {
            Self::ClaudeGrant(id) => (0, id),
            Self::CodexCredit(_) => (0, ""),
            Self::Zai(reset) => (reset.record_id, ""),
        }
    }
}

/// One reset on the Rate Limit Resets timeline: one row each, for every provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetItem {
    pub kind: ResetKind,
    pub expires_at: DateTime<Utc>,
    pub claim: ResetRef,
    /// Whether the row offers Use.
    pub usable: bool,
    /// The confirmation's sentence; `None` keeps the general wording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_detail: Option<String>,
}

impl ResetItem {
    /// By expiry, then kind, then record, so the list is stable.
    pub fn timeline_order(a: &Self, b: &Self) -> std::cmp::Ordering {
        (a.expires_at, a.kind, a.claim.tie_breaker()).cmp(&(b.expires_at, b.kind, b.claim.tie_breaker()))
    }

    /// ChatGPT's credits: one `Full` reset per expiry.
    pub fn codex(expiries: &[DateTime<Utc>]) -> Vec<Self> {
        expiries
            .iter()
            .map(|&at| Self { kind: ResetKind::Full, expires_at: at, claim: ResetRef::CodexCredit(at), usable: true, confirm_detail: None })
            .collect()
    }
}

/// "soon" within five minutes, else the time left.
pub fn when_label(at: DateTime<Utc>, now: DateTime<Utc>) -> Option<String> {
    if (at - now).num_seconds() <= 5 * 60 {
        return Some("soon".into());
    }
    duration(now, at)
}

impl ClaimOutcome {
    pub fn message(&self, now: DateTime<Utc>) -> String {
        match self {
            Self::Success => "Reset claimed. Enjoy!".into(),
            Self::NothingToReset => "Your usage doesn't need a reset yet".into(),
            Self::NoCredit => "That reset is no longer available".into(),
            Self::Unconfirmed => "Your reset may still be going through. Check again in a minute.".into(),
            Self::UnavailableNow { until } => {
                let wait = until.and_then(|at| when_label(at, now)).map(|w| if w == "soon" { " Try again soon.".to_string() } else { format!(" Try again in {w}.") });
                format!("Resets can't be used right now.{}", wait.unwrap_or_default())
            }
            Self::NotNextReset { next } => {
                format!("Claude wants {} used first. Nothing was used.", next.map_or("another reset".to_string(), |k| format!("your {}", k.title())))
            }
            Self::Failed(ClaimFailure::SignInExpired) => "Your sign-in expired. Sign in again from the account's page.".into(),
            Self::Failed(ClaimFailure::NotPermitted) => "This sign-in can't use resets. Remove the account and add it again.".into(),
            Self::Failed(ClaimFailure::Connection) => "Couldn't connect. Check your internet connection and try again.".into(),
            Self::Failed(ClaimFailure::Refused) => "Couldn't reset usage. Please try again.".into(),
        }
    }

    /// A settled claim's request id is done: the next claim on that record is a new one. An unsettled claim
    /// keeps its id, so a retry can't spend a second reset.
    pub fn settled(&self) -> bool {
        !matches!(self, Self::Unconfirmed | Self::Failed(_))
    }

    pub fn failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

impl From<&ProviderError> for ClaimFailure {
    fn from(error: &ProviderError) -> Self {
        match error {
            ProviderError::SignInExpired => Self::SignInExpired,
            ProviderError::ConnectionFailed | ProviderError::Unreachable(_) => Self::Connection,
            _ => Self::Refused,
        }
    }
}

/// The credit a Codex claim matched, by request id, so a retry replays the same pair: after a consume whose
/// answer was lost the credit is gone from the list, and only the replay lets `already_redeemed` prove it landed.
static MATCHED_CREDITS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn matched_credit(request_id: &str) -> Option<String> {
    MATCHED_CREDITS.lock().ok()?.as_ref()?.get(request_id).cloned()
}

fn remember_credit(request_id: &str, credit: Option<String>) {
    if let (Some(credit), Ok(mut map)) = (credit, MATCHED_CREDITS.lock()) {
        map.get_or_insert_with(HashMap::new).insert(request_id.into(), credit);
    }
}

fn forget_credit(request_id: &str) {
    if let Ok(mut map) = MATCHED_CREDITS.lock() {
        map.as_mut().and_then(|m| m.remove(request_id));
    }
}

fn failed(card_id: &str, step: &str, error: &ProviderError) -> ClaimOutcome {
    eprintln!("headroom: {card_id}: reset claim: {step}: {error}");
    ClaimOutcome::Failed(error.into())
}

/// Spends the reset `claim` names on the account `card_id`: loads its credential, calls the provider and
/// saves rotated tokens (also when the claim fails). A claim that did not fail clears the account's
/// secondary-read time, so the next refresh reads the reset list again and drops the spent row.
pub async fn claim_reset(store: &Store, card_id: &str, claim: &ResetRef, request_id: &str, now: DateTime<Utc>) -> ClaimOutcome {
    let refused = ClaimOutcome::Failed(ClaimFailure::Refused);
    let account = match store.accounts() {
        Ok(accounts) => accounts.into_iter().find(|a| a.card_id() == card_id),
        Err(error) => {
            eprintln!("headroom: {card_id}: reset claim: could not read the accounts: {error}");
            return refused;
        }
    };
    let Some(account) = account.filter(|a| a.hub.is_none()) else {
        eprintln!("headroom: {card_id}: reset claim: no local account to claim on");
        return refused;
    };
    let credential = match store::load_credential(account.id) {
        Ok(credential) => credential,
        Err(error) => {
            eprintln!("headroom: {card_id}: reset claim: could not read the credential: {error}");
            return refused;
        }
    };
    let outcome = match (account.kind, claim, credential) {
        (ProviderKind::Claude, ResetRef::ClaudeGrant(grant), Credential::OAuth(tokens)) => {
            let mut current = tokens.clone();
            let outcome = claim_claude(&mut current, card_id, grant, request_id, now).await;
            save_if_changed(account.id, &tokens, current);
            outcome
        }
        (ProviderKind::Codex, ResetRef::CodexCredit(expiry), Credential::OAuth(tokens)) => {
            let mut current = tokens.clone();
            let outcome = claim_codex(&CodexClient::default(), &mut current, card_id, *expiry, request_id, now).await;
            save_if_changed(account.id, &tokens, current);
            outcome
        }
        (ProviderKind::Zai, ResetRef::Zai(reset), Credential::ApiKey { key }) => match ZaiClient::default().claim_reset(&key, reset, request_id, now).await {
            Ok(outcome) => outcome,
            Err(error) => failed(card_id, "request failed", &error),
        },
        _ => {
            eprintln!("headroom: {card_id}: reset claim: the reset does not belong to this account");
            refused
        }
    };
    if outcome.settled() {
        forget_credit(request_id);
    }
    if !outcome.failed() {
        let again = || -> std::io::Result<()> {
            let mut states = store.provider_state()?;
            if let Some(state) = states.get_mut(card_id) {
                state.last_secondary_read = None;
                store.save_provider_state(&states)?;
            }
            Ok(())
        };
        if let Err(error) = again() {
            eprintln!("headroom: {card_id}: reset claim: could not schedule the list read: {error}");
        }
    }
    outcome
}

/// Saves renewed tokens, on Ok and on Err of the claim.
fn save_if_changed(account: uuid::Uuid, before: &OAuthTokens, after: OAuthTokens) {
    if &after != before {
        if let Err(error) = store::save_credential(account, &Credential::OAuth(after)) {
            eprintln!("headroom: could not save the renewed sign-in: {error}");
        }
    }
}

/// A fresh grants read and the organization come first: a reset listed from the saved answer is checked
/// against a fresh one before anything is spent.
async fn claim_claude(tokens: &mut OAuthTokens, card_id: &str, grant_id: &str, request_id: &str, now: DateTime<Utc>) -> ClaimOutcome {
    let client = ClaudeClient::default();
    let fetched = match client.fetch(tokens, true, now).await {
        Ok(fetched) => fetched,
        Err(error) => return failed(card_id, "couldn't prepare the claim", &error),
    };
    let (Some(status), Some(organization)) = (&fetched.grants, &fetched.organization_uuid) else {
        return failed(card_id, "couldn't prepare the claim", &ProviderError::InvalidResponse);
    };
    let Some(chosen) = status.claimable(grant_id) else {
        eprintln!("headroom: {card_id}: reset claim: the chosen grant has no reset left in the last check");
        return ClaimOutcome::NoCredit;
    };
    let result = client.claim_reset_grant(tokens, organization, grant_id, request_id, now).await;
    match result {
        // Anthropic names the reset it wants first; say so unless it is the kind chosen.
        Ok(ClaimOutcome::NotNextReset { .. }) => ClaimOutcome::NotNextReset { next: status.next_kind().filter(|k| *k != chosen.kind()) },
        Ok(outcome) => outcome,
        Err(error) => failed(card_id, "request failed", &error),
    }
}

async fn claim_codex(client: &CodexClient, tokens: &mut OAuthTokens, card_id: &str, expiry: DateTime<Utc>, request_id: &str, now: DateTime<Utc>) -> ClaimOutcome {
    let mut matched = matched_credit(request_id);
    let result = client.claim_reset_credit(tokens, expiry, request_id, &mut matched, now).await;
    remember_credit(request_id, matched);
    match result {
        Ok(outcome) => outcome,
        Err(error) => failed(card_id, "request failed", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn messages_are_friendly() {
        let now = at("2026-11-01T00:00:00Z");
        let m = |o: ClaimOutcome| o.message(now);
        assert_eq!(m(ClaimOutcome::Success), "Reset claimed. Enjoy!");
        assert_eq!(m(ClaimOutcome::NothingToReset), "Your usage doesn't need a reset yet");
        assert_eq!(m(ClaimOutcome::NoCredit), "That reset is no longer available");
        assert_eq!(m(ClaimOutcome::Unconfirmed), "Your reset may still be going through. Check again in a minute.");
        assert_eq!(m(ClaimOutcome::UnavailableNow { until: None }), "Resets can't be used right now.");
        assert_eq!(m(ClaimOutcome::UnavailableNow { until: Some(at("2026-11-01T00:03:00Z")) }), "Resets can't be used right now. Try again soon.");
        assert_eq!(m(ClaimOutcome::UnavailableNow { until: Some(at("2026-11-01T02:30:00Z")) }), "Resets can't be used right now. Try again in 2h 30m.");
        assert_eq!(m(ClaimOutcome::NotNextReset { next: None }), "Claude wants another reset used first. Nothing was used.");
        assert_eq!(m(ClaimOutcome::NotNextReset { next: Some(ResetKind::Weekly) }), "Claude wants your Weekly Reset used first. Nothing was used.");
        assert_eq!(m(ClaimOutcome::Failed(ClaimFailure::SignInExpired)), "Your sign-in expired. Sign in again from the account's page.");
        assert_eq!(m(ClaimOutcome::Failed(ClaimFailure::NotPermitted)), "This sign-in can't use resets. Remove the account and add it again.");
        assert_eq!(m(ClaimOutcome::Failed(ClaimFailure::Connection)), "Couldn't connect. Check your internet connection and try again.");
        assert_eq!(m(ClaimOutcome::Failed(ClaimFailure::Refused)), "Couldn't reset usage. Please try again.");
    }

    #[test]
    fn only_unconfirmed_and_failed_claims_stay_unsettled() {
        assert!(ClaimOutcome::Success.settled() && ClaimOutcome::NoCredit.settled() && ClaimOutcome::NotNextReset { next: None }.settled());
        assert!(!ClaimOutcome::Unconfirmed.settled() && !ClaimOutcome::Failed(ClaimFailure::Refused).settled());
    }

    #[test]
    fn errors_map_to_failures() {
        assert_eq!(ClaimFailure::from(&ProviderError::SignInExpired), ClaimFailure::SignInExpired);
        assert_eq!(ClaimFailure::from(&ProviderError::ConnectionFailed), ClaimFailure::Connection);
        assert_eq!(ClaimFailure::from(&ProviderError::Unreachable("hub".into())), ClaimFailure::Connection);
        assert_eq!(ClaimFailure::from(&ProviderError::InvalidResponse), ClaimFailure::Refused);
    }

    #[test]
    fn the_timeline_orders_by_expiry_kind_then_record() {
        let item = |kind, at, claim| ResetItem { kind, expires_at: at, claim, usable: true, confirm_detail: None };
        let (early, late) = (at("2026-11-01T00:00:00Z"), at("2026-12-01T00:00:00Z"));
        let mut items = [
            item(ResetKind::Full, early, ResetRef::ClaudeGrant("b".into())),
            item(ResetKind::Session, late, ResetRef::ClaudeGrant("a".into())),
            item(ResetKind::Full, early, ResetRef::ClaudeGrant("a".into())),
            item(ResetKind::Session, early, ResetRef::ClaudeGrant("z".into())),
        ];
        items.sort_by(ResetItem::timeline_order);
        let ids: Vec<_> = items.iter().map(|i| (i.kind, i.claim.tie_breaker().1.to_string())).collect();
        assert_eq!(ids, [(ResetKind::Session, "z".into()), (ResetKind::Full, "a".into()), (ResetKind::Full, "b".into()), (ResetKind::Session, "a".into())]);
    }

    #[test]
    fn a_reset_ref_round_trips_as_json() {
        let refs = [
            ResetRef::ClaudeGrant("grant_1".into()),
            ResetRef::CodexCredit(at("2026-12-01T00:00:00Z")),
            ResetRef::Zai(Reset { window: crate::zai::ResetWindow::Week, record_id: 7, expires_at: at("2026-12-01T00:00:00Z"), grant_type: "GIFT".into() }),
        ];
        for r in refs {
            assert_eq!(serde_json::from_str::<ResetRef>(&serde_json::to_string(&r).unwrap()).unwrap(), r);
        }
        assert_eq!(serde_json::to_string(&ResetRef::ClaudeGrant("g".into())).unwrap(), r#"{"claudeGrant":"g"}"#);
    }

    #[tokio::test]
    async fn a_codex_claim_that_errors_still_hands_back_the_renewed_tokens() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let renewed = serde_json::json!({"access_token": "new", "refresh_token": "r2", "expires_in": 3600});
        Mock::given(method("POST")).and(path("/oauth/token")).respond_with(ResponseTemplate::new(200).set_body_json(renewed)).mount(&server).await;
        Mock::given(method("GET")).and(path("/backend-api/wham/rate-limit-reset-credits")).respond_with(ResponseTemplate::new(500)).mount(&server).await;
        let config = crate::codex::CodexConfig { token_url: format!("{}/oauth/token", server.uri()), api_base: format!("{}/backend-api", server.uri()), ..Default::default() };
        let now = at("2026-11-19T12:00:00Z");
        let mut tokens = OAuthTokens { access_token: "old".into(), refresh_token: Some("r1".into()), expires_at: Some(now - chrono::Duration::minutes(1)), extras: HashMap::new() };
        let outcome = claim_codex(&CodexClient::new(config), &mut tokens, "card", at("2026-12-01T00:00:00Z"), "req-1", now).await;
        assert_eq!(outcome, ClaimOutcome::Failed(ClaimFailure::Refused));
        assert_eq!((tokens.access_token.as_str(), tokens.refresh_token.as_deref()), ("new", Some("r2")));
        assert_eq!(matched_credit("req-1"), None);
    }
}
