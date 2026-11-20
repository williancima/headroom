//! Codex usage mapping. Pure functions.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value};

use super::resets::{epoch_secs, int, read_reset_credits, LINE_LABEL};
use crate::models::{MetricLine, MetricValue, ProgressFormat};
use crate::openrouter::number;
use crate::provider::ProviderError;

const SESSION_MS: i64 = 5 * 60 * 60 * 1000;
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// OpenAI sells ChatGPT credits at 4 cents each; the usage API reports only the count.
const CREDIT_USD_RATE: f64 = 0.04;

/// The `x-codex-*` response headers the usage read falls back to.
#[derive(Debug, Default, Clone)]
pub struct Headers {
    pub primary_percent: Option<String>,
    pub secondary_percent: Option<String>,
    pub has_credits: Option<String>,
    pub balance: Option<String>,
}

#[derive(Debug)]
pub struct Usage {
    pub plan: Option<String>,
    pub lines: Vec<MetricLine>,
    /// One per "Rate Limit Resets" credit whose expiry is known, sorted.
    pub reset_expiries: Vec<DateTime<Utc>>,
}

/// Session, Weekly, Spark, Spark Weekly, Rate Limit Resets, Credits, Credit Limit, each only when the
/// response has it. `credit_list` is the dedicated reset-credit list, when one was read.
pub fn map_usage(body: &[u8], headers: &Headers, credit_list: Option<&[u8]>, now: DateTime<Utc>) -> Result<Usage, ProviderError> {
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(body) else { return Err(ProviderError::InvalidResponse) };
    let rate_limit = body.get("rate_limit").and_then(Value::as_object);
    let header_percents = (number_text(&headers.primary_percent), number_text(&headers.secondary_percent));
    let mut lines = window_lines(rate_limit, ("Session", "Weekly"), header_percents, now);
    lines.extend(spark_lines(&body, now));

    let mut reset_expiries = vec![];
    if let Some((count, expiries)) = read_reset_credits(&body, credit_list) {
        reset_expiries = expiries;
        lines.push(MetricLine::Values { label: LINE_LABEL.into(), values: vec![MetricValue { number: count as f64, currency_code: None, label: None }] });
    }
    if let Some(remaining) = credits_remaining(&body, headers).filter(|r| int(*r).is_some()) {
        lines.push(MetricLine::Values { label: "Credits".into(), values: credit_values(remaining) });
    }
    lines.extend(credit_limit(&body));
    Ok(Usage { plan: format_plan(body.get("plan_type")), lines, reset_expiries })
}

fn number_text(text: &Option<String>) -> Option<f64> {
    number(text.as_deref().map(|t| Value::String(t.into())).as_ref())
}

fn period_ms(window: &Map<String, Value>) -> Option<i64> {
    int(number(window.get("limit_window_seconds"))? * 1000.0)
}

fn reset_date(window: &Map<String, Value>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if let Some(at) = number(window.get("reset_at")) {
        return epoch_secs(at);
    }
    let after = number(window.get("reset_after_seconds"))?;
    Some(now + Duration::milliseconds((after * 1000.0) as i64))
}

/// Codex normally returns the five-hour window as `primary_window` and the weekly window as
/// `secondary_window`, but it can move a temporarily sole weekly limit into the primary slot. Each meter
/// takes the window whose explicit duration is its own; failing that, the window in its own slot when that
/// window's duration is absent or unfamiliar. A slot known only from its header percent is a window with
/// no fields. The meter keeps the percentage Codex reported and the window's own period, falling back to
/// the meter's; a chosen window without a percentage shows nothing.
fn window_lines(
    rate_limit: Option<&Map<String, Value>>,
    labels: (&str, &str),
    header_percents: (Option<f64>, Option<f64>),
    now: DateTime<Utc>,
) -> Vec<MetricLine> {
    let empty = Map::new();
    let slot = |key: &str, header: Option<f64>| {
        let window = rate_limit.and_then(|r| r.get(key)).and_then(Value::as_object).or_else(|| header.map(|_| &empty))?;
        Some((window, number(window.get("used_percent")).or(header)))
    };
    let slots = [slot("primary_window", header_percents.0), slot("secondary_window", header_percents.1)];
    let meters = [(labels.0, SESSION_MS), (labels.1, WEEK_MS)];
    let mut lines = vec![];
    for (index, (label, meter_period)) in meters.into_iter().enumerate() {
        let exact = slots.iter().flatten().find(|(w, _)| period_ms(w) == Some(meter_period));
        let fallback = slots[index].as_ref().filter(|(w, _)| !period_ms(w).is_some_and(|p| meters.iter().any(|m| m.1 == p)));
        let Some((window, Some(used))) = exact.or(fallback) else { continue };
        lines.push(MetricLine::Progress {
            label: label.into(),
            used: *used,
            limit: 100.0,
            format: ProgressFormat::Percent,
            resets_at: reset_date(window, now),
            period_ms: Some(period_ms(window).unwrap_or(meter_period)),
        });
    }
    lines
}

