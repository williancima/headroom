//! Z.ai banked resets.
//! Pure functions. Claiming from the CLI waits for phase 4.

use chrono::{DateTime, Duration, FixedOffset, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::claim::{ResetItem, ResetRef};
use crate::claude::resets::ResetKind;
use crate::claude::{ClaimFailure, ClaimOutcome};
use crate::models::{MetricLine, MetricValue};
use crate::provider::ProviderError;

pub use crate::claude::resets::LINE_LABEL;

const LIST_INTERVAL_SECS: i64 = 60 * 60;

/// A banked reset refills one window: the five-hour Session or the Weekly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResetWindow {
    #[serde(rename = "FIVE_HOUR")]
    FiveHour,
    #[serde(rename = "WEEK")]
    Week,
}

impl ResetWindow {
    const ALL: [Self; 2] = [Self::FiveHour, Self::Week];

    /// The `resetType` a claim sends.
    pub fn reset_type(self) -> &'static str {
        match self {
            Self::FiveHour => "FIVE_HOUR",
            Self::Week => "WEEK",
        }
    }

    fn kind(self) -> ResetKind {
        match self {
            Self::FiveHour => ResetKind::Session,
            Self::Week => ResetKind::Weekly,
        }
    }

    fn confirm_detail(self) -> &'static str {
        match self {
            Self::FiveHour => "Refills your session limit now. This can't be undone.",
            Self::Week => "Refills your weekly limit now. This can't be undone.",
        }
    }

    fn list_key(self) -> &'static str {
        match self {
            Self::FiveHour => "fiveHourResets",
            Self::Week => "weekResets",
        }
    }
}

/// One available reset record. A claim names its record exactly: two resets can share an expiry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Reset {
    pub window: ResetWindow,
    pub record_id: i64,
    pub expires_at: DateTime<Utc>,
    pub grant_type: String,
}

/// The reset list is read at most hourly (`RefreshSetting.secondaryReadInterval`), with the plan. The
/// caller records `now` whenever it asked, even on failure.
pub fn list_due(last_read: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last_read.is_none_or(|at| now - at >= Duration::seconds(LIST_INTERVAL_SECS))
}

/// The available, unexpired resets of both windows, earliest first.
pub fn parse_resets(body: &[u8], now: DateTime<Utc>) -> Result<Vec<Reset>, ProviderError> {
    let invalid = || ProviderError::InvalidResponse;
    let json: Value = serde_json::from_slice(body).map_err(|_| invalid())?;
    if json.get("success").and_then(Value::as_bool) == Some(false) {
        return Err(invalid());
    }
    let data = json.get("data").and_then(Value::as_object).ok_or_else(invalid)?;
    let mut resets = vec![];
    for window in ResetWindow::ALL {
        let Some(records) = data.get(window.list_key()) else { continue };
        for record in records.as_array().ok_or_else(invalid)?.iter().filter_map(Value::as_object) {
            if record.get("available").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let record_id = record.get("recordId").and_then(Value::as_i64).ok_or_else(invalid)?;
            let expires_at = record.get("expireTime").and_then(Value::as_str).and_then(expiry).ok_or_else(invalid)?;
            let grant_type = record.get("grantType").and_then(Value::as_str).filter(|g| !g.is_empty()).ok_or_else(invalid)?;
            if expires_at > now {
                resets.push(Reset { window, record_id, expires_at, grant_type: grant_type.to_string() });
            }
        }
    }
    resets.sort_by_key(|r| (r.expires_at, r.record_id));
    Ok(resets)
}

/// The "Rate Limit Resets" count line; the expiries go in `ProviderSnapshot::reset_expiries`.
pub fn line(resets: &[Reset]) -> MetricLine {
    MetricLine::Values { label: LINE_LABEL.into(), values: vec![MetricValue { number: resets.len() as f64, currency_code: None, label: None }] }
}

/// One timeline across both windows. Z.ai spends a window's resets earliest first, so only the earliest of
/// each window offers Use.
pub fn items(resets: &[Reset]) -> Vec<ResetItem> {
    let mut sorted: Vec<&Reset> = resets.iter().collect();
    sorted.sort_by_key(|r| (r.expires_at, r.record_id));
    let mut seen = vec![];
    sorted
        .into_iter()
        .map(|reset| {
            let usable = !seen.contains(&reset.window);
            seen.push(reset.window);
            ResetItem {
                kind: reset.window.kind(),
                expires_at: reset.expires_at,
                claim: ResetRef::Zai(reset.clone()),
                usable,
                confirm_detail: Some(reset.window.confirm_detail().into()),
            }
        })
        .collect()
}

/// Only a successful status with `success: true` spent the reset.
pub fn claim_outcome(status: u16, auth_failure: bool, body: &[u8]) -> ClaimOutcome {
    if auth_failure {
        return ClaimOutcome::Failed(ClaimFailure::SignInExpired);
    }
    let spent = serde_json::from_slice::<Value>(body).ok().and_then(|b| b.get("success")?.as_bool()) == Some(true);
    if (200..300).contains(&status) && spent {
        ClaimOutcome::Success
    } else {
        ClaimOutcome::Failed(ClaimFailure::Refused)
    }
}

/// Z.ai writes expiries as `yyyy-MM-dd HH:mm:ss` in its own time zone, UTC+8. The text must round-trip, so
/// an unpadded or out-of-range field is not a date.
fn expiry(text: &str) -> Option<DateTime<Utc>> {
    const FORMAT: &str = "%Y-%m-%d %H:%M:%S";
    let local = NaiveDateTime::parse_from_str(text, FORMAT).ok().filter(|d| d.format(FORMAT).to_string() == text)?;
    Some(local.and_local_timezone(FixedOffset::east_opt(8 * 3600)?).single()?.with_timezone(&Utc))
}
