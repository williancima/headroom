use super::*;
use crate::claim::ResetItem;
use crate::layout::Layout;
use crate::priority::BurningFast;
use crate::view::{usage_groups, PanelView};
use crate::ProviderSnapshot;
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;

fn now() -> DateTime<Utc> {
    "2027-01-15T08:00:00Z".parse().unwrap()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn bar(label: &str, used: f64, limit: f64, format: ProgressFormat, left_hours: i64) -> MetricLine {
    MetricLine::Progress { label: label.into(), used, limit, format, resets_at: Some(now() + Duration::hours(left_hours)), period_ms: Some(1000) }
}

fn pct(label: &str, used: f64, left_hours: i64) -> MetricLine {
    bar(label, used, 100.0, ProgressFormat::Percent, left_hours)
}

fn used_limit(line: &MetricLine) -> (f64, f64, Option<DateTime<Utc>>, Option<i64>) {
    match line {
        MetricLine::Progress { used, limit, resets_at, period_ms, .. } => (*used, *limit, *resets_at, *period_ms),
        other => panic!("not a bar: {other:?}"),
    }
}

fn snap(plan: &str, lines: Vec<MetricLine>) -> ProviderSnapshot {
    ProviderSnapshot { plan: Some(plan.into()), lines, refreshed_at: now(), reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() }
}

fn model(accounts: &[(&Account, ProviderSnapshot)], layout: &Layout) -> PanelModel {
    let snapshots: HashMap<_, _> = accounts.iter().map(|(a, s)| (a.card_id(), s.clone())).collect();
    PanelModel::build(accounts.iter().map(|(a, _)| (*a).clone()).collect(), vec![], snapshots, &HashMap::new(), &BurningFast::default(), layout, &crate::priority::Settings::default(), now())
}

#[test]
fn merged_order_puts_a_missing_key_before_the_next_shared_one() {
    let orders = vec![strs(&["a", "c"]), strs(&["a", "b", "c", "d"]), strs(&["x", "d"])];
    assert_eq!(merged_order(&orders), strs(&["a", "b", "c", "x", "d"]));
    assert_eq!(merged_order(&[strs(&["a"]), strs(&["b"])]), strs(&["a", "b"]));
    assert!(merged_order(&[]).is_empty());
}

#[test]
fn two_percent_limits_pool_cap_weighted_with_the_earliest_reset() {
    let (a, b) = (pct("Weekly", 20.0, 50), pct("Weekly", 60.0, 10));
    assert_eq!(used_limit(&pool(ProviderKind::Claude, &[&a, &b])), (40.0, 100.0, Some(now() + Duration::hours(10)), None));
    // Different caps weigh in: 50 of 100 and 10 of 20 is 60 of 120.
    let (c, d) = (bar("W", 50.0, 100.0, ProgressFormat::Percent, 1), bar("W", 10.0, 20.0, ProgressFormat::Percent, 1));
    assert_eq!(used_limit(&pool(ProviderKind::Claude, &[&c, &d])).0, 50.0);
}

#[test]
fn an_account_over_its_cap_cannot_spend_anothers_room() {
    let dollars = |u| bar("Extra", u, 10.0, ProgressFormat::Dollars, 1);
    let (a, b) = (dollars(15.0), dollars(0.0));
    assert_eq!(used_limit(&pool(ProviderKind::Claude, &[&a, &b])).0, 10.0);
}

#[test]
fn a_pooled_limit_is_just_under_full_unless_every_account_is_spent() {
    let (a, b) = (pct("W", 100.0, 1), pct("W", 99.4, 1));
    // 199.4 of 200 rounds to spent, but one account has room.
    let almost = pool(ProviderKind::Claude, &[&a, &b]);
    assert_eq!(used_limit(&almost).0, 99.0);
    let (c, d) = (pct("W", 100.0, 1), pct("W", 100.0, 1));
    assert_eq!(used_limit(&pool(ProviderKind::Claude, &[&c, &d])).0, 100.0);
}

#[test]
fn mixed_formats_fall_back_to_summed_values_and_badges_join() {
    let (a, b, c) = (pct("X", 30.0, 1), bar("X", 5.0, 10.0, ProgressFormat::Dollars, 1), bar("X", 2.0, 10.0, ProgressFormat::Dollars, 1));
    let MetricLine::Values { values, .. } = pool(ProviderKind::Claude, &[&a, &b, &c]) else { panic!("not values") };
    assert_eq!(values.iter().map(|v| (v.number, v.currency_code.clone())).collect::<Vec<_>>(), [(30.0, None), (7.0, Some("USD".into()))]);
    let badge = |t: &str| MetricLine::Badge { label: "Plan".into(), text: t.into() };
    let (x, y, z) = (badge("Pro"), badge("Max"), badge("Pro"));
    assert_eq!(pool(ProviderKind::Claude, &[&x, &y, &z]), badge("Pro · Max"));
    let v = |n| MetricLine::Values { label: "Bal".into(), values: vec![MetricValue { number: n, currency_code: Some("usd".into()), label: None }] };
    let MetricLine::Values { values, .. } = pool(ProviderKind::Codex, &[&v(1.0), &v(2.5)]) else { panic!() };
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].number, 3.5);
}

#[test]
fn a_pool_with_no_limit_reads_the_first_line() {
    let (a, b) = (bar("W", 1.0, 0.0, ProgressFormat::Percent, 1), bar("W", 2.0, 0.0, ProgressFormat::Percent, 1));
    assert_eq!(pool(ProviderKind::Claude, &[&a, &b]), a);
}

