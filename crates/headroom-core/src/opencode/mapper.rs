//! OpenCode Go usage mapping. Pure functions.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::models::{MetricLine, ProgressFormat};
use crate::openrouter::number;
use crate::provider::ProviderError;

pub const SESSION_MS: i64 = 5 * 60 * 60 * 1000;
pub const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
pub const MONTH_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Maximum distance from exactly one full period for a zero-usage rolling reset to count as the
/// untouched-window placeholder. OpenCode rounds `resetsAt` up to whole seconds and HTTP `Date` headers
/// carry whole-second precision, so two seconds covers both rounding steps.
pub const PLACEHOLDER_TOLERANCE_SECS: f64 = 2.0;

/// Session, Weekly, Monthly. `captured_at` is the response `Date` header when present, else now.
pub fn meter_lines(body: &[u8], captured_at: DateTime<Utc>) -> Result<Vec<MetricLine>, ProviderError> {
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(body) else { return Err(ProviderError::InvalidResponse) };
    let usage = body.get("usage").and_then(Value::as_object).ok_or(ProviderError::InvalidResponse)?;
    Ok(vec![
        window(usage.get("rolling"), "Session", SESSION_MS, captured_at, true)?,
        window(usage.get("weekly"), "Weekly", WEEK_MS, captured_at, false)?,
        window(usage.get("monthly"), "Monthly", MONTH_MS, captured_at, false)?,
    ])
}

/// The plan by the names OpenCode's own pages give it.
pub fn plan(go_status: &[u8]) -> Result<String, ProviderError> {
    let json: Value = serde_json::from_slice(go_status).map_err(|_| ProviderError::InvalidResponse)?;
    match json.get("product").and_then(Value::as_str) {
        Some("go") => Ok("Go".into()),
        Some("go-plus") => Ok("Go Plus".into()),
        Some("go-teams") => Ok("Go Teams".into()),
        _ => Err(ProviderError::InvalidResponse),
    }
}

/// `error.type` of the documented error body, trimmed; `None` when absent or empty.
pub fn error_type(body: &[u8]) -> Option<String> {
    let json: Value = serde_json::from_slice(body).ok()?;
    let kind = json.get("error")?.get("type")?.as_str()?.trim();
    (!kind.is_empty()).then(|| kind.to_string())
}

/// One window. With `drops_placeholder_reset` (the rolling session only), an untouched window answers
/// `resetsAt = now + 5h` and re-derives it on every request, so that placeholder is dropped here, at
/// capture time. A window with real usage anchors its reset to the oldest call and keeps it; weekly and
/// monthly resets are calendar instants that exist regardless of usage.
fn window(raw: Option<&Value>, label: &str, period_ms: i64, captured_at: DateTime<Utc>, drops_placeholder_reset: bool) -> Result<MetricLine, ProviderError> {
    let object = raw.and_then(Value::as_object).ok_or(ProviderError::InvalidResponse)?;
    let percent = number(object.get("percent")).ok_or(ProviderError::InvalidResponse)?;
    let reset = object.get("resetsAt").and_then(Value::as_str).ok_or(ProviderError::InvalidResponse)?;
    let reported: DateTime<Utc> = DateTime::parse_from_rfc3339(reset.trim()).map_err(|_| ProviderError::InvalidResponse)?.into();
    let used = percent.clamp(0.0, 100.0);
    let mut resets_at = Some(reported);
    if drops_placeholder_reset && used == 0.0 {
        let until_reset = (reported - captured_at).num_milliseconds() as f64 / 1000.0;
        if (until_reset - period_ms as f64 / 1000.0).abs() <= PLACEHOLDER_TOLERANCE_SECS {
            resets_at = None;
        }
    }
    Ok(MetricLine::Progress { label: label.into(), used, limit: 100.0, format: ProgressFormat::Percent, resets_at, period_ms: Some(period_ms) })
}
