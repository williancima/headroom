//! The `headroom.limits.v1` JSON (docs/cli.md).

use crate::models::{iso, Account, MetricLine, ProgressFormat, ProviderKind, ProviderSnapshot};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

pub const SCHEMA: &str = "headroom.limits.v1";

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Consumption,
    Balance,
}

/// Which line shapes a resource reads.
#[derive(Clone, Copy, PartialEq)]
enum Source {
    Progress,
    /// A values line; `count_only` reads the unitless count (no currency code) instead of the first value.
    Value { count_only: bool },
    /// Progress, or the first value of a values line.
    Either,
}

struct Desc {
    label: &'static str,
    key: &'static str,
    kind: Kind,
    unit: &'static str,
    source: Source,
}

const fn d(label: &'static str, key: &'static str, kind: Kind, unit: &'static str, source: Source) -> Desc {
    Desc { label, key, kind, unit, source }
}

const fn pct(label: &'static str, key: &'static str) -> Desc {
    d(label, key, Kind::Consumption, "percent", Source::Progress)
}

const RESETS: Desc = d("Rate Limit Resets", "rateLimitResets", Kind::Balance, "resets", Source::Value { count_only: true });
const FIRST: Source = Source::Value { count_only: false };

/// The export keys of `docs/cli.md`, by metric line label.
fn descriptors(kind: ProviderKind) -> Vec<Desc> {
    use Kind::{Balance, Consumption};
    match kind {
        ProviderKind::Openrouter => vec![
            d("Balance", "balance", Balance, "usd", FIRST),
            d("Key Limit", "keyLimit", Consumption, "usd", Source::Progress),
        ],
        ProviderKind::Claude => vec![
            pct("Session", "session"),
            pct("Weekly", "weekly"),
            pct("Sonnet", "sonnet"),
            pct("Fable", "fable"),
            d("Extra usage spent", "extraUsage", Consumption, "usd", Source::Either),
            RESETS,
        ],
        ProviderKind::Codex => vec![
            pct("Session", "session"),
            pct("Weekly", "weekly"),
            pct("Spark", "spark"),
            pct("Spark Weekly", "sparkWeekly"),
            d("Credits", "credits", Balance, "credits", Source::Value { count_only: true }),
            RESETS,
        ],
        ProviderKind::Cursor => vec![
            pct("Total usage", "totalUsage"),
            pct("Cursor Models", "autoUsage"),
            pct("Other Models", "apiUsage"),
            d("On-demand", "onDemand", Consumption, "usd", Source::Either),
            pct("Grok Bot usage", "grokBot"),
            d("Requests", "requests", Consumption, "requests", Source::Progress),
            d("Credits", "credits", Balance, "usd", FIRST),
        ],
        ProviderKind::Antigravity => vec![
            pct("Session", "geminiSession"),
            pct("Weekly", "geminiWeekly"),
            pct("Argon Weekly", "argonWeekly"),
            pct("Claude", "nonGeminiSession"),
            pct("Claude Weekly", "nonGeminiWeekly"),
        ],
        ProviderKind::Devin => vec![
            pct("Daily quota", "daily"),
            pct("Weekly quota", "weekly"),
            d("Extra usage balance", "extraUsageBalance", Balance, "usd", FIRST),
        ],
        _ => vec![],
    }
}

pub fn encode(accounts: &[Account], snapshots: &HashMap<String, ProviderSnapshot>, generated_at: DateTime<Utc>, refresh_minutes: i64) -> Value {
    let mut providers = Map::new();
    for account in accounts {
        let card_id = account.card_id();
        let Some(snapshot) = snapshots.get(&card_id) else { continue };
        let expires_at = snapshot.refreshed_at + Duration::minutes(2 * refresh_minutes);
        let mut resources = Map::new();
        for desc in &descriptors(account.kind) {
            if let Some(mut resource) = snapshot.line(desc.label).and_then(|line| resource(desc, line)) {
                if desc.key == RESETS.key && !snapshot.reset_expiries.is_empty() {
                    let mut dates = snapshot.reset_expiries.clone();
                    dates.sort();
                    resource["expiresAt"] = json!(dates.into_iter().map(iso).collect::<Vec<_>>());
                }
                resources.insert(desc.key.to_string(), resource);
            }
        }
        let mut entry = json!({
            "displayName": account.full_name(),
            "plan": snapshot.plan,
            "fetchedAt": iso(snapshot.refreshed_at),
            "expiresAt": iso(expires_at),
            "stale": generated_at >= expires_at,
            "resources": resources,
        });
        if let Some(notice) = &snapshot.notice {
            entry["notice"] = json!(notice);
        }
        providers.insert(card_id, entry);
    }
    json!({ "schema": SCHEMA, "generatedAt": iso(generated_at), "providers": providers })
}

