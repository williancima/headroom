//! What the panel remembers about resets between redraws, and the `claim-reset` action's input. Pure, so it
//! tests without GTK. Every model rebuilds the rows, so the open lists, the confirms and the request ids live here.

use headroom_core::claim::ResetRef;
use headroom_core::view::ResetEntry;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

/// One row of one account's list. Identical resets (Claude repeats a grant per reset left) are told apart by `nth`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub card_id: String,
    pub claim: ResetRef,
    pub nth: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Confirming,
    Claiming,
}

/// `card_id` is the row's account; an entry of a unified card names its own.
pub fn keys(card_id: &str, entries: &[ResetEntry]) -> Vec<Key> {
    let mut seen: HashMap<&ResetRef, usize> = HashMap::new();
    entries
        .iter()
        .map(|e| {
            let nth = seen.entry(&e.claim).and_modify(|n| *n += 1).or_insert(0);
            Key { card_id: e.card_id.as_deref().unwrap_or(card_id).into(), claim: e.claim.clone(), nth: *nth }
        })
        .collect()
}

#[derive(Default)]
pub struct Flow {
    open: HashSet<String>,
    steps: HashMap<Key, Step>,
    /// Minted when a confirm opens and kept until the claim settles, so a retry can't spend a second reset.
    request_ids: HashMap<(String, ResetRef), String>,
    claiming: HashMap<String, Key>,
}

impl Flow {
    pub fn is_open(&self, card_id: &str) -> bool {
        self.open.contains(card_id)
    }

    pub fn set_open(&mut self, card_id: &str, open: bool) {
        if open {
            self.open.insert(card_id.into());
        } else {
            self.open.remove(card_id);
        }
    }

    pub fn step(&self, key: &Key) -> Option<Step> {
        self.steps.get(key).copied()
    }

    /// Use was clicked.
    pub fn ask(&mut self, key: Key) {
        if self.steps.contains_key(&key) {
            return;
        }
        self.request_ids.entry((key.card_id.clone(), key.claim.clone())).or_insert_with(|| uuid::Uuid::new_v4().to_string());
        self.steps.insert(key, Step::Confirming);
    }

    pub fn cancel(&mut self, key: &Key) {
        if self.steps.get(key) == Some(&Step::Confirming) {
            self.steps.remove(key);
        }
    }

    /// The confirm's Use. Returns the request id to send; `None` when there is nothing to confirm, or an
    /// identical reset is already being claimed under that id.
    pub fn confirm(&mut self, key: &Key) -> Option<String> {
        if self.steps.get(key) != Some(&Step::Confirming) {
            return None;
        }
        let request_id = self.request_ids.get(&(key.card_id.clone(), key.claim.clone()))?.clone();
        if self.claiming.contains_key(&request_id) {
            return None;
        }
        self.steps.insert(key.clone(), Step::Claiming);
        self.claiming.insert(request_id.clone(), key.clone());
        Some(request_id)
    }

    /// The claim answered. A settled one retires its request id; an unsettled one keeps it for the retry.
    pub fn finish(&mut self, request_id: &str, settled: bool) {
        if let Some(key) = self.claiming.remove(request_id) {
            self.steps.remove(&key);
            if settled {
                self.request_ids.remove(&(key.card_id, key.claim));
            }
        }
    }

    /// A refresh that drops a row cancels its confirm.
    pub fn reconcile(&mut self, live: &HashSet<Key>) {
        self.steps.retain(|key, step| *step == Step::Claiming || live.contains(key));
    }
}

/// The `claim-reset` action's parameter: `{"card_id":..,"claim":<ResetRef>,"request_id":..}`.
#[derive(Debug, Deserialize, PartialEq)]
pub struct ClaimRequest {
    pub card_id: String,
    pub claim: ResetRef,
    pub request_id: String,
}

