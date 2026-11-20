//! Claude usage and profile mapping. Pure functions.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::models::{MetricLine, MetricValue, ProgressFormat};
use crate::openrouter::number;
use crate::provider::ProviderError;

const SESSION_MS: i64 = 5 * 60 * 60 * 1000;
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Profile {
    pub account: ProfileAccount,
    pub organization: Option<ProfileOrganization>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ProfileAccount {
    pub uuid: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ProfileOrganization {
    pub uuid: String,
    pub organization_type: Option<String>,
    pub rate_limit_tier: Option<String>,
}

impl Profile {
    pub fn parse(body: &[u8]) -> Result<Self, ProviderError> {
        serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)
    }

    /// `accountUUID[/orgUUID]`.
    pub fn identity(&self) -> String {
        match &self.organization {
            Some(org) => format!("{}/{}", self.account.uuid, org.uuid),
            None => self.account.uuid.clone(),
        }
    }

    pub fn label(&self) -> String {
        self.account.email.clone().or_else(|| self.email.clone()).unwrap_or_else(|| "Claude Account".into())
    }

    pub fn plan(&self) -> Option<String> {
        format_plan(self)
    }
}

/// Session, Weekly, Sonnet, Fable, then Extra usage spent, each only when the response has it.
pub fn map_usage(body: &[u8]) -> Result<Vec<MetricLine>, ProviderError> {
    let body: Value = serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)?;
    let Value::Object(body) = body else { return Err(ProviderError::InvalidResponse) };
    let mut lines = Vec::new();
    for (key, label, period) in [("five_hour", "Session", SESSION_MS), ("seven_day", "Weekly", WEEK_MS), ("seven_day_sonnet", "Sonnet", WEEK_MS)] {
        if let Some(window) = body.get(key).and_then(Value::as_object) {
            if let Some(used) = number(window.get("utilization")) {
                lines.push(percent(label, used, window.get("resets_at"), period));
            }
        }
    }
    lines.extend(scoped_weekly(body.get("limits"), "Fable"));
    lines.extend(extra_usage(body.get("extra_usage")));
    Ok(lines)
}

fn percent(label: &str, used: f64, resets_at: Option<&Value>, period_ms: i64) -> MetricLine {
    MetricLine::Progress {
        label: label.into(),
        used,
        limit: 100.0,
        format: ProgressFormat::Percent,
        resets_at: reset_date(resets_at),
        period_ms: Some(period_ms),
    }
}

/// `limits[]` entry with `kind: "weekly_scoped"` and `scope.model.display_name == model`; `percent` is 0-100.
fn scoped_weekly(limits: Option<&Value>, model: &str) -> Option<MetricLine> {
    limits?.as_array()?.iter().find_map(|entry| {
        let entry = entry.as_object()?;
        (entry.get("kind")?.as_str()? == "weekly_scoped").then_some(())?;
        (entry.get("scope")?.get("model")?.get("display_name")?.as_str()? == model).then_some(())?;
        Some(percent(model, number(entry.get("percent"))?, entry.get("resets_at"), WEEK_MS))
    })
}

fn extra_usage(value: Option<&Value>) -> Option<MetricLine> {
    let object = value?.as_object()?;
    (object.get("is_enabled").and_then(Value::as_bool) == Some(true)).then_some(())?;
    let currency = object.get("currency").and_then(Value::as_str).unwrap_or("USD").to_string();
    let minor = 10f64.powf(number(object.get("decimal_places")).unwrap_or(2.0));
    let used = number(object.get("used_credits")).unwrap_or(0.0).round() / minor;
    let label = "Extra usage spent".to_string();
    Some(match number(object.get("monthly_limit")).filter(|l| *l > 0.0) {
        Some(limit) => MetricLine::Progress {
            label,
            used,
            limit: limit.round() / minor,
            format: ProgressFormat::Currency(currency),
            resets_at: None,
            period_ms: None,
        },
        None => MetricLine::Values { label, values: vec![MetricValue { number: used, currency_code: Some(currency), label: None }] },
    })
}

