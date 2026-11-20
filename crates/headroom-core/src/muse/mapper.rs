//! Muse Code key-response decoding and usage mapping. Pure functions over bodies.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::models::{MetricLine, ProgressFormat};
use crate::provider::ProviderError;

pub const SESSION_MS: i64 = 5 * 60 * 60 * 1000;
pub const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The fields read from Muse Code's key response, and nothing else: the answer also carries the account's
/// Model API key, which has no field here and so goes nowhere.
#[derive(Debug, Deserialize)]
pub struct MuseAccount {
    #[serde(rename = "is_subs_active")]
    pub is_subscribed: bool,
    #[serde(rename = "subs_tier_name")]
    pub plan: Option<String>,
    #[serde(rename = "user_email")]
    pub email: Option<String>,
    #[serde(rename = "subs_usage")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct Usage {
    pub window: Option<Window>,
    pub weekly: Option<Window>,
}

#[derive(Debug, Deserialize)]
pub struct Window {
    pub used_percent: f64,
    /// Epoch seconds.
    pub resets_at: Option<f64>,
}

pub struct MappedUsage {
    pub plan: Option<String>,
    pub lines: Vec<MetricLine>,
}

pub fn parse_account(body: &[u8]) -> Result<MuseAccount, ProviderError> {
    serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)
}

fn percent(label: &str, used: f64, resets_at: Option<DateTime<Utc>>, period_ms: i64) -> MetricLine {
    MetricLine::Progress { label: label.into(), used, limit: 100.0, format: ProgressFormat::Percent, resets_at, period_ms: Some(period_ms) }
}

/// Muse leaves out `subs_usage` until the plan's first use and while the 5-hour window is idle, even when
/// the week has usage, so the card keeps its last known values then (`last_known`: the previous
/// snapshot's lines). A kept window whose reset has passed, and a window Muse has never reported, read 0%.
pub fn map(account: &MuseAccount, last_known: &[MetricLine], now: DateTime<Utc>) -> Result<MappedUsage, ProviderError> {
    if !account.is_subscribed {
        return Err(ProviderError::Unavailable("No Muse Code subscription on this account.".into()));
    }
    let usage = account.usage.as_ref();
    let lines = [("Session", usage.and_then(|u| u.window.as_ref()), SESSION_MS), ("Weekly", usage.and_then(|u| u.weekly.as_ref()), WEEK_MS)]
        .into_iter()
        .map(|(label, window, period_ms)| {
            if let Some(window) = window {
                let resets_at = window.resets_at.and_then(|s| DateTime::from_timestamp_millis((s * 1000.0) as i64));
                return percent(label, window.used_percent, resets_at, period_ms);
            }
            match last_known.iter().find(|l| l.label() == label) {
                Some(kept) if usage.is_none() => aged(kept, now),
                _ => percent(label, 0.0, None, period_ms),
            }
        })
        .collect();
    Ok(MappedUsage { plan: account.plan.clone(), lines })
}

/// A kept line whose reset has passed reads 0% with no reset.
fn aged(line: &MetricLine, now: DateTime<Utc>) -> MetricLine {
    match line {
        MetricLine::Progress { label, resets_at: Some(reset), period_ms: Some(period_ms), .. } if *reset <= now => percent(label, 0.0, None, *period_ms),
        other => other.clone(),
    }
}
