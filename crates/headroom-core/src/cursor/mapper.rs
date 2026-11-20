//! Cursor usage mapping. Pure functions over JSON objects.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::{Map, Value};

use crate::models::{MetricLine, MetricValue, ProgressFormat};
use crate::openrouter::number;
use crate::provider::ProviderError;

pub(super) type Obj = Map<String, Value>;

pub const MONTH_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const ORG_MEMBERSHIPS: [&str; 2] = ["enterprise", "team"];

#[derive(Debug)]
pub struct Usage {
    pub plan: Option<String>,
    pub lines: Vec<MetricLine>,
    /// Display titles that replace a line's default one (`"On-demand"` -> `"Team Spend"`).
    pub line_titles: Vec<(String, String)>,
}

/// What an account whose Cursor, X and Grok plans are merged reports in place of credit grants, the
/// Stripe balance and the on-demand spend limit.
#[derive(Debug, Default)]
pub struct Unified {
    pub credit_balance: Option<Obj>,
    pub usage_status: Option<Obj>,
}

pub(super) fn obj<'a>(map: &'a Obj, key: &str) -> Option<&'a Obj> {
    map.get(key).and_then(Value::as_object)
}

pub(super) fn num(map: &Obj, key: &str) -> Option<f64> {
    number(map.get(key))
}

fn text<'a>(map: &'a Obj, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

fn int(value: f64) -> Option<i64> {
    (value.abs() < i64::MAX as f64).then_some(value as i64)
}

fn epoch_ms(ms: f64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(int(ms)?)
}

/// ISO 8601 text with or without a zone or fraction (no zone means UTC).
pub(super) fn iso_date(map: &Obj, key: &str) -> Option<DateTime<Utc>> {
    let raw = text(map, key)?.trim();
    let raw = raw.strip_suffix(" UTC").map(|s| format!("{s}Z")).unwrap_or_else(|| raw.to_string());
    if let Ok(date) = DateTime::parse_from_rfc3339(&raw) {
        return Some(date.into());
    }
    ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(&raw, f).ok())
        .map(|d| d.and_utc())
}

pub(super) fn dollars(cents: f64) -> f64 {
    cents.round() / 100.0
}

pub(super) fn percent(label: &str, used: f64, resets_at: Option<DateTime<Utc>>, period_ms: i64) -> MetricLine {
    MetricLine::Progress {
        label: label.into(),
        used,
        limit: 100.0,
        format: ProgressFormat::Percent,
        resets_at,
        period_ms: Some(period_ms),
    }
}

pub(super) fn dollar_progress(label: &str, used_cents: f64, limit_cents: f64, resets_at: Option<DateTime<Utc>>, period_ms: i64) -> MetricLine {
    MetricLine::Progress {
        label: label.into(),
        used: dollars(used_cents),
        limit: dollars(limit_cents),
        format: ProgressFormat::Dollars,
        resets_at,
        period_ms: Some(period_ms),
    }
}

pub(super) fn dollar_value(label: &str, dollars: f64) -> MetricLine {
    MetricLine::Values { label: label.into(), values: vec![MetricValue { number: dollars, currency_code: Some("usd".into()), label: None }] }
}

/// The numbers of `planUsage` and `spendLimitUsage` that decide which usage shape an account has.
#[derive(Debug)]
pub struct PlanFacts {
    pub is_enabled: bool,
    pub has_plan_usage: bool,
    pub limit: Option<f64>,
    pub total_percent_used: Option<f64>,
    pub has_model_pools: bool,
    pub spend_limit_type: Option<String>,
    pub pooled_limit: f64,
}

impl PlanFacts {
    pub fn new(usage: &Obj) -> Self {
        let plan = obj(usage, "planUsage");
        let get = |key: &str| plan.and_then(|p| num(p, key));
        let limit = get("limit");
        let spent = get("totalSpend").unwrap_or_else(|| limit.unwrap_or(0.0) - get("remaining").or(limit).unwrap_or(0.0));
        let has_model_pools = match (get("autoPercentUsed"), get("apiPercentUsed")) {
            (Some(auto), Some(api)) if auto >= 0.0 && api >= 0.0 => auto > 0.0 || api > 0.0 || spent == 0.0,
            _ => false,
        };
        let spend = obj(usage, "spendLimitUsage");
        Self {
            is_enabled: usage.get("enabled").and_then(Value::as_bool) != Some(false),
            has_plan_usage: plan.is_some(),
            limit,
            total_percent_used: get("totalPercentUsed"),
            has_model_pools,
            spend_limit_type: spend.and_then(|s| text(s, "limitType")).map(str::to_lowercase),
            pooled_limit: spend.and_then(|s| num(s, "pooledLimit")).unwrap_or(0.0),
        }
    }