/// The amount of a `balance` resource when `line` is a values line for one.
pub fn balance_amount(kind: ProviderKind, line: &MetricLine) -> Option<f64> {
    if !matches!(line, MetricLine::Values { .. }) {
        return None;
    }
    let desc = descriptors(kind).into_iter().find(|desc| desc.kind == Kind::Balance && desc.label == line.label())?;
    resource(&desc, line)?["available"].as_f64()
}

fn resource(desc: &Desc, line: &MetricLine) -> Option<Value> {
    let (kind, default_unit) = (desc.kind, desc.unit);
    let kind_name = if kind == Kind::Balance { "balance" } else { "consumption" };
    match line {
        MetricLine::Progress { used, limit, format, resets_at, period_ms, .. } if !matches!(desc.source, Source::Value { .. }) => {
            let unit = match format {
                ProgressFormat::Percent => Some("percent".to_string()),
                ProgressFormat::Dollars => Some("usd".to_string()),
                ProgressFormat::Currency(code) => Some(code.to_lowercase()),
                ProgressFormat::Count(suffix) => Some(suffix.trim().to_string()).filter(|s| !s.is_empty()),
            };
            let (used, limit) = (used.max(0.0), limit.max(0.0));
            let mut value = json!({
                "kind": kind_name,
                "unit": unit.unwrap_or_else(|| default_unit.to_string()),
                "limit": limit,
                "remaining": (limit - used).max(0.0),
            });
            value[if kind == Kind::Balance { "available" } else { "used" }] = json!(used);
            if limit > 0.0 {
                value["utilization"] = json!(used / limit);
            }
            if let Some(date) = resets_at {
                value["resetsAt"] = json!(iso(*date));
            }
            if let Some(ms) = period_ms {
                value["windowSeconds"] = json!(*ms as f64 / 1000.0);
            }
            Some(value)
        }
        MetricLine::Values { values, .. } => {
            let metric = match desc.source {
                Source::Progress => return None,
                Source::Value { count_only: true } => values.iter().find(|v| v.currency_code.is_none())?,
                _ => values.first()?,
            };
            let unit = metric.currency_code.as_ref().map_or(default_unit.to_string(), |c| c.to_lowercase());
            let name = if kind == Kind::Balance { "available" } else { "used" };
            Some(json!({ "kind": kind_name, "unit": unit, name: metric.number }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::MetricValue;

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn account() -> Account {
        let mut account = Account::new(ProviderKind::Openrouter, "Work");
        account.id = "1a2b3c4d-0000-4000-8000-000000000000".parse().unwrap();
        account
    }

    fn snapshot(lines: Vec<MetricLine>) -> ProviderSnapshot {
        ProviderSnapshot { plan: Some("Pay-as-you-go".into()), lines, refreshed_at: at("2026-07-13T01:39:30Z"), reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() }
    }

    fn encode_one(lines: Vec<MetricLine>, now: &str) -> Value {
        let account = account();
        let snapshots = HashMap::from([(account.card_id(), snapshot(lines))]);
        encode(&[account], &snapshots, at(now), 5)
    }

    fn balance(number: f64) -> MetricLine {
        MetricLine::Values { label: "Balance".into(), values: vec![MetricValue { number, currency_code: None, label: None }] }
    }

    fn key_limit(used: f64, limit: f64, reset: Option<&str>) -> MetricLine {
        MetricLine::Progress {
            label: "Key Limit".into(),
            used,
            limit,
            format: ProgressFormat::Dollars,
            resets_at: reset.map(at),
            period_ms: reset.map(|_| 86_400_000),
        }
    }

    #[test]
    fn a_notice_is_a_top_level_field_only_when_present() {
        let account = account();
        let mut with = snapshot(vec![]);
        with.notice = Some("No personal quota.".into());
        let out = encode(std::slice::from_ref(&account), &HashMap::from([(account.card_id(), with)]), at("2026-07-13T01:40:00Z"), 5);
        assert_eq!(out["providers"][account.card_id()]["notice"], "No personal quota.");
        let out = encode_one(vec![], "2026-07-13T01:40:00Z");
        assert!(out["providers"][account.card_id()].get("notice").is_none());
    }

    #[test]
    fn encodes_the_documented_shape() {
        let out = encode_one(
            vec![balance(77.75), key_limit(25.5, 100.0, Some("2026-07-14T00:00:00Z"))],
            "2026-07-13T01:40:00Z",
        );
        let expected = json!({
            "schema": "headroom.limits.v1",
            "generatedAt": "2026-07-13T01:40:00.000Z",
            "providers": { "openrouter@1a2b3c4d": {
                "displayName": "OpenRouter · Work",
                "plan": "Pay-as-you-go",
                "fetchedAt": "2026-07-13T01:39:30.000Z",
                "expiresAt": "2026-07-13T01:49:30.000Z",
                "stale": false,
                "resources": {
                    "balance": { "kind": "balance", "unit": "usd", "available": 77.75 },
                    "keyLimit": { "kind": "consumption", "unit": "usd", "used": 25.5, "limit": 100.0,
                        "remaining": 74.5, "utilization": 0.255,
                        "resetsAt": "2026-07-14T00:00:00.000Z", "windowSeconds": 86400.0 }
                }
            }}
        });
        assert_eq!(out, expected);
    }

    #[test]
    fn keys_are_sorted_in_the_printed_text() {
        let text = serde_json::to_string(&encode_one(vec![balance(1.0)], "2026-07-13T01:40:00Z")).unwrap();
        assert!(text.find("\"generatedAt\"").unwrap() < text.find("\"providers\"").unwrap());
        assert!(text.find("\"providers\"").unwrap() < text.find("\"schema\"").unwrap());
    }

    #[test]
    fn only_resources_with_a_line_are_emitted_and_stale_flips_at_expiry() {
        let out = encode_one(vec![balance(1.0)], "2026-07-13T01:49:30Z");
        let provider = &out["providers"]["openrouter@1a2b3c4d"];
        assert_eq!(provider["stale"], true);
        assert!(provider["resources"].get("keyLimit").is_none());
    }

    #[test]
    fn stale_follows_two_refresh_intervals() {
        let account = account();
        let snapshots = HashMap::from([(account.card_id(), snapshot(vec![balance(1.0)]))]);
        let stale = |now: &str| encode(std::slice::from_ref(&account), &snapshots, at(now), 30)["providers"][account.card_id()]["stale"].clone();
        assert_eq!(stale("2026-07-13T02:38:00Z"), false);
        assert_eq!(stale("2026-07-13T02:40:00Z"), true);
    }

    #[test]
    fn negatives_clamp_and_zero_limit_has_no_utilization() {
        let out = encode_one(vec![key_limit(-5.0, 0.0, None)], "2026-07-13T01:40:00Z");
        let resource = &out["providers"]["openrouter@1a2b3c4d"]["resources"]["keyLimit"];
        assert_eq!(resource["used"], 0.0);
        assert_eq!(resource["remaining"], 0.0);
        assert!(resource.get("utilization").is_none());
        assert!(resource.get("resetsAt").is_none());
    }

    #[test]
    fn units_follow_the_progress_format() {
        for (format, unit) in [
            (ProgressFormat::Percent, "percent"),
            (ProgressFormat::Currency("EUR".into()), "eur"),
            (ProgressFormat::Count(" requests ".into()), "requests"),
            (ProgressFormat::Count("  ".into()), "usd"),
        ] {
            let line = MetricLine::Progress {
                label: "Key Limit".into(), used: 1.0, limit: 2.0, format, resets_at: None, period_ms: None,
            };
            assert_eq!(resource(&d("Key Limit", "keyLimit", Kind::Consumption, "usd", Source::Progress), &line).unwrap()["unit"], unit);
        }
    }

    #[test]
    fn values_unit_takes_the_currency_code_and_accounts_without_snapshot_are_skipped() {
        let line = MetricLine::Values {
            label: "Balance".into(),
            values: vec![MetricValue { number: 3.0, currency_code: Some("BRL".into()), label: None }],
        };
        assert_eq!(resource(&d("Balance", "balance", Kind::Balance, "usd", FIRST), &line).unwrap()["unit"], "brl");
        let out = encode(&[account()], &HashMap::new(), at("2026-07-13T01:40:00Z"), 5);
        assert_eq!(out["providers"], json!({}));
    }

    fn line_of(kind: ProviderKind, line: MetricLine, expiries: Vec<DateTime<Utc>>) -> Value {
        let mut account = Account::new(kind, "x");
        account.id = "1a2b3c4d-0000-4000-8000-000000000000".parse().unwrap();
        let mut snap = snapshot(vec![line]);
        snap.reset_expiries = expiries;
        let snapshots = HashMap::from([(account.card_id(), snap)]);
        let out = encode(&[account.clone()], &snapshots, at("2026-07-13T01:40:00Z"), 5);
        out["providers"][account.card_id()]["resources"].clone()
    }

    fn pct_line(label: &str) -> MetricLine {
        MetricLine::Progress {
            label: label.into(), used: 42.0, limit: 100.0, format: ProgressFormat::Percent,
            resets_at: Some(at("2026-07-13T06:00:00Z")), period_ms: Some(18_000_000),
        }
    }

    fn count_line(label: &str, number: f64, code: Option<&str>) -> MetricLine {
        MetricLine::Values { label: label.into(), values: vec![MetricValue { number, currency_code: code.map(Into::into), label: None }] }
    }

    #[test]
    fn claude_session_and_extra_usage_in_both_shapes() {
        let out = line_of(ProviderKind::Claude, pct_line("Session"), vec![]);
        assert_eq!(out["session"], json!({ "kind": "consumption", "unit": "percent", "used": 42.0, "limit": 100.0,
            "remaining": 58.0, "utilization": 0.42, "resetsAt": "2026-07-13T06:00:00.000Z", "windowSeconds": 18000.0 }));
        let value = line_of(ProviderKind::Claude, count_line("Extra usage spent", 2.5, Some("EUR")), vec![]);
        assert_eq!(value["extraUsage"], json!({ "kind": "consumption", "unit": "eur", "used": 2.5 }));
        let bounded = MetricLine::Progress {
            label: "Extra usage spent".into(), used: 1.0, limit: 10.0, format: ProgressFormat::Currency("USD".into()),
            resets_at: None, period_ms: None,
        };
        assert_eq!(line_of(ProviderKind::Claude, bounded, vec![])["extraUsage"]["unit"], "usd");
    }

    #[test]
    fn codex_credits_read_the_count_and_resets_list_their_expiries() {
        let credits = MetricLine::Values {
            label: "Credits".into(),
            values: vec![
                MetricValue { number: 8.0, currency_code: Some("usd".into()), label: None },
                MetricValue { number: 200.0, currency_code: None, label: None },
            ],
        };
        assert_eq!(line_of(ProviderKind::Codex, credits, vec![])["credits"], json!({ "kind": "balance", "unit": "credits", "available": 200.0 }));
        let resets = line_of(
            ProviderKind::Codex,
            count_line("Rate Limit Resets", 2.0, None),
            vec![at("2026-08-01T00:00:00Z"), at("2026-07-20T00:00:00Z")],
        );
        assert_eq!(resets["rateLimitResets"], json!({ "kind": "balance", "unit": "resets", "available": 2.0,
            "expiresAt": ["2026-07-20T00:00:00.000Z", "2026-08-01T00:00:00.000Z"] }));
    }

    #[test]
    fn cursor_requests_use_the_count_suffix_and_credits_are_dollars() {
        let requests = MetricLine::Progress {
            label: "Requests".into(), used: 10.0, limit: 500.0, format: ProgressFormat::Count("requests".into()),
            resets_at: None, period_ms: None,
        };
        assert_eq!(line_of(ProviderKind::Cursor, requests, vec![])["requests"]["unit"], "requests");
        let credits = line_of(ProviderKind::Cursor, count_line("Credits", 5.0, Some("usd")), vec![]);
        assert_eq!(credits["credits"], json!({ "kind": "balance", "unit": "usd", "available": 5.0 }));
        assert!(line_of(ProviderKind::Cursor, pct_line("Total usage"), vec![]).get("totalUsage").is_some());
    }

    #[test]
    fn antigravity_and_devin_keys() {
        assert!(line_of(ProviderKind::Antigravity, pct_line("Claude Weekly"), vec![]).get("nonGeminiWeekly").is_some());
        assert!(line_of(ProviderKind::Antigravity, pct_line("Session"), vec![]).get("geminiSession").is_some());
        assert!(line_of(ProviderKind::Devin, pct_line("Daily quota"), vec![]).get("daily").is_some());
        let extra = line_of(ProviderKind::Devin, count_line("Extra usage balance", 3.0, Some("usd")), vec![]);
        assert_eq!(extra["extraUsageBalance"], json!({ "kind": "balance", "unit": "usd", "available": 3.0 }));
    }

    #[test]
    fn a_line_of_the_wrong_shape_is_left_out() {
        assert!(line_of(ProviderKind::Claude, count_line("Session", 1.0, None), vec![]).get("session").is_none());
        assert!(line_of(ProviderKind::Codex, pct_line("Credits"), vec![]).get("credits").is_none());
    }

    #[test]
    fn full_name_hides_a_label_that_repeats_the_provider() {
        let mut account = Account::new(ProviderKind::Openrouter, "openrouter");
        assert_eq!(account.full_name(), "OpenRouter");
        account.label = String::new();
        assert_eq!(account.full_name(), "OpenRouter");
        account.label = "Work".into();
        assert_eq!(account.full_name(), "OpenRouter · Work");
    }
}
