//! Quota summary and `loadCodeAssist` mapping.
//! Never log response bodies.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::models::{MetricLine, ProgressFormat};

pub const SESSION_MS: i64 = 5 * 60 * 60 * 1000;
pub const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const ARGON_GROUP: &str = "Gemini 4 Argon";
const ARGON_WEEKLY_LABEL: &str = "Argon Weekly";
const CLAUDE_LABELS: [&str; 2] = ["Claude", "Claude Weekly"];

/// `(bucket id, label, period)` in display order.
const SUMMARY_BUCKETS: [(&str, &str, i64); 4] = [
    ("gemini-5h", "Session", SESSION_MS),
    ("gemini-weekly", "Weekly", WEEK_MS),
    ("3p-5h", "Claude", SESSION_MS),
    ("3p-weekly", "Claude Weekly", WEEK_MS),
];

struct Group {
    name: Option<String>,
    buckets: Vec<Bucket>,
}

struct Bucket {
    id: Option<String>,
    fraction: Option<f64>,
    reset_time: Option<String>,
    window: Option<String>,
    disabled: Option<bool>,
    invalid_disabled: bool,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(String::from)
}

impl Bucket {
    /// Never fails: a field of the wrong type reads as absent (the disabled flag as invalid).
    fn parse(value: &Value) -> Self {
        let disabled = value.get("disabled").filter(|d| !d.is_null());
        Self {
            id: text(value, "bucketId"),
            fraction: value.get("remainingFraction").and_then(Value::as_f64).filter(|f| f.is_finite()),
            reset_time: text(value, "resetTime"),
            window: text(value, "window"),
            disabled: disabled.and_then(Value::as_bool),
            invalid_disabled: disabled.is_some_and(|d| !d.is_boolean()),
        }
    }
}

/// `Err` when the value would not decode (a group that is not an object, `buckets` that is not a list).
fn parse_groups(value: Option<&Value>) -> Result<Option<Vec<Group>>, ()> {
    let list = match value {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(list)) => list,
        Some(_) => return Err(()),
    };
    list.iter()
        .map(|group| {
            if !group.is_object() {
                return Err(());
            }
            let buckets = match group.get("buckets") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(list)) => list.iter().map(Bucket::parse).collect(),
                Some(_) => return Err(()),
            };
            Ok(Group { name: text(group, "displayName"), buckets })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// `response.groups`, else `groups`; `None` when the body is not a quota summary.
fn summary_groups(body: &[u8]) -> Option<Vec<Group>> {
    let root: Value = serde_json::from_slice(body).ok()?;
    if !root.is_object() {
        return None;
    }
    let response = match root.get("response") {
        None | Some(Value::Null) => None,
        Some(r) if r.is_object() => Some(r),
        Some(_) => return None,
    };
    let nested = parse_groups(response.and_then(|r| r.get("groups"))).ok()?;
    let top = parse_groups(root.get("groups")).ok()?;
    nested.or(top)
}

fn is_shared(group: &Group) -> bool {
    group.name.as_deref() != Some(ARGON_GROUP)
}

fn reset_date(value: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?.trim()).ok().map(Into::into)
}

/// Used percent from the remaining fraction, clamped to 0...1.
fn line(label: &str, fraction: f64, reset_time: Option<&str>, period_ms: i64) -> MetricLine {
    MetricLine::Progress {
        label: label.into(),
        used: (1.0 - fraction.clamp(0.0, 1.0)) * 100.0,
        limit: 100.0,
        format: ProgressFormat::Percent,
        resets_at: reset_date(reset_time),
        period_ms: Some(period_ms),
    }
}

/// True when the shared groups hold a usable fraction for all four pool buckets.
pub fn is_complete_summary(body: &[u8]) -> bool {
    let Some(groups) = summary_groups(body) else { return false };
    SUMMARY_BUCKETS.iter().all(|(id, ..)| {
        groups.iter().filter(|g| is_shared(g)).flat_map(|g| &g.buckets).any(|b| b.fraction.is_some() && b.id.as_deref() == Some(id))
    })
}

