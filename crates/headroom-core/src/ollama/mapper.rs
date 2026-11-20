//! Ollama usage mapping. Pure functions over bodies.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::models::{MetricLine, MetricValue, ProgressFormat};
use crate::openrouter::number;
use crate::provider::ProviderError;

const SESSION_MS: i64 = 5 * 60 * 60 * 1000;
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;

fn date(value: Option<&Value>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?.as_str()?.trim()).ok().map(Into::into)
}

/// Session, Weekly, Monthly (dollars) and Purchased Credits, each only when the response has it.
pub fn balance_lines(body: &[u8]) -> Result<Vec<MetricLine>, ProviderError> {
    let json: Value = serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)?;
    let included = json.get("included").and_then(Value::as_object).ok_or(ProviderError::InvalidResponse)?;
    let lines: Vec<MetricLine> = [
        legacy_line(included.get("session"), "Session", SESSION_MS),
        legacy_line(included.get("weekly"), "Weekly", WEEK_MS),
        monthly_line(included),
        purchased_line(json.get("purchased")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if lines.is_empty() {
        return Err(ProviderError::InvalidResponse);
    }
    Ok(lines)
}

/// The account's plan, capitalized for the header badge ("pro" -> "Pro"). Called directly, ollama.com
/// capitalizes its JSON keys (`Plan`); the local server lowercases them when it proxies the same
/// response, so both spellings are accepted.
pub fn plan(body: &[u8]) -> Option<String> {
    let json: Value = serde_json::from_slice(body).ok()?;
    let text = json.get("Plan").or_else(|| json.get("plan"))?.as_str()?.trim();
    (!text.is_empty()).then(|| {
        text.split(' ')
            .map(|w| {
                let mut chars = w.chars();
                chars.next().map(|c| c.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect::<String>()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ")
    })
}

pub fn email(body: &[u8]) -> Option<String> {
    let json: Value = serde_json::from_slice(body).ok()?;
    let text = json.get("Email").or_else(|| json.get("email"))?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn legacy_line(entry: Option<&Value>, label: &str, period_ms: i64) -> Option<MetricLine> {
    let entry = entry?.as_object()?;
    let remaining = number(entry.get("remaining_percent"))?;
    let resets_at = date(entry.get("resets_at"));
    Some(MetricLine::Progress {
        label: label.into(),
        used: (100.0 - remaining).clamp(0.0, 100.0),
        limit: 100.0,
        format: ProgressFormat::Percent,
        resets_at,
        period_ms: resets_at.map(|_| period_ms),
    })
}

fn monthly_line(included: &serde_json::Map<String, Value>) -> Option<MetricLine> {
    let allowance = number(included.get("allowance_usd")).filter(|a| *a > 0.0)?;
    let balance = number(included.get("balance_usd"))?;
    let period = included.get("period");
    let (from, until) = (date(period.and_then(|p| p.get("from"))), date(period.and_then(|p| p.get("until"))));
    let period_ms = match (from, until) {
        (Some(from), Some(until)) if until > from => Some((until - from).num_milliseconds()),
        _ => None,
    };
    Some(MetricLine::Progress {
        label: "Monthly".into(),
        used: (allowance - balance).max(0.0).min(allowance),
        limit: allowance,
        format: ProgressFormat::Dollars,
        resets_at: until,
        period_ms,
    })
}

fn purchased_line(purchased: Option<&Value>) -> Option<MetricLine> {
    let balance = number(purchased?.as_object()?.get("balance_usd"))?;
    Some(MetricLine::Values { label: "Purchased Credits".into(), values: vec![MetricValue { number: balance.max(0.0), currency_code: Some("usd".into()), label: None }] })
}
