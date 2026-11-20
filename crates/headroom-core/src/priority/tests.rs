//! Priority ranking, burning fast, use it or lose it and the tray summary.

use super::*;
use crate::models::{MetricValue, ProgressFormat, ProviderKind};
use chrono::Duration;

fn now() -> DateTime<Utc> {
    "2027-01-15T08:00:00Z".parse().unwrap()
}

const HOUR: f64 = 3600.0;
const DAY: f64 = 24.0 * HOUR;
const WEEK_MS: i64 = 7 * DAY_MS;
const DAY_MS: i64 = 24 * 3600 * 1000;

fn secs(s: f64) -> Duration {
    Duration::milliseconds((s * 1000.0) as i64)
}

/// A percent limit resetting `left` seconds from now over `period_ms`.
fn paced(label: &str, used: f64, left: f64, period_ms: i64) -> MetricLine {
    MetricLine::Progress {
        label: label.into(),
        used,
        limit: 100.0,
        format: ProgressFormat::Percent,
        resets_at: Some(now() + secs(left)),
        period_ms: Some(period_ms),
    }
}

fn weekly(label: &str, used: f64, left: f64) -> MetricLine {
    paced(label, used, left, WEEK_MS)
}

fn unpaced(label: &str, used: f64, limit: f64) -> MetricLine {
    MetricLine::Progress { label: label.into(), used, limit, format: ProgressFormat::Dollars, resets_at: None, period_ms: None }
}

fn snapshot(lines: Vec<MetricLine>) -> ProviderSnapshot {
    ProviderSnapshot { plan: None, lines, refreshed_at: now(), reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() }
}

struct Fixture {
    accounts: Vec<Account>,
    snapshots: HashMap<String, ProviderSnapshot>,
}

impl Fixture {
    fn new(rows: Vec<(ProviderKind, Vec<MetricLine>)>) -> Self {
        let mut fixture = Self { accounts: vec![], snapshots: HashMap::new() };
        for (kind, lines) in rows {
            let account = Account::new(kind, "x");
            fixture.snapshots.insert(account.card_id(), snapshot(lines));
            fixture.accounts.push(account);
        }
        fixture
    }

    fn id(&self, index: usize, label: &str) -> String {
        format!("{}/{label}", self.accounts[index].card_id())
    }

    fn board_with(&self, settings: &Settings, pinned: &[String], latched: &BurningFast) -> PriorityBoard {
        let pinned = pinned.iter().cloned().collect();
        PriorityBoard::make(&self.accounts, &self.snapshots, &HashMap::new(), &pinned, latched, settings, now())
    }

    fn board(&self) -> PriorityBoard {
        self.board_with(&off(), &[], &BurningFast::default())
    }
}

/// Burning fast and use it or lose it off.
fn off() -> Settings {
    Settings { burning_fast: false, use_it_or_lose_it: false, ..Settings::default() }
}

fn ids(items: &[PriorityItem]) -> Vec<&str> {
    items.iter().map(|i| i.line.id.as_str()).collect()
}

fn labels(items: &[PriorityItem]) -> Vec<&str> {
    items.iter().map(|i| i.line.label.as_str()).collect()
}

#[test]
fn a_limit_is_listed_at_30_percent_left_however_slow_or_fast_it_is_burning() {
    let fx = Fixture::new(vec![(
        ProviderKind::Claude,
        vec![weekly("Nearly Reset", 70.0, HOUR), weekly("Fresh", 70.0, 6.9 * DAY), weekly("Burning", 69.0, 6.9 * DAY)],
    )]);
    let board = fx.board();
    let mut listed = labels(&board.attention);
    listed.sort();
    assert_eq!(listed, ["Fresh", "Nearly Reset"]);
}

#[test]
fn warning_at_30_percent_left_and_critical_at_a_tenth_left() {
    let severity = |used| {
        let fx = Fixture::new(vec![(ProviderKind::Openrouter, vec![unpaced("Credits", used, 100.0)])]);
        fx.board().attention.first().map(|i| i.reason.severity())
    };
    assert_eq!(severity(69.0), None);
    assert_eq!(severity(70.0), Some(Severity::Warning));
    assert_eq!(severity(89.0), Some(Severity::Warning));
    assert_eq!(severity(90.0), Some(Severity::Critical));
}

