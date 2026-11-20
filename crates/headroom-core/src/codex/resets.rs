//! Codex rate-limit reset credits and when their list is read. Pure functions.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value};

use crate::claude::{ClaimFailure, ClaimOutcome};
use crate::openrouter::number;

pub const LINE_LABEL: &str = "Rate Limit Resets";
const CREDIT_LIST_INTERVAL_SECS: i64 = 60 * 60;

/// The credit list is read at most hourly (`RefreshSetting.secondaryReadInterval`); a claim forgets the
/// last read, so the caller passes `None` then. The caller records `now` whenever it asked, even on failure.
pub fn credit_list_due(last_read: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last_read.is_none_or(|at| now - at >= Duration::seconds(CREDIT_LIST_INTERVAL_SECS))
}

/// Epoch seconds, as ChatGPT sends them.
pub(super) fn epoch_secs(seconds: f64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis((seconds * 1000.0) as i64)
}

/// `Int(_:)` that returns `None` instead of trapping outside the range.
pub(super) fn int(value: f64) -> Option<i64> {
    (value.abs() < i64::MAX as f64).then_some(value as i64)
}

/// `available_count`: a non-negative number, floored.
fn available_count(source: &Map<String, Value>) -> Option<i64> {
    number(source.get("available_count")).filter(|c| *c >= 0.0).and_then(|c| int(c.floor()))
}

fn expiry(value: Option<&Value>) -> Option<DateTime<Utc>> {
    if let Some(text) = value.and_then(Value::as_str) {
        if let Ok(date) = DateTime::parse_from_rfc3339(text) {
            return Some(date.into());
        }
    }
    epoch_secs(number(value)?)
}

/// `(id, expiry)` of each credit whose status is `available` or missing.
fn available_credits(source: &Map<String, Value>) -> Vec<(Option<String>, Option<DateTime<Utc>>)> {
    let Some(credits) = source.get("credits").and_then(Value::as_array) else { return vec![] };
    let Some(credits) = credits.iter().map(Value::as_object).collect::<Option<Vec<_>>>() else { return vec![] };
    credits
        .into_iter()
        .filter(|c| c.get("status").and_then(Value::as_str).is_none_or(|s| s == "available"))
        .map(|c| (c.get("id").and_then(Value::as_str).map(String::from), expiry(c.get("expires_at"))))
        .collect()
}

fn expiries(source: &Map<String, Value>) -> Vec<DateTime<Utc>> {
    let mut out: Vec<_> = available_credits(source).into_iter().filter_map(|(_, e)| e).collect();
    out.sort();
    out
}

/// The count comes from the usage body of this pass; the dedicated list supplies the count only when the
/// usage body has none, and its expiry times only while it agrees with the count, so an hour-old list
/// never stands in for a reset spent or granted since.
pub fn read_reset_credits(body: &Map<String, Value>, credit_list: Option<&[u8]>) -> Option<(i64, Vec<DateTime<Utc>>)> {
    let embedded = body.get("rate_limit_reset_credits").and_then(Value::as_object);
    let dedicated = credit_list
        .and_then(|b| match serde_json::from_slice::<Value>(b).ok()? {
            Value::Object(o) => Some(o),
            _ => None,
        })
        .filter(|o| available_count(o).is_some());
    let count = embedded.and_then(available_count).or_else(|| dedicated.as_ref().and_then(available_count))?;
    if count == 0 {
        return Some((0, vec![]));
    }
    if let Some(list) = dedicated.as_ref().filter(|d| available_count(d) == Some(count)) {
        return Some((count, expiries(list)));
    }
    Some((count, embedded.map(expiries).unwrap_or_default()))
}

/// The available credit whose expiry is within a second of `expiry`.
pub fn credit_id(list: &Value, expiry: DateTime<Utc>) -> Option<String> {
    available_credits(list.as_object()?)
        .into_iter()
        .find(|(_, e)| e.is_some_and(|e| (e - expiry).num_milliseconds().abs() < 1000))
        .and_then(|(id, _)| id)
}