/// ISO 8601 text, else epoch seconds or milliseconds (below 1e10 counts as seconds).
pub fn reset_date(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let value = value?;
    if let Some(text) = value.as_str().map(str::trim).filter(|t| !t.is_empty()) {
        if let Ok(date) = DateTime::parse_from_rfc3339(text) {
            return Some(date.into());
        }
    }
    let n = number(Some(value))?;
    DateTime::from_timestamp_millis((if n.abs() < 1e10 { n * 1000.0 } else { n }) as i64)
}

/// `claude_max` + tier -> "Max" / "Ultra"; other plans keep their name plus the tier multiplier ("Team 5x").
pub fn format_plan(profile: &Profile) -> Option<String> {
    let org = profile.organization.as_ref()?;
    let kind = org.organization_type.as_deref()?;
    let raw = kind.strip_prefix("claude_").unwrap_or(kind).trim();
    if raw.is_empty() {
        return None;
    }
    let base = raw
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map(|c| c.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ");
    let tier = org.rate_limit_tier.as_deref().unwrap_or("");
    if base == "Ultra" || (base == "Max" && (tier.contains("ultra") || tier.contains("20x"))) {
        return Some("Ultra".into());
    }
    if base == "Max" {
        return Some(base);
    }
    Some(match multiplier(tier) {
        Some(m) => format!("{base} {m}"),
        None => base,
    })
}

/// First `\d+x` in `tier`.
fn multiplier(tier: &str) -> Option<&str> {
    let bytes = tier.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if bytes.get(i) == Some(&b'x') {
                return Some(&tier[start..=i]);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// An Enterprise organization is billed by what it spends, so Extra usage is the whole bill.
pub fn line_title<'a>(plan: Option<&str>, label: &'a str) -> &'a str {
    if plan.is_some_and(|p| p.starts_with("Enterprise")) && label == "Extra usage spent" {
        "Spend"
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(org_type: &str, tier: Option<&str>) -> Option<String> {
        let tier = tier.map(|t| format!(r#","rate_limit_tier":"{t}""#)).unwrap_or_default();
        let json = format!(r#"{{"account":{{"uuid":"acc-1"}},"organization":{{"uuid":"org-1","organization_type":"{org_type}"{tier}}}}}"#);
        Profile::parse(json.as_bytes()).unwrap().plan()
    }

    #[test]
    fn plan_names() {
        assert_eq!(plan("claude_max", Some("default_claude_max_5x")).as_deref(), Some("Max"));
        assert_eq!(plan("claude_max", Some("default_claude_max_20x")).as_deref(), Some("Ultra"));
        assert_eq!(plan("claude_max", None).as_deref(), Some("Max"));
        assert_eq!(plan("claude_max", Some("default_claude_ultra")).as_deref(), Some("Ultra"));
        assert_eq!(plan("claude_ultra", Some("default_claude_ultra")).as_deref(), Some("Ultra"));
        assert_eq!(plan("claude_ultra", Some("default_claude_max_20x")).as_deref(), Some("Ultra"));
        assert_eq!(plan("claude_pro", Some("default_claude_ai")).as_deref(), Some("Pro"));
        assert_eq!(plan("claude_team", Some("default_claude_max_5x")).as_deref(), Some("Team 5x"));
        assert_eq!(plan("claude_enterprise", None).as_deref(), Some("Enterprise"));
    }

    #[test]
    fn profile_identity_label_and_missing_plan() {
        let p = Profile::parse(br#"{"account":{"uuid":"a","email":"me@x.io"},"organization":{"uuid":"o"},"email":"top@x.io"}"#).unwrap();
        assert_eq!((p.identity().as_str(), p.label().as_str(), p.plan()), ("a/o", "me@x.io", None));
        let p = Profile::parse(br#"{"account":{"uuid":"a"},"email":"top@x.io"}"#).unwrap();
        assert_eq!((p.identity().as_str(), p.label().as_str()), ("a", "top@x.io"));
        assert_eq!(Profile::parse(br#"{"account":{"uuid":"a"}}"#).unwrap().label(), "Claude Account");
        assert_eq!(Profile::parse(b"{}"), Err(ProviderError::InvalidResponse));
    }

    fn pct(label: &str, used: f64, reset: Option<&str>, period: i64) -> MetricLine {
        MetricLine::Progress {
            label: label.into(),
            used,
            limit: 100.0,
            format: ProgressFormat::Percent,
            resets_at: reset.map(|r| DateTime::parse_from_rfc3339(r).unwrap().into()),
            period_ms: Some(period),
        }
    }

    #[test]
    fn usage_lines_in_order_with_the_fable_limit_read_by_name() {
        let body = r#"{
          "five_hour": {"utilization": 12.5, "resets_at": "2026-11-19T18:00:00.123456+00:00"},
          "seven_day": {"utilization": "40", "resets_at": 1795000000},
          "seven_day_sonnet": {"utilization": null},
          "limits": [
            {"kind":"weekly_scoped","scope":{"model":{"display_name":"Other"}},"percent":1},
            {"kind":"daily","scope":{"model":{"display_name":"Fable"}},"percent":2},
            {"kind":"weekly_scoped","scope":{"model":{"display_name":"Fable"}},"percent":33,"resets_at":"2026-11-25T00:00:00Z"}],
          "extra_usage": {"is_enabled": false}}"#;
        let lines = map_usage(body.as_bytes()).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], pct("Session", 12.5, Some("2026-11-19T18:00:00.123456Z"), SESSION_MS));
        assert_eq!(lines[1], pct("Weekly", 40.0, Some("2026-11-18T11:06:40Z"), WEEK_MS));
        assert_eq!(lines[2], pct("Fable", 33.0, Some("2026-11-25T00:00:00Z"), WEEK_MS));
    }

    #[test]
    fn extra_usage_is_a_currency_progress_with_a_limit_and_a_value_without() {
        let limited = r#"{"extra_usage":{"is_enabled":true,"currency":"USD","decimal_places":2,"used_credits":1234.4,"monthly_limit":5000}}"#;
        assert_eq!(
            map_usage(limited.as_bytes()).unwrap(),
            vec![MetricLine::Progress {
                label: "Extra usage spent".into(),
                used: 12.34,
                limit: 50.0,
                format: ProgressFormat::Currency("USD".into()),
                resets_at: None,
                period_ms: None
            }]
        );
        let unlimited = r#"{"extra_usage":{"is_enabled":true,"used_credits":250,"monthly_limit":null}}"#;
        assert_eq!(
            map_usage(unlimited.as_bytes()).unwrap(),
            vec![MetricLine::Values { label: "Extra usage spent".into(), values: vec![MetricValue { number: 2.5, currency_code: Some("USD".into()), label: None }] }]
        );
    }

    #[test]
    fn non_object_or_non_json_is_invalid() {
        assert_eq!(map_usage(b"[]"), Err(ProviderError::InvalidResponse));
        assert_eq!(map_usage(b"nope"), Err(ProviderError::InvalidResponse));
        assert_eq!(map_usage(b"{}"), Ok(vec![]));
    }

    #[test]
    fn enterprise_titles_extra_usage_as_spend() {
        assert_eq!(line_title(Some("Enterprise"), "Extra usage spent"), "Spend");
        assert_eq!(line_title(Some("Max"), "Extra usage spent"), "Extra usage spent");
        assert_eq!(line_title(Some("Enterprise"), "Session"), "Session");
    }

    #[test]
    fn reset_dates_accept_text_seconds_and_millis() {
        let want = DateTime::from_timestamp(1_795_000_000, 0);
        assert_eq!(reset_date(Some(&Value::from(1_795_000_000))), want);
        assert_eq!(reset_date(Some(&Value::from(1_795_000_000_000i64))), want);
        assert_eq!(reset_date(Some(&Value::from("  "))), None);
        assert_eq!(reset_date(None), None);
    }
}
