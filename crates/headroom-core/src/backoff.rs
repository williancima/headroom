//! Refresh scheduling rules, pure: when an account is due and what a failure costs.
//! No timers or threads here; the loop that calls these lives in the app's worker.

use crate::provider::ProviderError;
use crate::store::ProviderState;
use crate::{Account, ProviderKind};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const INTERVAL: Duration = Duration::minutes(5);
pub const BACKOFF_BASE: Duration = INTERVAL;
pub const BACKOFF_CAP: Duration = Duration::hours(1);
/// A provider that has not answered after this long counts as failed.
pub const REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The error a timed-out refresh records. Like a refusal, it follows the doubling schedule.
pub fn timeout_error() -> ProviderError {
    ProviderError::Unavailable(format!("Refresh timed out after {}s", REFRESH_TIMEOUT.as_secs()))
}

/// How a failed refresh is scheduled again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FailureKind {
    /// The provider refused the sign-in or key, or answered with an error: doubling waits.
    Refused,
    /// The provider asked to wait, for `retry_after` if it said how long.
    RateLimited { retry_after: Option<Duration> },
    /// The request never reached the provider: no extra wait.
    Unreachable,
}

impl FailureKind {
    pub fn of(error: &ProviderError) -> Self {
        match error {
            ProviderError::RateLimited { retry_after_secs } => {
                // Clamp before converting so a huge Retry-After cannot overflow.
                let secs = retry_after_secs.map(|s| s.min(BACKOFF_CAP.num_seconds() as u64) as i64);
                Self::RateLimited { retry_after: secs.map(Duration::seconds) }
            }
            ProviderError::ConnectionFailed | ProviderError::Unreachable(_) => Self::Unreachable,
            _ => Self::Refused,
        }
    }
}

/// The wait after a failed refresh, kept in `provider_state.json` so it survives a relaunch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Backoff {
    pub failures: u32,
    pub retry_at: DateTime<Utc>,
    /// The provider set this wait itself (rate limit): a manual refresh does not skip it.
    pub provider_asked: bool,
    pub message: String,
}

/// 5 minutes, doubling with each failure in a row, up to an hour.
pub fn delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    (BACKOFF_BASE * 2i32.pow(doublings)).min(BACKOFF_CAP)
}

impl Backoff {
    fn after(kind: FailureKind, message: String, previous: Option<&Backoff>, now: DateTime<Utc>) -> Self {
        let failures = previous.map_or(0, |b| b.failures);
        match kind {
            FailureKind::Unreachable => Self { failures, retry_at: now + BACKOFF_BASE, provider_asked: false, message },
            FailureKind::Refused => Self { failures: failures + 1, retry_at: now + delay(failures + 1), provider_asked: false, message },
            FailureKind::RateLimited { retry_after } => {
                let wait = retry_after.unwrap_or_else(|| delay(failures + 1));
                Self { failures: failures + 1, retry_at: now + wait, provider_asked: true, message }
            }
        }
    }
}

/// Whether the account should be fetched now. A manual refresh (`forced`) skips every wait except one the
/// provider asked for.
pub fn due(state: &ProviderState, now: DateTime<Utc>, forced: bool) -> bool {
    state.backoff.as_ref().is_none_or(|b| !(now < b.retry_at && (b.provider_asked || !forced)))
}

/// Success clears the wait; a failure extends it.
pub fn record(state: &mut ProviderState, outcome: Result<(), &ProviderError>, now: DateTime<Utc>) {
    state.backoff = match outcome {
        Ok(()) => None,
        Err(error) => Some(Backoff::after(FailureKind::of(error), error.to_string(), state.backoff.as_ref(), now)),
    };
}