fn two_claudes() -> (Account, Account) {
    (Account::new(ProviderKind::Claude, "A"), Account::new(ProviderKind::Claude, "B"))
}

#[test]
fn a_unified_group_is_one_card_with_pooled_rows_members_and_copy_ids() {
    let (a, b) = two_claudes();
    let mut layout = Layout::default();
    layout.set_unified(ProviderKind::Claude, true);
    layout.set_pinned(&[format!("{}/Weekly", b.card_id())], true);
    let m = model(&[(&a, snap("Pro", vec![pct("Weekly", 20.0, 5), pct("Session", 10.0, 1)])), (&b, snap("Max", vec![pct("Weekly", 60.0, 9)]))], &layout);
    let groups = usage_groups(&m);
    let g = &groups[0];
    assert_eq!((g.kind.as_str(), g.unifiable, g.unified), ("claude", true, true));
    assert_eq!((g.rows[0].title.as_str(), g.rows[0].subtitle.as_str()), ("2 Accounts", "Multiple"));
    assert_eq!(g.rows[0].kind, "claude");
    assert!(g.rows[1..].iter().all(|r| r.kind.is_empty() && r.members.iter().all(|m| m.kind.is_empty())));
    let weekly = &g.rows[1];
    assert_eq!(weekly.id.as_deref(), Some("claude.unified/Weekly"));
    assert_eq!(weekly.copy_ids, [format!("{}/Weekly", a.card_id()), format!("{}/Weekly", b.card_id())]);
    assert!(weekly.pinned);
    assert_eq!(weekly.value.as_deref(), Some("40%"));
    assert_eq!(weekly.members.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["Claude · A", "Claude · B"]);
    let session = &g.rows[2];
    assert!(!session.pinned);
    assert_eq!((session.members.len(), session.copy_ids.len()), (1, 2));
    // Priority keeps one item per account.
    assert_eq!(m.board.pinned.len(), 1);
    let json = serde_json::to_value(PanelView::new(&m)).unwrap();
    assert_eq!(json["usage"][0]["unified"], true);
    assert!(json["usage"][0]["rows"][0].get("copy_ids").is_none());
}

#[test]
fn the_shared_plan_shows_and_a_single_account_never_unifies() {
    let (a, b) = two_claudes();
    let mut layout = Layout::default();
    layout.set_unified(ProviderKind::Claude, true);
    let m = model(&[(&a, snap("Max", vec![])), (&b, snap("Max", vec![]))], &layout);
    assert_eq!(usage_groups(&m)[0].rows[0].subtitle, "Max");
    let m = model(&[(&a, snap("Max", vec![]))], &layout);
    let g = &usage_groups(&m)[0];
    assert_eq!((g.unifiable, g.unified, g.rows[0].title.as_str()), (false, false, "Claude · A"));
}

#[test]
fn the_unified_resets_row_lists_every_accounts_resets() {
    let (a, b) = two_claudes();
    let mut layout = Layout::default();
    layout.set_unified(ProviderKind::Claude, true);
    let line = || MetricLine::Values { label: LINE_LABEL.into(), values: vec![MetricValue { number: 1.0, currency_code: None, label: None }] };
    let (mut sa, mut sb) = (snap("Max", vec![line()]), snap("Max", vec![line()]));
    sb.reset_items = ResetItem::codex(&[now() + Duration::hours(30)]);
    sa.reset_items = ResetItem::codex(&[now() + Duration::hours(5)]);
    let m = model(&[(&a, sa), (&b, sb)], &layout);
    let row = &usage_groups(&m)[0].rows[1];
    assert_eq!(row.card_id.as_deref(), Some(a.card_id().as_str()));
    assert_eq!(row.resets.iter().map(|e| e.card_id.clone().unwrap()).collect::<Vec<_>>(), [a.card_id(), b.card_id()]);
}

#[test]
fn order_and_hidden_apply_and_the_header_stays_when_everything_is_hidden() {
    let a = Account::new(ProviderKind::Claude, "A");
    let mut layout = Layout::default();
    layout.metric_order.insert(a.card_id(), strs(&["Weekly", "Session"]));
    let lines = || vec![pct("Session", 1.0, 1), pct("Weekly", 2.0, 1), pct("Opus", 3.0, 1)];
    let m = model(&[(&a, snap("Max", lines()))], &layout);
    let rows = &usage_groups(&m)[0].rows;
    assert_eq!(rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["Claude · A", "Weekly", "Session", "Opus"]);
    assert_eq!(rows[0].card_id.as_deref(), Some(a.card_id().as_str()));
    assert_eq!(rows[1].id, Some(format!("{}/Weekly", a.card_id())));
    layout.set_hidden(&["Weekly", "Session", "Opus"].map(|l| format!("{}/{l}", a.card_id())), true);
    let m = model(&[(&a, snap("Max", lines()))], &layout);
    assert_eq!(usage_groups(&m)[0].rows.len(), 1);
}

#[test]
fn a_pin_reaches_the_priority_board() {
    let a = Account::new(ProviderKind::Claude, "A");
    let mut layout = Layout::default();
    layout.set_pinned(&[format!("{}/Weekly", a.card_id())], true);
    let m = model(&[(&a, snap("Max", vec![pct("Weekly", 10.0, 100)]))], &layout);
    assert_eq!(m.board.pinned.len(), 1);
    let rows = &usage_groups(&m)[0].rows;
    assert!(rows[1].pinned);
}
