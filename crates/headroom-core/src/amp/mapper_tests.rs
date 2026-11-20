use chrono::DateTime;

use super::mapper::*;
use crate::models::{MetricLine, MetricValue, ProgressFormat};
use crate::provider::ProviderError;

/// A real `userDisplayBalanceInfo` answer for a Megawatt account, with the email and username redacted.
pub const MEGAWATT: &str = r#"{"ok":true,"result":{"displayText":"Signed in as amp@example.com (example)\nAmp Megawatt Tier: agent usage $19.98 of $20.00 remaining (100%), orb usage 737.6h of 750h a1.small orb hours remaining (98%) - period 2026-11-09 to 2026-12-09, ends in 29 days\nIndividual credits: $57.45 remaining (set up auto-reload to avoid running out) - https://ampcode.com/settings"}}"#;
const PERIOD_START: i64 = 1_794_182_400;
const PERIOD_END: i64 = 1_796_774_400;

pub fn balance(lines: &str) -> String {
    format!(r#"{{"ok":true,"result":{{"displayText":"Signed in as amp@example.com (example)\n{lines}"}}}}"#)
}

fn usd(number: f64) -> MetricLine {
    MetricLine::Values { label: "Individual Credits".into(), values: vec![MetricValue { number, currency_code: Some("usd".into()), label: None }] }
}

fn percent(used: f64) -> MetricLine {
    MetricLine::Progress { label: "Amp Free".into(), used, limit: 100.0, format: ProgressFormat::Percent, resets_at: None, period_ms: Some(86_400_000) }
}

#[test]
fn a_tier_account_shows_agent_usage_orb_hours_and_credits() {
    let usage = map(MEGAWATT.as_bytes()).unwrap();
    let end = DateTime::from_timestamp(PERIOD_END, 0);
    let period = Some((PERIOD_END - PERIOD_START) * 1000);
    assert_eq!(usage.plan.as_deref(), Some("Megawatt"));
    assert_eq!(
        usage.lines,
        [
            MetricLine::Progress { label: "Agent Usage".into(), used: 0.02, limit: 20.0, format: ProgressFormat::Dollars, resets_at: end, period_ms: period },
            MetricLine::Progress { label: "Orb Hours".into(), used: 12.4, limit: 750.0, format: ProgressFormat::Count("hours".into()), resets_at: end, period_ms: period },
            usd(57.45),
        ]
    );
}

#[test]
fn a_free_account_shows_its_daily_allowance_and_an_empty_credit_balance() {
    let text = balance(r"Amp Free: 61% remaining today (resets daily) - https://ampcode.com/settings#amp-free\nIndividual credits: $0 remaining - https://ampcode.com/settings");
    let usage = map(text.as_bytes()).unwrap();
    assert_eq!(usage.plan.as_deref(), Some("Free"));
    assert_eq!(usage.lines, [percent(39.0), usd(0.0)], "a balance shows even at $0");
}

#[test]
fn amp_free_in_dollars_reads_as_the_share_spent() {
    let text = balance("Amp Free: $7.50/$10.00 remaining (replenishes +$0.42/hour) - https://ampcode.com/settings#amp-free");
    assert_eq!(map(text.as_bytes()).unwrap().lines, [percent(25.0)], "no reset time is made up from the replenish rate");
}

#[test]
fn text_with_no_allowance_is_an_invalid_response() {
    let error = |body: &str| map(body.as_bytes()).err();
    assert_eq!(error(&balance("Amp is down for maintenance")), Some(ProviderError::InvalidResponse));
    assert_eq!(error("not json"), Some(ProviderError::InvalidResponse));
    assert_eq!(error(r#"{"result":{}}"#), Some(ProviderError::InvalidResponse));
}

#[test]
fn a_tier_without_a_period_has_no_reset_and_a_backwards_period_is_ignored() {
    for rest in ["", " - period 2026-12-09 to 2026-11-09", " - period 2026-1-9 to 2026-12-09"] {
        let text = balance(&format!("Amp Gigawatt Tier: agent usage $5.00 of $10.00 remaining (50%){rest}"));
        let usage = map(text.as_bytes()).unwrap();
        assert_eq!(usage.plan.as_deref(), Some("Gigawatt"));
        assert!(matches!(&usage.lines[0], MetricLine::Progress { used, resets_at: None, period_ms: None, .. } if *used == 5.0), "{rest}");
    }
}

#[test]
fn thousands_separators_and_bold_markers_are_handled() {
    let text = balance("Amp **Big** Tier: agent usage $1,000.50 of $2,000.00 remaining");
    let usage = map(text.as_bytes()).unwrap();
    assert_eq!(usage.plan.as_deref(), Some("Big"));
    assert!(matches!(&usage.lines[0], MetricLine::Progress { used, limit, .. } if (*used, *limit) == (999.5, 2000.0)));
}

#[test]
fn the_email_is_the_address_after_signed_in_as() {
    assert_eq!(email(MEGAWATT.as_bytes()).as_deref(), Some("amp@example.com"));
    assert_eq!(email(balance("x").as_bytes()).as_deref(), Some("amp@example.com"));
    assert_eq!(email(br#"{"result":{"displayText":"Welcome"}}"#), None);
}
