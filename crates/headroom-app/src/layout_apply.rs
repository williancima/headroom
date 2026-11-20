//! Applies a customization command to what is on disk: `layout.json` for hidden, pinned, unified and metric
//! order, `accounts.json` for account and provider order. Blocking; the worker runs it off the async threads.

use crate::Command;
use headroom_core::layout::{move_account, move_provider};
use headroom_core::store::Store;
use headroom_core::Account;

/// Whether anything changed and was saved. Other commands change nothing.
pub fn apply(store: &Store, command: &Command) -> std::io::Result<bool> {
    match command {
        Command::MoveAccount { dragged, target } => reorder(store, |accounts| move_account(accounts, dragged, target)),
        Command::MoveProvider { dragged, target } => reorder(store, |accounts| move_provider(accounts, *dragged, *target)),
        Command::SetHidden { .. } | Command::SetPinned { .. } | Command::SetUnified { .. } | Command::MoveMetric { .. } => {
            let mut layout = store.layout()?;
            let changed = match command {
                Command::SetHidden { ids, hidden } => layout.set_hidden(ids, *hidden),
                Command::SetPinned { ids, pinned } => layout.set_pinned(ids, *pinned),
                Command::SetUnified { kind, on } => layout.set_unified(*kind, *on),
                Command::MoveMetric { card_id, dragged, target } => {
                    let snapshots = store.snapshots()?;
                    let labels: Vec<String> = snapshots.get(card_id).map(|s| s.lines.iter().map(|l| l.label().to_string()).collect()).unwrap_or_default();
                    layout.move_metric(card_id, &labels, dragged, target)
                }
                _ => false,
            };
            if changed {
                let card_ids: Vec<String> = store.accounts()?.iter().map(Account::card_id).collect();
                layout.retain_accounts(&card_ids);
                store.save_layout(&layout)?;
            }
            Ok(changed)
        }
        _ => Ok(false),
    }
}

fn reorder(store: &Store, op: impl FnOnce(&mut [Account]) -> bool) -> std::io::Result<bool> {
    let mut accounts = store.accounts()?;
    let changed = op(&mut accounts);
    if changed {
        store.save_accounts(&accounts)?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use headroom_core::{MetricLine, ProviderKind, ProviderSnapshot};
    use std::collections::HashMap;

    fn temp_store() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("headroom-layout-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (Store::at(&dir, &dir), dir)
    }

    fn snapshot(labels: &[&str]) -> ProviderSnapshot {
        let lines = labels.iter().map(|l| MetricLine::Badge { label: (*l).into(), text: "x".into() }).collect();
        ProviderSnapshot { plan: None, lines, refreshed_at: Utc::now(), reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: HashMap::new() }
    }

    #[test]
    fn layout_commands_save_and_a_removed_accounts_ids_go() {
        let (store, dir) = temp_store();
        let (a, b) = (Account::new(ProviderKind::Claude, "a"), Account::new(ProviderKind::Claude, "b"));
        let (ida, idb) = (a.card_id(), b.card_id());
        store.save_accounts(&[a, b]).unwrap();
        store.save_snapshots(&HashMap::from([(ida.clone(), snapshot(&["Session", "Weekly"]))])).unwrap();
        let mut stale = store.layout().unwrap();
        stale.hidden.insert("gone@00000000/Session".into());
        store.save_layout(&stale).unwrap();

        let hide = Command::SetHidden { ids: vec![format!("{ida}/Session")], hidden: true };
        assert!(apply(&store, &hide).unwrap());
        let layout = store.layout().unwrap();
        assert!(layout.hidden.contains(&format!("{ida}/Session")) && !layout.hidden.contains("gone@00000000/Session"));
        assert!(!apply(&store, &hide).unwrap(), "no change, no save");

        assert!(apply(&store, &Command::SetUnified { kind: ProviderKind::Claude, on: true }).unwrap());
        assert!(!apply(&store, &Command::SetUnified { kind: ProviderKind::Zai, on: true }).unwrap());
        assert!(apply(&store, &Command::MoveMetric { card_id: ida.clone(), dragged: "Weekly".into(), target: "Session".into() }).unwrap());
        assert_eq!(store.layout().unwrap().metric_order[&ida], ["Weekly", "Session"]);
        assert!(!apply(&store, &Command::MoveMetric { card_id: ida.clone(), dragged: "Nope".into(), target: "Session".into() }).unwrap());

        assert!(apply(&store, &Command::MoveAccount { dragged: idb.clone(), target: ida.clone() }).unwrap());
        assert_eq!(store.accounts().unwrap()[0].card_id(), idb);
        assert!(!apply(&store, &Command::MoveProvider { dragged: ProviderKind::Claude, target: ProviderKind::Claude }).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
