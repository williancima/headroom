//! The limits that need the user, most urgent first, for the tray label and the Priority tab.
//! Pure: no I/O, no threads.
//!
//! Every account is its own row (unified multi-account rows are phase 8). A line id is
//! `"{card_id}/{label}"`. The Priority tab strings (`runOutText`, `resetsInText`, `usedUpNote`) are in `text`;
//! its durations use UTC calendar months and its reset readings are relative only.

mod menu_bar;
mod meter;
mod text;
#[cfg(test)]
mod tests;

pub use menu_bar::{menu_bar_content, short_label, MenuBarContent, MenuBarItem, MenuBarLimit};
pub use text::{duration, values_text};
pub use meter::{reset_window_advanced, BurningFast, Meter, MeterState, Severity};

use crate::envelope::balance_amount;
use crate::models::{Account, MetricLine, ProviderSnapshot};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};

/// Defaults: burning fast on and use it or lose it on, tray up to 3, always show off, thresholds 30/10,
/// shown as used. There is no settings screen yet, so `Default` is what the app uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub burning_fast: bool,
    pub use_it_or_lose_it: bool,
    pub menu_bar_limit: MenuBarLimit,
    pub always_show: bool,
    /// Percent left at or under which a limit is a warning.
    pub warning: i64,
    /// Percent left at or under which a limit is critical.
    pub critical: i64,
    /// "Show Usage As" left instead of used, for the tray value.
    pub show_remaining: bool,
    /// Minutes between refreshes: a reading older than this shows "Last updated".
    pub refresh_minutes: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            burning_fast: true,
            use_it_or_lose_it: true,
            menu_bar_limit: MenuBarLimit::UpToThree,
            always_show: false,
            warning: 30,
            critical: 10,
            show_remaining: false,
            refresh_minutes: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReadStatus {
    Waiting,
    Current,
    Unavailable(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    RunningOut,
    UsedUp,
    UseItOrLoseIt,
}

/// Why a limit is on the board.
#[derive(Debug, Clone, PartialEq)]
pub enum Reason {
    /// Listed for what's left, the same rule that colors its bar.
    Low { severity: Severity, remaining: f64, runway: Option<f64> },
    /// Plenty left, but it would run out before it resets at the current pace.
    BurningFast { runway: f64 },
    UsedUp { resets_at: Option<DateTime<Utc>> },
    EmptyBalance,
    UseItOrLoseIt { resets_at: DateTime<Utc> },
}

impl Reason {
    pub fn severity(&self) -> Severity {
        match self {
            Self::Low { severity, .. } => *severity,
            Self::BurningFast { .. } | Self::UseItOrLoseIt { .. } => Severity::Normal,
            Self::UsedUp { .. } | Self::EmptyBalance => Severity::Critical,
        }
    }

    pub fn section(&self) -> Section {
        match self {
            Self::Low { .. } | Self::BurningFast { .. } => Section::RunningOut,
            Self::UsedUp { .. } | Self::EmptyBalance => Section::UsedUp,
            Self::UseItOrLoseIt { .. } => Section::UseItOrLoseIt,
        }
    }

    /// Sorts ascending: what runs out soonest first, then what has no pace to tell by, by what's left.
    fn rank(&self) -> (u8, f64) {
        match self {
            Self::Low { runway: Some(runway), .. } | Self::BurningFast { runway } => (0, *runway),
            Self::Low { remaining, runway: None, .. } => (1, *remaining),
            Self::UsedUp { resets_at } => (2, resets_at.map_or(f64::INFINITY, |d| d.timestamp_millis() as f64)),
            Self::EmptyBalance => (2, f64::INFINITY),
            Self::UseItOrLoseIt { resets_at } => (3, resets_at.timestamp_millis() as f64),
        }
    }
}

/// A line of the board, named as the panel names it.
#[derive(Debug, Clone, PartialEq)]
pub struct LineRef {
    pub id: String,
    pub card_id: String,
    pub label: String,
    pub menu_bar_value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PriorityItem {
    pub line: LineRef,
    pub reason: Reason,
    /// The line's meter; None for an empty balance, which has no limit.
    pub meter: Option<Meter>,
    /// The user pinned it. A pinned item stays under Pinned unless it's used up.
    pub is_pinned: bool,
}

/// A metric the user pinned, listed under Pinned until it's used up.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedMetric {
    pub line: LineRef,
    /// Why Running Out or Use It or Lose It would list it if it weren't pinned.
    pub item: Option<PriorityItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Summary {
    WaitingForUsage,
    NoReportedLimits,
    Unavailable,
    AllClear,
    Listed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountNotice {
    pub card_id: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PriorityBoard {
    /// In dashboard order. A pinned metric that's used up is listed only under Used Up.
    pub pinned: Vec<PinnedMetric>,
    /// Every limit running out, pinned or not, most urgent first. The tray label shows these.
    pub running_out: Vec<PriorityItem>,
    /// The Running Out section: what's running out and isn't pinned.
    pub attention: Vec<PriorityItem>,
    pub used_up: Vec<PriorityItem>,
    /// Over half left, resetting within a day, soonest reset first.
    pub use_it_or_lose_it: Vec<PriorityItem>,
    pub notices: Vec<AccountNotice>,
    /// Card ids of accounts with no first reading yet, in dashboard order.
    pub waiting: Vec<String>,
    pub summary: Summary,
    /// Every account reported and nothing, pinned or not, is running out.
    pub shows_clear_row: bool,
}

impl PriorityBoard {
    /// Several running-low limits can change places as their windows age, and a burning one can slow off the list.
    pub fn requires_ranking_clock(&self, now: DateTime<Utc>) -> bool {
        if self.running_out.iter().any(|i| matches!(i.reason, Reason::BurningFast { .. })) {
            return true;
        }
        self.running_out.len() > 1
            && self.running_out.iter().filter_map(|i| i.meter.as_ref()).any(|m| {
                m.resets_at.is_some_and(|reset| reset > now) && m.period_ms.is_some_and(|period| period > 0)
            })
    }

    pub fn has_rows(&self) -> bool {
        self.summary == Summary::Listed || !self.pinned.is_empty()
    }

    /// `accounts` is in the user's saved order, which breaks every tie. A card id missing from `status` reads as
    /// Current with a snapshot and Waiting without one.
    pub fn make(
        accounts: &[Account],
        snapshots: &HashMap<String, ProviderSnapshot>,
        status: &HashMap<String, ReadStatus>,
        pinned_ids: &HashSet<String>,
        latched: &BurningFast,
        settings: &Settings,
        now: DateTime<Utc>,
    ) -> Self {
        let mut ranked: Vec<PriorityItem> = vec![];
        let mut pinned: Vec<PinnedMetric> = vec![];
        let mut any_bounded = false;
        for account in accounts {
            let card_id = account.card_id();
            let Some(snapshot) = snapshots.get(&card_id) else { continue };
            for line in &snapshot.lines {
                let meter = Meter::from_line(account.kind, line);
                let (value, amount) = match line {
                    MetricLine::Progress { .. } => (meter.as_ref().map(|m| m.menu_bar_value(settings.show_remaining)), None),
                    MetricLine::Values { values, .. } => {
                        (Some(menu_bar::values_menu_bar_value(values)), balance_amount(account.kind, line))
                    }
                    MetricLine::Badge { .. } => continue,
                };
                any_bounded |= meter.is_some();
                let line_ref = LineRef {
                    id: crate::layout::line_id(&card_id, line.label()),
                    card_id: card_id.clone(),
                    label: line.label().to_string(),
                    menu_bar_value: value.unwrap_or_default(),
                };
                let is_pinned = pinned_ids.contains(&line_ref.id);
                let reason = if amount == Some(0.0) {
                    Some(Reason::EmptyBalance)
                } else {
                    meter.as_ref().and_then(|m| reason(&line_ref.id, m, latched, settings, now))
                };
                let item = reason.map(|reason| PriorityItem { line: line_ref.clone(), reason, meter: meter.clone(), is_pinned });
                if is_pinned {
                    pinned.push(PinnedMetric { line: line_ref, item: item.clone() });
                }
                ranked.extend(item);
            }
        }
        // A stable sort keeps the saved order for equal ranks.
        ranked.sort_by(|a, b| {
            let (l, r) = (a.reason.rank(), b.reason.rank());
            l.0.cmp(&r.0).then(l.1.total_cmp(&r.1))
        });

        let mut waiting = vec![];
        let mut notices = vec![];
        for account in accounts {
            let card_id = account.card_id();
            let read = status.get(&card_id).cloned().unwrap_or(if snapshots.contains_key(&card_id) {
                ReadStatus::Current
            } else {
                ReadStatus::Waiting
            });
            match read {
                ReadStatus::Waiting => waiting.push(card_id),
                ReadStatus::Unavailable(message) => notices.push(AccountNotice { card_id, message }),
                ReadStatus::Current => {}
            }
        }
        let awaiting_usage = accounts.is_empty() || !waiting.is_empty();
        let summary = if !ranked.is_empty() {
            Summary::Listed
        } else if !notices.is_empty() {
            Summary::Unavailable
        } else if awaiting_usage {
            Summary::WaitingForUsage
        } else if any_bounded {
            Summary::AllClear
        } else {
            Summary::NoReportedLimits
        };
        let in_section = |section: Section| ranked.iter().filter(move |i| i.reason.section() == section).cloned();
        let running_out: Vec<_> = in_section(Section::RunningOut).collect();
        Self {
            pinned: pinned.into_iter().filter(|p| p.item.as_ref().map(|i| i.reason.section()) != Some(Section::UsedUp)).collect(),
            attention: running_out.iter().filter(|i| !i.is_pinned).cloned().collect(),
            shows_clear_row: !awaiting_usage && notices.is_empty() && running_out.is_empty(),
            running_out,
            used_up: in_section(Section::UsedUp).collect(),
            use_it_or_lose_it: in_section(Section::UseItOrLoseIt).filter(|i| !i.is_pinned).collect(),
            notices,
            waiting,
            summary,
        }
    }
}

impl BurningFast {
    /// The latch after a reading: every limit with a reset that is
    /// burning now, or still close to its limit in the window it was burning in. Save it and pass it to `make`.
    pub fn after_reading(&self, accounts: &[Account], snapshots: &HashMap<String, ProviderSnapshot>, now: DateTime<Utc>) -> Self {
        let mut latched = HashMap::new();
        for account in accounts {
            let card_id = account.card_id();
            let Some(snapshot) = snapshots.get(&card_id) else { continue };
            for line in &snapshot.lines {
                let Some(meter) = Meter::from_line(account.kind, line) else { continue };
                let id = crate::layout::line_id(&card_id, line.label());
                if let Some(reset) = meter.resets_at.filter(|_| self.contains(&id, &meter, now)) {
                    latched.insert(id, reset);
                }
            }
        }
        Self { latched }
    }
}

/// The reason of a bounded meter.
fn reason(id: &str, meter: &Meter, latched: &BurningFast, settings: &Settings, now: DateTime<Utc>) -> Option<Reason> {
    if meter.state(now) == MeterState::Spent {
        return Some(Reason::UsedUp { resets_at: meter.resets_at });
    }
    let severity = meter.severity(settings.warning, settings.critical);
    if severity != Severity::Normal {
        return Some(Reason::Low { severity, remaining: meter.remaining_fraction(), runway: meter.runway(now) });
    }
    if settings.burning_fast && latched.contains(id, meter, now) {
        if let Some(runway) = meter.runway(now) {
            return Some(Reason::BurningFast { runway });
        }
    }
    if settings.use_it_or_lose_it && meter.is_unused_near_reset(now) {
        return Some(Reason::UseItOrLoseIt { resets_at: meter.resets_at? });
    }
    None
}
