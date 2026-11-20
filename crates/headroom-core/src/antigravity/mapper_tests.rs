//! Ports of AntigravityArgonQuotaTests, AntigravityQuotaSummaryMergeTests and the plan test.

use chrono::{DateTime, Utc};

use super::mapper::*;
use crate::models::{MetricLine, ProgressFormat};

fn summary(group_name: &str, buckets: &str) -> Vec<u8> {
    format!(
        r#"{{"groups":[
          {{"displayName":"{group_name}","buckets":[{buckets}]}},
          {{"displayName":"Gemini Models","buckets":[{{"bucketId":"gemini-5h","remainingFraction":0.8}},{{"bucketId":"gemini-weekly","remainingFraction":0.7}}]}},
          {{"displayName":"Claude and GPT models","buckets":[{{"bucketId":"3p-5h","remainingFraction":0.6}},{{"bucketId":"3p-weekly","remainingFraction":0.5}}]}}
        ]}}"#
    )
    .into_bytes()
}

fn labels(lines: &Option<Vec<MetricLine>>) -> Vec<&str> {
    lines.as_ref().unwrap().iter().map(MetricLine::label).collect()
}

fn used(line: &MetricLine) -> f64 {
    match line {
        MetricLine::Progress { used, .. } => *used,
        other => panic!("{other:?}"),
    }
}

fn assert_shared_only(body: Vec<u8>) {
    let lines = parse_quota_summaries(&[body]);
    assert_eq!(labels(&lines), ["Session", "Weekly", "Claude", "Claude Weekly"]);
    for (line, expected) in lines.unwrap().iter().zip([20.0, 30.0, 40.0, 50.0]) {
        assert!((used(line) - expected).abs() < 1e-4);
    }
}

fn argon(lines: &Option<Vec<MetricLine>>) -> Option<&MetricLine> {
    lines.as_ref()?.iter().find(|l| l.label() == "Argon Weekly")
}

fn progress(used: f64, resets_at: Option<DateTime<Utc>>) -> MetricLine {
    MetricLine::Progress { label: "Argon Weekly".into(), used, limit: 100.0, format: ProgressFormat::Percent, resets_at, period_ms: Some(WEEK_MS) }
}

#[test]
fn unavailable_argon_preserves_shared_quotas() {
    for buckets in [
        "",
        r#"{"bucketId":"test-id","window":"weekly"}"#,
        r#"{"bucketId":"test-id","window":"weekly","remainingFraction":"unknown"}"#,
        r#"{"bucketId":"test-id","window":"weekly","remainingFraction":1,"disabled":true}"#,
        r#"{"bucketId":"test-id","window":"weekly","remainingAmount":100}"#,
    ] {
        assert_shared_only(summary("Gemini 4 Argon", buckets));
    }
}

