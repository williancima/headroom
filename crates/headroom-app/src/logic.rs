//! Decisions of the app loop, pure so they can be tested without a clock, network or D-Bus.

use chrono::{DateTime, Duration, Utc};
use headroom_core::priority::PriorityBoard;
use headroom_core::{Account, ProviderSnapshot};

const HUB_SYNC_EVERY: Duration = Duration::hours(1);

/// Missing or at least one interval old: what launch fetches.
pub fn stale(snapshot: Option<&ProviderSnapshot>, now: DateTime<Utc>, interval: Duration) -> bool {
    snapshot.is_none_or(|s| now - s.refreshed_at >= interval)
}

/// Checked on every 30 s tick. A wake from sleep shows as a wall-clock jump, so this covers it too.
pub fn pass_due(last_pass: DateTime<Utc>, now: DateTime<Utc>, interval: Duration) -> bool {
    now - last_pass >= interval
}

/// Hub login lists are read on launch (`last` is `None`), hourly, and on a manual refresh.
pub fn hub_sync_due(last: Option<DateTime<Utc>>, now: DateTime<Utc>, forced: bool) -> bool {
    forced || last.is_none_or(|at| now - at >= HUB_SYNC_EVERY)
}

/// One line per limit running out, then the accounts that could not be read.
pub fn tooltip_text(board: &PriorityBoard, accounts: &[Account]) -> String {
    let name = |card_id: &str| accounts.iter().find(|a| a.card_id() == card_id).map_or(card_id.to_string(), |a| a.full_name());
    let mut lines: Vec<String> =
        board.running_out.iter().map(|i| format!("{}: {} {}", name(&i.line.card_id), i.line.label, i.line.menu_bar_value)).collect();
    lines.extend(board.notices.iter().map(|n| format!("{}: {}", name(&n.card_id), n.message)));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-11-19T12:00:00Z".parse().unwrap()
    }

    fn snapshot_aged(age: Duration) -> ProviderSnapshot {
        ProviderSnapshot { plan: None, lines: vec![], refreshed_at: now() - age, reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() }
    }

    #[test]
    fn launch_fetches_missing_and_five_minute_old_snapshots() {
        let five = Duration::minutes(5);
        assert!(stale(None, now(), five));
        assert!(stale(Some(&snapshot_aged(Duration::minutes(5))), now(), five));
        assert!(!stale(Some(&snapshot_aged(Duration::minutes(4))), now(), five));
        assert!(!stale(Some(&snapshot_aged(Duration::minutes(29))), now(), Duration::minutes(30)));
    }

    #[test]
    fn a_pass_is_due_after_the_interval_or_a_clock_jump() {
        let five = Duration::minutes(5);
        assert!(!pass_due(now(), now() + Duration::minutes(4), five));
        assert!(pass_due(now(), now() + Duration::minutes(5), five));
        assert!(pass_due(now(), now() + Duration::hours(8), five));
        assert!(!pass_due(now(), now() + Duration::minutes(29), Duration::minutes(30)));
        // The clock moved back: wait for it to catch up.
        assert!(!pass_due(now(), now() - Duration::hours(1), five));
    }

    #[test]
    fn hub_lists_sync_on_launch_hourly_and_on_a_manual_refresh() {
        assert!(hub_sync_due(None, now(), false));
        assert!(!hub_sync_due(Some(now() - Duration::minutes(59)), now(), false));
        assert!(hub_sync_due(Some(now() - Duration::hours(1)), now(), false));
        assert!(hub_sync_due(Some(now()), now(), true));
    }
}