    fn plan_usage_unusable(&self) -> bool {
        !self.has_plan_usage || (self.limit.is_none() && !self.has_model_pools)
    }

    pub fn is_team_by_shape(&self) -> bool {
        self.spend_limit_type.as_deref() == Some("team") || self.pooled_limit > 0.0
    }

    pub fn should_try_generic_request_fallback(&self) -> bool {
        self.is_enabled && self.has_plan_usage && self.limit.is_none() && self.total_percent_used.is_none() && !self.has_model_pools
    }
}

/// Grok Bot is Cursor's "Sand" product, with its own weekly allowance and reset window. Pooled enterprise
/// accounts and accounts without an included allowance have no separate personal meter.
pub fn map_grok_bot_usage(usage: &Obj) -> Option<MetricLine> {
    let flag = |key: &str| usage.get(key).and_then(Value::as_bool);
    if flag("usesPooledEnterpriseAllowance") == Some(true) || flag("hasNonZeroIncludedLimit") == Some(false) || flag("includedLimitZero") == Some(true) {
        return None;
    }
    let used = num(usage, "usagePercent").filter(|p| *p >= 0.0)?;
    let reset = iso_date(usage, "nextResetTimestampUtc");
    let period = match (iso_date(usage, "currentPeriodStart"), reset) {
        (Some(start), Some(reset)) if reset > start => (reset - start).num_milliseconds(),
        _ => WEEK_MS,
    };
    Some(percent("Grok Bot usage", used.clamp(0.0, 100.0), reset, period))
}

fn billing_cycle(usage: &Obj) -> (Option<DateTime<Utc>>, i64) {
    let (start, end) = (num(usage, "billingCycleStart"), num(usage, "billingCycleEnd"));
    if let (Some(start), Some(end)) = (start, end) {
        if let (true, Some(period)) = (end > start, int(end - start)) {
            return (epoch_ms(end), period);
        }
    }
    (end.and_then(epoch_ms), MONTH_MS)
}

/// Total usage, Cursor Models, Other Models, On-demand, each when the response has it, after Credits.
/// Errors with `Unavailable` when the response has no usable total.
pub fn map_usage(
    usage: &Obj,
    plan_name: Option<&str>,
    credit_grants: Option<&Obj>,
    stripe_balance_cents: f64,
    unified: Option<&Unified>,
) -> Result<Usage, ProviderError> {
    let facts = PlanFacts::new(usage);
    let plan_usage = obj(usage, "planUsage").filter(|_| facts.is_enabled);
    let Some(plan_usage) = plan_usage else { return Err(ProviderError::Unavailable("No active Cursor subscription.".into())) };
    if facts.limit.is_none() && facts.total_percent_used.is_none() && !facts.has_model_pools {
        return Err(ProviderError::Unavailable("Total usage limit missing from API response.".into()));
    }
    let normalized_plan = plan_name.map(|p| p.trim().to_lowercase()).unwrap_or_default();
    let mut lines = vec![];
    let mut line_titles = vec![];
    match unified {
        Some(u) => append_unified_credits(u.credit_balance.as_ref(), &mut lines),
        None => append_credits(credit_grants, stripe_balance_cents, &mut lines),
    }

    let used_cents = num(plan_usage, "totalSpend").unwrap_or_else(|| facts.limit.unwrap_or(0.0) - num(plan_usage, "remaining").unwrap_or(0.0));
    let computed = facts.limit.filter(|l| *l > 0.0).map_or(0.0, |limit| used_cents / limit * 100.0);
    let total_percent = facts.total_percent_used.unwrap_or(computed);
    let (resets_at, period) = billing_cycle(usage);
    let is_team = normalized_plan == "team" || facts.is_team_by_shape();

    if is_team && facts.has_model_pools {
        // Modern Teams seats still report the old $20 allowance. Structured pool data wins; never
        // synthesize a total from stale dollars when Cursor omits totalPercentUsed.
        if let Some(p) = facts.total_percent_used {
            lines.push(percent("Total usage", p, resets_at, period));
        }
    } else if is_team {
        if let Some(limit) = facts.limit {
            lines.push(dollar_progress("Total usage", used_cents, limit, resets_at, period));
        } else if let Some(p) = facts.total_percent_used {
            lines.push(percent("Total usage", p, resets_at, period));
        } else {
            return Err(ProviderError::Unavailable("Cursor request-based usage data unavailable. Try again later.".into()));
        }
    } else {
        lines.push(percent("Total usage", total_percent, resets_at, period));
    }

    if let Some(u) = unified {
        append_additional_usage(u.usage_status.as_ref(), &mut lines);
        return Ok(Usage { plan: plan_label(plan_name), lines, line_titles });
    }
    for (key, label) in [("autoPercentUsed", "Cursor Models"), ("apiPercentUsed", "Other Models")] {
        if let Some(p) = num(plan_usage, key) {
            lines.push(percent(label, p, resets_at, period));
        }
    }
    if let Some(spend) = obj(usage, "spendLimitUsage") {
        append_on_demand(spend, &facts, (resets_at, period), &mut lines, &mut line_titles);
    }
    Ok(Usage { plan: plan_label(plan_name), lines, line_titles })
}

