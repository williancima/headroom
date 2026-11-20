//! Z.ai quota mapping. Pure functions over bodies.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::models::{MetricLine, ProgressFormat};
use crate::openrouter::number;
use crate::provider::ProviderError;

const HOUR_MS: f64 = 60.0 * 60.0 * 1000.0;
const DAY_MS: f64 = 24.0 * HOUR_MS;
const MONTH_MS: i64 = 30 * 24 * 60 * 60 * 1000;

type Obj = Map<String, Value>;

/// Session and Weekly from the percentage limits, then Web Searches. A body Z.ai refused inside an OK
/// response is `Unavailable` (no coding plan) or `SignInExpired` (the caller reports a rejected key).
pub fn map_quota(body: &[u8]) -> Result<Vec<MetricLine>, ProviderError> {
    let Ok(Value::Object(root)) = serde_json::from_slice::<Value>(body) else { return Err(ProviderError::InvalidResponse) };
    let failed = root.get("success").and_then(Value::as_bool) == Some(false);
    if failed && root.get("msg").and_then(Value::as_str).is_some_and(|m| m.to_lowercase().contains("coding plan")) {
        return Err(ProviderError::Unavailable("No active GLM Coding Plan. Subscribe at z.ai/subscribe to see usage.".into()));
    }
    if failed && root.get("code").and_then(Value::as_i64) == Some(401) {
        return Err(ProviderError::SignInExpired);
    }
    let container = match root.get("data") {
        Some(Value::Object(data)) => data,
        Some(_) => return Err(ProviderError::InvalidResponse),
        None => &root,
    };
    let limits: Vec<&Obj> = container.get("limits").and_then(Value::as_array).ok_or(ProviderError::InvalidResponse)?.iter().filter_map(Value::as_object).collect();
    let mut lines = vec![];
    for entry in &limits {
        let kind = entry.get("type").and_then(Value::as_str).or_else(|| entry.get("name").and_then(Value::as_str));
        if !matches!(kind, Some("CREDIT_LIMIT" | "TOKENS_LIMIT")) {
            continue;
        }
        let Some(period_ms) = token_window_ms(entry)? else { continue };
        let label = if period_ms < DAY_MS as i64 { "Session" } else { "Weekly" };
        lines.push(percent_line(entry, label, period_ms)?);
    }
    if let Some(web) = limits.iter().find(|e| ["type", "name"].iter().any(|k| e.get(*k).and_then(Value::as_str) == Some("TIME_LIMIT"))) {
        lines.push(web_search_line(web)?);
    }
    Ok(lines)
}

/// The plan: `productName` of the first subscription.
pub fn plan_name(body: &[u8]) -> Option<String> {
    let json: Value = serde_json::from_slice(body).ok()?;
    let name = json.get("data")?.as_array()?.first()?.get("productName")?.as_str()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// The window length for the entry's unit (3 hours, 4 days, 6 weeks, 5 months) times its number; `None` for
/// an unknown unit; `InvalidResponse` for a missing or non-positive number.
fn token_window_ms(entry: &Obj) -> Result<Option<i64>, ProviderError> {
    let (Some(unit), Some(count)) = (number(entry.get("unit")), number(entry.get("number"))) else { return Err(ProviderError::InvalidResponse) };
    if count <= 0.0 {
        return Err(ProviderError::InvalidResponse);
    }
    let unit_ms = match unit as i64 {
        3 => HOUR_MS,
        4 => DAY_MS,
        6 => 7.0 * DAY_MS,
        5 => 30.0 * DAY_MS,
        _ => return Ok(None),
    };
    let duration = unit_ms * count;
    if duration < 1.0 || duration.abs() >= i64::MAX as f64 {
        return Err(ProviderError::InvalidResponse);
    }
    Ok(Some(duration as i64))
}

fn reset_date(entry: &Obj) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(number(entry.get("nextResetTime"))? as i64)
}

fn percent_line(entry: &Obj, label: &str, period_ms: i64) -> Result<MetricLine, ProviderError> {
    let percentage = number(entry.get("percentage")).ok_or(ProviderError::InvalidResponse)?;
    Ok(MetricLine::Progress {
        label: label.into(),
        used: percentage.clamp(0.0, 100.0),
        limit: 100.0,
        format: ProgressFormat::Percent,
        resets_at: reset_date(entry),
        period_ms: Some(period_ms),
    })
}

fn web_search_line(entry: &Obj) -> Result<MetricLine, ProviderError> {
    let (Some(used), Some(limit)) = (number(entry.get("currentValue")), number(entry.get("usage"))) else { return Err(ProviderError::InvalidResponse) };
    if used < 0.0 || limit < 0.0 {
        return Err(ProviderError::InvalidResponse);
    }
    Ok(MetricLine::Progress { label: "Web Searches".into(), used, limit, format: ProgressFormat::Count("searches".into()), resets_at: reset_date(entry), period_ms: Some(MONTH_MS) })
}
