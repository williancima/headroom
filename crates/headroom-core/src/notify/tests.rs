//! Pace, use-it-or-lose-it and quota notification rules.

use super::*;
use crate::models::{ProgressFormat, ProviderKind};
use crate::settings::NotifyToggles;
use chrono::Duration;

const WEEK_MS: i64 = 7 * 24 * 3600 * 1000;

fn base() -> DateTime<Utc> {
    "2027-01-15T08:00:00Z".parse().unwrap()
}

fn all_on() -> Settings {
    Settings { notify: NotifyToggles { critical: true, warning: true, burning_fast: true, use_it_or_lose_it: true }, ..Default::default() }
}

struct Fixture {
    account: Account,
    state: NotifyState,
}

impl Fixture {
    fn new() -> Self {
        Self { account: Account::new(ProviderKind::Claude, "Work"), state: NotifyState::default() }
    }

    /// A weekly limit with 10% of the week left (resets at `reset`), `used` percent used.
    fn read(&mut self, used: f64, reset: DateTime<Utc>, now: DateTime<Utc>, settings: &Settings) -> Vec<Notice> {
        let line = MetricLine::Progress {
            label: "Weekly".into(),
            used,
            limit: 100.0,
            format: ProgressFormat::Percent,
            resets_at: Some(reset),
            period_ms: Some(WEEK_MS),
        };
        let snapshot = ProviderSnapshot { plan: None, lines: vec![line], refreshed_at: now, reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() };
        let snapshots = HashMap::from([(self.account.card_id(), snapshot)]);
        self.state.evaluate(std::slice::from_ref(&self.account), &snapshots, settings, now)
    }

    fn week(&mut self, used: f64, settings: &Settings) -> Vec<Notice> {
        self.read(used, base() + Duration::hours(16), base(), settings)
    }
}

fn milestones(notices: &[Notice]) -> Vec<Milestone> {
    notices.iter().map(|n| n.milestone).collect()
}

#[test]
fn first_reading_primes_and_fires_nothing() {
    let mut f = Fixture::new();
    assert!(f.week(95.0, &all_on()).is_empty());
    assert!(f.week(95.0, &all_on()).is_empty());
}

#[test]
fn warning_fires_once_on_leaving_normal() {
    let mut f = Fixture::new();
    assert!(f.week(40.0, &all_on()).is_empty());
    let n = f.week(75.0, &all_on());
    assert_eq!(n.len(), 1);
    assert_eq!(n[0].id, format!("{}.warning", f.account.card_id()));
    assert_eq!(n[0].title, "Warning: Claude · Work Weekly");
    assert_eq!(n[0].body, "30% or less left for this window.");
    assert!(f.week(78.0, &all_on()).is_empty());
}

#[test]
fn a_drop_past_both_levels_sends_only_critical() {
    let mut f = Fixture::new();
    f.week(40.0, &all_on());
    let n = f.week(95.0, &all_on());
    assert!(milestones(&n).contains(&Milestone::Critical) && !milestones(&n).contains(&Milestone::Warning));
    assert_eq!(n.iter().find(|n| n.milestone == Milestone::Critical).unwrap().body, "10% or less left for this window.");
}

#[test]
fn a_drop_past_both_levels_with_critical_off_sends_warning() {
    let mut settings = all_on();
    settings.notify.critical = false;
    let mut f = Fixture::new();
    f.week(40.0, &settings);
    assert!(milestones(&f.week(95.0, &settings)).contains(&Milestone::Warning));
    assert!(!milestones(&f.week(96.0, &settings)).contains(&Milestone::Warning));
}

#[test]
fn critical_after_warning_and_rearm_after_recovery() {
    let mut f = Fixture::new();
    f.week(40.0, &all_on());
    assert_eq!(milestones(&f.week(75.0, &all_on())), vec![Milestone::Warning]);
    assert_eq!(milestones(&f.week(95.0, &all_on())), vec![Milestone::BurningFast, Milestone::Critical]);
    f.week(40.0, &all_on());
    assert_eq!(milestones(&f.week(75.0, &all_on())), vec![Milestone::Warning]);
}

#[test]
fn burning_fast_fires_on_entering_running_out_and_rearms_at_healthy() {
    let mut settings = all_on();
    settings.notify.critical = false;
    settings.notify.warning = false;
    let mut f = Fixture::new();
    f.week(40.0, &settings);
    let n = f.week(95.0, &settings);
    assert_eq!(milestones(&n), vec![Milestone::BurningFast]);
    assert_eq!(n[0].title, "Burning Fast: Claude · Work Weekly");
    assert_eq!(n[0].body, "May run out before the limit resets.");
    assert!(f.week(96.0, &settings).is_empty());
    f.week(40.0, &settings);
    assert_eq!(milestones(&f.week(95.0, &settings)), vec![Milestone::BurningFast]);
}

#[test]
fn use_it_or_lose_it_primed_stays_quiet_then_fires_on_entering() {
    let mut f = Fixture::new();
    // Resets in 12 hours with 60% left: unused near its reset. Primed, so no notice.
    assert!(f.read(40.0, base() + Duration::hours(12), base(), &all_on()).is_empty());
    assert!(f.read(40.0, base() + Duration::hours(11), base() + Duration::hours(1), &all_on()).is_empty());

    let mut f = Fixture::new();
    assert!(f.read(40.0, base() + Duration::hours(30), base(), &all_on()).is_empty());
    let n = f.read(40.0, base() + Duration::hours(20), base() + Duration::hours(10), &all_on());
    assert_eq!(milestones(&n), vec![Milestone::UseItOrLoseIt]);
    assert_eq!(n[0].title, "Use It or Lose It: Claude · Work Weekly");
    assert_eq!(n[0].body, "Over 50% left, resets in under 24 hours.");
    assert!(f.read(40.0, base() + Duration::hours(19), base() + Duration::hours(11), &all_on()).is_empty());
}

#[test]
fn a_new_window_rearms_what_fired() {
    let mut f = Fixture::new();
    let reset = base() + Duration::hours(16);
    f.read(40.0, reset, base(), &all_on());
    assert_eq!(milestones(&f.read(75.0, reset, base(), &all_on())), vec![Milestone::Warning]);
    // Past the reset, the window is the next week; the first reading in it primes.
    let now = reset + Duration::minutes(1);
    let next = reset + Duration::days(7);
    assert!(f.read(10.0, next, now, &all_on()).is_empty());
    assert_eq!(milestones(&f.read(75.0, next, now, &all_on())), vec![Milestone::Warning]);
}

#[test]
fn a_reset_that_jitters_is_the_same_window() {
    let mut f = Fixture::new();
    let reset = base() + Duration::hours(16);
    f.read(40.0, reset, base(), &all_on());
    f.read(75.0, reset, base(), &all_on());
    assert!(f.read(75.0, reset + Duration::milliseconds(500), base(), &all_on()).is_empty());
}

#[test]
fn disabled_toggles_fire_nothing_and_fire_once_enabled() {
    let off = Settings::default();
    let mut f = Fixture::new();
    f.week(40.0, &off);
    assert!(f.week(95.0, &off).is_empty());
    assert_eq!(milestones(&f.week(95.0, &all_on())), vec![Milestone::BurningFast, Milestone::Critical]);
}

#[test]
fn unbounded_and_missing_snapshots_are_skipped() {
    let mut state = NotifyState::default();
    let account = Account::new(ProviderKind::Claude, "x");
    assert!(state.evaluate(&[account], &HashMap::new(), &all_on(), base()).is_empty());
}
