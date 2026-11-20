//! Amp usage mapping. Pure functions.
//!
//! `userDisplayBalanceInfo` answers with the text `amp usage` prints, one line per allowance:
//!
//! ```text
//! Signed in as amp@example.com (example)
//! Amp Megawatt Tier: agent usage $19.98 of $20.00 remaining (100%), orb usage 737.6h of 750h a1.small
//!   orb hours remaining (98%) - period 2026-11-09 to 2026-12-09, ends in 29 days
//! Amp Free: 61% remaining today (resets daily)
//! Individual credits: $57.45 remaining (set up auto-reload to avoid running out) - https://ampcode.com/settings
//! ```
//!
//! Parsed by hand: the grammar is a handful of fixed phrases, not worth a `regex` dependency.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;

use crate::models::{MetricLine, MetricValue, ProgressFormat};
use crate::provider::ProviderError;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

pub struct Usage {
    pub plan: Option<String>,
    pub lines: Vec<MetricLine>,
}

/// The display text, with Markdown bold markers removed.
fn display_text(body: &[u8]) -> Option<String> {
    let json: Value = serde_json::from_slice(body).ok()?;
    Some(json.get("result")?.get("displayText")?.as_str()?.replace("**", ""))
}

/// The address after "Signed in as", which names the account.
pub fn email(body: &[u8]) -> Option<String> {
    let text = display_text(body)?;
    let rest = text.split_once("Signed in as ")?.1;
    let end = rest.find(|c: char| c.is_whitespace() || c == '(').unwrap_or(rest.len());
    (end > 0).then(|| rest[..end].to_string())
}

/// Tier usage, Orb hours, Amp Free and Individual credits, each only when the text states it.
pub fn map(body: &[u8]) -> Result<Usage, ProviderError> {
    let text = display_text(body).ok_or(ProviderError::InvalidResponse)?;
    let mut plan = None;
    let mut lines = vec![];

    if let Some(tier) = text.lines().find_map(tier) {
        plan = Some(tier.plan);
        let period = billing_period(&tier.rest);
        let (resets_at, period_ms) = (period.map(|p| p.1), period.map(|p| (p.1 - p.0).num_milliseconds()));
        lines.push(progress("Agent Usage", round(tier.limit - tier.left, 100.0), tier.limit, ProgressFormat::Dollars, resets_at, period_ms));
        if let Some((left, limit)) = orb(&tier.rest) {
            lines.push(progress("Orb Hours", round(limit - left, 10.0), limit, ProgressFormat::Count("hours".into()), resets_at, period_ms));
        }
    }

    if let Some(used) = free_percent_used(&text) {
        lines.push(progress("Amp Free", used, 100.0, ProgressFormat::Percent, None, Some(DAY_MS)));
        plan.get_or_insert_with(|| "Free".into());
    }

    if let Some(left) = text.split_once("Individual credits: $").and_then(|(_, r)| number_then(r, " remaining")) {
        lines.push(MetricLine::Values { label: "Individual Credits".into(), values: vec![MetricValue { number: left, currency_code: Some("usd".into()), label: None }] });
    }

    if lines.is_empty() {
        return Err(ProviderError::InvalidResponse);
    }
    Ok(Usage { plan, lines })
}

fn round(value: f64, scale: f64) -> f64 {
    (value * scale).round() / scale
}

fn progress(label: &str, used: f64, limit: f64, format: ProgressFormat, resets_at: Option<DateTime<Utc>>, period_ms: Option<i64>) -> MetricLine {
    MetricLine::Progress { label: label.into(), used, limit, format, resets_at, period_ms }
}

/// A leading run of digits, dots and commas, as a number, plus what follows it.
fn number(s: &str) -> Option<(f64, &str)> {
    let end = s.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(s.len());
    Some((s[..end].replace(',', "").parse().ok()?, &s[end..]))
}

/// A number directly followed by `suffix`.
fn number_then(s: &str, suffix: &str) -> Option<f64> {
    let (n, rest) = number(s)?;
    rest.starts_with(suffix).then_some(n)
}

struct Tier {
    plan: String,
    left: f64,
    limit: f64,
    rest: String,
}

/// `Amp <plan> Tier: agent usage $<left> of $<limit> remaining<rest>` on one line; the limit is positive.
fn tier(line: &str) -> Option<Tier> {
    let (head, tail) = line.split_once(" Tier: agent usage $")?;
    let plan = head.split_once("Amp ")?.1;
    let (left, tail) = number(tail)?;
    let (limit, tail) = number(tail.strip_prefix(" of $")?)?;
    let rest = tail.strip_prefix(" remaining")?;
    (!plan.is_empty() && limit > 0.0).then(|| Tier { plan: plan.to_string(), left, limit, rest: rest.to_string() })
}

/// `orb usage <left>h of <limit>h`, with a positive limit.
fn orb(rest: &str) -> Option<(f64, f64)> {
    let (left, tail) = number(rest.split_once("orb usage ")?.1)?;
    let (limit, tail) = number(tail.strip_prefix("h of ")?)?;
    (tail.starts_with('h') && limit > 0.0).then_some((left, limit))
}

/// Amp Free either in dollars (`$left/$limit remaining`) or as a percent (`N% remaining`); returns the share used.
fn free_percent_used(text: &str) -> Option<f64> {
    let rest = text.split_once("Amp Free: ")?.1;
    let dollars = || {
        let (left, tail) = number(rest.strip_prefix('$')?)?;
        let (limit, tail) = number(tail.trim_start().strip_prefix('/')?.trim_start().strip_prefix('$')?)?;
        (tail.starts_with(" remaining") && limit > 0.0).then(|| (limit - left) / limit * 100.0)
    };
    dollars().or_else(|| {
        let (left, tail) = number(rest)?;
        tail.starts_with("% remaining").then_some(100.0 - left)
    })
}

/// `period YYYY-MM-DD to YYYY-MM-DD`: whole days read as UTC midnights, the end after the start.
fn billing_period(rest: &str) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let tail = rest.split_once("period ")?.1;
    let day = |s: &str| NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok().map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc());
    let (start, end) = (day(tail)?, day(tail.get(14..)?)?);
    (tail.get(10..14)? == " to " && end > start).then_some((start, end))
}