fn spark_lines(body: &Map<String, Value>, now: DateTime<Utc>) -> Vec<MetricLine> {
    let Some(entries) = body.get("additional_rate_limits").and_then(Value::as_array) else { return vec![] };
    let is_spark = |entry: &&Map<String, Value>| {
        ["limit_name", "metered_feature"].iter().filter_map(|k| entry.get(*k)?.as_str()).any(|s| s.to_lowercase().contains("spark"))
    };
    let Some(spark) = entries.iter().filter_map(Value::as_object).find(is_spark) else { return vec![] };
    let rate_limit = spark.get("rate_limit").and_then(Value::as_object);
    if rate_limit.is_none() {
        return vec![];
    }
    window_lines(rate_limit, ("Spark", "Spark Weekly"), (None, None), now)
}

/// Dollars at 4 cents per credit, then the whole-credit count.
pub fn credit_values(remaining: f64) -> Vec<MetricValue> {
    let credits = remaining.floor().max(0.0);
    vec![
        MetricValue { number: credits * CREDIT_USD_RATE, currency_code: Some("usd".into()), label: None },
        MetricValue { number: credits, currency_code: None, label: None },
    ]
}

fn bool_value(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => Some(n.as_f64()? != 0.0),
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn credits_remaining(body: &Map<String, Value>, headers: &Headers) -> Option<f64> {
    let credits = body.get("credits").and_then(Value::as_object);
    let header_flag = headers.has_credits.as_ref().map(|h| Value::String(h.clone()));
    let has_credits = bool_value(credits.and_then(|c| c.get("has_credits"))).or_else(|| bool_value(header_flag.as_ref()));
    if has_credits == Some(false) {
        return None;
    }
    number(credits.and_then(|c| c.get("balance"))).or_else(|| number_text(&headers.balance))
}

/// The credit limit a workspace sets on the member; ChatGPT sends its numbers as decimal strings.
fn credit_limit(body: &Map<String, Value>) -> Option<MetricLine> {
    let limit = body.get("spend_control")?.get("individual_limit")?.as_object()?;
    let used = number(limit.get("used")).filter(|u| *u >= 0.0)?;
    let cap = number(limit.get("limit")).filter(|c| *c >= 0.0)?;
    Some(MetricLine::Progress {
        label: "Credit Limit".into(),
        used,
        limit: cap,
        format: ProgressFormat::Count("credits".into()),
        resets_at: number(limit.get("reset_at")).and_then(epoch_secs),
        period_ms: None,
    })
}

/// `prolite` -> "Pro 100", `pro` -> "Pro 200", `promax` -> "Pro 500"; anything else is title-cased.
pub fn format_plan(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    Some(match raw.to_lowercase().as_str() {
        "prolite" => "Pro 100".into(),
        "pro" => "Pro 200".into(),
        "promax" => "Pro 500".into(),
        "self_serve_business_prolite" => "Business Premium".into(),
        _ => raw
            .split('_')
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut chars = w.chars();
                chars.next().map(|c| c.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }

    fn map(body: &str) -> Usage {
        map_usage(body.as_bytes(), &Headers::default(), None, now()).unwrap()
    }

    type Meter = (String, f64, Option<DateTime<Utc>>, Option<i64>);

    /// `(label, used, resets_at, period_ms)` of each percent meter.
    fn meters(lines: &[MetricLine]) -> Vec<Meter> {
        lines
            .iter()
            .filter_map(|l| match l {
                MetricLine::Progress { label, used, limit, format: ProgressFormat::Percent, resets_at, period_ms } if *limit == 100.0 => {
                    Some((label.clone(), *used, *resets_at, *period_ms))
                }
                _ => None,
            })
            .collect()
    }

    fn windows(rate_limit: &str) -> Vec<Meter> {
        meters(&map(&format!(r#"{{"rate_limit":{rate_limit}}}"#)).lines)
    }

    fn labels(m: &[Meter]) -> Vec<&str> {
        m.iter().map(|x| x.0.as_str()).collect()
    }

    #[test]
    fn a_weekly_window_in_the_primary_slot_is_the_weekly_meter() {
        let m = windows(r#"{"primary_window":{"used_percent":30,"limit_window_seconds":604800,"reset_after_seconds":100}}"#);
        assert_eq!(labels(&m), ["Weekly"]);
        assert_eq!((m[0].1, m[0].3), (30.0, Some(WEEK_MS)));
        assert_eq!(m[0].2, Some(now() + Duration::seconds(100)));
    }

    #[test]
    fn swapped_slots_are_read_by_duration() {
        let m = windows(r#"{"primary_window":{"used_percent":70,"limit_window_seconds":604800},"secondary_window":{"used_percent":20,"limit_window_seconds":18000}}"#);
        assert_eq!(labels(&m), ["Session", "Weekly"]);
        assert_eq!(m.iter().map(|x| x.1).collect::<Vec<_>>(), [20.0, 70.0]);
    }

    #[test]
    fn an_unfamiliar_duration_stays_in_its_slot_with_its_own_period() {
        let m = windows(r#"{"primary_window":{"used_percent":10,"limit_window_seconds":3600},"secondary_window":{"used_percent":40,"limit_window_seconds":2592000}}"#);
        assert_eq!(labels(&m), ["Session", "Weekly"]);
        assert_eq!(m.iter().map(|x| x.1).collect::<Vec<_>>(), [10.0, 40.0]);
        assert_eq!(m.iter().map(|x| x.3).collect::<Vec<_>>(), [Some(3_600_000), Some(2_592_000_000)]);
    }

    #[test]
    fn a_matched_window_without_a_percent_shows_nothing_and_borrows_no_other_window() {
        let m = windows(r#"{"primary_window":{"limit_window_seconds":604800},"secondary_window":{"used_percent":40,"limit_window_seconds":86400}}"#);
        assert!(m.is_empty());
    }

    #[test]
    fn a_header_percent_alone_is_the_slots_meter_with_the_default_period() {
        let headers = Headers { primary_percent: Some("55".into()), ..Headers::default() };
        let usage = map_usage(br#"{"rate_limit":{}}"#, &headers, None, now()).unwrap();
        let m = meters(&usage.lines);
        assert_eq!(labels(&m), ["Session"]);
        assert_eq!((m[0].1, m[0].2, m[0].3), (55.0, None, Some(SESSION_MS)));
    }

    #[test]
    fn reset_at_wins_over_reset_after() {
        let m = windows(r#"{"primary_window":{"used_percent":1,"reset_at":1800003600,"reset_after_seconds":5}}"#);
        assert_eq!(m[0].2, Some(now() + Duration::hours(1)));
    }

    #[test]
    fn oversized_window_length_count_and_credits_are_dropped() {
        let usage = map(r#"{"rate_limit":{"primary_window":{"used_percent":42,"limit_window_seconds":1e300}},
            "rate_limit_reset_credits":{"available_count":1e300},"credits":{"balance":1e300}}"#);
        assert_eq!(usage.lines.iter().map(MetricLine::label).collect::<Vec<_>>(), ["Session"]);
        assert_eq!(meters(&usage.lines)[0].3, Some(SESSION_MS));
    }

    fn credits_line(credits: &str) -> Option<MetricLine> {
        map(&format!(r#"{{"credits":{credits}}}"#)).lines.into_iter().find(|l| l.label() == "Credits")
    }

    #[test]
    fn credits_hide_while_off_and_show_at_zero_while_on() {
        let cases = [
            (r#"{"has_credits":false,"balance":"0"}"#, None),
            (r#"{"has_credits":false}"#, None),
            (r#"{"has_credits":true,"balance":"0"}"#, Some(0.0)),
            (r#"{"balance":0}"#, Some(0.0)),
            (r#"{"has_credits":true,"balance":"1"}"#, Some(1.0)),
        ];
        for (credits, expected) in cases {
            let got = credits_line(credits).map(|l| match l {
                MetricLine::Values { values, .. } => values[1].number,
                _ => panic!(),
            });
            assert_eq!(got, expected, "{credits}");
        }
    }

    #[test]
    fn credits_are_four_cents_each_floored_to_whole_credits() {
        let Some(MetricLine::Values { values, .. }) = credits_line(r#"{"balance":"250.9"}"#) else { panic!() };
        assert_eq!(values, [MetricValue { number: 250.0 * 0.04, currency_code: Some("usd".into()), label: None }, MetricValue { number: 250.0, currency_code: None, label: None }]);
        assert_eq!(credit_values(-3.0)[1].number, 0.0);
    }

    #[test]
    fn credit_headers_back_up_the_body() {
        let headers = Headers { has_credits: Some("true".into()), balance: Some("12".into()), ..Headers::default() };
        let usage = map_usage(b"{}", &headers, None, now()).unwrap();
        let Some(MetricLine::Values { values, .. }) = usage.lines.first() else { panic!() };
        assert_eq!(values[1].number, 12.0);
        let off = Headers { has_credits: Some("false".into()), balance: Some("12".into()), ..Headers::default() };
        assert!(map_usage(b"{}", &off, None, now()).unwrap().lines.is_empty());
    }

    #[test]
    fn spark_windows_and_credit_limit() {
        let usage = map(r#"{"additional_rate_limits":[{"limit_name":"other","rate_limit":{"primary_window":{"used_percent":1}}},
            {"metered_feature":"codex_SPARK","rate_limit":{"primary_window":{"used_percent":5,"limit_window_seconds":18000},"secondary_window":{"used_percent":6}}}],
            "spend_control":{"individual_limit":{"used":"3","limit":"10","reset_at":1800003600}}}"#);
        assert_eq!(usage.lines.iter().map(MetricLine::label).collect::<Vec<_>>(), ["Spark", "Spark Weekly", "Credit Limit"]);
        assert_eq!(
            usage.lines[2],
            MetricLine::Progress {
                label: "Credit Limit".into(), used: 3.0, limit: 10.0, format: ProgressFormat::Count("credits".into()),
                resets_at: Some(now() + Duration::hours(1)), period_ms: None,
            }
        );
        assert!(map(r#"{"spend_control":{"individual_limit":{"used":-1,"limit":10}}}"#).lines.is_empty());
    }

    #[test]
    fn lines_keep_their_order_and_the_resets_row_carries_expiries() {
        let usage = map(r#"{"plan_type":"prolite","rate_limit":{"primary_window":{"used_percent":1}},
            "rate_limit_reset_credits":{"available_count":1,"credits":[{"expires_at":1800000100}]},
            "credits":{"balance":2},"spend_control":{"individual_limit":{"used":1,"limit":2}}}"#);
        assert_eq!(usage.lines.iter().map(MetricLine::label).collect::<Vec<_>>(), ["Session", "Rate Limit Resets", "Credits", "Credit Limit"]);
        assert_eq!(usage.reset_expiries, [now() + Duration::seconds(100)]);
        assert_eq!(usage.plan.as_deref(), Some("Pro 100"));
    }

    #[test]
    fn plan_names() {
        let plan = |s: &str| format_plan(Some(&Value::from(s)));
        assert_eq!(plan("prolite").as_deref(), Some("Pro 100"));
        assert_eq!(plan("Pro").as_deref(), Some("Pro 200"));
        assert_eq!(plan("promax").as_deref(), Some("Pro 500"));
        assert_eq!(plan("self_serve_business_prolite").as_deref(), Some("Business Premium"));
        assert_eq!(plan(" team ").as_deref(), Some("Team"));
        assert_eq!(plan("enterprise_plus").as_deref(), Some("Enterprise Plus"));
        assert_eq!(plan("  "), None);
        assert_eq!(format_plan(Some(&Value::Null)), None);
    }

    #[test]
    fn a_body_that_is_not_an_object_is_invalid() {
        for body in ["[]", "nope", ""] {
            assert_eq!(map_usage(body.as_bytes(), &Headers::default(), None, now()).unwrap_err(), ProviderError::InvalidResponse);
        }
    }
}
