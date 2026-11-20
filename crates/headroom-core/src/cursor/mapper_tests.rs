use serde_json::{json, Value};

use super::mapper::*;
use super::summary;
use crate::models::{MetricLine, ProgressFormat};
use crate::provider::ProviderError;

fn o(value: Value) -> Obj {
    value.as_object().unwrap().clone()
}

fn line<'a>(lines: &'a [MetricLine], label: &str) -> Option<&'a MetricLine> {
    lines.iter().find(|l| l.label() == label)
}

fn assert_percent(lines: &[MetricLine], label: &str, expected: f64) {
    match line(lines, label) {
        Some(MetricLine::Progress { used, limit, format: ProgressFormat::Percent, .. }) => {
            assert!((used - expected).abs() < 1e-6, "{label}: {used}");
            assert_eq!(*limit, 100.0);
        }
        other => panic!("missing {label}: {other:?}"),
    }
}

fn dollar_meter(lines: &[MetricLine], label: &str) -> (f64, f64) {
    match line(lines, label) {
        Some(MetricLine::Progress { used, limit, format: ProgressFormat::Dollars, .. }) => (*used, *limit),
        other => panic!("missing dollar meter {label}: {other:?}"),
    }
}

fn team(plan: Value) -> Result<Usage, ProviderError> {
    let usage = json!({"enabled": true, "planUsage": plan, "billingCycleStart": 1_770_000_000_000_i64,
        "billingCycleEnd": 1_772_592_000_000_i64, "spendLimitUsage": {"limitType": "team", "pooledUsed": 0}});
    map_usage(&o(usage), Some("Team"), None, 0.0, None)
}

#[test]
fn a_reported_standard_seat_uses_the_structured_total_not_the_legacy_twenty_dollars() {
    let mapped = team(json!({"totalSpend": 1022, "includedSpend": 1022, "remaining": 978, "limit": 2000,
        "autoPercentUsed": 2.6476190476190475, "apiPercentUsed": 11.65, "totalPercentUsed": 4.088}))
    .unwrap();
    assert_percent(&mapped.lines, "Total usage", 4.088);
    assert_percent(&mapped.lines, "Cursor Models", 2.6476190476190475);
    assert_percent(&mapped.lines, "Other Models", 11.65);
}

#[test]
fn a_premium_seat_without_a_total_invents_none() {
    let mapped = team(json!({"limit": 2000, "totalSpend": 666, "autoPercentUsed": 0.3728571428571429, "apiPercentUsed": 2.025})).unwrap();
    assert!(line(&mapped.lines, "Total usage").is_none());
    assert_percent(&mapped.lines, "Cursor Models", 0.3728571428571429);
    assert_percent(&mapped.lines, "Other Models", 2.025);
}

#[test]
fn pools_without_a_limit_do_not_trigger_the_request_fallback() {
    let usage = o(json!({"enabled": true, "planUsage": {"autoPercentUsed": 0, "apiPercentUsed": 0}, "spendLimitUsage": {"limitType": "team"}}));
    let facts = PlanFacts::new(&usage);
    for name in [Some("Team"), Some("Enterprise"), None] {
        assert!(!should_use_request_based_fallback(&facts, name).0, "{name:?}");
    }
    assert!(!facts.should_try_generic_request_fallback());
    let mapped = map_usage(&usage, None, None, 0.0, None).unwrap();
    assert_percent(&mapped.lines, "Cursor Models", 0.0);
    assert_percent(&mapped.lines, "Other Models", 0.0);
}

#[test]
fn a_billing_cycle_too_long_for_an_int_falls_back_to_a_month() {
    let usage = o(json!({"enabled": true, "planUsage": {"autoPercentUsed": 0, "apiPercentUsed": 0}, "billingCycleStart": 0, "billingCycleEnd": 1e300}));
    let mapped = map_usage(&usage, None, None, 0.0, None).unwrap();
    assert!(matches!(line(&mapped.lines, "Cursor Models"), Some(MetricLine::Progress { period_ms: Some(MONTH_MS), .. })));
}

#[test]
fn a_legacy_pool_with_zero_placeholders_and_malformed_pools_keep_the_dollar_meter() {
    let mapped = team(json!({"limit": 500_000, "totalSpend": 234_500, "autoPercentUsed": 0, "apiPercentUsed": 0, "totalPercentUsed": 0})).unwrap();
    assert_eq!(dollar_meter(&mapped.lines, "Total usage"), (2345.0, 5000.0));
    for pools in [json!({"autoPercentUsed": 1}), json!({"autoPercentUsed": true, "apiPercentUsed": 2}), json!({"autoPercentUsed": -1, "apiPercentUsed": 2})] {
        let mut plan = pools;
        plan["limit"] = json!(500_000);
        plan["totalSpend"] = json!(234_500);
        let mapped = team(plan).unwrap();
        assert_eq!(dollar_meter(&mapped.lines, "Total usage"), (2345.0, 5000.0));
    }
}

