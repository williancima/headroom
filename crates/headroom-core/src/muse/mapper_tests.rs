use chrono::{DateTime, Duration, Utc};

use super::mapper::*;
use crate::models::{MetricLine, ProgressFormat};
use crate::provider::ProviderError;

pub const KEY: &str = "LLM|1234|SYNTHETIC-inference-key";

pub fn start() -> DateTime<Utc> {
    DateTime::from_timestamp(1_793_790_000, 0).unwrap()
}

pub fn body(subscribed: bool, usage: Option<&str>) -> String {
    let usage = usage.map(|u| format!(r#","subs_usage":{u}"#)).unwrap_or_default();
    format!(r#"{{"api_key":"{KEY}","user_email":"muse@example.com","is_subs_active":{subscribed},"subs_tier_name":"Power Usage"{usage}}}"#)
}

pub fn active() -> String {
    let (session, weekly) = ((start() + Duration::hours(3)).timestamp(), (start() + Duration::days(3)).timestamp());
    body(true, Some(&format!(r#"{{"window":{{"used_percent":42,"resets_at":{session},"window_duration_mins":300}},"weekly":{{"used_percent":18,"resets_at":{weekly}}}}}"#)))
}

pub fn percent(label: &str, used: f64, resets_at: Option<DateTime<Utc>>, period_ms: i64) -> MetricLine {
    MetricLine::Progress { label: label.into(), used, limit: 100.0, format: ProgressFormat::Percent, resets_at, period_ms: Some(period_ms) }
}

fn run(text: &str, last_known: &[MetricLine], now: DateTime<Utc>) -> Result<Vec<MetricLine>, ProviderError> {
    Ok(map(&parse_account(text.as_bytes())?, last_known, now)?.lines)
}

fn reported() -> Vec<MetricLine> {
    run(&active(), &[], start()).unwrap()
}

#[test]
fn reads_the_plan_session_and_weekly() {
    let account = parse_account(active().as_bytes()).unwrap();
    let usage = map(&account, &[], start()).unwrap();
    assert_eq!(usage.plan.as_deref(), Some("Power Usage"));
    assert_eq!(
        usage.lines,
        [
            percent("Session", 42.0, Some(start() + Duration::hours(3)), SESSION_MS),
            percent("Weekly", 18.0, Some(start() + Duration::days(3)), WEEK_MS),
        ]
    );
}

#[test]
fn a_window_muse_leaves_out_keeps_its_last_known_value() {
    let kept = reported();
    assert_eq!(run(&body(true, None), &kept, start()).unwrap(), kept, "an idle window is not 0%");
    let after_reset = run(&body(true, None), &kept, start() + Duration::hours(3) + Duration::minutes(1)).unwrap();
    assert_eq!(after_reset, [percent("Session", 0.0, None, SESSION_MS), percent("Weekly", 18.0, Some(start() + Duration::days(3)), WEEK_MS)], "a kept window past its reset has reset");
}

#[test]
fn a_reported_usage_keeps_nothing_it_left_out() {
    let partial = body(true, Some(r#"{"weekly":{"used_percent":20}}"#));
    assert_eq!(run(&partial, &reported(), start()).unwrap(), [percent("Session", 0.0, None, SESSION_MS), percent("Weekly", 20.0, None, WEEK_MS)]);
}

#[test]
fn a_plan_not_used_yet_shows_session_and_weekly_at_zero() {
    assert_eq!(run(&body(true, None), &[], start()).unwrap(), [percent("Session", 0.0, None, SESSION_MS), percent("Weekly", 0.0, None, WEEK_MS)]);
}

#[test]
fn usage_over_the_limit_is_kept() {
    let over = body(true, Some(r#"{"weekly":{"used_percent":130.5,"resets_at":1794000000}}"#));
    assert_eq!(run(&over, &[], start()).unwrap()[1], percent("Weekly", 130.5, DateTime::from_timestamp(1_794_000_000, 0), WEEK_MS));
}

#[test]
fn an_account_without_a_subscription_says_so() {
    let error = run(&body(false, None), &[], start()).unwrap_err();
    assert_eq!(error.to_string(), "No Muse Code subscription on this account.");
}

#[test]
fn the_inference_key_has_no_field_and_an_unreadable_body_is_invalid() {
    let account = parse_account(active().as_bytes()).unwrap();
    assert!(!format!("{account:?}").contains("SYNTHETIC-inference-key"));
    let unreadable = format!(r#"{{"api_key":"{KEY}","is_subs_active":"yes"}}"#);
    assert_eq!(parse_account(unreadable.as_bytes()).unwrap_err(), ProviderError::InvalidResponse);
}