fn append_on_demand(spend: &Obj, facts: &PlanFacts, (resets_at, period): (Option<DateTime<Utc>>, i64), lines: &mut Vec<MetricLine>, titles: &mut Vec<(String, String)>) {
    let individual_limit = num(spend, "individualLimit");
    let has_team_spend = facts.is_team_by_shape() || num(spend, "pooledUsed").unwrap_or(0.0) > 0.0;
    let (limit, spent_keys, remaining_key): (f64, &[&str], Option<&str>) = if let Some(limit) = individual_limit {
        (limit, if has_team_spend { &["individualUsed"] } else { &["individualUsed", "totalSpend"] }, Some("individualRemaining"))
    } else if facts.pooled_limit > 0.0 {
        (facts.pooled_limit, &["pooledUsed"], Some("pooledRemaining"))
    } else {
        (0.0, &["individualUsed", "pooledUsed", "totalSpend"], None)
    };
    let reported: Vec<f64> = spent_keys.iter().filter_map(|k| num(spend, k)).collect();
    let remaining = remaining_key.and_then(|k| num(spend, k));
    let spent = on_demand_spend_cents(&reported, limit, remaining);
    if limit > 0.0 {
        lines.push(dollar_progress("On-demand", spent, limit, resets_at, period));
    } else if spent > 0.0 {
        lines.push(dollar_value("On-demand", dollars(spent)));
    }
    if individual_limit.is_none() && facts.is_team_by_shape() && (limit > 0.0 || spent > 0.0) {
        titles.push(("On-demand".into(), "Team Spend".into()));
    }
}

fn on_demand_spend_cents(reported: &[f64], limit: f64, remaining: Option<f64>) -> f64 {
    if let Some(positive) = reported.iter().find(|v| **v > 0.0) {
        return *positive;
    }
    let inferred = remaining.map_or(0.0, |r| (limit - r).max(0.0));
    if inferred > 0.0 {
        inferred
    } else {
        reported.first().copied().unwrap_or(0.0)
    }
}

/// `(should_fall_back, message)`: the usage read has no usable plan data, so the REST endpoints stand in.
pub fn should_use_request_based_fallback(facts: &PlanFacts, plan_name: Option<&str>) -> (bool, &'static str) {
    if !facts.is_enabled {
        return (false, "");
    }
    let plan = plan_name.map(|p| p.trim().to_lowercase()).unwrap_or_default();
    let blind = facts.plan_usage_unusable() && facts.total_percent_used.is_none();
    if blind && plan == "enterprise" {
        return (true, "Enterprise usage data unavailable. Try again later.");
    }
    if blind && plan == "team" {
        return (true, "Team request-based usage data unavailable. Try again later.");
    }
    if (blind && plan.is_empty() && plan_name.is_none()) || (facts.is_team_by_shape() && facts.limit.is_none() && facts.has_plan_usage && !facts.has_model_pools) {
        return (true, "Cursor request-based usage data unavailable. Try again later.");
    }
    (false, "")
}