#[test]
fn the_team_name_alone_recognizes_pools_and_keeps_extra_usage_and_credits() {
    let usage = o(json!({"enabled": true, "planUsage": {"limit": 2000, "autoPercentUsed": 0, "apiPercentUsed": 12},
        "spendLimitUsage": {"individualLimit": 5000, "individualRemaining": 4000}}));
    let mapped = map_usage(&usage, Some(" Team "), None, 1000.0, None).unwrap();
    assert!(line(&mapped.lines, "Total usage").is_none());
    assert!(line(&mapped.lines, "On-demand").is_some());
    assert!(line(&mapped.lines, "Credits").is_some());
}

#[test]
fn extra_usage_reads_its_spend_from_the_same_budget_as_its_limit() {
    let on_demand = |spend: Value| {
        let usage = o(json!({"enabled": true, "planUsage": {"totalPercentUsed": 10}, "spendLimitUsage": spend}));
        let mapped = map_usage(&usage, Some("Team"), None, 0.0, None).unwrap();
        let (used, limit) = dollar_meter(&mapped.lines, "On-demand");
        (used, limit, mapped.line_titles.first().map(|t| t.1.clone()))
    };
    let personal = on_demand(json!({"individualLimit": 10000, "individualUsed": 0, "individualRemaining": 10000,
        "pooledLimit": 50000, "pooledUsed": 12000, "pooledRemaining": 38000, "totalSpend": 12000, "limitType": "user"}));
    assert_eq!(personal, (0.0, 100.0, None));
    let pool = on_demand(json!({"pooledLimit": 50000, "pooledUsed": 12000, "pooledRemaining": 38000, "individualUsed": 3400, "limitType": "team"}));
    assert_eq!(pool, (120.0, 500.0, Some("Team Spend".into())));
    let inferred = on_demand(json!({"individualLimit": 10000, "individualRemaining": 7500, "pooledUsed": 12000, "totalSpend": 12000}));
    assert_eq!((inferred.0, inferred.1), (25.0, 100.0));
    let own_total = on_demand(json!({"individualLimit": 5000, "individualUsed": 0, "individualRemaining": 4500, "totalSpend": 1200}));
    assert_eq!(own_total.0, 12.0);
}

#[test]
fn a_current_period_usage_fills_the_tiles_in_order() {
    let usage = o(json!({"billingCycleEnd": "1764547200000", "billingCycleStart": "1761955200000", "enabled": true,
        "planUsage": {"apiPercentUsed": 12.5, "autoPercentUsed": 3.25, "limit": 2000, "remaining": 1500, "totalPercentUsed": 25},
        "spendLimitUsage": {"individualLimit": 5000, "individualRemaining": 4000, "limitType": "user"}}));
    let mapped = map_usage(&usage, Some("Pro"), Some(&o(json!({"hasCreditGrants": false}))), 0.0, None).unwrap();
    let labels: Vec<&str> = mapped.lines.iter().map(MetricLine::label).collect();
    assert_eq!(labels, ["Credits", "Total usage", "Cursor Models", "Other Models", "On-demand"]);
    let Some(MetricLine::Progress { used, resets_at, .. }) = line(&mapped.lines, "Total usage") else { panic!() };
    assert_eq!((*used, resets_at.unwrap().timestamp()), (25.0, 1_764_547_200));
    assert_eq!(dollar_meter(&mapped.lines, "On-demand"), (10.0, 50.0));
    assert_eq!(mapped.plan.as_deref(), Some("Pro"));
}

#[test]
fn unavailable_usage_errors() {
    let unavailable = |m: &str| ProviderError::Unavailable(m.into());
    assert_eq!(map_usage(&o(json!({"enabled": false, "planUsage": {}})), None, None, 0.0, None).unwrap_err(), unavailable("No active Cursor subscription."));
    assert_eq!(map_usage(&o(json!({"enabled": true})), None, None, 0.0, None).unwrap_err(), unavailable("No active Cursor subscription."));
    assert_eq!(
        map_usage(&o(json!({"enabled": true, "planUsage": {}})), None, None, 0.0, None).unwrap_err(),
        unavailable("Total usage limit missing from API response.")
    );
}

