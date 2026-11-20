use super::*;
use crate::store::Store;
use uuid::Uuid;

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_layout_round_trips_and_a_missing_or_old_file_is_the_default() {
    let dir = std::env::temp_dir().join(format!("headroom-test-{}", Uuid::new_v4()));
    let store = Store::at(&dir.join("config"), &dir.join("state"));
    assert_eq!(store.layout().unwrap(), Layout::default());
    let mut layout = Layout::default();
    layout.set_hidden(&strs(&["claude@1/Weekly"]), true);
    layout.set_pinned(&strs(&["claude@1/Session"]), true);
    layout.set_unified(ProviderKind::Claude, true);
    layout.metric_order.insert("claude@1".into(), strs(&["Weekly", "Session"]));
    store.save_layout(&layout).unwrap();
    assert_eq!(store.layout().unwrap(), layout);
    std::fs::write(dir.join("config/layout.json"), r#"{"hidden":["a/b"]}"#).unwrap();
    let old = store.layout().unwrap();
    assert_eq!((old.hidden.len(), old.pins.len(), old.unified.len()), (1, 0, 0));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hiding_unpins_and_a_hidden_metric_cannot_be_pinned() {
    let mut l = Layout::default();
    let ids = strs(&["c@1/A"]);
    assert!(l.set_pinned(&ids, true));
    assert!(!l.set_pinned(&ids, true));
    assert!(l.set_hidden(&ids, true));
    assert!(l.pins.is_empty());
    assert!(!l.set_pinned(&ids, true));
    assert!(l.pins.is_empty());
    assert!(l.set_hidden(&ids, false));
    assert!(!l.set_hidden(&ids, false));
}

#[test]
fn only_claude_and_codex_unify() {
    let mut l = Layout::default();
    assert!(!l.set_unified(ProviderKind::Cursor, true));
    assert!(l.set_unified(ProviderKind::Codex, true));
    assert!(!l.set_unified(ProviderKind::Codex, true));
    assert!(l.set_unified(ProviderKind::Codex, false));
    assert!(l.unified.is_empty());
}

#[test]
fn reordered_inserts_after_when_moving_down_and_before_when_moving_up() {
    let ids = strs(&["a", "b", "c", "d"]);
    assert_eq!(reordered(&ids, "a", "c"), Some(strs(&["b", "c", "a", "d"])));
    assert_eq!(reordered(&ids, "d", "b"), Some(strs(&["a", "d", "b", "c"])));
    assert_eq!(reordered(&ids, "a", "b"), Some(strs(&["b", "a", "c", "d"])));
    assert_eq!(reordered(&ids, "a", "a"), None);
    assert_eq!(reordered(&ids, "x", "a"), None);
    assert_eq!(reordered(&ids, "a", "x"), None);
}

#[test]
fn metric_order_drops_stale_and_duplicate_labels_and_appends_new_ones() {
    let mut l = Layout::default();
    l.metric_order.insert("c".into(), strs(&["Old", "B", "B", "A"]));
    assert_eq!(l.metric_order("c", &strs(&["A", "B", "New"])), strs(&["B", "A", "New"]));
    assert_eq!(l.metric_order("other", &strs(&["A", "B"])), strs(&["A", "B"]));
    assert!(l.move_metric("c", &strs(&["A", "B", "New"]), "New", "B"));
    assert_eq!(l.metric_order["c"], strs(&["New", "B", "A"]));
    assert!(!l.move_metric("c", &strs(&["A", "B", "New"]), "New", "New"));
}

fn ids(accounts: &[Account]) -> Vec<String> {
    accounts.iter().map(Account::card_id).collect()
}

#[test]
fn accounts_move_within_a_provider_or_when_exactly_two_remain() {
    let mut a = vec![Account::new(ProviderKind::Claude, "1"), Account::new(ProviderKind::Grok, ""), Account::new(ProviderKind::Claude, "2")];
    let id = ids(&a);
    assert!(move_account(&mut a, &id[2], &id[0]));
    assert_eq!(ids(&a), [id[2].clone(), id[0].clone(), id[1].clone()]);
    // Three accounts across Claude and Grok: refused.
    let (x, y) = (ids(&a)[0].clone(), ids(&a)[2].clone());
    assert!(!move_account(&mut a, &x, &y));
    assert!(!move_account(&mut a, &x, "nope"));
    // Exactly two accounts for the two providers: allowed.
    let mut two = vec![Account::new(ProviderKind::Claude, ""), Account::new(ProviderKind::Grok, "")];
    let id = ids(&two);
    assert!(move_account(&mut two, &id[0], &id[1]));
    assert_eq!(ids(&two), [id[1].clone(), id[0].clone()]);
}

#[test]
fn a_provider_moves_with_all_its_accounts() {
    let mut a = vec![
        Account::new(ProviderKind::Claude, "1"),
        Account::new(ProviderKind::Grok, ""),
        Account::new(ProviderKind::Claude, "2"),
        Account::new(ProviderKind::Amp, ""),
    ];
    let id = ids(&a);
    assert!(move_provider(&mut a, ProviderKind::Claude, ProviderKind::Amp));
    assert_eq!(ids(&a), [id[1].clone(), id[3].clone(), id[0].clone(), id[2].clone()]);
    assert!(!move_provider(&mut a, ProviderKind::Amp, ProviderKind::Amp));
    assert!(!move_provider(&mut a, ProviderKind::Amp, ProviderKind::Zai));
}

#[test]
fn retain_accounts_drops_entries_of_removed_accounts() {
    let mut l = Layout::default();
    l.set_hidden(&strs(&["claude@1/A", "claude@2/A/b"]), true);
    l.set_pinned(&strs(&["claude@1/B", "claude@2/B"]), true);
    l.metric_order.insert("claude@1".into(), strs(&["A"]));
    l.metric_order.insert("claude@2".into(), strs(&["A"]));
    assert!(l.retain_accounts(&strs(&["claude@1"])));
    assert_eq!((l.hidden.len(), l.pins.len(), l.metric_order.len()), (1, 1, 1));
    assert!(!l.retain_accounts(&strs(&["claude@1"])));
}