/// Validated at the boundary: the request id is what Anthropic and OpenAI see, so it has to be a plain token.
pub fn parse_claim_request(text: &str) -> Result<ClaimRequest, String> {
    let request: ClaimRequest = serde_json::from_str(text).map_err(|e| format!("not a claim request: {e}"))?;
    let plain = |s: &str| (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if request.card_id.is_empty() || !plain(&request.request_id) {
        return Err("a claim request needs a card id and a request id of letters, digits, '-' and '_' (up to 64)".into());
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(nth: usize) -> Key {
        Key { card_id: "claude:a".into(), claim: ResetRef::ClaudeGrant("g".into()), nth }
    }

    #[test]
    fn use_confirms_then_claims_once_and_a_settled_claim_retires_its_id() {
        let mut flow = Flow::default();
        assert_eq!(flow.confirm(&key(0)), None);
        flow.ask(key(0));
        assert_eq!(flow.step(&key(0)), Some(Step::Confirming));
        let id = flow.confirm(&key(0)).unwrap();
        assert_eq!(flow.step(&key(0)), Some(Step::Claiming));
        assert_eq!(flow.confirm(&key(0)), None);
        // An identical reset waits while the first is in flight under the same id.
        flow.ask(key(1));
        assert_eq!(flow.confirm(&key(1)), None);
        flow.finish(&id, true);
        assert_eq!(flow.step(&key(0)), None);
        flow.cancel(&key(1));
        flow.ask(key(0));
        assert_ne!(flow.confirm(&key(0)).unwrap(), id);
    }

    #[test]
    fn an_entry_of_a_unified_card_claims_on_its_own_account() {
        let entry = |card_id: Option<&str>| ResetEntry {
            title: String::new(),
            expires: String::new(),
            usable: true,
            confirm_detail: None,
            claim: ResetRef::ClaudeGrant("g".into()),
            card_id: card_id.map(String::from),
        };
        let keys = keys("claude:first", &[entry(Some("claude:second")), entry(None)]);
        assert_eq!((keys[0].card_id.as_str(), keys[1].card_id.as_str()), ("claude:second", "claude:first"));
    }

    #[test]
    fn an_unsettled_claim_is_retried_under_the_same_id() {
        let mut flow = Flow::default();
        flow.ask(key(0));
        let id = flow.confirm(&key(0)).unwrap();
        flow.finish(&id, false);
        flow.ask(key(0));
        assert_eq!(flow.confirm(&key(0)), Some(id));
    }

    #[test]
    fn cancel_and_a_dropped_row_end_a_confirm_but_not_a_claim_in_flight() {
        let mut flow = Flow::default();
        flow.ask(key(0));
        flow.cancel(&key(0));
        assert_eq!(flow.step(&key(0)), None);
        flow.ask(key(0));
        flow.reconcile(&HashSet::new());
        assert_eq!(flow.step(&key(0)), None);
        flow.ask(key(0));
        flow.confirm(&key(0));
        flow.reconcile(&HashSet::new());
        assert_eq!(flow.step(&key(0)), Some(Step::Claiming));
        flow.set_open("c", true);
        assert!(flow.is_open("c"));
        flow.set_open("c", false);
        assert!(!flow.is_open("c"));
    }

    #[test]
    fn claim_requests_parse_and_bad_input_is_refused() {
        let ok = r#"{"card_id":"claude:a","claim":{"claudeGrant":"g1"},"request_id":"3f2a-b_9"}"#;
        let parsed = parse_claim_request(ok).unwrap();
        assert_eq!((parsed.card_id.as_str(), parsed.claim, parsed.request_id.as_str()), ("claude:a", ResetRef::ClaudeGrant("g1".into()), "3f2a-b_9"));
        for bad in [
            "",
            "not json",
            r#"{"card_id":"a","claim":{"claudeGrant":"g"}}"#,
            r#"{"card_id":"a","claim":{"nope":1},"request_id":"r"}"#,
            r#"{"card_id":"","claim":{"claudeGrant":"g"},"request_id":"r"}"#,
            r#"{"card_id":"a","claim":{"claudeGrant":"g"},"request_id":"bad id!"}"#,
            r#"{"card_id":"a","claim":{"claudeGrant":"g"},"request_id":""}"#,
        ] {
            assert!(parse_claim_request(bad).is_err(), "{bad}");
        }
    }
}
