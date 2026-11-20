//! Devin usage mapping: the `GetUserStatus` body to metric lines. Never log response bodies.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::models::{MetricLine, MetricValue, ProgressFormat, ProviderSnapshot};
use crate::openrouter::number;
use crate::provider::ProviderError;

pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;
pub const WEEK_MS: i64 = 7 * DAY_MS;
const NO_QUOTA_MSG: &str = "Devin reports no quota for this account.";

/// Share of a quota left, and when it resets.
type Quota = (f64, Option<DateTime<Utc>>);

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

/// Devin reports the share of a quota left. Proto3 JSON omits a zero, so a window with a reset and no
/// share has none left. A share present but unreadable is a changed schema, not an empty quota.
fn quota(plan_status: &Map<String, Value>, remaining_key: &str, reset_key: &str) -> Result<Option<Quota>, ProviderError> {
    let resets_at = number(plan_status.get(reset_key)).and_then(|secs| DateTime::from_timestamp_millis((secs * 1000.0) as i64));
    match plan_status.get(remaining_key) {
        None => Ok(resets_at.map(|at| (0.0, Some(at)))),
        Some(value) => Ok(Some((number(Some(value)).ok_or(ProviderError::InvalidResponse)?, resets_at))),
    }
}

fn quota_line(label: &str, (remaining, resets_at): Quota, period_ms: i64) -> MetricLine {
    MetricLine::Progress {
        label: label.into(),
        used: (100.0 - remaining).clamp(0.0, 100.0),
        limit: 100.0,
        format: ProgressFormat::Percent,
        resets_at,
        period_ms: Some(period_ms),
    }
}

/// "Daily quota", "Weekly quota" (a plan that hides its daily quota shows one Weekly row, from the weekly
/// window, else from the daily share without a reset), then "Extra usage balance" in dollars.
pub fn map_usage(body: &[u8], now: DateTime<Utc>) -> Result<ProviderSnapshot, ProviderError> {
    let root: Value = serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)?;
    let user_status = root.get("userStatus").and_then(Value::as_object).ok_or(ProviderError::InvalidResponse)?;
    let empty = Map::new();
    let plan_status = user_status.get("planStatus").and_then(Value::as_object).unwrap_or(&empty);
    let plan_info = plan_status.get("planInfo").and_then(Value::as_object).unwrap_or(&empty);
    let daily = quota(plan_status, "dailyQuotaRemainingPercent", "dailyQuotaResetAtUnix")?;
    let weekly = quota(plan_status, "weeklyQuotaRemainingPercent", "weeklyQuotaResetAtUnix")?;

    let mut lines = Vec::new();
    if bool_value(plan_info.get("hideDailyQuota")) == Some(true) {
        if let Some(shown) = weekly.or(daily.map(|(remaining, _)| (remaining, None))) {
            lines.push(quota_line("Weekly quota", shown, WEEK_MS));
        }
    } else {
        lines.extend(daily.map(|q| quota_line("Daily quota", q, DAY_MS)));
        lines.extend(weekly.map(|q| quota_line("Weekly quota", q, WEEK_MS)));
    }
    if let Some(micros) = number(plan_status.get("overageBalanceMicros")) {
        lines.push(MetricLine::Values {
            label: "Extra usage balance".into(),
            values: vec![MetricValue { number: micros.max(0.0) / 1_000_000.0, currency_code: Some("usd".into()), label: None }],
        });
    }
    if lines.is_empty() {
        return Err(ProviderError::Unavailable(NO_QUOTA_MSG.into()));
    }
    let plan = plan_info.get("planName").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()).map(String::from);
    Ok(ProviderSnapshot { plan, lines, refreshed_at: now, reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() })
}