#[test]
fn the_soonest_to_run_out_comes_first_then_what_has_no_pace() {
    let fx = Fixture::new(vec![(
        ProviderKind::Claude,
        vec![
            weekly("Weekly", 75.0, 3.5 * DAY),
            paced("Session", 75.0, 2.5 * HOUR, 5 * 3600 * 1000),
            unpaced("Extra Usage", 95.0, 100.0),
        ],
    )]);
    assert_eq!(labels(&fx.board().attention), ["Session", "Weekly", "Extra Usage"]);
}

#[test]
fn equal_ranks_keep_the_saved_order() {
    let line = weekly("Weekly", 75.0, 3.5 * DAY);
    let fx = Fixture::new(vec![(ProviderKind::Claude, vec![line.clone()]), (ProviderKind::Codex, vec![line.clone()])]);
    assert_eq!(ids(&fx.board().attention), [fx.id(0, "Weekly"), fx.id(1, "Weekly")]);
    let level = unpaced("Credits", 80.0, 100.0);
    let fx = Fixture::new(vec![(ProviderKind::Codex, vec![level.clone()]), (ProviderKind::Claude, vec![level])]);
    assert_eq!(ids(&fx.board().attention), [fx.id(0, "Credits"), fx.id(1, "Credits")]);
}

#[test]
fn limits_with_no_pace_follow_the_paced_ones_least_left_first() {
    let fx = Fixture::new(vec![
        (ProviderKind::Claude, vec![unpaced("Warning", 80.0, 100.0), unpaced("Critical", 95.0, 100.0), unpaced("Fine", 40.0, 100.0)]),
        (ProviderKind::Codex, vec![unpaced("Warned More", 170.0, 200.0), weekly("Paced", 75.0, 3.5 * DAY)]),
    ]);
    let board = fx.board();
    assert_eq!(labels(&board.attention), ["Paced", "Critical", "Warned More", "Warning"]);
    let severities: Vec<_> = board.attention.iter().map(|i| i.reason.severity()).collect();
    assert_eq!(severities, [Severity::Warning, Severity::Critical, Severity::Warning, Severity::Warning]);
}

#[test]
fn used_up_sorts_by_soonest_reset_with_no_reset_last_and_never_needs_attention() {
    let fx = Fixture::new(vec![
        (ProviderKind::Claude, vec![unpaced("No Reset", 100.0, 100.0), weekly("Later", 100.0, 2.0 * DAY)]),
        (ProviderKind::Codex, vec![weekly("Sooner", 100.0, 3.0 * HOUR), weekly("Running Out", 75.0, 3.5 * DAY)]),
    ]);
    let board = fx.board();
    assert_eq!(labels(&board.used_up), ["Sooner", "Later", "No Reset"]);
    assert_eq!(labels(&board.attention), ["Running Out"]);
    assert_eq!(board.used_up[1].reason, Reason::UsedUp { resets_at: Some(now() + secs(2.0 * DAY)) });
}

#[test]
fn healthy_quiet_and_unbounded_lines_are_not_listed() {
    let balance = MetricLine::Values { label: "Other".into(), values: vec![MetricValue { number: 12.0, currency_code: Some("usd".into()), label: None }] };
    let fx = Fixture::new(vec![(ProviderKind::Claude, vec![weekly("Healthy", 20.0, 3.5 * DAY), unpaced("Quiet", 10.0, 100.0), balance])]);
    let board = fx.board();
    assert!(board.attention.is_empty() && board.used_up.is_empty());
    assert_eq!(board.summary, Summary::AllClear);
    assert!(board.shows_clear_row);
}

#[test]
fn an_empty_balance_is_used_up_and_a_funded_one_is_not_listed() {
    let balance = |n| MetricLine::Values { label: "Balance".into(), values: vec![MetricValue { number: n, currency_code: Some("usd".into()), label: None }] };
    let fx = Fixture::new(vec![(ProviderKind::Openrouter, vec![balance(0.0)])]);
    let board = fx.board();
    assert_eq!(board.used_up[0].reason, Reason::EmptyBalance);
    assert_eq!(board.used_up[0].reason.severity(), Severity::Critical);
    assert!(Fixture::new(vec![(ProviderKind::Openrouter, vec![balance(5.0)])]).board().used_up.is_empty());
    // A zero values line that is not a balance resource is not an empty balance.
    let other = MetricLine::Values { label: "Whatever".into(), values: vec![MetricValue { number: 0.0, currency_code: None, label: None }] };
    assert!(Fixture::new(vec![(ProviderKind::Openrouter, vec![other])]).board().used_up.is_empty());
}