fn argon_weekly_line(groups: &[Group]) -> Option<MetricLine> {
    let mut argon = groups.iter().filter(|g| !is_shared(g)).peekable();
    argon.peek()?;
    let mut weekly = argon.flat_map(|g| &g.buckets).filter(|b| b.window.as_deref() == Some("weekly"));
    let (bucket, None) = (weekly.next()?, weekly.next()) else { return None };
    let id = bucket.id.as_deref().filter(|id| !id.is_empty())?;
    if SUMMARY_BUCKETS.iter().any(|(shared, ..)| *shared == id) || bucket.invalid_disabled || bucket.disabled == Some(true) {
        return None;
    }
    Some(line(ARGON_WEEKLY_LABEL, bucket.fraction?, bucket.reset_time.as_deref(), WEEK_MS))
}

/// Merges the bodies (first usable bucket per id wins) into Session, Weekly, [Argon Weekly,] Claude,
/// Claude Weekly. Argon sits before the first Claude line, else last. `None` when no body is a summary.
pub fn parse_quota_summaries(bodies: &[Vec<u8>]) -> Option<Vec<MetricLine>> {
    let lists: Vec<Vec<Group>> = bodies.iter().filter_map(|b| summary_groups(b)).collect();
    if lists.is_empty() {
        return None;
    }
    let mut pooled: HashMap<&str, (f64, Option<&str>)> = HashMap::new();
    for bucket in lists.iter().flatten().filter(|g| is_shared(g)).flat_map(|g| &g.buckets) {
        let Some(id) = bucket.id.as_deref().filter(|id| SUMMARY_BUCKETS.iter().any(|(b, ..)| b == id)) else { continue };
        if pooled.contains_key(id) {
            continue;
        }
        if let Some(fraction) = bucket.fraction {
            pooled.insert(id, (fraction, bucket.reset_time.as_deref()));
        }
    }
    let mut lines: Vec<MetricLine> = SUMMARY_BUCKETS
        .iter()
        .filter_map(|(id, label, period)| pooled.get(id).map(|(fraction, reset)| line(label, *fraction, *reset, *period)))
        .collect();
    if let Some(argon) = lists.iter().find_map(|groups| argon_weekly_line(groups)) {
        let at = lines.iter().position(|l| CLAUDE_LABELS.contains(&l.label())).unwrap_or(lines.len());
        lines.insert(at, argon);
    }
    Some(lines)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Tier {
    id: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoadEnvelope {
    cloudaicompanion_project: Option<String>,
    current_tier: Option<Tier>,
    paid_tier: Option<Tier>,
}

/// Google's own names for the paid tiers; anything else is formatted from the tier name.
pub fn parse_plan(body: &[u8]) -> Option<String> {
    let envelope: LoadEnvelope = serde_json::from_slice(body).ok()?;
    if let Some(plan) = envelope.paid_tier.as_ref().and_then(|t| match t.id.as_deref()? {
        "g1-pro-tier" => Some("Google AI Pro"),
        "g1-ultra-tier" => Some("Google AI Ultra"),
        "g1-ultra-lite-tier" => Some("Google AI Ultra Lite"),
        _ => None,
    }) {
        return Some(plan.into());
    }
    format_plan(envelope.paid_tier.and_then(|t| t.name).or(envelope.current_tier.and_then(|t| t.name)).as_deref())
}

/// The Cloud Code project the quota summary must name for free-tier accounts.
pub fn parse_project(body: &[u8]) -> Option<String> {
    let envelope: LoadEnvelope = serde_json::from_slice(body).ok()?;
    envelope.cloudaicompanion_project.filter(|p| !p.is_empty())
}

fn format_plan(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_lowercase();
    if let Some(tier) = ["Ultra Lite", "Ultra", "Pro"].into_iter().find(|t| lower.contains(&t.to_lowercase())) {
        return Some(format!("Google AI {tier}"));
    }
    let words = trimmed.split_whitespace().map(|w| {
        let mut chars = w.chars();
        chars.next().map(|c| c.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
    });
    Some(words.collect::<Vec<_>>().join(" "))
}
