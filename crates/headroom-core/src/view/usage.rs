//! The All Usage tab: one group per provider, rows in the user's layout.

use super::unified::unified_rows;
use super::{join, Group, PanelModel, ResetEntry, Row};
use crate::claude::resets::LINE_LABEL;
use crate::layout::{line_id, UNIFIABLE};
use crate::priority::{duration, values_text, Meter, ReadStatus};
use crate::{Account, MetricLine, ProviderKind};

/// An account's status under its name: the wait it is in, the error, or how old the reading is
/// (notice first, then staleness).
fn status_text(model: &PanelModel, account: &Account) -> Option<String> {
    let card_id = account.card_id();
    let snapshot = model.snapshots.get(&card_id);
    match (model.status.get(&card_id), snapshot) {
        (Some(ReadStatus::Unavailable(message)), _) => Some(message.clone()),
        (_, None) => Some("Loading…".into()),
        (_, Some(s)) if model.now - s.refreshed_at >= model.old_after => {
            duration(s.refreshed_at, model.now).map(|d| format!("Last updated {d} ago"))
        }
        _ => None,
    }
}

pub(super) fn line_row(model: &PanelModel, kind: ProviderKind, line: &MetricLine) -> Row {
    let title = line.label().to_string();
    match line {
        MetricLine::Progress { .. } => {
            let meter = Meter::from_line(kind, line);
            Row {
                title,
                subtitle: meter.as_ref().and_then(|m| m.reset_text(model.now)).unwrap_or_default(),
                meter: meter.as_ref().map(|m| m.fill_fraction(false)),
                value: meter.map(|m| m.value_text(false)),
                ..Row::default()
            }
        }
        MetricLine::Values { values, .. } => Row { title, value: Some(values_text(values)), ..Row::default() },
        MetricLine::Badge { text, .. } => Row { title, value: Some(text.clone()), ..Row::default() },
    }
}

/// The account's lines the user sees: in the saved order, hidden ones skipped.
pub(super) fn shown_lines<'a>(model: &'a PanelModel, account: &Account) -> Vec<&'a MetricLine> {
    let card_id = account.card_id();
    let Some(snapshot) = model.snapshots.get(&card_id) else { return vec![] };
    let labels: Vec<String> = snapshot.lines.iter().map(|l| l.label().to_string()).collect();
    let order = model.layout.metric_order(&card_id, &labels);
    let shown = order.iter().filter(|label| !model.layout.hidden.contains(&line_id(&card_id, label)));
    shown.filter_map(|label| snapshot.line(label)).collect()
}

/// An account's header, its notice and its shown metrics.
fn account_rows(model: &PanelModel, account: &Account) -> Vec<Row> {
    let card_id = account.card_id();
    let snapshot = model.snapshots.get(&card_id);
    let plan = snapshot.and_then(|s| s.plan.clone());
    let mut rows = vec![Row { title: account.full_name(), subtitle: join([plan, status_text(model, account)]), card_id: Some(card_id.clone()), kind: account.kind.id().into(), ..Row::default() }];
    let Some(snapshot) = snapshot else { return rows };
    rows.extend(snapshot.notice.iter().map(|n| Row { title: n.clone(), ..Row::default() }));
    for line in shown_lines(model, account) {
        let id = line_id(&card_id, line.label());
        let mut row = line_row(model, account.kind, line);
        row.pinned = model.layout.pins.contains(&id);
        row.id = Some(id);
        row.breakdown = snapshot.breakdowns.get(line.label()).map(|b| b.view(model.now));
        if row.breakdown.is_some() {
            row.card_id = Some(card_id.clone());
        }
        if line.label() == LINE_LABEL && !snapshot.reset_items.is_empty() {
            row.card_id = Some(card_id.clone());
            row.resets = snapshot.reset_items.iter().map(|i| ResetEntry::new(i, model.now)).collect();
        }
        rows.push(row);
    }
    rows
}

/// One group per provider in the order providers first appear in the saved accounts; never sorted.
pub fn usage_groups(model: &PanelModel) -> Vec<Group> {
    let mut kinds: Vec<ProviderKind> = vec![];
    for account in &model.accounts {
        if !kinds.contains(&account.kind) {
            kinds.push(account.kind);
        }
    }
    kinds
        .into_iter()
        .map(|kind| {
            let accounts: Vec<&Account> = model.accounts.iter().filter(|a| a.kind == kind).collect();
            let unifiable = UNIFIABLE.contains(&kind) && accounts.len() > 1;
            let unified = unifiable && model.layout.unified.contains(&kind);
            let rows = if unified { unified_rows(model, kind, &accounts) } else { accounts.iter().flat_map(|a| account_rows(model, a)).collect() };
            Group { title: kind.display_name().to_string(), kind: kind.id().into(), unifiable, unified, rows }
        })
        .collect()
}
