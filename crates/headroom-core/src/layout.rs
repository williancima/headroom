//! The user's customization: hidden and pinned metrics, metric
//! order and unified providers in `layout.json`; account and provider order stay in `accounts.json`.

use crate::{Account, ProviderKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The providers whose accounts can be unified.
pub const UNIFIABLE: [ProviderKind; 2] = [ProviderKind::Claude, ProviderKind::Codex];

/// A metric's id: its account's card id and its label. Priority pins use the same id.
pub fn line_id(card_id: &str, label: &str) -> String {
    format!("{card_id}/{label}")
}

/// Every field defaults, so a file written by an older build (or none) loads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    /// Line ids.
    #[serde(default)]
    pub hidden: BTreeSet<String>,
    /// Line ids pinned to Priority.
    #[serde(default)]
    pub pins: BTreeSet<String>,
    /// Card id to the labels in the user's order.
    #[serde(default)]
    pub metric_order: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub unified: BTreeSet<ProviderKind>,
}

impl Layout {
    /// Hiding a metric unpins it. Returns whether anything changed.
    pub fn set_hidden(&mut self, ids: &[String], hidden: bool) -> bool {
        let mut changed = false;
        for id in ids {
            changed |= if hidden { self.hidden.insert(id.clone()) | self.pins.remove(id) } else { self.hidden.remove(id) };
        }
        changed
    }

    /// Only a shown metric can be pinned.
    pub fn set_pinned(&mut self, ids: &[String], pinned: bool) -> bool {
        let mut changed = false;
        for id in ids {
            changed |= if pinned { !self.hidden.contains(id) && self.pins.insert(id.clone()) } else { self.pins.remove(id) };
        }
        changed
    }

    /// Only the `UNIFIABLE` providers.
    pub fn set_unified(&mut self, kind: ProviderKind, on: bool) -> bool {
        UNIFIABLE.contains(&kind) && if on { self.unified.insert(kind) } else { self.unified.remove(&kind) }
    }

    /// The saved labels that still exist, then new labels in snapshot order.
    pub fn metric_order(&self, card_id: &str, labels: &[String]) -> Vec<String> {
        let mut order: Vec<String> = vec![];
        for label in self.metric_order.get(card_id).into_iter().flatten().chain(labels) {
            if labels.contains(label) && !order.contains(label) {
                order.push(label.clone());
            }
        }
        order
    }

    /// Stores the whole order.
    pub fn move_metric(&mut self, card_id: &str, current_labels: &[String], dragged: &str, target: &str) -> bool {
        let Some(next) = reordered(&self.metric_order(card_id, current_labels), dragged, target) else { return false };
        self.metric_order.insert(card_id.to_string(), next);
        true
    }

    /// Drops what belongs to removed accounts; the caller runs it before saving. Returns whether anything went.
    pub fn retain_accounts(&mut self, card_ids: &[String]) -> bool {
        let before = (self.hidden.len(), self.pins.len(), self.metric_order.len());
        let kept = |id: &String| card_ids.iter().any(|c| Some(c.as_str()) == id.split('/').next());
        self.hidden.retain(kept);
        self.pins.retain(kept);
        self.metric_order.retain(|id, _| card_ids.contains(id));
        before != (self.hidden.len(), self.pins.len(), self.metric_order.len())
    }
}

/// `dragged` goes after `target` when moving down, before it when moving up.
/// None when either is missing or they are the same.
pub fn reordered(ids: &[String], dragged: &str, target: &str) -> Option<Vec<String>> {
    let from = ids.iter().position(|i| i == dragged).filter(|_| dragged != target)?;
    let to = ids.iter().position(|i| i == target)?;
    let mut next = ids.to_vec();
    let id = next.remove(from);
    let adjusted = next.iter().position(|i| i == target)?;
    next.insert(if from < to { adjusted + 1 } else { adjusted }, id);
    Some(next)
}

/// Accounts of one provider, or the only two accounts of two providers. The caller saves.
pub fn move_account(accounts: &mut [Account], dragged: &str, target: &str) -> bool {
    let kind_of = |card_id: &str| accounts.iter().find(|a| a.card_id() == card_id).map(|a| a.kind);
    let (Some(from), Some(to)) = (kind_of(dragged), kind_of(target)) else { return false };
    if from != to && accounts.iter().filter(|a| a.kind == from || a.kind == to).count() != 2 {
        return false;
    }
    let ids: Vec<String> = accounts.iter().map(Account::card_id).collect();
    let Some(next) = reordered(&ids, dragged, target) else { return false };
    accounts.sort_by_key(|a| next.iter().position(|id| *id == a.card_id()));
    true
}

/// The whole group of a provider's accounts moves. The caller saves.
pub fn move_provider(accounts: &mut [Account], dragged: ProviderKind, target: ProviderKind) -> bool {
    let mut families: Vec<String> = vec![];
    for a in accounts.iter() {
        if !families.iter().any(|f| f == a.kind.id()) {
            families.push(a.kind.id().into());
        }
    }
    let Some(next) = reordered(&families, dragged.id(), target.id()) else { return false };
    accounts.sort_by_key(|a| next.iter().position(|f| f == a.kind.id()));
    true
}

#[cfg(test)]
mod tests;
