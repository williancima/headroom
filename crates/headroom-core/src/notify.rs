//! When to tell the user a limit changed, based on pace and
//! quota rules. Pure: the app delivers the notices and reports the ones that failed.
//!
//! A notice's title is `"{milestone}: {account full name} {metric label}"`.

use crate::models::{Account, MetricLine, ProviderSnapshot};
use crate::priority::{reset_window_advanced, Meter, MeterState, Severity};
use crate::settings::Settings;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Milestone {
    Critical,
    Warning,
    BurningFast,
    UseItOrLoseIt,
}

impl Milestone {
    fn id(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Warning => "warning",
            Self::BurningFast => "burning-fast",
            Self::UseItOrLoseIt => "use-it-or-lose-it",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Critical => "Critical",
            Self::Warning => "Warning",
            Self::BurningFast => "Burning Fast",
            Self::UseItOrLoseIt => "Use It or Lose It",
        }
    }

    fn is_level(self) -> bool {
        matches!(self, Self::Critical | Self::Warning)
    }

    fn is_on(self, settings: &Settings) -> bool {
        let n = settings.notify;
        match self {
            Self::Critical => n.critical,
            Self::Warning => n.warning,
            Self::BurningFast => n.burning_fast,
            Self::UseItOrLoseIt => n.use_it_or_lose_it,
        }
    }

    fn body(self, settings: &Settings) -> String {
        match self {
            Self::Critical => format!("{}% or less left for this window.", settings.critical),
            Self::Warning => format!("{}% or less left for this window.", settings.warning),
            Self::BurningFast => "May run out before the limit resets.".into(),
            Self::UseItOrLoseIt => "Over 50% left, resets in under 24 hours.".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Bucket {
    Untracked,
    Healthy,
    Close,
    RunningOut,
}

fn bucket(state: MeterState) -> Bucket {
    match state {
        MeterState::Unpaced => Bucket::Untracked,
        MeterState::Healthy => Bucket::Healthy,
        MeterState::CloseToLimit => Bucket::Close,
        MeterState::RunningOut | MeterState::Spent => Bucket::RunningOut,
    }
}

#[derive(Debug, Clone, PartialEq)]
struct LimitState {
    resets_at: Option<DateTime<Utc>>,
    fired: HashSet<Milestone>,
    bucket: Bucket,
    severity: Severity,
    primed: bool,
}

impl Default for LimitState {
    fn default() -> Self {
        Self { resets_at: None, fired: HashSet::new(), bucket: Bucket::Untracked, severity: Severity::Normal, primed: false }
    }
}

impl LimitState {
    /// A new window clears what fired in the old one.
    fn carried_over(&self, resets_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Self {
        let mut next = self.clone();
        if reset_window_advanced(resets_at, self.resets_at, now) {
            next.fired.clear();
            next.severity = Severity::Normal;
            next.bucket = Bucket::Untracked;
        }
        next.resets_at = resets_at.or(self.resets_at);
        next
    }
}

/// Milestones that would fire now, and the state after.
fn transitions(
    state: MeterState,
    severity: Severity,
    resets_at: Option<DateTime<Utc>>,
    unused_near_reset: bool,
    previous: &LimitState,
    now: DateTime<Utc>,
) -> (Vec<Milestone>, LimitState) {
    let mut next = previous.carried_over(resets_at, now);
    let current = bucket(state);
    if !next.primed {
        next.primed = true;
        next.bucket = current;
        next.severity = severity;
        next.fired = if unused_near_reset { HashSet::from([Milestone::UseItOrLoseIt]) } else { HashSet::new() };
        return (vec![], next);
    }
    let mut fire = vec![];
    if current != Bucket::Untracked {
        if current >= Bucket::RunningOut && next.bucket < Bucket::RunningOut && !next.fired.contains(&Milestone::BurningFast) {
            fire.push(Milestone::BurningFast);
        }
        // Re-arms only once the pace eases to 90% of the limit, where Priority stops listing it as burning fast.
        if current == Bucket::Healthy {
            next.fired.remove(&Milestone::BurningFast);
        }
        next.bucket = current;
    }
    if severity == Severity::Critical && next.severity != Severity::Critical && !next.fired.contains(&Milestone::Critical) {
        fire.push(Milestone::Critical);
    }
    if severity != Severity::Normal && next.severity == Severity::Normal && !next.fired.contains(&Milestone::Warning) {
        fire.push(Milestone::Warning);
    }
    if severity != Severity::Critical {
        next.fired.remove(&Milestone::Critical);
    }
    if severity == Severity::Normal {
        next.fired.remove(&Milestone::Warning);
    }
    next.severity = severity;
    if unused_near_reset && !next.fired.contains(&Milestone::UseItOrLoseIt) {
        fire.push(Milestone::UseItOrLoseIt);
    }
    (fire, next)
}

/// One notification to deliver. Use `id` as the notification id (`"<card id>.<milestone>"`).
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub id: String,
    pub title: String,
    pub body: String,
    pub milestone: Milestone,
}

/// What each bounded limit last looked like. In memory only: a restart re-primes, so it sends no repeat burst.
#[derive(Debug, Default)]
pub struct NotifyState {
    states: HashMap<String, LimitState>,
}

impl NotifyState {
    /// Reads every bounded limit and returns what to deliver. Delivery can't be confirmed (GIO notifications are
    /// fire-and-forget), so each notice counts as delivered.
    pub fn evaluate(
        &mut self,
        accounts: &[Account],
        snapshots: &HashMap<String, ProviderSnapshot>,
        settings: &Settings,
        now: DateTime<Utc>,
    ) -> Vec<Notice> {
        let (mut notices, mut states) = (vec![], HashMap::new());
        for account in accounts {
            let card_id = account.card_id();
            let Some(snapshot) = snapshots.get(&card_id) else { continue };
            for line in &snapshot.lines {
                let Some(meter) = Meter::from_line(account.kind, line) else { continue };
                let key = crate::layout::line_id(&card_id, line.label());
                let previous = self.states.get(&key).cloned().unwrap_or_default();
                let severity = meter.severity(settings.warning, settings.critical);
                let (fire, mut next) =
                    transitions(meter.state(now), severity, meter.resets_at, meter.is_unused_near_reset(now), &previous, now);
                let before = previous.carried_over(meter.resets_at, now);
                let (mut pace_delivered, mut level_delivered) = (false, false);
                for milestone in &fire {
                    // A drop past both levels sends the lowest one that's switched on, not both.
                    if level_delivered && milestone.is_level() {
                        next.fired.insert(*milestone);
                    } else if milestone.is_on(settings) {
                        pace_delivered |= *milestone == Milestone::BurningFast;
                        level_delivered |= milestone.is_level();
                        next.fired.insert(*milestone);
                        notices.push(notice(account, &card_id, line, *milestone, settings));
                    }
                }
                // What didn't go out keeps its old reading, so it is tried again.
                if fire.contains(&Milestone::BurningFast) && !pace_delivered {
                    next.bucket = before.bucket;
                }
                if fire.iter().any(|m| m.is_level()) && !level_delivered {
                    next.severity = before.severity;
                }
                states.insert(key, next);
            }
        }
        self.states = states;
        notices
    }

}

fn notice(account: &Account, card_id: &str, line: &MetricLine, milestone: Milestone, settings: &Settings) -> Notice {
    Notice {
        id: format!("{card_id}.{}", milestone.id()),
        title: format!("{}: {} {}", milestone.title(), account.full_name(), line.label()),
        body: milestone.body(settings),
        milestone,
    }
}
