use chrono::DateTime;
use serde_json::json;

use super::mapper::*;
use crate::models::{MetricLine, MetricValue, ProgressFormat};
use crate::provider::ProviderError;

const MONTH_MS: i64 = 30 * 24 * 3600 * 1000;

fn map(body: serde_json::Value) -> Result<Usage, ProviderError> {
    map_usage(body.to_string().as_bytes())
}

fn pct(label: &str, used: f64, reset: Option<&str>) -> MetricLine {
    MetricLine::Progress {
        label: label.into(),
        used,
        limit: 100.0,
        format: ProgressFormat::Percent,
        resets_at: reset.map(|r| DateTime::parse_from_rfc3339(r).unwrap().into()),
        period_ms: Some(MONTH_MS),
    }
}

fn count(label: &str, number: f64) -> MetricLine {
    MetricLine::Values { label: label.into(), values: vec![MetricValue { number, currency_code: None, label: None }] }
}

#[test]
fn uses_the_provider_reported_remaining_credits() {
    let usage = map(json!({"copilot_plan": "pro", "quota_snapshots": {"premium_interactions": {"entitlement": 100, "remaining": 75}}})).unwrap();
    assert_eq!(usage.plan.as_deref(), Some("Pro"));
    assert_eq!(usage.lines, [pct("Credits", 25.0, None)]);
}

#[test]
fn percent_remaining_wins_and_the_reset_date_may_be_a_bare_day() {
    let usage = map(json!({"copilot_plan": "copilot_for-BUSINESS", "quota_reset_date": "2026-12-01",
        "quota_snapshots": {"premium_interactions": {"entitlement": 300, "remaining": 240, "percent_remaining": 80}}})).unwrap();
    assert_eq!(usage.plan.as_deref(), Some("Copilot For Business"));
    assert_eq!(usage.lines, [pct("Credits", 20.0, Some("2026-12-01T00:00:00Z"))]);
}

#[test]
fn extra_usage_hides_when_not_permitted_and_shows_zero_when_permitted() {
    let lines = |permitted: bool| {
        map(json!({"copilot_plan": "individual", "quota_reset_date": "2026-12-01", "quota_snapshots": {"premium_interactions":
            {"entitlement": 300, "remaining": 300, "percent_remaining": 100, "overage_permitted": permitted, "overage_count": 0}}}))
        .unwrap()
        .lines
    };
    assert_eq!(lines(false).len(), 1);
    assert_eq!(lines(true)[1], count("Extra Usage", 0.0));
}

#[test]
fn unlimited_and_zero_entitlement_snapshots_are_suppressed_and_chat_completions_follow() {
    let usage = map(json!({"quota_snapshots": {
        "premium_interactions": {"entitlement": 0, "remaining": 0},
        "chat": {"unlimited": true},
        "completions": {"entitlement": 100, "remaining": -1}}}));
    assert_eq!(usage.err(), Some(ProviderError::Unavailable("Copilot usage data is unavailable for this account.".into())));
    let shown = map(json!({"quota_snapshots": {"chat": {"entitlement": 50, "remaining": 10}, "completions": {"entitlement": 50, "remaining": 50}}})).unwrap();
    assert_eq!(shown.lines, [pct("Chat", 80.0, None), pct("Completions", 0.0, None)]);
}

#[test]
fn limited_user_quotas_are_the_fallback() {
    let usage = map(json!({"limited_user_reset_date": "2026-12-01T00:00:00Z", "limited_user_quotas": {"chat": 40, "completions": 2000}, "monthly_quotas": {"chat": 50, "completions": 2000}})).unwrap();
    assert_eq!(usage.lines, [pct("Chat", 20.0, Some("2026-12-01T00:00:00Z")), pct("Completions", 0.0, Some("2026-12-01T00:00:00Z"))]);
}

#[test]
fn token_based_billing_shows_credits_used_only_when_positive() {
    let billing = |used: i64| map(json!({"token_based_billing": true, "quota_snapshots": {"premium_interactions": {"credits_used": used}}})).unwrap().lines;
    assert_eq!(billing(12), [count("Credits", 12.0)]);
    assert!(billing(0).is_empty());
}

#[test]
fn an_unreadable_body_is_invalid() {
    assert_eq!(map_usage(b"not json").err(), Some(ProviderError::InvalidResponse));
}

fn usd(number: f64) -> MetricLine {
    MetricLine::Values { label: "Org Spend".into(), values: vec![MetricValue { number, currency_code: Some("usd".into()), label: None }] }
}

#[test]
fn org_usage_counts_copilot_credits_and_spend_only() {
    let body = br#"{"timePeriod":{"year":2025},"organization":"GitHub","usageItems":[{"product":"Copilot","sku":"Copilot AI Credits","grossQuantity":100,"netAmount":1.0},{"product":"Actions","grossQuantity":500,"netAmount":4.0}]}"#;
    assert_eq!(org_usage_lines(body), Some(vec![count("Org Credits", 100.0), usd(1.0)]));
    assert_eq!(org_usage_lines(br#"{"usageItems":[{"product":"Actions","grossQuantity":500,"netAmount":4.0}]}"#), Some(vec![count("Org Credits", 0.0), usd(0.0)]), "an org with no Copilot usage yet shows zero");
}

#[test]
fn an_invalid_org_report_maps_to_nothing() {
    for body in ["not json", "{}", r#"{"usageItems":{"product":"Copilot"}}"#, r#"{"usageItems":[{"product":"Copilot","grossQuantity":100}]}"#] {
        assert_eq!(org_usage_lines(body.as_bytes()), None, "{body}");
    }
}
