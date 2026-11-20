//! Copilot usage and organization billing mapping.
//! Pure functions over bodies.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;

use crate::models::{MetricLine, MetricValue, ProgressFormat};
use crate::openrouter::number;
use crate::provider::ProviderError;

const MONTH_MS: i64 = 30 * 24 * 60 * 60 * 1000;

pub struct Usage {
    pub plan: Option<String>,
    pub lines: Vec<MetricLine>,
}

fn bool_value(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|n| n != 0.0),
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn count(label: &str, number: f64) -> MetricLine {
    MetricLine::Values { label: label.into(), values: vec![MetricValue { number, currency_code: None, label: None }] }
}

fn percent(label: &str, used: f64, resets_at: Option<DateTime<Utc>>) -> MetricLine {
    MetricLine::Progress { label: label.into(), used, limit: 100.0, format: ProgressFormat::Percent, resets_at, period_ms: Some(MONTH_MS) }
}

fn clamp(value: f64) -> f64 {
    value.clamp(0.0, 100.0)
}

/// Credits (and Extra Usage), Chat, Completions; or the limited-user quotas; or, on token-based billing,
/// the credits used.
pub fn map_usage(body: &[u8]) -> Result<Usage, ProviderError> {
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(body) else { return Err(ProviderError::InvalidResponse) };
    let plan = plan_label(body.get("copilot_plan"));
    let resets_at = reset_date(body.get("quota_reset_date")).or_else(|| reset_date(body.get("limited_user_reset_date")));
    let snapshots = body.get("quota_snapshots").and_then(Value::as_object);
    let premium = snapshots.and_then(|s| s.get("premium_interactions"));

    let mut lines = vec![];
    if let Some(credits) = snapshot_line("Credits", premium, resets_at) {
        lines.push(credits);
        lines.extend(overage_line(premium));
    }
    lines.extend(snapshot_line("Chat", snapshots.and_then(|s| s.get("chat")), resets_at));
    lines.extend(snapshot_line("Completions", snapshots.and_then(|s| s.get("completions")), resets_at));

    if lines.is_empty() {
        let limited = body.get("limited_user_quotas").and_then(Value::as_object);
        let monthly = body.get("monthly_quotas").and_then(Value::as_object);
        for (label, key) in [("Chat", "chat"), ("Completions", "completions")] {
            lines.extend(limited_line(label, limited.and_then(|l| l.get(key)), monthly.and_then(|m| m.get(key)), resets_at));
        }
    }
    if lines.is_empty() {
        if bool_value(body.get("token_based_billing")) != Some(true) {
            return Err(ProviderError::Unavailable("Copilot usage data is unavailable for this account.".into()));
        }
        return Ok(Usage { plan, lines: personal_credits(premium) });
    }
    Ok(Usage { plan, lines })
}

fn snapshot_line(label: &str, raw: Option<&Value>, resets_at: Option<DateTime<Utc>>) -> Option<MetricLine> {
    let snapshot = raw?.as_object()?;
    let entitlement = number(snapshot.get("entitlement"));
    let remaining = number(snapshot.get("remaining"));
    // Unlimited: the explicit flag, or GitHub's `-1` sentinel on entitlement/remaining. Suppress.
    if bool_value(snapshot.get("unlimited")) == Some(true) || entitlement == Some(-1.0) || remaining == Some(-1.0) || entitlement == Some(0.0) {
        return None;
    }
    let used = if let Some(percent_remaining) = number(snapshot.get("percent_remaining")) {
        clamp(100.0 - percent_remaining)
    } else if let (Some(entitlement), Some(remaining)) = (entitlement.filter(|e| *e > 0.0), remaining) {
        clamp(100.0 - remaining / entitlement * 100.0)
    } else {
        return None;
    };
    Some(percent(label, used, resets_at))
}

fn personal_credits(raw: Option<&Value>) -> Vec<MetricLine> {
    let used = raw.and_then(Value::as_object).and_then(|s| number(s.get("credits_used"))).filter(|u| *u > 0.0);
    used.map(|u| count("Credits", u)).into_iter().collect()
}

fn overage_line(raw: Option<&Value>) -> Option<MetricLine> {
    let snapshot = raw?.as_object()?;
    (bool_value(snapshot.get("overage_permitted")) == Some(true)).then(|| count("Extra Usage", number(snapshot.get("overage_count")).unwrap_or(0.0).max(0.0)))
}

fn limited_line(label: &str, remaining: Option<&Value>, total: Option<&Value>, resets_at: Option<DateTime<Utc>>) -> Option<MetricLine> {
    let total = number(total).filter(|t| *t > 0.0)?;
    let remaining = number(remaining)?;
    Some(percent(label, clamp((total - remaining).max(0.0) / total * 100.0), resets_at))
}

/// Title-cased on `_`, space and `-`, with the rest of each word lowercased.
fn plan_label(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str()?.trim();
    let words: Vec<String> = raw
        .split(['_', ' ', '-'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map(|c| c.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect()).unwrap_or_default()
        })
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

fn reset_date(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let raw = value?.as_str()?.trim();
    if let Ok(date) = DateTime::parse_from_rfc3339(raw) {
        return Some(date.into());
    }
    Some(NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?.and_hms_opt(0, 0, 0)?.and_utc())
}

/// Org Credits and Org Spend, summed over the Copilot items. `None` for a report that isn't GitHub's shape,
/// so it never shows as zero usage.
pub fn org_usage_lines(body: &[u8]) -> Option<Vec<MetricLine>> {
    let json: Value = serde_json::from_slice(body).ok()?;
    let (mut credits, mut spend) = (0.0, 0.0);
    for item in json.get("usageItems")?.as_array()?.iter().filter_map(Value::as_object) {
        if item.get("product").and_then(Value::as_str) == Some("Copilot") {
            credits += number(item.get("grossQuantity"))?;
            spend += number(item.get("netAmount"))?;
        }
    }
    Some(vec![
        count("Org Credits", credits),
        MetricLine::Values { label: "Org Spend".into(), values: vec![MetricValue { number: spend, currency_code: Some("usd".into()), label: None }] },
    ])
}
