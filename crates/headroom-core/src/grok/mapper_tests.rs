use super::mapper::*;
use crate::models::{MetricLine, ProgressFormat};
use crate::provider::ProviderError;

const START: &str = "2026-06-30T21:36:52.140114+00:00";
const END: &str = "2026-07-07T21:36:52.140114+00:00";

fn body(percent: Option<serde_json::Value>, cap: Option<serde_json::Value>, start: &str, end: &str) -> Vec<u8> {
    let mut config = serde_json::json!({"currentPeriod": {"type": WEEKLY_PERIOD_TYPE, "start": start, "end": end}});
    if let Some(p) = percent {
        config["creditUsagePercent"] = p;
    }
    if let Some(c) = cap {
        config["onDemandCap"] = serde_json::json!({"val": c});
    }
    serde_json::json!({"config": config}).to_string().into_bytes()
}

fn default_body() -> Vec<u8> {
    body(Some(99.0.into()), Some(2500.into()), START, END)
}

#[test]
fn absent_zero_valued_fields_decode_as_zero() {
    assert_eq!(decode_credits_config(&body(None, Some(2500.into()), START, END)).unwrap().used_percent, 0.0);
    assert_eq!(decode_credits_config(&body(Some(99.0.into()), None, START, END)).unwrap().on_demand_cap, 0.0);
}

#[test]
fn non_numeric_fields_are_rejected() {
    for case in [body(Some("high".into()), Some(2500.into()), START, END), body(Some(99.0.into()), Some("lots".into()), START, END)] {
        assert_eq!(decode_credits_config(&case).unwrap_err(), ProviderError::InvalidResponse);
    }
}

#[test]
fn a_period_that_does_not_move_forward_is_rejected() {
    assert_eq!(decode_credits_config(&body(None, None, END, START)).unwrap_err(), ProviderError::InvalidResponse);
}

#[test]
fn missing_config_fields_are_rejected() {
    for case in ["{}", r#"{"config":{}}"#, r#"{"config":{"currentPeriod":{}}}"#, "not json"] {
        assert_eq!(decode_credits_config(case.as_bytes()).unwrap_err(), ProviderError::InvalidResponse, "{case}");
    }
}

#[test]
fn a_pay_as_you_go_cap_too_large_for_an_int_is_an_invalid_response() {
    let case = br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-06-30T21:36:52Z","end":"2026-07-07T21:36:52Z"},"onDemandCap":{"val":1e300}}}"#;
    assert_eq!(map_credits_config(case).unwrap_err(), ProviderError::InvalidResponse);
}

#[test]
fn the_weekly_limit_and_the_cap_are_mapped() {
    let lines = map_credits_config(&default_body()).unwrap();
    let config = decode_credits_config(&default_body()).unwrap();
    assert_eq!(config.period_duration_ms(), 7 * 24 * 3600 * 1000);
    assert_eq!(
        lines,
        [
            MetricLine::Progress { label: "Weekly limit".into(), used: 99.0, limit: 100.0, format: ProgressFormat::Percent, resets_at: Some(config.period_end), period_ms: Some(604_800_000) },
            MetricLine::Badge { label: "Pay as you go".into(), text: "$25 cap".into() },
        ]
    );
}

#[test]
fn the_cap_hides_at_zero_and_formats_like_the_row() {
    assert_eq!(map_credits_config(&body(None, None, START, END)).unwrap().len(), 1);
    let text = |cents: i64| match &map_credits_config(&body(None, Some(cents.into()), START, END)).unwrap()[1] {
        MetricLine::Badge { text, .. } => text.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!((text(2550).as_str(), text(150_000).as_str(), text(100_000).as_str()), ("$25.50 cap", "$1.5K cap", "$1K cap"));
}

#[test]
fn percent_is_clamped_and_a_non_weekly_period_has_no_weekly_line() {
    let lines = map_credits_config(&body(Some(250.into()), None, START, END)).unwrap();
    assert!(matches!(&lines[0], MetricLine::Progress { used, .. } if *used == 100.0));
    let monthly = br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_MONTHLY","start":"2026-06-30T21:36:52Z","end":"2026-07-07T21:36:52Z"}}}"#;
    assert!(map_credits_config(monthly).unwrap().is_empty());
}

#[test]
fn the_team_refusal_is_a_412_naming_no_personal_team() {
    assert!(is_team_refusal(412, br#"{"error":"User is a No Personal Team principal"}"#));
    assert!(!is_team_refusal(412, br#"{"error":"precondition failed"}"#));
    assert!(!is_team_refusal(403, br#"{"error":"no personal team"}"#));
}

#[test]
fn the_plan_is_the_trimmed_tier_display() {
    assert_eq!(plan_name(br#"{"subscription_tier_display":" SuperGrok "}"#).as_deref(), Some("SuperGrok"));
    assert_eq!(plan_name(br#"{"subscription_tier_display":"  "}"#), None);
    assert_eq!(plan_name(b"{}"), None);
}
