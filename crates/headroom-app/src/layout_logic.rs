//! What the customization actions, menus, drags and the Accounts metric list need, as plain data so it tests
//! without GTK: the action parameters, what a drag carries, and which ids a menu acts on.

use crate::Command;
use headroom_core::layout::{line_id, Layout};
use headroom_core::view::{Group, Row};
use headroom_core::{Account, ProviderKind, ProviderSnapshot};
use relm4::gtk::glib::Variant;
use relm4::gtk::prelude::ToVariant;
use std::collections::HashMap;

/// The actions the app registers, with their parameter types (also what the GNOME Shell extension calls).
pub const ACTIONS: [(&str, &str); 6] = [
    ("set-hidden", "(asb)"),
    ("set-pinned", "(asb)"),
    ("set-unified", "(sb)"),
    ("move-metric", "(sss)"),
    ("move-account", "(ss)"),
    ("move-provider", "(ss)"),
];

/// The command an action's parameter asks for; Err names what was wrong.
pub fn command(name: &str, param: &Variant) -> Result<Command, String> {
    let bad = || format!("bad parameter for {name}: {}", param.type_());
    let kind = |id: &str| ProviderKind::parse(id).ok_or_else(|| format!("unknown provider {id}"));
    Ok(match name {
        "set-hidden" => param.get::<(Vec<String>, bool)>().map(|(ids, hidden)| Command::SetHidden { ids, hidden }).ok_or_else(bad)?,
        "set-pinned" => param.get::<(Vec<String>, bool)>().map(|(ids, pinned)| Command::SetPinned { ids, pinned }).ok_or_else(bad)?,
        "set-unified" => {
            let (id, on) = param.get::<(String, bool)>().ok_or_else(bad)?;
            Command::SetUnified { kind: kind(&id)?, on }
        }
        "move-metric" => {
            param.get::<(String, String, String)>().map(|(card_id, dragged, target)| Command::MoveMetric { card_id, dragged, target }).ok_or_else(bad)?
        }
        "move-account" => param.get::<(String, String)>().map(|(dragged, target)| Command::MoveAccount { dragged, target }).ok_or_else(bad)?,
        "move-provider" => {
            let (dragged, target) = param.get::<(String, String)>().ok_or_else(bad)?;
            Command::MoveProvider { dragged: kind(&dragged)?, target: kind(&target)? }
        }
        other => return Err(format!("unknown action {other}")),
    })
}

/// What a drag carries, and what it can drop on. Travels as a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drag {
    Metric { card_id: String, label: String },
    Account { card_id: String },
    Provider { kind: String },
}

const SEP: char = '\u{1f}';

impl Drag {
    pub fn encode(&self) -> String {
        match self {
            Self::Metric { card_id, label } => format!("metric{SEP}{card_id}{SEP}{label}"),
            Self::Account { card_id } => format!("account{SEP}{card_id}"),
            Self::Provider { kind } => format!("provider{SEP}{kind}"),
        }
    }

    pub fn decode(text: &str) -> Option<Self> {
        let parts: Vec<&str> = text.split(SEP).collect();
        match parts[..] {
            ["metric", card_id, label] => Some(Self::Metric { card_id: card_id.into(), label: label.into() }),
            ["account", card_id] => Some(Self::Account { card_id: card_id.into() }),
            ["provider", kind] => Some(Self::Provider { kind: kind.into() }),
            _ => None,
        }
    }

    /// The action a drop on `target` calls; None across types, or across accounts for metrics. The core refuses the rest.
    pub fn dropped_on(&self, target: &Self) -> Option<(&'static str, Variant)> {
        match (self, target) {
            (Self::Metric { card_id, label }, Self::Metric { card_id: other, label: to }) if card_id == other => {
                Some(("move-metric", (card_id.as_str(), label.as_str(), to.as_str()).to_variant()))
            }
            (Self::Account { card_id }, Self::Account { card_id: to }) => Some(("move-account", (card_id.as_str(), to.as_str()).to_variant())),
            (Self::Provider { kind }, Self::Provider { kind: to }) => Some(("move-provider", (kind.as_str(), to.as_str()).to_variant())),
            _ => None,
        }
    }
}

