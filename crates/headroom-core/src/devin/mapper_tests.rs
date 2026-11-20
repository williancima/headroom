//! Devin usage mapping: plan, quota and extra-usage lines.

use chrono::{DateTime, Utc};

use super::mapper::*;
use crate::models::{MetricLine, MetricValue, ProgressFormat, ProviderSnapshot};
use crate::provider::ProviderError;

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000, 0).unwrap()
}

fn at(secs: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(secs, 0)
}

fn map(plan_status: &str) -> Result<ProviderSnapshot, ProviderError> {
    map_usage(format!(r#"{{"userStatus":{{"planStatus":{plan_status}}}}}"#).as_bytes(), now())
}

fn percent(label: &str, used: f64, resets_at: Option<DateTime<Utc>>, period_ms: i64) -> MetricLine {
    MetricLine::Progress { label: label.into(), used, limit: 100.0, format: ProgressFormat::Percent, resets_at, period_ms: Some(period_ms) }
}

fn balance(number: f64) -> MetricLine {
    MetricLine::Values { label: "Extra usage balance".into(), values: vec![MetricValue { number, currency_code: Some("usd".into()), label: None }] }
}

#[test]
fn maps_the_plan_both_quotas_as_used_and_the_balance_in_dollars() {
    let usage = map(
        r#"{"planInfo":{"planName":" Max ","billingStrategy":"BILLING_STRATEGY_QUOTA"},
         "dailyQuotaRemainingPercent":100,"weeklyQuotaRemainingPercent":40,"overageBalanceMicros":"964220000",
         "dailyQuotaResetAtUnix":"1774080000","weeklyQuotaResetAtUnix":"1774166400"}"#,
    )
    .unwrap();
    assert_eq!(usage.plan.as_deref(), Some("Max"));
    assert_eq!(
        usage.lines,
        [percent("Daily quota", 0.0, at(1_774_080_000), DAY_MS), percent("Weekly quota", 60.0, at(1_774_166_400), WEEK_MS), balance(964.22)]
    );
}

#[test]
fn a_zero_or_negative_balance_reads_zero_dollars() {
    for micros in [r#""0""#, r#""-44347""#] {
        let usage = map(&format!(r#"{{"weeklyQuotaRemainingPercent":40,"overageBalanceMicros":{micros}}}"#)).unwrap();
        assert_eq!(usage.lines.last(), Some(&balance(0.0)), "{micros}");
    }
}

#[test]
fn a_hidden_daily_quota_fills_the_weekly_row_when_there_is_no_weekly_quota() {
    let usage = map(r#"{"planInfo":{"planName":"Pro","hideDailyQuota":true},"dailyQuotaRemainingPercent":30,"dailyQuotaResetAtUnix":"1774080000"}"#).unwrap();
    assert_eq!(usage.lines, [percent("Weekly quota", 70.0, None, WEEK_MS)]);
}

#[test]
fn an_omitted_weekly_share_beside_a_reset_is_used_up() {
    for hide in [true, false] {
        let usage = map(&format!(
            r#"{{"planInfo":{{"planName":"Max","hideDailyQuota":{hide}}},"dailyQuotaRemainingPercent":100,"dailyQuotaResetAtUnix":"1774080000","weeklyQuotaResetAtUnix":"1774166400"}}"#
        ))
        .unwrap();
        assert_eq!(usage.lines.iter().find(|l| l.label() == "Weekly quota"), Some(&percent("Weekly quota", 100.0, at(1_774_166_400), WEEK_MS)), "{hide}");
    }
}

#[test]
fn an_omitted_daily_share_beside_a_reset_is_used_up() {
    let usage = map(r#"{"weeklyQuotaRemainingPercent":40,"dailyQuotaResetAtUnix":"1774080000"}"#).unwrap();
    assert_eq!(usage.lines.first(), Some(&percent("Daily quota", 100.0, at(1_774_080_000), DAY_MS)));
}

#[test]
fn an_unreadable_share_is_an_invalid_response_not_a_used_up_quota() {
    for key in ["dailyQuotaRemainingPercent", "weeklyQuotaRemainingPercent"] {
        for malformed in [r#""abc""#, "true", "{}"] {
            let result = map(&format!(r#"{{"{key}":{malformed},"dailyQuotaResetAtUnix":"1774080000","weeklyQuotaResetAtUnix":"1774166400"}}"#));
            assert_eq!(result.unwrap_err(), ProviderError::InvalidResponse, "{key}: {malformed}");
        }
    }
}

#[test]
fn an_account_with_no_quota_and_no_balance_says_so() {
    let error = map(r#"{"planInfo":{"planName":"Max"}}"#).unwrap_err();
    assert_eq!(error.to_string(), "Devin reports no quota for this account.");
}

#[test]
fn a_body_without_a_user_status_is_an_invalid_response() {
    assert_eq!(map_usage(br#"{"code":"internal"}"#, now()).unwrap_err(), ProviderError::InvalidResponse);
    assert_eq!(map_usage(b"nope", now()).unwrap_err(), ProviderError::InvalidResponse);
}
