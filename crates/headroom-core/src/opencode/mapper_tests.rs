use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::json;

use super::mapper::*;
use crate::models::{MetricLine, ProgressFormat};
use crate::provider::ProviderError;

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).unwrap().into()
}

fn t0() -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000, 0).unwrap()
}

fn iso(date: DateTime<Utc>) -> String {
    date.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn body(rolling: serde_json::Value, weekly: serde_json::Value, monthly: serde_json::Value) -> Vec<u8> {
    json!({"usage": {"rolling": rolling, "weekly": weekly, "monthly": monthly}}).to_string().into_bytes()
}

/// Zero-usage weekly and monthly windows far from now, with the given rolling window.
fn with_rolling(rolling: serde_json::Value) -> Vec<u8> {
    body(
        rolling,
        json!({"percent": 0, "resetsAt": iso(t0() + Duration::days(3))}),
        json!({"percent": 0, "resetsAt": iso(t0() + Duration::days(20))}),
    )
}

fn progress(line: &MetricLine) -> (f64, f64, &ProgressFormat, Option<DateTime<Utc>>, Option<i64>) {
    match line {
        MetricLine::Progress { used, limit, format, resets_at, period_ms, .. } => (*used, *limit, format, *resets_at, *period_ms),
        other => panic!("not a progress line: {other:?}"),
    }
}

fn session_reset(body: &[u8], captured_at: DateTime<Utc>) -> Option<DateTime<Utc>> {
    progress(&meter_lines(body, captured_at).unwrap()[0]).3
}

#[test]
fn the_plan_takes_the_name_opencode_gives_the_product() {
    for (product, name) in [("go", "Go"), ("go-plus", "Go Plus"), ("go-teams", "Go Teams")] {
        assert_eq!(plan(format!(r#"{{"product":"{product}"}}"#).as_bytes()).unwrap(), name);
    }
    assert_eq!(plan(br#"{"product":null}"#), Err(ProviderError::InvalidResponse));
}

#[test]
fn meter_lines_match_dashboard_percents_and_resets() {
    let sample = body(
        json!({"status": "ok", "percent": 12, "resetsAt": "2026-07-12T13:30:00.662Z"}),
        json!({"status": "ok", "percent": 8, "resetsAt": "2026-07-13T00:00:00.662Z"}),
        json!({"status": "rate-limited", "percent": 100, "resetsAt": "2026-08-04T11:18:32.662Z"}),
    );
    let lines = meter_lines(&sample, t0()).unwrap();
    assert_eq!(lines.iter().map(MetricLine::label).collect::<Vec<_>>(), ["Session", "Weekly", "Monthly"]);
    let (used, limit, format, reset, period) = progress(&lines[0]);
    assert_eq!((used, limit, format, period), (12.0, 100.0, &ProgressFormat::Percent, Some(SESSION_MS)));
    assert_eq!(reset, Some(at("2026-07-12T13:30:00.662Z")));
    let (used, _, format, reset, period) = progress(&lines[1]);
    assert_eq!((used, format, period), (8.0, &ProgressFormat::Percent, Some(WEEK_MS)));
    assert_eq!(reset, Some(at("2026-07-13T00:00:00.662Z")));
    let (used, _, format, _, period) = progress(&lines[2]);
    assert_eq!((used, format, period), (100.0, &ProgressFormat::Percent, Some(MONTH_MS)));
}

#[test]
fn zero_percent_is_a_real_meter() {
    let lines = meter_lines(&with_rolling(json!({"percent": 0, "resetsAt": iso(t0() + Duration::hours(2))})), t0()).unwrap();
    let (used, limit, format, ..) = progress(&lines[0]);
    assert_eq!((used, limit, format), (0.0, 100.0, &ProgressFormat::Percent));
}

#[test]
fn an_untouched_rolling_window_drops_the_placeholder_reset() {
    let placeholder = t0() + Duration::milliseconds(5 * 3_600_000 + 800); // the live endpoint overshoots slightly
    assert_eq!(session_reset(&with_rolling(json!({"percent": 0, "resetsAt": iso(placeholder)})), t0()), None);
}

#[test]
fn a_sub_one_percent_rolling_window_keeps_its_anchored_reset() {
    for age in [30, 60] {
        let anchored = t0() + Duration::seconds(SESSION_MS / 1000 - age);
        assert_eq!(session_reset(&with_rolling(json!({"percent": 0, "resetsAt": iso(anchored)})), t0()), Some(anchored), "aged {age}s");
    }
}

#[test]
fn the_placeholder_comparison_is_bounded_on_both_sides() {
    let reset_for = |offset_ms: i64| {
        let reset = t0() + Duration::milliseconds(SESSION_MS + offset_ms);
        session_reset(&with_rolling(json!({"percent": 0, "resetsAt": iso(reset)})), t0())
    };
    let tolerance = (PLACEHOLDER_TOLERANCE_SECS * 1000.0) as i64;
    assert_eq!(reset_for(-tolerance), None);
    assert_eq!(reset_for(tolerance), None);
    assert!(reset_for(-tolerance - 500).is_some());
    assert!(reset_for(tolerance + 500).is_some());
    assert!(reset_for(5 * 60 * 1000).is_some(), "an implausibly distant reset is not a placeholder");
}

#[test]
fn the_capture_time_decides_the_placeholder() {
    // A skewed local clock would not match; the server's `Date` passed as `captured_at` does.
    let server = at("2027-01-15T08:00:00Z");
    let placeholder = server + Duration::milliseconds(5 * 3_600_000 + 500);
    let rolling = body(
        json!({"percent": 0, "resetsAt": iso(placeholder)}),
        json!({"percent": 0, "resetsAt": "2027-01-18T00:00:00.000Z"}),
        json!({"percent": 0, "resetsAt": "2027-02-10T08:00:00.000Z"}),
    );
    assert_eq!(session_reset(&rolling, server), None);
    assert!(session_reset(&rolling, server + Duration::minutes(2)).is_some());
}

#[test]
fn full_period_weekly_and_monthly_resets_survive_at_zero_usage() {
    let (weekly, monthly) = (t0() + Duration::days(7), t0() + Duration::days(30));
    let lines = meter_lines(
        &body(
            json!({"percent": 0, "resetsAt": iso(t0() + Duration::hours(2))}),
            json!({"percent": 0, "resetsAt": iso(weekly)}),
            json!({"percent": 0, "resetsAt": iso(monthly)}),
        ),
        t0(),
    )
    .unwrap();
    assert_eq!((progress(&lines[1]).3, progress(&lines[2]).3), (Some(weekly), Some(monthly)));
}

#[test]
fn a_missing_or_malformed_reset_is_invalid() {
    let ok = json!({"percent": 0, "resetsAt": "2027-02-10T08:00:00.000Z"});
    for case in [
        body(ok.clone(), json!({"percent": 0}), ok.clone()),
        body(json!({"percent": 0, "resetsAt": "not-a-date"}), ok.clone(), ok.clone()),
    ] {
        assert_eq!(meter_lines(&case, t0()).unwrap_err(), ProviderError::InvalidResponse);
    }
}

#[test]
fn percent_is_clamped() {
    let lines = meter_lines(
        &body(
            json!({"percent": 150, "resetsAt": "2026-07-12T13:30:00.662Z"}),
            json!({"percent": -4, "resetsAt": "2026-07-13T00:00:00.662Z"}),
            json!({"percent": 35, "resetsAt": "2026-08-04T11:18:32.662Z"}),
        ),
        t0(),
    )
    .unwrap();
    assert_eq!((progress(&lines[0]).0, progress(&lines[1]).0), (100.0, 0.0));
}

#[test]
fn a_missing_usage_or_window_is_invalid() {
    for case in [&b"{}"[..], br#"{"usage":{"weekly":{"percent":1}}}"#, b"not json"] {
        assert_eq!(meter_lines(case, t0()).unwrap_err(), ProviderError::InvalidResponse);
    }
}

#[test]
fn error_type_reads_the_documented_error_body() {
    let error = |kind: &str| format!(r#"{{"type":"error","error":{{"type":"{kind}","message":"x"}}}}"#).into_bytes();
    assert_eq!(error_type(&error("EntitlementError")).as_deref(), Some("EntitlementError"));
    assert_eq!(error_type(&error("AuthError")).as_deref(), Some("AuthError"));
    assert_eq!(error_type(&error("  ")), None);
    assert_eq!(error_type(b"<html>"), None);
}