#[test]
fn a_limit_burning_fast_joins_running_out_with_plenty_left_and_only_with_the_setting_on() {
    // 60% used at half the week: projected 120% of the limit.
    let fx = Fixture::new(vec![(ProviderKind::Claude, vec![weekly("Weekly", 60.0, 3.5 * DAY), weekly("Steady", 40.0, 3.5 * DAY)])]);
    let on = fx.board_with(&Settings::default(), &[], &BurningFast::default());
    assert_eq!(labels(&on.attention), ["Weekly"]);
    assert_eq!(on.attention[0].reason.severity(), Severity::Normal);
    assert!(matches!(on.attention[0].reason, Reason::BurningFast { runway } if (runway - 2.0 * DAY - 8.0 * HOUR).abs() < 1.0));
    assert!(on.requires_ranking_clock(now()));
    assert!(fx.board().attention.is_empty());
}

#[test]
fn a_burning_limit_stays_listed_until_its_pace_eases_to_ninety_percent() {
    let latched = |fx: &Fixture| BurningFast { latched: HashMap::from([(fx.id(0, "Weekly"), now() + secs(3.5 * DAY))]) };
    let nearly_even = Fixture::new(vec![(ProviderKind::Claude, vec![weekly("Weekly", 47.5, 3.5 * DAY)])]);
    let eased = Fixture::new(vec![(ProviderKind::Claude, vec![weekly("Weekly", 42.5, 3.5 * DAY)])]);
    let on = Settings::default();
    assert!(nearly_even.board_with(&on, &[], &BurningFast::default()).attention.is_empty(), "95% does not start listing it");
    assert_eq!(labels(&nearly_even.board_with(&on, &[], &latched(&nearly_even)).attention), ["Weekly"]);
    assert!(eased.board_with(&on, &[], &latched(&eased)).attention.is_empty(), "85% lets it go");
}

#[test]
fn the_latch_follows_each_reading() {
    let reset = now() + secs(3.5 * DAY);
    let burning = Fixture::new(vec![(ProviderKind::Claude, vec![weekly("Weekly", 60.0, 3.5 * DAY), weekly("Steady", 40.0, 3.5 * DAY)])]);
    let latch = BurningFast::default().after_reading(&burning.accounts, &burning.snapshots, now());
    assert_eq!(latch.latched, HashMap::from([(burning.id(0, "Weekly"), reset)]));
    // At 95% projected it stays latched, at 85% it is let go.
    let nearly_even = HashMap::from([(burning.accounts[0].card_id(), snapshot(vec![weekly("Weekly", 47.5, 3.5 * DAY)]))]);
    assert_eq!(latch.after_reading(&burning.accounts, &nearly_even, now()).latched.len(), 1);
    let eased = HashMap::from([(burning.accounts[0].card_id(), snapshot(vec![weekly("Weekly", 42.5, 3.5 * DAY)]))]);
    assert!(latch.after_reading(&burning.accounts, &eased, now()).latched.is_empty());
}

#[test]
fn a_fresh_session_window_is_not_burning() {
    // Claude's session with no reset date is an untouched window, whatever the pace maths would say.
    let session = MetricLine::Progress {
        label: "Session".into(), used: 0.0, limit: 100.0, format: ProgressFormat::Percent, resets_at: None, period_ms: Some(5 * 3600 * 1000),
    };
    let meter = Meter::from_line(ProviderKind::Claude, &session).unwrap();
    assert_eq!(meter.state(now()), MeterState::Unpaced);
}

#[test]
fn a_pinned_limit_stays_pinned_and_still_runs_out_unless_used_up() {
    let fx = Fixture::new(vec![(ProviderKind::Claude, vec![weekly("Low", 80.0, 3.5 * DAY), weekly("Spent", 100.0, DAY), weekly("Fine", 10.0, DAY)])]);
    let pins = [fx.id(0, "Low"), fx.id(0, "Spent"), fx.id(0, "Fine")];
    let board = fx.board_with(&off(), &pins, &BurningFast::default());
    assert_eq!(board.pinned.iter().map(|p| p.line.label.as_str()).collect::<Vec<_>>(), ["Low", "Fine"]);
    assert!(board.attention.is_empty());
    assert_eq!(labels(&board.running_out), ["Low"]);
    assert_eq!(labels(&board.used_up), ["Spent"]);
    assert!(!board.shows_clear_row);
}

