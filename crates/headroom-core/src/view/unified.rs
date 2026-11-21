//! Unify Multiple Accounts: a provider's accounts shown as one card with each limit once.

use super::usage::{line_row, shown_lines};
use super::{PanelModel, ResetEntry, Row};
use crate::claude::resets::LINE_LABEL;
use crate::layout::line_id;
use crate::priority::Meter;
use crate::{Account, MetricLine, MetricValue, ProgressFormat, ProviderKind};
use std::collections::BTreeSet;

/// Every account's own order, kept; a label one account lacks goes just before the next
/// label it shares with the others.
pub(super) fn merged_order(orders: &[Vec<String>]) -> Vec<String> {
    let mut merged: Vec<String> = vec![];
    for order in orders {
        for (index, key) in order.iter().enumerate() {
            if merged.contains(key) {
                continue;
            }
            let next = order[index + 1..].iter().find_map(|k| merged.iter().position(|m| m == k));
            merged.insert(next.unwrap_or(merged.len()), key.clone());
        }
    }
    merged
}

/// The least a pooled bar shows short of full.
fn least_shown(format: &ProgressFormat) -> f64 {
    match format {
        ProgressFormat::Percent => 1.0,
        ProgressFormat::Count(_) => 0.1,
        ProgressFormat::Dollars | ProgressFormat::Currency(_) => 0.01,
    }
}

/// Pooling: limits add up what each account used, up to its cap, against what each allows, so
/// percents average, and a limit is used up only when every account's is. Limits in different units add up what
/// each spent.
pub(super) fn pool(kind: ProviderKind, lines: &[&MetricLine]) -> MetricLine {
    let first = lines[0];
    let label = first.label().to_string();
    if let MetricLine::Progress { format, .. } = first {
        if lines.iter().all(|l| matches!(l, MetricLine::Progress { format: f, .. } if f == format)) {
            let (mut used, mut limit, mut reset) = (0.0, 0.0, None);
            for line in lines {
                if let MetricLine::Progress { used: u, limit: l, resets_at, .. } = line {
                    // An account over its own cap can't spend another account's room.
                    (used, limit) = (used + u.min(*l), limit + l);
                    reset = [reset, *resets_at].into_iter().flatten().min();
                }
            }
            if limit <= 0.0 {
                return first.clone();
            }
            let scale = if *format == ProgressFormat::Percent { 100.0 / limit } else { 1.0 };
            let make = |used: f64| MetricLine::Progress { label: label.clone(), used, limit: limit * scale, format: format.clone(), resets_at: reset, period_ms: None };
            let spent = |l: &MetricLine| Meter::from_line(kind, l).is_some_and(|m| m.is_spent());
            let pooled = make(used * scale);
            // Shown just under full unless every account is spent.
            return if spent(&pooled) && lines.iter().any(|l| !spent(l)) { make(limit * scale - least_shown(format)) } else { pooled };
        }
    }
    let mut badges: Vec<&str> = vec![];
    for line in lines {
        if let MetricLine::Badge { text, .. } = line {
            if !badges.contains(&text.as_str()) {
                badges.push(text);
            }
        }
    }
    if !badges.is_empty() {
        return MetricLine::Badge { label, text: badges.join(" · ") };
    }
    // Values add up by label and currency in first-appearance order; a bounded line adds what it used.
    let mut sums: Vec<MetricValue> = vec![];
    for value in lines.iter().flat_map(|l| line_values(l)) {
        match sums.iter_mut().find(|s| s.label == value.label && s.currency_code == value.currency_code) {
            Some(sum) => sum.number += value.number,
            None => sums.push(value),
        }
    }
    MetricLine::Values { label, values: sums }
}

fn line_values(line: &MetricLine) -> Vec<MetricValue> {
    match line {
        MetricLine::Values { values, .. } => values.clone(),
        MetricLine::Progress { used, format, .. } => {
            let (currency_code, label) = match format {
                ProgressFormat::Dollars => (Some("USD".to_string()), None),
                ProgressFormat::Currency(code) => (Some(code.clone()), None),
                ProgressFormat::Count(unit) => (None, Some(unit.clone())),
                ProgressFormat::Percent => (None, None),
            };
            vec![MetricValue { number: *used, currency_code, label }]
        }
        MetricLine::Badge { .. } => vec![],
    }
}

/// The unified card's rows: one header, then each label once. Unlike a plain account it shows only the plan (an
/// account's status and notice stay on the split view), and it carries no history breakdown.
pub(super) fn unified_rows(model: &PanelModel, kind: ProviderKind, accounts: &[&Account]) -> Vec<Row> {
    let plans: BTreeSet<&str> = accounts.iter().filter_map(|a| model.snapshots.get(&a.card_id())?.plan.as_deref()).collect();
    let plan = if plans.len() > 1 { "Multiple".to_string() } else { plans.into_iter().next().unwrap_or_default().to_string() };
    let mut rows = vec![Row { title: format!("{} Accounts", accounts.len()), subtitle: plan, kind: kind.id().into(), ..Row::default() }];
    let shown: Vec<Vec<&MetricLine>> = accounts.iter().map(|a| shown_lines(model, a)).collect();
    let orders: Vec<Vec<String>> = shown.iter().map(|lines| lines.iter().map(|l| l.label().to_string()).collect()).collect();
    let unified_id = format!("{}.unified", kind.id());
    for label in merged_order(&orders) {
        let members: Vec<(&&Account, &MetricLine)> =
            accounts.iter().zip(&shown).filter_map(|(a, lines)| Some((a, *lines.iter().find(|l| l.label() == label)?))).collect();
        let pooled = pool(kind, &members.iter().map(|m| m.1).collect::<Vec<_>>());
        let copy_ids: Vec<String> = accounts.iter().map(|a| line_id(&a.card_id(), &label)).collect();
        let mut row = line_row(model, kind, &pooled);
        row.id = Some(line_id(&unified_id, &label));
        row.pinned = copy_ids.iter().any(|id| model.layout.pins.contains(id));
        row.members = members
            .iter()
            .map(|(account, line)| {
                let id = line_id(&account.card_id(), &label);
                Row { title: account.full_name(), pinned: model.layout.pins.contains(&id), id: Some(id), ..line_row(model, kind, line) }
            })
            .collect();
        row.copy_ids = copy_ids;
        if label == LINE_LABEL {
            row.resets = accounts
                .iter()
                .filter_map(|a| Some((a.card_id(), &model.snapshots.get(&a.card_id())?.reset_items)))
                .flat_map(|(card_id, items)| items.iter().map(move |i| ResetEntry { card_id: Some(card_id.clone()), ..ResetEntry::new(i, model.now) }))
                .collect();
            row.card_id = row.resets.first().and_then(|e| e.card_id.clone());
        }
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests;