/// What a row of a group is, for the drags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    Header(String),
    /// The account it belongs to; None on a unified row, which has no single account.
    Metric(Option<String>),
    Other,
}

pub fn roles(group: &Group) -> Vec<Role> {
    let mut account: Option<&str> = None;
    group
        .rows
        .iter()
        .map(|row| match (&row.id, &row.card_id) {
            (None, Some(card_id)) if row.resets.is_empty() && row.breakdown.is_none() => {
                account = Some(card_id);
                Role::Header(card_id.clone())
            }
            (Some(_), _) if group.unified || !row.members.is_empty() => Role::Metric(None),
            (Some(_), _) => Role::Metric(account.map(String::from)),
            _ => Role::Other,
        })
        .collect()
}

/// The line ids Pin and Hide act on: every account's copy on a unified row.
pub fn menu_ids(row: &Row) -> Option<Vec<String>> {
    let id = row.id.as_ref()?;
    Some(if row.copy_ids.is_empty() { vec![id.clone()] } else { row.copy_ids.clone() })
}

/// One metric of an account on the Accounts page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricSetting {
    pub label: String,
    pub id: String,
    pub shown: bool,
    pub pinned: bool,
}

/// The account's metrics in the user's order, hidden ones included.
pub fn metric_settings(account: &Account, snapshots: &HashMap<String, ProviderSnapshot>, layout: &Layout) -> Vec<MetricSetting> {
    let card_id = account.card_id();
    let Some(snapshot) = snapshots.get(&card_id) else { return vec![] };
    let labels: Vec<String> = snapshot.lines.iter().map(|l| l.label().to_string()).collect();
    layout
        .metric_order(&card_id, &labels)
        .into_iter()
        .map(|label| {
            let id = line_id(&card_id, &label);
            MetricSetting { shown: !layout.hidden.contains(&id), pinned: layout.pins.contains(&id), label, id }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use headroom_core::history::BreakdownView;
    use headroom_core::MetricLine;

    fn row(id: Option<&str>, card_id: Option<&str>) -> Row {
        Row { id: id.map(String::from), card_id: card_id.map(String::from), ..Row::default() }
    }

    fn group(unified: bool, rows: Vec<Row>) -> Group {
        Group { title: "Claude".into(), kind: "claude".into(), unifiable: true, unified, rows }
    }

    #[test]
    fn actions_parse_into_commands_and_bad_input_is_refused() {
        let ids = vec!["a@1/Session".to_string()];
        assert!(matches!(command("set-hidden", &(ids.clone(), true).to_variant()), Ok(Command::SetHidden { hidden: true, .. })));
        assert!(matches!(command("set-pinned", &(ids, false).to_variant()), Ok(Command::SetPinned { pinned: false, .. })));
        assert!(matches!(command("set-unified", &("claude", true).to_variant()), Ok(Command::SetUnified { kind: ProviderKind::Claude, on: true })));
        assert!(command("set-unified", &("nope", true).to_variant()).is_err());
        assert!(matches!(command("move-metric", &("c", "a", "b").to_variant()), Ok(Command::MoveMetric { .. })));
        assert!(matches!(command("move-account", &("c", "d").to_variant()), Ok(Command::MoveAccount { .. })));
        assert!(matches!(command("move-provider", &("claude", "codex").to_variant()), Ok(Command::MoveProvider { .. })));
        assert!(command("move-provider", &("claude", "nope").to_variant()).is_err());
        assert!(command("move-account", &"text".to_variant()).is_err());
        assert!(command("other", &"text".to_variant()).is_err());
    }

    #[test]
    fn a_drag_round_trips_and_drops_only_on_its_own_kind() {
        let metric = Drag::Metric { card_id: "c@1".into(), label: "Weekly".into() };
        assert_eq!(Drag::decode(&metric.encode()), Some(metric.clone()));
        assert_eq!(Drag::decode("junk"), None);
        let other = Drag::Metric { card_id: "c@1".into(), label: "Session".into() };
        assert_eq!(metric.dropped_on(&other).map(|a| a.0), Some("move-metric"));
        assert!(metric.dropped_on(&Drag::Metric { card_id: "c@2".into(), label: "Session".into() }).is_none());
        let account = Drag::Account { card_id: "c@1".into() };
        assert!(metric.dropped_on(&account).is_none());
        assert_eq!(account.dropped_on(&Drag::Account { card_id: "c@2".into() }).map(|a| a.0), Some("move-account"));
        let provider = Drag::Provider { kind: "claude".into() };
        assert_eq!(provider.dropped_on(&Drag::Provider { kind: "codex".into() }).map(|a| a.0), Some("move-provider"));
        assert!(provider.dropped_on(&account).is_none());
    }

    #[test]
    fn a_metric_row_belongs_to_the_header_before_it_and_unified_rows_to_none() {
        let mut breakdown = row(Some("a@1/Today"), Some("a@1"));
        breakdown.breakdown = Some(BreakdownView { title: String::new(), empty: true, figures: vec![], token_parts: vec![], models_label: None, models: vec![] });
        let rows = vec![row(None, Some("a@1")), row(Some("a@1/Session"), None), breakdown, row(None, None), row(None, Some("a@2")), row(Some("a@2/Session"), None)];
        let roles = roles(&group(false, rows));
        assert_eq!(roles[0], Role::Header("a@1".into()));
        assert_eq!(roles[1], Role::Metric(Some("a@1".into())));
        assert_eq!(roles[2], Role::Metric(Some("a@1".into())));
        assert_eq!(roles[3], Role::Other);
        assert_eq!(roles[5], Role::Metric(Some("a@2".into())));
        let unified = roles_of_unified();
        assert_eq!(unified, vec![Role::Metric(None)]);
    }

    fn roles_of_unified() -> Vec<Role> {
        roles(&group(true, vec![row(Some("claude.unified/Session"), None)]))
    }

    #[test]
    fn a_menu_acts_on_every_copy_of_a_unified_row_and_on_the_id_otherwise() {
        let mut unified = row(Some("claude.unified/Session"), None);
        unified.copy_ids = vec!["a@1/Session".into(), "a@2/Session".into()];
        assert_eq!(menu_ids(&unified), Some(vec!["a@1/Session".to_string(), "a@2/Session".to_string()]));
        assert_eq!(menu_ids(&row(Some("a@1/Weekly"), None)), Some(vec!["a@1/Weekly".to_string()]));
        assert_eq!(menu_ids(&row(None, Some("a@1"))), None);
    }

    #[test]
    fn the_accounts_page_lists_every_metric_in_order_with_hidden_and_pinned_marked() {
        let account = Account::new(ProviderKind::Claude, "w");
        let card_id = account.card_id();
        let badge = |l: &str| MetricLine::Badge { label: l.into(), text: "x".into() };
        let snapshot = ProviderSnapshot {
            plan: None,
            lines: vec![badge("Session"), badge("Weekly")],
            refreshed_at: chrono::Utc::now(),
            reset_expiries: vec![],
            reset_items: vec![],
            notice: None,
            breakdowns: HashMap::new(),
        };
        let mut layout = Layout::default();
        layout.metric_order.insert(card_id.clone(), vec!["Weekly".into(), "Session".into()]);
        layout.hidden.insert(line_id(&card_id, "Session"));
        layout.pins.insert(line_id(&card_id, "Weekly"));
        let list = metric_settings(&account, &HashMap::from([(card_id.clone(), snapshot)]), &layout);
        assert_eq!(list.iter().map(|m| (m.label.as_str(), m.shown, m.pinned)).collect::<Vec<_>>(), [("Weekly", true, true), ("Session", false, false)]);
        assert!(metric_settings(&account, &HashMap::new(), &layout).is_empty());
    }
}