#[test]
fn use_it_or_lose_it_lists_long_windows_about_to_reset_unused_soonest_first_and_only_with_the_switch_on() {
    let fx = Fixture::new(vec![(
        ProviderKind::Claude,
        vec![
            weekly("Weekly", 30.0, 18.0 * HOUR),
            weekly("Sooner", 20.0, 3.0 * HOUR),
            weekly("Half", 50.0, 3.0 * HOUR),
            weekly("Far", 10.0, 25.0 * HOUR),
            paced("Session", 10.0, 3.0 * HOUR, 5 * 3600 * 1000),
            paced("Daily", 10.0, 3.0 * HOUR, DAY_MS),
            MetricLine::Progress {
                label: "Cap".into(), used: 10.0, limit: 100.0, format: ProgressFormat::Dollars,
                resets_at: Some(now() + secs(3.0 * HOUR)), period_ms: Some(WEEK_MS),
            },
        ],
    )]);
    let on = Settings { burning_fast: false, ..Settings::default() };
    assert_eq!(labels(&fx.board_with(&on, &[], &BurningFast::default()).use_it_or_lose_it), ["Sooner", "Weekly"]);
    assert!(fx.board().use_it_or_lose_it.is_empty());
    let pinned = fx.board_with(&on, &[fx.id(0, "Weekly")], &BurningFast::default());
    assert_eq!(labels(&pinned.use_it_or_lose_it), ["Sooner"]);
    assert_eq!(pinned.pinned.len(), 1);
}

#[test]
fn summary_follows_what_is_waiting_unavailable_and_listed() {
    let fx = Fixture::new(vec![(ProviderKind::Claude, vec![weekly("Low", 80.0, DAY)])]);
    let none = PriorityBoard::make(&[], &HashMap::new(), &HashMap::new(), &HashSet::new(), &BurningFast::default(), &off(), now());
    assert_eq!(none.summary, Summary::WaitingForUsage);
    assert_eq!(fx.board().summary, Summary::Listed);

    let account = Account::new(ProviderKind::Claude, "x");
    let card = account.card_id();
    let make = |snaps: &HashMap<String, ProviderSnapshot>, status: HashMap<String, ReadStatus>| {
        PriorityBoard::make(std::slice::from_ref(&account), snaps, &status, &HashSet::new(), &BurningFast::default(), &off(), now())
    };
    let waiting = make(&HashMap::new(), HashMap::new());
    assert_eq!((waiting.summary, waiting.waiting.clone()), (Summary::WaitingForUsage, vec![card.clone()]));
    let down = make(&HashMap::new(), HashMap::from([(card.clone(), ReadStatus::Unavailable("offline".into()))]));
    assert_eq!((down.summary, down.notices[0].message.as_str()), (Summary::Unavailable, "offline"));
    assert!(!down.shows_clear_row);
    let empty = HashMap::from([(card.clone(), snapshot(vec![]))]);
    assert_eq!(make(&empty, HashMap::new()).summary, Summary::NoReportedLimits);
    let fine = HashMap::from([(card, snapshot(vec![weekly("Fine", 10.0, DAY)]))]);
    assert_eq!(make(&fine, HashMap::new()).summary, Summary::AllClear);
}

#[test]
fn the_ranking_clock_is_needed_only_for_several_running_low_limits_with_a_window() {
    let one = Fixture::new(vec![(ProviderKind::Claude, vec![weekly("A", 80.0, DAY)])]).board();
    assert!(!one.requires_ranking_clock(now()));
    let two = Fixture::new(vec![(ProviderKind::Claude, vec![weekly("A", 80.0, DAY), weekly("B", 85.0, DAY)])]).board();
    assert!(two.requires_ranking_clock(now()));
    let flat = Fixture::new(vec![(ProviderKind::Claude, vec![unpaced("A", 80.0, 100.0), unpaced("B", 85.0, 100.0)])]).board();
    assert!(!flat.requires_ranking_clock(now()));
}

fn content(limit: MenuBarLimit, always: bool, lines: Vec<MetricLine>) -> MenuBarContent {
    menu_bar_content(&Fixture::new(vec![(ProviderKind::Openrouter, lines)]).board(), limit, always)
}

