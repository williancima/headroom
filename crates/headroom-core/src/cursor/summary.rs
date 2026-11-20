//! Cursor's cookie-authenticated `usage-summary` and request-based `usage` mapping. Pure functions.

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use super::mapper::{dollar_progress, dollar_value, dollars, iso_date, num, obj, percent, resolved_plan_label, Obj, Usage, MONTH_MS};
use crate::models::{MetricLine, ProgressFormat};
use crate::provider::ProviderError;

type Cycle = (Option<DateTime<Utc>>, i64);

fn enabled(bucket: &Obj) -> bool {
    bucket.get("enabled").and_then(Value::as_bool) != Some(false)
}

fn bucket(value: Option<&Value>) -> Option<&Obj> {
    value.and_then(Value::as_object).filter(|b| enabled(b))
}

/// `(used, limit)` in cents of a bucket with a positive limit; spend is inferred from `remaining` when
/// `used` is not positive.
fn dollar_meter(value: Option<&Value>) -> Option<(f64, f64)> {
    let bucket = bucket(value)?;
    let limit = num(bucket, "limit").filter(|l| *l > 0.0)?;
    let inferred = (limit - num(bucket, "remaining").unwrap_or(limit)).max(0.0);
    let used = num(bucket, "used").filter(|u| *u > 0.0).unwrap_or(inferred);
    Some((used.max(0.0), limit))
}

fn usable_dollar_bucket(value: Option<&Value>) -> bool {
    bucket(value).is_some_and(|b| dollar_meter(value).is_some() || num(b, "used").unwrap_or(0.0) > 0.0)
}

pub fn has_usable_summary_payload(summary: &Obj) -> bool {
    if let (Some(start), Some(end)) = (iso_date(summary, "billingCycleStart"), iso_date(summary, "billingCycleEnd")) {
        if end > start {
            return true;
        }
    }
    let individual = obj(summary, "individualUsage");
    let team = obj(summary, "teamUsage");
    if let Some(plan) = individual.and_then(|i| obj(i, "plan")) {
        if ["totalPercentUsed", "autoPercentUsed", "apiPercentUsed"].iter().any(|k| num(plan, k).is_some()) {
            return true;
        }
    }
    [individual.and_then(|i| i.get("onDemand")), individual.and_then(|i| i.get("overall")), team.and_then(|t| t.get("onDemand")), team.and_then(|t| t.get("pooled"))]
        .into_iter()
        .any(usable_dollar_bucket)
}

fn billing_cycle(summary: Option<&Obj>, request_usage: Option<&Obj>) -> Cycle {
    if let Some(s) = summary {
        if let (Some(start), Some(end)) = (iso_date(s, "billingCycleStart"), iso_date(s, "billingCycleEnd")) {
            if end > start {
                return (Some(end), (end - start).num_milliseconds());
            }
        }
    }
    let start = request_usage.and_then(|r| iso_date(r, "startOfMonth"));
    (start.map(|s| s + Duration::milliseconds(MONTH_MS)), MONTH_MS)
}

/// Requests (when the REST usage has a request limit), else the summary's total, then the structured
/// percentages, then On-demand. Errors with `unavailable_message` when nothing maps.
pub fn map(summary: Option<&Obj>, request_usage: Option<&Obj>, plan_name: Option<&str>, unavailable_message: &str) -> Result<Usage, ProviderError> {
    let cycle = billing_cycle(summary, request_usage);
    let mut lines = vec![];
    if !append_requests(request_usage, cycle, &mut lines) {
        append_summary_total(summary, cycle, &mut lines);
    }
    append_structured_percentages(summary, cycle, &mut lines);
    append_on_demand(summary, cycle, &mut lines);
    if lines.is_empty() {
        return Err(ProviderError::Unavailable(unavailable_message.into()));
    }
    let membership = summary.and_then(|s| s.get("membershipType")).and_then(Value::as_str);
    Ok(Usage { plan: resolved_plan_label(plan_name, membership), lines, line_titles: vec![] })
}