#[test]
fn credits_combine_grants_and_prepaid_balance() {
    let usage = o(json!({"enabled": true, "planUsage": {"totalPercentUsed": 1}}));
    let grants = o(json!({"hasCreditGrants": true, "totalCents": 10000, "usedCents": 4000}));
    let mapped = map_usage(&usage, None, Some(&grants), stripe_balance_cents(Some(&o(json!({"customerBalance": -2500})))), None).unwrap();
    let Some(MetricLine::Values { values, .. }) = line(&mapped.lines, "Credits") else { panic!() };
    assert_eq!((values[0].number, values[0].currency_code.as_deref()), (85.0, Some("usd")));
    assert_eq!(stripe_balance_cents(Some(&o(json!({"customerBalance": 100})))), 0.0);
}

#[test]
fn grok_bot_usage() {
    let sand = o(json!({"currentPeriodStart": "2026-10-20T00:00:00Z", "hasNonZeroIncludedLimit": true, "nextResetTimestampUtc": "2026-10-27T00:00:00Z", "usagePercent": 30}));
    let Some(MetricLine::Progress { label, used, resets_at, period_ms, .. }) = map_grok_bot_usage(&sand) else { panic!() };
    assert_eq!((label.as_str(), used, period_ms), ("Grok Bot usage", 30.0, Some(7 * 24 * 3600 * 1000)));
    assert_eq!(resets_at.unwrap().to_rfc3339(), "2026-10-27T00:00:00+00:00");
    for off in [json!({"usesPooledEnterpriseAllowance": true}), json!({"hasNonZeroIncludedLimit": false}), json!({"includedLimitZero": true}), json!({"usagePercent": -1})] {
        let mut body = sand.clone();
        body.extend(off.as_object().unwrap().clone());
        assert!(map_grok_bot_usage(&body).is_none(), "{off}");
    }
}

#[test]
fn plan_names_give_way_to_an_org_membership_only_when_missing_or_pro() {
    for plan in ["Ultra", "Business", "Enterprise", "Pro Plus"] {
        assert_eq!(resolved_plan_label(Some(plan), Some("team")).as_deref(), Some(plan));
    }
    for plan in [None, Some(""), Some("  "), Some("Pro"), Some(" pro plan ")] {
        assert_eq!(resolved_plan_label(plan, Some("enterprise")).as_deref(), Some("Enterprise"));
    }
    assert_eq!(resolved_plan_label(Some("Ultra"), Some("pro")).as_deref(), Some("Ultra"));
    assert_eq!(resolved_plan_label(None, Some("unified_ultra")).as_deref(), Some("Unified_ultra"));
}

#[test]
fn the_summary_maps_provider_returned_on_demand_dollars() {
    let body = o(json!({"membershipType": "enterprise", "individualUsage": {
        "plan": {"enabled": true, "limit": 0, "totalPercentUsed": 6.25},
        "onDemand": {"enabled": true, "used": 12_345, "limit": 200_000}}}));
    let mapped = summary::map(Some(&body), None, Some("Enterprise"), "unavailable").unwrap();
    assert_eq!(mapped.plan.as_deref(), Some("Enterprise"));
    assert_eq!(dollar_meter(&mapped.lines, "On-demand"), (123.45, 2000.0));
}

#[test]
fn the_summary_rejects_a_response_without_usable_metrics() {
    let body = o(json!({"individualUsage": {"plan": {"limit": 0}}}));
    assert_eq!(summary::map(Some(&body), None, None, "unavailable").unwrap_err(), ProviderError::Unavailable("unavailable".into()));
}

#[test]
fn request_usage_gives_total_usage_and_requests() {
    let requests = o(json!({"gpt-4": {"numRequests": 120, "maxRequestUsage": 500}, "startOfMonth": "2026-10-01T00:00:00Z"}));
    let mapped = summary::map(None, Some(&requests), None, "").unwrap();
    let labels: Vec<&str> = mapped.lines.iter().map(MetricLine::label).collect();
    assert_eq!(labels, ["Total usage", "Requests"]);
    assert!(matches!(&mapped.lines[1], MetricLine::Progress { used, limit, format: ProgressFormat::Count(s), .. } if (*used, *limit, s.as_str()) == (120.0, 500.0, "requests")));
}

#[test]
fn usable_summary_payloads() {
    assert!(summary::has_usable_summary_payload(&o(json!({"billingCycleStart": "2026-10-01T00:00:00Z", "billingCycleEnd": "2026-11-01T00:00:00Z"}))));
    assert!(summary::has_usable_summary_payload(&o(json!({"teamUsage": {"onDemand": {"enabled": true, "used": 5}}}))));
    assert!(!summary::has_usable_summary_payload(&o(json!({"individualUsage": {"onDemand": {"enabled": false, "used": 5}}}))));
}