fn level(label: &str, used: f64) -> MetricLine {
    unpaced(label, used, 100.0)
}

#[test]
fn the_most_urgent_limits_show_up_to_the_setting_and_the_rest_become_the_overflow() {
    let lines = vec![level("A", 75.0), level("B", 80.0), level("C", 85.0), level("D", 90.0), level("E", 95.0)];
    let three = content(MenuBarLimit::UpToThree, false, lines.clone());
    assert_eq!(three.items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["E", "D", "C"]);
    assert_eq!(three.overflow, 2);
    assert_eq!(content(MenuBarLimit::IconOnly, false, lines.clone()), MenuBarContent::default());
    assert_eq!(content(MenuBarLimit::UpToFive, false, lines).overflow, 0);
    assert_eq!(content(MenuBarLimit::UpToThree, false, vec![level("Fine", 10.0)]), MenuBarContent::default());
}

#[test]
fn used_up_never_shows_or_counts_unless_always_showing_and_then_follows_running_out() {
    let lines = vec![level("Spent", 100.0), level("Spent Too", 100.0), level("Low", 95.0), level("Lower", 90.0)];
    assert!(content(MenuBarLimit::UpToFive, false, lines.clone()).items.iter().all(|i| i.label.starts_with("Low")));
    let one = content(MenuBarLimit::UpToOne, true, lines.clone());
    assert_eq!((one.items[0].short_label.as_str(), one.overflow), ("Low", 1));
    let five = content(MenuBarLimit::UpToFive, true, lines);
    assert_eq!(five.items.iter().map(|i| i.short_label.as_str()).collect::<Vec<_>>(), ["Low", "Lower", "Spent", "Spent Too"]);
    assert_eq!(five.items.iter().map(|i| i.value.as_str()).collect::<Vec<_>>(), ["$95", "$90", "$100", "$100"]);
    assert_eq!(five.overflow, 0);
}

#[test]
fn always_showing_lists_pinned_first_then_running_out_then_use_it_or_lose_it() {
    let fx = Fixture::new(vec![(
        ProviderKind::Claude,
        vec![level("Low", 80.0), weekly("Unused", 20.0, 3.0 * HOUR), level("Pinned", 10.0)],
    )]);
    let board = fx.board_with(&Settings { burning_fast: false, ..Settings::default() }, &[fx.id(0, "Pinned")], &BurningFast::default());
    let shown = menu_bar_content(&board, MenuBarLimit::UpToFive, true);
    assert_eq!(shown.items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["Pinned", "Low", "Unused"]);
    assert_eq!(shown.items[0].card_id, fx.accounts[0].card_id());
    // Not always showing: only running out.
    let plain = menu_bar_content(&board, MenuBarLimit::UpToFive, false);
    assert_eq!(plain.items.len(), 1);
}

#[test]
fn the_short_label_drops_a_trailing_generic_word_when_a_word_remains() {
    for (label, short) in [("Cursor Models", "Cursor"), ("Grok Bot", "Grok"), ("Extra USAGE", "Extra"), ("Usage", "Usage"), ("Usage Limit", "Usage Limit")] {
        assert_eq!(short_label(label), short, "{label}");
    }
}

#[test]
fn the_tray_value_follows_each_format() {
    let meter = |format, used, limit| {
        Meter::from_line(
            ProviderKind::Claude,
            &MetricLine::Progress { label: "x".into(), used, limit, format, resets_at: None, period_ms: None },
        )
        .unwrap()
    };
    assert_eq!(meter(ProgressFormat::Percent, 47.6, 100.0).menu_bar_value(false), "48%");
    assert_eq!(meter(ProgressFormat::Percent, 47.6, 100.0).menu_bar_value(true), "52%");
    assert_eq!(meter(ProgressFormat::Percent, 5.0, 20.0).menu_bar_value(false), "25%");
    assert_eq!(meter(ProgressFormat::Dollars, 0.5, 10.0).menu_bar_value(false), "$0.50");
    assert_eq!(meter(ProgressFormat::Dollars, 12.4, 100.0).menu_bar_value(false), "$12");
    assert_eq!(meter(ProgressFormat::Dollars, 1500.0, 5000.0).menu_bar_value(false), "$1.5K");
    assert_eq!(meter(ProgressFormat::Currency("BRL".into()), 80.0, 100.0).menu_bar_value(false), "R$80");
    assert_eq!(meter(ProgressFormat::Count("req".into()), 12.0, 50.0).menu_bar_value(false), "12");
    assert_eq!(meter(ProgressFormat::Count("req".into()), 12.34, 50.0).menu_bar_value(false), "12.3");
    assert_eq!(meter(ProgressFormat::Count("req".into()), 12000.0, 50000.0).menu_bar_value(false), "12K");
}