fn append_requests(usage: Option<&Obj>, (resets_at, period): Cycle, lines: &mut Vec<MetricLine>) -> bool {
    let Some(requests) = usage.and_then(|u| obj(u, "gpt-4")) else { return false };
    let Some(limit) = num(requests, "maxRequestUsage").filter(|l| *l > 0.0) else { return false };
    let used = num(requests, "numRequests").or_else(|| num(requests, "numRequestsTotal")).unwrap_or(0.0).max(0.0);
    for label in ["Total usage", "Requests"] {
        lines.push(MetricLine::Progress {
            label: label.into(),
            used,
            limit,
            format: ProgressFormat::Count("requests".into()),
            resets_at,
            period_ms: Some(period),
        });
    }
    true
}

fn append_summary_total(summary: Option<&Obj>, (resets_at, period): Cycle, lines: &mut Vec<MetricLine>) {
    let individual = summary.and_then(|s| obj(s, "individualUsage"));
    let team = summary.and_then(|s| obj(s, "teamUsage"));
    let limit_type = summary.and_then(|s| s.get("limitType")).and_then(Value::as_str).map(str::to_lowercase);
    let pooled = dollar_meter(team.and_then(|t| t.get("pooled")));
    let progress = |(used, limit): (f64, f64)| dollar_progress("Total usage", used, limit, resets_at, period);
    if let (Some("team"), Some(pooled)) = (limit_type.as_deref(), pooled) {
        lines.push(progress(pooled));
    } else if let Some(p) = individual.and_then(|i| obj(i, "plan")).and_then(|p| num(p, "totalPercentUsed")) {
        lines.push(percent("Total usage", p, resets_at, period));
    } else if let Some(overall) = dollar_meter(individual.and_then(|i| i.get("overall"))) {
        lines.push(progress(overall));
    } else if let Some(pooled) = pooled {
        lines.push(progress(pooled));
    }
}

fn append_structured_percentages(summary: Option<&Obj>, (resets_at, period): Cycle, lines: &mut Vec<MetricLine>) {
    let plan = summary.and_then(|s| obj(s, "individualUsage")).and_then(|i| obj(i, "plan"));
    for (key, label) in [("autoPercentUsed", "Cursor Models"), ("apiPercentUsed", "Other Models")] {
        if let Some(p) = plan.and_then(|p| num(p, key)) {
            lines.push(percent(label, p, resets_at, period));
        }
    }
}

fn append_on_demand(summary: Option<&Obj>, cycle: Cycle, lines: &mut Vec<MetricLine>) {
    let individual = summary.and_then(|s| obj(s, "individualUsage"));
    let team = summary.and_then(|s| obj(s, "teamUsage"));
    if !append_on_demand_bucket(individual.and_then(|i| i.get("onDemand")), cycle, lines) {
        append_on_demand_bucket(team.and_then(|t| t.get("onDemand")), cycle, lines);
    }
}

pub fn append_missing_on_demand(summary: &Obj, lines: &mut Vec<MetricLine>) {
    if lines.iter().all(|l| l.label() != "On-demand") {
        append_on_demand(Some(summary), billing_cycle(Some(summary), None), lines);
    }
}

/// Whether a usable row was emitted. An Enterprise response can carry an individual placeholder bucket
/// (`enabled: false` or a zero limit) next to a valid team bucket, so finding the individual object must
/// not suppress the team fallback.
fn append_on_demand_bucket(value: Option<&Value>, (resets_at, period): Cycle, lines: &mut Vec<MetricLine>) -> bool {
    let Some(bucket) = bucket(value) else { return false };
    if let Some((used, limit)) = dollar_meter(value) {
        lines.push(dollar_progress("On-demand", used, limit, resets_at, period));
        return true;
    }
    if let Some(used) = num(bucket, "used").filter(|u| *u > 0.0) {
        lines.push(dollar_value("On-demand", dollars(used)));
        return true;
    }
    false
}
