//! Grok usage mapping and credits-config decoding.
//! Pure functions over response bodies.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::models::{MetricLine, ProgressFormat};
use crate::openrouter::number;
use crate::provider::ProviderError;

pub const WEEKLY_PERIOD_TYPE: &str = "USAGE_PERIOD_TYPE_WEEKLY";
/// Shown instead of usage when the account is a team principal.
pub const TEAM_NOTICE: &str = "Grok team accounts have no personal quota.";

/// The credits config's current period and caps. Proto-JSON omits zero values, so an absent percent or
/// cap is a genuine 0.
#[derive(Debug, PartialEq)]
pub struct CreditsConfig {
    pub period_type: String,
    pub used_percent: f64,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    /// In cents.
    pub on_demand_cap: f64,
}

impl CreditsConfig {
    pub fn period_duration_ms(&self) -> i64 {
        (self.period_end - self.period_start).num_milliseconds()
    }
}

fn date(value: Option<&Value>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?.as_str()?.trim()).ok().map(Into::into)
}

/// A present but malformed percent or cap, a period that does not move forward, or a body without the
/// config is `InvalidResponse`.
pub fn decode_credits_config(body: &[u8]) -> Result<CreditsConfig, ProviderError> {
    let json: Value = serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)?;
    let config = json.get("config").and_then(Value::as_object).ok_or(ProviderError::InvalidResponse)?;
    let period = config.get("currentPeriod").and_then(Value::as_object).ok_or(ProviderError::InvalidResponse)?;
    let period_type = period.get("type").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).ok_or(ProviderError::InvalidResponse)?;
    let (Some(period_start), Some(period_end)) = (date(period.get("start")), date(period.get("end"))) else {
        return Err(ProviderError::InvalidResponse);
    };
    if period_end <= period_start {
        return Err(ProviderError::InvalidResponse);
    }
    let used_percent = match config.get("creditUsagePercent") {
        Some(raw) => number(Some(raw)).ok_or(ProviderError::InvalidResponse)?,
        None => 0.0,
    };
    Ok(CreditsConfig { period_type: period_type.to_string(), used_percent, period_start, period_end, on_demand_cap: on_demand_cap(config)? })
}

fn on_demand_cap(config: &Map<String, Value>) -> Result<f64, ProviderError> {
    let Some(raw) = config.get("onDemandCap") else { return Ok(0.0) };
    let object = raw.as_object().ok_or(ProviderError::InvalidResponse)?;
    let cap = match object.get("val") {
        Some(val) => number(Some(val)).ok_or(ProviderError::InvalidResponse)?,
        None => 0.0,
    };
    // Too large for an integer is a schema change.
    (cap.abs() < i64::MAX as f64).then_some(cap).ok_or(ProviderError::InvalidResponse)
}

/// The weekly limit and the pay-as-you-go cap, each only when the config has it.
pub fn map_credits_config(body: &[u8]) -> Result<Vec<MetricLine>, ProviderError> {
    let config = decode_credits_config(body)?;
    let mut lines = vec![];
    if config.period_type == WEEKLY_PERIOD_TYPE {
        lines.push(MetricLine::Progress {
            label: "Weekly limit".into(),
            used: config.used_percent.clamp(0.0, 100.0),
            limit: 100.0,
            format: ProgressFormat::Percent,
            resets_at: Some(config.period_end),
            period_ms: Some(config.period_duration_ms()),
        });
    }
    if config.on_demand_cap > 0.0 {
        lines.push(MetricLine::Badge { label: "Pay as you go".into(), text: format!("{} cap", dollars(config.on_demand_cap.round() / 100.0)) });
    }
    Ok(lines)
}

/// A 412 whose error says the account has no personal team.
pub fn is_team_refusal(status: u16, body: &[u8]) -> bool {
    status == 412
        && serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|j| j.get("error")?.as_str().map(|e| e.to_lowercase().contains("no personal team")))
            .unwrap_or(false)
}

/// `subscription_tier_display`, trimmed; `None` when absent or empty.
pub fn plan_name(body: &[u8]) -> Option<String> {
    let json: Value = serde_json::from_slice(body).ok()?;
    let plan = json.get("subscription_tier_display")?.as_str()?.trim();
    (!plan.is_empty()).then(|| plan.to_string())
}

/// Row-style dollars: whole amounts without cents, others with two decimals, and thousands compacted.
fn dollars(value: f64) -> String {
    for (divisor, suffix) in [(1e9, "B"), (1e6, "M"), (1e3, "K")] {
        if value.abs() >= divisor {
            let scaled = format!("{:.1}", value / divisor);
            return format!("${}{suffix}", scaled.strip_suffix(".0").unwrap_or(&scaled));
        }
    }
    if (value * 100.0).round() % 100.0 == 0.0 {
        format!("${value:.0}")
    } else {
        format!("${value:.2}")
    }
}