/// Every protocol code arrives as HTTP 200 with the outcome in the body's `code`; `already_redeemed` is
/// the idempotency key absorbing a retry of a claim that already landed, so it counts as success.
/// `auth_failure`: 401, or a 403 that is not a rate limit.
pub fn claim_outcome(status: u16, auth_failure: bool, body: &[u8]) -> ClaimOutcome {
    if auth_failure {
        return ClaimOutcome::Failed(ClaimFailure::SignInExpired);
    }
    let code = serde_json::from_slice::<Value>(body).ok().and_then(|b| b.get("code")?.as_str().map(String::from));
    match (200..300).contains(&status).then_some(code).flatten().as_deref() {
        Some("reset" | "already_redeemed") => ClaimOutcome::Success,
        Some("nothing_to_reset") => ClaimOutcome::NothingToReset,
        Some("no_credit") => ClaimOutcome::NoCredit,
        _ => ClaimOutcome::Failed(ClaimFailure::Refused),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const EXPIRY: i64 = 1_800_000_000;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn iso(secs: i64) -> String {
        at(secs).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    fn list(status: &str) -> Value {
        json!({"credits": [
            {"id": "RateLimitResetCredit_other", "status": "available", "expires_at": iso(EXPIRY + 999_999)},
            {"id": "RateLimitResetCredit_target", "status": status, "expires_at": iso(EXPIRY)}
        ], "available_count": 2})
    }

    fn outcome(code: &str, status: u16) -> ClaimOutcome {
        claim_outcome(status, false, json!({"code": code, "windows_reset": 2}).to_string().as_bytes())
    }

    #[test]
    fn consume_outcome_decodes_all_protocol_codes() {
        assert_eq!(outcome("reset", 200), ClaimOutcome::Success);
        assert_eq!(outcome("already_redeemed", 200), ClaimOutcome::Success);
        assert_eq!(outcome("nothing_to_reset", 200), ClaimOutcome::NothingToReset);
        assert_eq!(outcome("no_credit", 200), ClaimOutcome::NoCredit);
        assert_eq!(outcome("something_new", 200), ClaimOutcome::Failed(ClaimFailure::Refused));
        assert_eq!(outcome("reset", 500), ClaimOutcome::Failed(ClaimFailure::Refused));
        assert_eq!(claim_outcome(401, true, b""), ClaimOutcome::Failed(ClaimFailure::SignInExpired));
        assert_eq!(claim_outcome(200, false, b"not json"), ClaimOutcome::Failed(ClaimFailure::Refused));
    }

    #[test]
    fn credit_matching_by_expiry() {
        let body = list("available");
        assert_eq!(credit_id(&body, at(EXPIRY)).as_deref(), Some("RateLimitResetCredit_target"));
        assert_eq!(credit_id(&body, at(EXPIRY) + Duration::milliseconds(500)).as_deref(), Some("RateLimitResetCredit_target"));
        assert_eq!(credit_id(&body, at(EXPIRY + 3600)), None);
    }

    #[test]
    fn credit_matching_skips_other_statuses_but_keeps_a_missing_one() {
        assert_eq!(credit_id(&list("redeemed"), at(EXPIRY)), None);
        let bare = json!({"credits": [{"id": "RateLimitResetCredit_bare", "expires_at": EXPIRY}]});
        assert_eq!(credit_id(&bare, at(EXPIRY)).as_deref(), Some("RateLimitResetCredit_bare"));
    }

    fn read(body: Value, dedicated: Option<&str>) -> Option<(i64, Vec<DateTime<Utc>>)> {
        read_reset_credits(body.as_object().unwrap(), dedicated.map(str::as_bytes))
    }

    #[test]
    fn invalid_dedicated_responses_fall_back_to_the_usage_count() {
        let usage = json!({"rate_limit_reset_credits": {"available_count": 3}});
        for dedicated in [None, Some(r#"{"available_count":null}"#), Some("<html>oops</html>")] {
            assert_eq!(read(usage.clone(), dedicated), Some((3, vec![])), "{dedicated:?}");
        }
    }

    #[test]
    fn expiries_come_from_the_list_only_while_it_agrees_with_the_count() {
        let usage = json!({"rate_limit_reset_credits": {"available_count": 2}});
        let body = list("available").to_string();
        assert_eq!(read(usage, Some(&body)), Some((2, vec![at(EXPIRY), at(EXPIRY + 999_999)])));
        let stale = json!({"rate_limit_reset_credits": {"available_count": 1}});
        assert_eq!(read(stale, Some(&body)), Some((1, vec![])));
        let embedded = json!({"rate_limit_reset_credits": {"available_count": 1, "credits": [{"expires_at": EXPIRY}]}});
        assert_eq!(read(embedded, Some(&body)), Some((1, vec![at(EXPIRY)])));
        let spent = json!({"rate_limit_reset_credits": {"available_count": 0}});
        assert_eq!(read(spent, Some(&body)), Some((0, vec![])));
        assert_eq!(read(json!({"rate_limit_reset_credits": {"available_count": 4}}), Some(&body)).unwrap().0, 4);
    }

    #[test]
    fn a_missing_malformed_or_oversized_count_shows_no_row() {
        assert_eq!(read(json!({"rate_limit_reset_credits": {"available_count": null}}), None), None);
        assert_eq!(read(json!({"rate_limit_reset_credits": {"available_count": -1}}), None), None);
        assert_eq!(read(json!({"rate_limit_reset_credits": {"available_count": 1e300}}), None), None);
        assert_eq!(read(json!({}), Some(&list("available").to_string())).unwrap().0, 2, "the list stands in");
    }

    #[test]
    fn the_credit_list_is_read_hourly() {
        let now = at(EXPIRY);
        assert!(credit_list_due(None, now));
        assert!(!credit_list_due(Some(now - Duration::minutes(59)), now));
        assert!(credit_list_due(Some(now - Duration::minutes(60)), now));
    }
}