mod strings {
    use super::*;

    fn item(fx: &Fixture, board: &PriorityBoard) -> PriorityItem {
        let _ = fx;
        board.attention.first().or(board.used_up.first()).or(board.use_it_or_lose_it.first()).unwrap().clone()
    }

    #[test]
    fn durations_round_up_to_the_minute_and_show_two_units() {
        let at = |s: f64| duration(now(), now() + secs(s));
        assert_eq!(at(22.0 * HOUR + 10.0 * 60.0).as_deref(), Some("22h 10m"));
        assert_eq!(at(18.0 * HOUR).as_deref(), Some("18h"));
        assert_eq!(at(2.0 * DAY + 12.0 * HOUR + 600.0).as_deref(), Some("2d 12h"));
        assert_eq!(at(61.0).as_deref(), Some("2m"));
        assert_eq!(at(0.0), None);
        assert_eq!(duration(now(), now() + secs(100.0 * DAY)).as_deref(), Some("3mo 10d"));
    }

    #[test]
    fn a_burning_limit_says_when_it_runs_out_until_the_pace_eases_past_its_reset() {
        let fx = Fixture::new(vec![(ProviderKind::Claude, vec![weekly("Weekly", 60.0, 3.5 * DAY)])]);
        let board = fx.board_with(&Settings::default(), &[], &BurningFast::default());
        let it = item(&fx, &board);
        assert_eq!(it.run_out_text(now()).as_deref(), Some("Out in 2d 8h"));
        assert_eq!(it.resets_in_text(now()), None);
        // Later, the runway outlasts the reset, so the row shows the reset instead.
        assert_eq!(it.run_out_text(now() + secs(3.4 * DAY)), None);
    }

    #[test]
    fn a_use_it_or_lose_it_limit_says_when_it_resets() {
        let fx = Fixture::new(vec![(ProviderKind::Claude, vec![weekly("Weekly", 10.0, 18.0 * HOUR)])]);
        let board = fx.board_with(&Settings::default(), &[], &BurningFast::default());
        let it = item(&fx, &board);
        assert_eq!(it.resets_in_text(now()).as_deref(), Some("Resets in 18h"));
        assert_eq!(it.run_out_text(now()), None);
    }

    #[test]
    fn a_used_up_note_is_the_time_to_reset_soon_nothing_or_a_balances_amount() {
        let fx = Fixture::new(vec![(ProviderKind::Claude, vec![weekly("Weekly", 100.0, 22.0 * HOUR + 600.0)])]);
        assert_eq!(item(&fx, &fx.board()).used_up_note(now()).as_deref(), Some("22h 10m"));
        assert_eq!(item(&fx, &fx.board()).used_up_note(now() + secs(23.0 * HOUR)).as_deref(), Some("soon"));
        let none = Fixture::new(vec![(ProviderKind::Claude, vec![unpaced("Credits", 100.0, 100.0)])]);
        assert_eq!(item(&none, &none.board()).used_up_note(now()), None);
        let balance = MetricLine::Values { label: "Balance".into(), values: vec![MetricValue { number: 0.0, currency_code: Some("usd".into()), label: None }] };
        let empty = Fixture::new(vec![(ProviderKind::Openrouter, vec![balance])]);
        assert_eq!(item(&empty, &empty.board()).used_up_note(now()).as_deref(), Some("$0"));
    }

    #[test]
    fn row_values_show_cents_unless_whole() {
        let m = |used, limit| Meter::from_line(ProviderKind::Claude, &unpaced("x", used, limit)).unwrap();
        assert_eq!(m(12.5, 50.0).value_text(false), "$12.50");
        assert_eq!(m(12.0, 50.0).value_text(true), "$38");
        assert_eq!(m(25.0, 50.0).fill_fraction(false), 0.5);
        assert_eq!(values_text(&[MetricValue { number: 3.0, currency_code: None, label: None }, MetricValue { number: 1500.0, currency_code: None, label: None }]), "3 · 1.5K");
    }
}