/// A negative `customerBalance` is credit the customer holds, in cents.
pub fn stripe_balance_cents(body: Option<&Obj>) -> f64 {
    body.and_then(|b| num(b, "customerBalance")).filter(|b| *b < 0.0).map_or(0.0, f64::abs)
}

fn append_credits(grants: Option<&Obj>, stripe_cents: f64, lines: &mut Vec<MetricLine>) {
    let has_grants = grants.and_then(|g| g.get("hasCreditGrants")).and_then(Value::as_bool) == Some(true);
    let grant = |key: &str| if has_grants { grants.and_then(|g| num(g, key)).unwrap_or(0.0) } else { 0.0 };
    let (total, used) = (grant("totalCents"), grant("usedCents"));
    let valid = has_grants && total > 0.0;
    let combined = if valid { total } else { 0.0 } + stripe_cents;
    let remaining = (combined - if valid { used } else { 0.0 }).max(0.0);
    if grants.is_some() || combined > 0.0 {
        lines.push(dollar_value("Credits", dollars(remaining)));
    }
}

/// The dashboard's Credits card. Cursor leaves out a zero balance.
fn append_unified_credits(balance: Option<&Obj>, lines: &mut Vec<MetricLine>) {
    if let Some(balance) = balance {
        lines.push(dollar_value("Credits", dollars(num(balance, "usdRemainingCents").unwrap_or(0.0).max(0.0))));
    }
}

/// The dashboard's Additional usage card: this month's spend past the weekly included pool, paid from
/// credits. It is off when Cursor says so or the limit is $0, and has no bar when there is no limit.
fn append_additional_usage(status: Option<&Obj>, lines: &mut Vec<MetricLine>) {
    let Some(status) = status else { return };
    if text(status, "availability").unwrap_or("available") != "available" {
        return;
    }
    let Some(monthly) = obj(status, "monthlyUsage") else { return };
    if monthly.get("additionalUsageOff").and_then(Value::as_bool) == Some(true) {
        return;
    }
    let limit = num(monthly, "limitCents");
    if limit == Some(0.0) {
        return;
    }
    let used = num(monthly, "usedCents").unwrap_or(0.0).max(0.0);
    let Some(limit) = limit.filter(|l| *l > 0.0) else {
        lines.push(dollar_value("On-demand", dollars(used)));
        return;
    };
    let resets_at = iso_date(monthly, "resetsAt");
    let period = match (iso_date(monthly, "periodStart"), resets_at) {
        (Some(start), Some(reset)) if reset > start => (reset - start).num_milliseconds(),
        _ => MONTH_MS,
    };
    lines.push(dollar_progress("On-demand", used, limit, resets_at, period));
}

pub fn membership_type(summary: Option<&Obj>) -> Option<String> {
    normalized(summary.and_then(|s| text(s, "membershipType")))
}

pub fn is_org_membership(membership: &str) -> bool {
    ORG_MEMBERSHIPS.contains(&membership)
}

fn normalized(value: Option<&str>) -> Option<String> {
    value.map(|v| v.trim().to_lowercase()).filter(|v| !v.is_empty())
}

/// Badge / display plan. An org billing membership beats GetPlanInfo when that is missing or still says Pro.
pub fn resolved_plan_name(plan_info_name: Option<&str>, membership: Option<&str>) -> Option<String> {
    let membership = normalized(membership);
    let plan_info = plan_info_name.map(str::trim).filter(|p| !p.is_empty());
    if let Some(m) = membership.as_deref().filter(|m| is_org_membership(m)) {
        if plan_info.is_none_or(|p| ["pro", "pro plan"].contains(&p.to_lowercase().as_str())) {
            return Some(m.to_string());
        }
    }
    plan_info.map(str::to_string).or(membership)
}

pub fn resolved_plan_label(plan_info_name: Option<&str>, membership: Option<&str>) -> Option<String> {
    plan_label(resolved_plan_name(plan_info_name, membership).as_deref())
}

/// Words split on whitespace, each with its first letter upper-cased.
fn plan_label(value: Option<&str>) -> Option<String> {
    let words: Vec<String> = value?
        .split_whitespace()
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
        })
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}