#[test]
fn only_the_explicit_argon_weekly_group_populates_the_row() {
    for group in ["Gemini Models", "Gemini 4 Argon Images", "Future Models"] {
        assert_shared_only(summary(group, r#"{"bucketId":"test-id","displayName":"Gemini 4 Argon Weekly Limit","window":"weekly","remainingFraction":0.1}"#));
    }
    for buckets in [
        r#"{"bucketId":"test-id","displayName":"Gemini 4 Argon Weekly Limit","remainingFraction":0.1}"#,
        r#"{"bucketId":"test-id","window":"5h","remainingFraction":0.1}"#,
        r#"{"window":"weekly","remainingFraction":0.1}"#,
        r#"{"bucketId":"","window":"weekly","remainingFraction":0.1}"#,
        r#"{"bucketId":"gemini-weekly","window":"weekly","remainingFraction":0.1}"#,
    ] {
        assert_shared_only(summary("Gemini 4 Argon", buckets));
    }
}

#[test]
fn disabled_flag_rejects_malformed_values_but_allows_omission_null_and_false() {
    for value in ["true", r#""true""#, r#""false""#, "1", "0"] {
        assert_shared_only(summary("Gemini 4 Argon", &format!(r#"{{"bucketId":"test-id","window":"weekly","remainingFraction":0.5,"disabled":{value}}}"#)));
    }
    for field in ["", r#", "disabled":null"#, r#", "disabled":false"#] {
        let lines = parse_quota_summaries(&[summary("Gemini 4 Argon", &format!(r#"{{"bucketId":"test-id","window":"weekly","remainingFraction":0.5{field}}}"#))]);
        assert_eq!(argon(&lines), Some(&progress(50.0, None)), "{field}");
    }
}

#[test]
fn ambiguous_weekly_buckets_do_not_choose_a_quota() {
    assert_shared_only(summary(
        "Gemini 4 Argon",
        r#"{"bucketId":"test-first","window":"weekly","remainingFraction":0.9},{"bucketId":"test-second","window":"weekly","remainingFraction":0.1}"#,
    ));
}

#[test]
fn argon_five_hour_bucket_does_not_override_its_weekly_quota() {
    let lines = parse_quota_summaries(&[summary(
        "Gemini 4 Argon",
        r#"{"bucketId":"test-session","window":"5h","remainingFraction":0},{"bucketId":"test-weekly","window":"weekly","remainingFraction":0.5}"#,
    )]);
    assert_eq!(labels(&lines), ["Session", "Weekly", "Argon Weekly", "Claude", "Claude Weekly"]);
    assert_eq!(argon(&lines), Some(&progress(50.0, None)));
}

#[test]
fn argon_only_summary_uses_weekly_period_and_keeps_zero_remaining() {
    let body = br#"{"groups":[{"displayName":"Gemini 4 Argon","buckets":[{"bucketId":"test-id","window":"weekly","remainingFraction":0,"disabled":false}]}]}"#;
    assert_eq!(parse_quota_summaries(&[body.to_vec()]), Some(vec![progress(100.0, None)]));
}

#[test]
fn argon_wtu_weekly_parses_bare_and_wrapped() {
    let group = r#"{"displayName":"Gemini 4 Argon","description":"x","buckets":[{"bucketId":"gemini-argon-wtu-weekly","displayName":"Gemini 4 Argon Weekly Limit","window":"weekly","remainingFraction":1.0,"resetTime":"2026-10-22T23:24:33Z"}]}"#;
    let reset = DateTime::parse_from_rfc3339("2026-10-22T23:24:33Z").unwrap().into();
    for json in [format!(r#"{{"groups":[{group}]}}"#), format!(r#"{{"response":{{"groups":[{group}]}}}}"#)] {
        assert_eq!(parse_quota_summaries(&[json.into_bytes()]), Some(vec![progress(0.0, Some(reset))]));
    }
}

#[test]
fn a_malformed_group_name_does_not_invalidate_shared_buckets() {
    let body = br#"{"groups":[{"displayName":42,"buckets":[{"bucketId":"gemini-5h","remainingFraction":0.8}]}]}"#;
    assert_eq!(labels(&parse_quota_summaries(&[body.to_vec()])), ["Session"]);
}

#[test]
fn merged_summaries_take_the_first_usable_bucket_per_id() {
    let first = br#"{"groups":[{"buckets":[{"bucketId":"gemini-5h"},{"bucketId":"3p-5h","remainingFraction":0.5}]}]}"#.to_vec();
    let second = br#"{"groups":[{"buckets":[{"bucketId":"gemini-5h","remainingFraction":0.75},{"bucketId":"3p-5h","remainingFraction":0.1}]}]}"#.to_vec();
    let lines = parse_quota_summaries(&[first, b"not json".to_vec(), second]);
    assert_eq!(labels(&lines), ["Session", "Claude"]);
    assert_eq!(lines.unwrap().iter().map(used).collect::<Vec<_>>(), [25.0, 50.0]);
    assert!(parse_quota_summaries(&[b"{}".to_vec()]).is_none());
}

#[test]
fn merged_summaries_take_argon_from_the_first_body_that_has_it() {
    let argon_body = |fraction: f64| {
        format!(r#"{{"groups":[{{"displayName":"Gemini 4 Argon","buckets":[{{"bucketId":"gemini-argon-wtu-weekly","window":"weekly","remainingFraction":{fraction}}}]}}]}}"#).into_bytes()
    };
    let lines = parse_quota_summaries(&[argon_body(0.6), argon_body(0.1)]);
    assert_eq!(labels(&lines), ["Argon Weekly"]);
    assert!((used(&lines.unwrap()[0]) - 40.0).abs() < 1e-4);
}

#[test]
fn is_complete_summary_needs_all_four_usable_buckets() {
    let complete = br#"{"response":{"groups":[{"buckets":[{"bucketId":"gemini-5h","remainingFraction":1},{"bucketId":"gemini-weekly","remainingFraction":1},{"bucketId":"3p-5h","remainingFraction":1},{"bucketId":"3p-weekly","remainingFraction":1}]}]}}"#;
    assert!(is_complete_summary(complete));
    let missing_weekly = br#"{"groups":[{"buckets":[{"bucketId":"gemini-5h","remainingFraction":1},{"bucketId":"3p-5h","remainingFraction":1},{"bucketId":"3p-weekly","remainingFraction":1}]}]}"#;
    assert!(!is_complete_summary(missing_weekly));
    let no_fraction = br#"{"groups":[{"buckets":[{"bucketId":"gemini-5h","remainingFraction":1},{"bucketId":"gemini-weekly"},{"bucketId":"3p-5h","remainingFraction":1},{"bucketId":"3p-weekly","remainingFraction":1}]}]}"#;
    assert!(!is_complete_summary(no_fraction));
    assert!(!is_complete_summary(b"not json"));
    let weekly_in_argon = br#"{"groups":[{"buckets":[{"bucketId":"gemini-5h","remainingFraction":1},{"bucketId":"3p-5h","remainingFraction":1},{"bucketId":"3p-weekly","remainingFraction":1}]},{"displayName":"Gemini 4 Argon","buckets":[{"bucketId":"gemini-weekly","window":"weekly","remainingFraction":1}]}]}"#;
    assert!(!is_complete_summary(weekly_in_argon));
}

#[test]
fn paid_tiers_read_as_google_sells_them() {
    let plan = |id: &str, name: &str| parse_plan(format!(r#"{{"paidTier":{{"id":"{id}","name":"{name}"}}}}"#).as_bytes());
    assert_eq!(plan("g1-pro-tier", "fake").as_deref(), Some("Google AI Pro"));
    assert_eq!(plan("g1-ultra-tier", "fake").as_deref(), Some("Google AI Ultra"));
    assert_eq!(plan("g1-ultra-lite-tier", "fake").as_deref(), Some("Google AI Ultra Lite"));
    assert_eq!(plan("unknown-tier", "Google AI Pro").as_deref(), Some("Google AI Pro"));
    assert_eq!(plan("unknown-tier", "Gemini Code Assist in Google One AI Pro").as_deref(), Some("Google AI Pro"));
    assert_eq!(plan("unknown-tier", "Google AI Ultra Lite").as_deref(), Some("Google AI Ultra Lite"));
    assert_eq!(plan("free-tier", "Antigravity Starter Quota").as_deref(), Some("Antigravity Starter Quota"));
    assert_eq!(plan("unknown-tier", ""), None);
    assert_eq!(parse_plan(br#"{"currentTier":{"name":"some plan"}}"#).as_deref(), Some("Some Plan"));
    assert_eq!(parse_project(br#"{"cloudaicompanionProject":"p1"}"#).as_deref(), Some("p1"));
    assert_eq!(parse_project(br#"{"cloudaicompanionProject":""}"#), None);
}