/// Claude budgets its usage endpoint per login, so after one Claude card is rate-limited every card on the
/// same login (`Account.identity`) waits too, and for at least the doubling delay even when `Retry-After`
/// was shorter. Call after `record`. Cards are keyed by `card_id`.
/// Left for the loop: skipping the request while the wait holds (`due` does it) and clearing the wait on
/// the next 2xx, which `record` does for the card that fetched.
pub fn share_claude_wait(source: &Account, accounts: &[Account], states: &mut HashMap<String, ProviderState>, now: DateTime<Utc>) {
    let Some(identity) = source.identity.as_ref().filter(|_| source.kind == ProviderKind::Claude) else { return };
    let Some(wait) = states.get_mut(&source.card_id()).and_then(|s| s.backoff.as_mut()).filter(|b| b.provider_asked) else { return };
    wait.retry_at = wait.retry_at.max(now + delay(wait.failures));
    let shared = wait.clone();
    for other in accounts.iter().filter(|a| a.kind == ProviderKind::Claude && a.identity.as_ref() == Some(identity)) {
        states.entry(other.card_id()).or_default().backoff = Some(shared.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-11-19T12:00:00Z".parse().unwrap()
    }

    fn refused() -> ProviderError {
        ProviderError::SignInExpired
    }

    fn limited(secs: Option<u64>) -> ProviderError {
        ProviderError::RateLimited { retry_after_secs: secs }
    }

    fn wait_of(state: &ProviderState) -> Duration {
        state.backoff.as_ref().unwrap().retry_at - now()
    }

    #[test]
    fn refused_doubles_up_to_the_cap() {
        let mut state = ProviderState::default();
        let waits: Vec<i64> = (0..6)
            .map(|_| {
                record(&mut state, Err(&refused()), now());
                wait_of(&state).num_minutes()
            })
            .collect();
        assert_eq!(waits, [5, 10, 20, 40, 60, 60]);
        assert!(!state.backoff.as_ref().unwrap().provider_asked);
    }

    #[test]
    fn every_non_network_error_is_refused() {
        for error in [ProviderError::KeyRejected("x".into()), ProviderError::InvalidResponse, ProviderError::RequestFailed(500), timeout_error()] {
            assert_eq!(FailureKind::of(&error), FailureKind::Refused);
        }
        assert_eq!(timeout_error().to_string(), "Refresh timed out after 120s");
    }

    #[test]
    fn unreachable_adds_no_wait_and_keeps_the_count() {
        let mut state = ProviderState::default();
        record(&mut state, Err(&refused()), now());
        record(&mut state, Err(&ProviderError::ConnectionFailed), now());
        assert_eq!(state.backoff.as_ref().unwrap().failures, 1);
        assert_eq!(wait_of(&state), BACKOFF_BASE);
        record(&mut state, Err(&ProviderError::Unreachable("hub".into())), now());
        assert_eq!(state.backoff.as_ref().unwrap().failures, 1);
        assert!(due(&state, now(), true));
    }

    #[test]
    fn rate_limit_uses_retry_after_or_the_schedule() {
        let mut state = ProviderState::default();
        record(&mut state, Err(&limited(Some(90))), now());
        assert_eq!(wait_of(&state), Duration::seconds(90));
        assert!(state.backoff.as_ref().unwrap().provider_asked);
        let mut state = ProviderState::default();
        record(&mut state, Err(&limited(None)), now());
        record(&mut state, Err(&limited(None)), now());
        assert_eq!(wait_of(&state), Duration::minutes(10));
    }

    #[test]
    fn retry_after_is_capped_at_an_hour() {
        let mut state = ProviderState::default();
        record(&mut state, Err(&limited(Some(u64::MAX))), now());
        assert_eq!(wait_of(&state), BACKOFF_CAP);
    }

    #[test]
    fn forced_skips_only_waits_the_provider_did_not_ask_for() {
        let mut state = ProviderState::default();
        assert!(due(&state, now(), false));
        record(&mut state, Err(&refused()), now());
        assert!(!due(&state, now(), false));
        assert!(due(&state, now(), true));
        assert!(due(&state, now() + Duration::minutes(5), false));
        record(&mut state, Err(&limited(Some(600))), now());
        assert!(!due(&state, now(), true));
        assert!(due(&state, now() + Duration::seconds(600), true));
    }

    #[test]
    fn success_clears_the_wait() {
        let mut state = ProviderState::default();
        record(&mut state, Err(&refused()), now());
        record(&mut state, Ok(()), now());
        assert_eq!(state.backoff, None);
        record(&mut state, Err(&refused()), now());
        assert_eq!(state.backoff.as_ref().unwrap().failures, 1);
    }

    fn claude(identity: Option<&str>) -> Account {
        let mut account = Account::new(ProviderKind::Claude, "Claude");
        account.identity = identity.map(String::from);
        account
    }

    #[test]
    fn claude_wait_is_shared_by_login() {
        let (a, b, other, nameless) = (claude(Some("acc/1")), claude(Some("acc/1")), claude(Some("acc/2")), claude(None));
        let accounts = [a.clone(), b.clone(), other.clone(), nameless.clone()];
        let mut states = HashMap::new();
        let mut state = ProviderState::default();
        record(&mut state, Err(&limited(Some(30))), now());
        states.insert(a.card_id(), state);
        share_claude_wait(&a, &accounts, &mut states, now());
        // Retry-After of 30 s is raised to the first doubling delay.
        let shared = states[&b.card_id()].backoff.clone().unwrap();
        assert_eq!(shared.retry_at - now(), Duration::minutes(5));
        assert!(!due(&states[&b.card_id()], now(), true));
        assert!(!states.contains_key(&other.card_id()) && !states.contains_key(&nameless.card_id()));
    }

    #[test]
    fn claude_wait_is_not_shared_for_other_failures() {
        let a = claude(Some("acc/1"));
        let mut states = HashMap::new();
        let mut state = ProviderState::default();
        record(&mut state, Err(&refused()), now());
        states.insert(a.card_id(), state);
        let b = claude(Some("acc/1"));
        share_claude_wait(&a, &[a.clone(), b.clone()], &mut states, now());
        assert!(!states.contains_key(&b.card_id()));
    }
}
