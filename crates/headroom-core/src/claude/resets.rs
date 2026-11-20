//! Claude reset grants (`cedar_ember`): their status, claiming one, and how often they are read.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::mapper::reset_date;
use crate::claim::{ResetItem, ResetRef};
use crate::models::{MetricLine, MetricValue};
use crate::openrouter::number;

pub const LINE_LABEL: &str = "Rate Limit Resets";
const GRANTS_INTERVAL_SECS: i64 = 60 * 60;
const FORCED_GRANTS_MIN_AGE_SECS: i64 = 15 * 60;

/// Anthropic rate-limits the grants read hard, so it goes out at most hourly, or every 15 minutes
/// when the user forces a refresh. The caller persists `last_checked`.
pub fn grants_due(last_checked: Option<DateTime<Utc>>, now: DateTime<Utc>, forced: bool) -> bool {
    let age = if forced { FORCED_GRANTS_MIN_AGE_SECS } else { GRANTS_INTERVAL_SECS };
    last_checked.is_none_or(|at| now - at >= Duration::seconds(age))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResetKind {
    Session,
    Weekly,
    Fable,
    Full,
}

impl ResetKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::Session => "Session Reset",
            Self::Weekly => "Weekly Reset",
            Self::Fable => "Fable Reset",
            Self::Full => "Full Reset",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Grant {
    pub id: String,
    pub resets_left: i64,
    pub ends_at: Option<DateTime<Utc>>,
    pub clears: Vec<String>,
    pub grant_type: Option<String>,
    pub gives_back_weekly: bool,
    pub usable_now: bool,
    pub paused: bool,
}

impl Grant {
    /// A grant that gives back the week's Fable use is a Fable reset, then the type, then the windows it clears.
    pub fn kind(&self) -> ResetKind {
        if self.gives_back_weekly {
            return ResetKind::Fable;
        }
        match self.grant_type.as_deref() {
            Some("session") => return ResetKind::Session,
            Some("weekly") => return ResetKind::Weekly,
            Some("full") => return ResetKind::Full,
            _ => {}
        }
        let session = self.clears.iter().any(|c| c == "five_hour");
        let other = self.clears.iter().any(|c| c != "five_hour");
        match (session, other) {
            (true, false) => ResetKind::Session,
            (false, true) => ResetKind::Weekly,
            _ => ResetKind::Full,
        }
    }

    /// `None` when a window is unrecognized, so the wording never understates what is refilled.
    pub fn confirm_detail(&self) -> Option<String> {
        let names = self
            .clears
            .iter()
            .map(|w| match w.as_str() {
                "five_hour" => Some("session"),
                "seven_day" => Some("weekly"),
                "seven_day_overage_included" => Some("Fable"),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let (last, rest) = names.split_last()?;
        let mut detail = if rest.is_empty() {
            format!("Refills your {last} limit now")
        } else {
            format!("Refills your {} and {last} limits now", rest.join(", "))
        };
        if self.gives_back_weekly && !self.clears.iter().any(|c| c == "seven_day") {
            detail.push_str(" and gives back what Fable used of your weekly limit");
        }
        Some(detail + ". This can't be undone.")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResetGrantStatus {
    pub eligible: bool,
    pub grants: Vec<Grant>,
    pub next_grant_id: Option<String>,
}

impl ResetGrantStatus {
    /// `None` for anything but an object, or a block with a malformed grant.
    pub fn parse(value: Option<&Value>) -> Option<Self> {
        let object = value?.as_object()?;
        let mut grants = Vec::new();
        for entry in object.get("grants").and_then(Value::as_array).into_iter().flatten() {
            let grant = entry.as_object()?;
            let id = grant.get("id")?.as_str().filter(|s| !s.is_empty())?;
            let left = number(grant.get("resets_left"))?.floor();
            if left.abs() >= 9e15 {
                return None;
            }
            let flag = |key: &str| grant.get(key).and_then(Value::as_bool) == Some(true);
            let text = |key: &str| grant.get(key).and_then(Value::as_str).map(String::from);
            let clears = grant
                .get("clears")
                .and_then(Value::as_array)
                .and_then(|a| a.iter().map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            grants.push(Grant {
                id: id.into(),
                resets_left: left.max(0.0) as i64,
                ends_at: reset_date(grant.get("ends_at")),
                clears,
                grant_type: text("type"),
                gives_back_weekly: flag("gives_back_weekly"),
                usable_now: flag("usable_now"),
                paused: flag("paused"),
            });
        }
        Some(Self {
            eligible: object.get("eligible").and_then(Value::as_bool) == Some(true),
            grants,
            next_grant_id: object.get("next_grant_id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from),
        })
    }

    /// The grants the row counts: eligible, with a reset left, and not yet expired.
    pub fn available(&self, now: DateTime<Utc>) -> Vec<&Grant> {
        if !self.eligible {
            return vec![];
        }
        self.grants.iter().filter(|g| g.resets_left >= 1 && g.ends_at.is_none_or(|e| e > now)).collect()
    }

    /// The grant a claim names, while the last check still lists a reset left on it.
    pub fn claimable(&self, grant_id: &str) -> Option<&Grant> {
        self.eligible.then(|| self.grants.iter().find(|g| g.id == grant_id && g.resets_left > 0)).flatten()
    }

    /// One item per reset left, each claiming its own grant; Use only where Anthropic says the grant is usable now.
    pub fn items(&self, now: DateTime<Utc>) -> Vec<ResetItem> {
        let mut items: Vec<_> = self
            .available(now)
            .into_iter()
            .filter_map(|g| {
                let item = ResetItem {
                    kind: g.kind(),
                    expires_at: g.ends_at?,
                    claim: ResetRef::ClaudeGrant(g.id.clone()),
                    usable: g.usable_now && !g.paused,
                    confirm_detail: g.confirm_detail(),
                };
                Some(std::iter::repeat_n(item, g.resets_left as usize))
            })
            .flatten()
            .collect();
        items.sort_by(ResetItem::timeline_order);
        items
    }

    /// The kind of the grant Anthropic wants used next.
    pub fn next_kind(&self) -> Option<ResetKind> {
        let id = self.next_grant_id.as_deref()?;
        self.grants.iter().find(|g| g.id == id).map(Grant::kind)
    }

    /// One expiry per reset left, sorted. Grants without a deadline add none.
    pub fn expiries(&self, now: DateTime<Utc>) -> Vec<DateTime<Utc>> {
        let mut out: Vec<_> = self
            .available(now)
            .into_iter()
            .flat_map(|g| g.ends_at.into_iter().flat_map(move |e| std::iter::repeat_n(e, g.resets_left as usize)))
            .collect();
        out.sort();
        out
    }

    /// "Rate Limit Resets": the count of resets available. Expiries come from `expiries`.
    pub fn line(&self, now: DateTime<Utc>) -> MetricLine {
        let count: i64 = self.available(now).iter().map(|g| g.resets_left).sum();
        MetricLine::Values { label: LINE_LABEL.into(), values: vec![MetricValue { number: count as f64, currency_code: None, label: None }] }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimFailure {
    SignInExpired,
    /// The token's scopes don't cover resets: sign in again to widen them.
    NotPermitted,
    Refused,
    /// The request never got an answer.
    Connection,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClaimOutcome {
    Success,
    NothingToReset,
    NoCredit,
    Unconfirmed,
    UnavailableNow { until: Option<DateTime<Utc>> },
    /// Another reset has to be used first; `next` is its kind when known.
    NotNextReset { next: Option<ResetKind> },
    Failed(ClaimFailure),
}

/// `rate_limited`: the response was a 429 or a 403 carrying `retry-after`/`x-ratelimit-remaining: 0`.
pub fn claim_outcome(status: u16, rate_limited: bool, body: &[u8]) -> ClaimOutcome {
    // A token whose scopes don't cover the endpoint gets a 403 "... does not meet scope requirement ...".
    if status == 403 && String::from_utf8_lossy(body).contains("scope requirement") {
        return ClaimOutcome::Failed(ClaimFailure::NotPermitted);
    }
    if status == 401 || (status == 403 && !rate_limited) {
        return ClaimOutcome::Failed(ClaimFailure::SignInExpired);
    }
    let refused = ClaimOutcome::Failed(ClaimFailure::Refused);
    let json = serde_json::from_slice::<Value>(body).ok();
    let (true, Some(Value::Object(body))) = ((200..300).contains(&status), json) else { return refused };
    let Some(result) = body.get("result").and_then(Value::as_str) else { return refused };
    let until = reset_date(body.get("cooldown_until"));
    let outcome = |code: &str| match code {
        "reset" | "already_used" => Some(ClaimOutcome::Success),
        "not_limited" => Some(ClaimOutcome::NothingToReset),
        "ineligible" | "no_grant" | "unknown_grant" => Some(ClaimOutcome::NoCredit),
        "not_next_grant" => Some(ClaimOutcome::NotNextReset { next: None }),
        "reset_unconfirmed" | "stamp_indeterminate" => Some(ClaimOutcome::Unconfirmed),
        "cooldown" | "paused" | "low_priority_active" => Some(ClaimOutcome::UnavailableNow { until }),
        _ => None,
    };
    let code = if result == "unavailable" { body.get("reason").and_then(Value::as_str) } else { Some(result) };
    code.and_then(outcome).unwrap_or(refused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().into()
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000, 0).unwrap()
    }

    fn status(v: Value) -> ResetGrantStatus {
        ResetGrantStatus::parse(Some(&v["cedar_ember"])).unwrap()
    }

    fn grants(list: Vec<Value>) -> ResetGrantStatus {
        status(json!({"cedar_ember": {"eligible": true, "grants": list}}))
    }

    fn grant(fields: Value) -> Value {
        let mut base = json!({"id":"g","resets_left":1,"ends_at":"2026-12-01T00:00:00+00:00","usable_now":true});
        base.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        base
    }

    #[test]
    fn cadence_is_hourly_or_fifteen_minutes_when_forced() {
        let n = now();
        assert!(grants_due(None, n, false));
        assert!(!grants_due(Some(n - Duration::minutes(59)), n, false));
        assert!(grants_due(Some(n - Duration::minutes(60)), n, false));
        assert!(!grants_due(Some(n - Duration::minutes(14)), n, true));
        assert!(grants_due(Some(n - Duration::minutes(15)), n, true));
    }

    #[test]
    fn line_counts_each_remaining_reset_with_its_deadline() {
        let s = status(json!({"cedar_ember": {"eligible": true, "at_limit": false, "grants": [
            {"id":"a","resets_total":1,"resets_left":1,"ends_at":"2026-10-22T16:00:00+00:00","clears":["five_hour","seven_day"],"paused":false,"usable_now":true},
            {"id":"other","resets_left":2,"ends_at":"2026-10-01T00:00:00+00:00"},
            {"id":"spent","resets_left":0,"ends_at":"2026-10-05T00:00:00+00:00"},
            {"id":"lapsed","resets_left":1,"ends_at":"2026-09-01T00:00:00+00:00"}],
            "next_grant_id":"a"}}));
        let n = at("2026-09-21T00:00:00Z");
        let MetricLine::Values { label, values, .. } = s.line(n) else { panic!() };
        assert_eq!((label.as_str(), values[0].number), (LINE_LABEL, 3.0), "spent and lapsed grants are not counted");
        assert_eq!(s.expiries(n), vec![at("2026-10-01T00:00:00Z"), at("2026-10-01T00:00:00Z"), at("2026-10-22T16:00:00Z")]);
        assert_eq!(s.available(at("2026-10-02T00:00:00Z")).len(), 1, "a grant leaves the row once it expires");
    }

    #[test]
    fn grant_without_deadline_counts_with_no_expiry_and_ineligible_counts_nothing() {
        let s = grants(vec![json!({"id":"g","resets_left":2})]);
        assert!(s.expiries(now()).is_empty());
        let MetricLine::Values { values, .. } = s.line(now()) else { panic!() };
        assert_eq!(values[0].number, 2.0);
        let ineligible = status(json!({"cedar_ember": {"eligible": false, "grants": [{"id":"g","resets_left":2}]}}));
        assert!(ineligible.available(now()).is_empty());
    }

    #[test]
    fn kind_follows_claude_code() {
        let cases = [
            (json!({"type":"full","gives_back_weekly":true,"clears":["seven_day_overage_included"]}), ResetKind::Fable),
            (json!({"type":"weekly","clears":["seven_day","seven_day_overage_included"]}), ResetKind::Weekly),
            (json!({"type":"session","clears":["five_hour"]}), ResetKind::Session),
            (json!({"type":"full","clears":["five_hour","seven_day"]}), ResetKind::Full),
            (json!({"clears":["five_hour"]}), ResetKind::Session),
            (json!({"clears":["seven_day"]}), ResetKind::Weekly),
            (json!({"clears":["five_hour","seven_day"]}), ResetKind::Full),
            (json!({"clears":[]}), ResetKind::Full),
            (json!({"label":"Fable reset","type":"weekly","gives_back_weekly":false,"clears":["seven_day"]}), ResetKind::Weekly),
        ];
        for (fields, kind) in cases {
            assert_eq!(grants(vec![grant(fields.clone())]).grants[0].kind(), kind, "{fields}");
        }
    }

    #[test]
    fn confirmation_names_what_is_refilled() {
        let weekly = grants(vec![grant(json!({"type":"weekly","clears":["seven_day","seven_day_overage_included"]}))]);
        assert_eq!(weekly.grants[0].confirm_detail().unwrap(), "Refills your weekly and Fable limits now. This can't be undone.");
        let fable = grants(vec![grant(json!({"type":"full","gives_back_weekly":true,"clears":["seven_day_overage_included"]}))]);
        assert_eq!(
            fable.grants[0].confirm_detail().unwrap(),
            "Refills your Fable limit now and gives back what Fable used of your weekly limit. This can't be undone."
        );
        let team = grants(vec![grant(json!({"type":"full","gives_back_weekly":true,"clears":["five_hour","seven_day_overage_included"]}))]);
        assert_eq!(
            team.grants[0].confirm_detail().unwrap(),
            "Refills your session and Fable limits now and gives back what Fable used of your weekly limit. This can't be undone."
        );
        assert!(grants(vec![grant(json!({"clears":["five_hour","some_new_window"]}))]).grants[0].confirm_detail().is_none());
    }

    #[test]
    fn use_flags_and_claimable() {
        let held = json!({"id":"held","resets_left":1,"usable_now":false});
        let paused = json!({"id":"paused","resets_left":1,"usable_now":true,"paused":true});
        let s = grants(vec![held, paused, grant(json!({"id":"live"})), json!({"id":"empty","resets_left":0})]);
        let usable: Vec<bool> = s.grants.iter().map(|g| g.usable_now && !g.paused).collect();
        assert_eq!(usable, [false, false, true, false]);
        assert!(s.claimable("live").is_some());
        assert!(s.claimable("empty").is_none() && s.claimable("nope").is_none());
    }

    #[test]
    fn items_repeat_per_reset_left_and_follow_usable_and_paused() {
        let s = grants(vec![
            grant(json!({"id":"a","resets_left":2,"type":"session","clears":["five_hour"]})),
            grant(json!({"id":"b","ends_at":"2026-11-01T00:00:00+00:00","usable_now":false,"type":"weekly"})),
            grant(json!({"id":"c","paused":true})),
            grant(json!({"id":"d","ends_at":null})),
            json!({"id":"gone","resets_left":0,"ends_at":"2026-12-01T00:00:00+00:00"}),
        ]);
        let items = s.items(at("2026-10-01T00:00:00Z"));
        let view: Vec<_> = items.iter().map(|i| (i.kind, i.usable, i.claim.clone())).collect();
        assert_eq!(
            view,
            [
                (ResetKind::Weekly, false, ResetRef::ClaudeGrant("b".into())),
                (ResetKind::Session, true, ResetRef::ClaudeGrant("a".into())),
                (ResetKind::Session, true, ResetRef::ClaudeGrant("a".into())),
                (ResetKind::Full, false, ResetRef::ClaudeGrant("c".into())),
            ]
        );
        assert_eq!(items[1].confirm_detail.as_deref(), Some("Refills your session limit now. This can't be undone."));
        assert!(grants(vec![]).items(now()).is_empty());
    }

    #[test]
    fn a_malformed_grant_drops_the_whole_block() {
        for grants in [json!([{"resets_left":1}]), json!([{"id":"g"}]), json!(["g"]), json!([{"id":"g","resets_left":1e300}])] {
            assert!(ResetGrantStatus::parse(Some(&json!({"eligible":true,"grants":grants}))).is_none());
        }
        assert!(ResetGrantStatus::parse(Some(&json!({"eligible":true,"grants":[{"id":"g","resets_left":1}]}))).is_some());
        assert!(ResetGrantStatus::parse(Some(&Value::Null)).is_none());
    }

    fn outcome(body: &str, status: u16) -> ClaimOutcome {
        claim_outcome(status, false, body.as_bytes())
    }

    #[test]
    fn claim_outcome_table() {
        use ClaimFailure::*;
        use ClaimOutcome::*;
        let cooldown = at("2026-10-18T13:00:00Z");
        assert_eq!(outcome(r#"{"result":"reset","resets_left":0}"#, 200), Success);
        assert_eq!(outcome(r#"{"result":"already_used"}"#, 200), Success);
        assert_eq!(outcome(r#"{"result":"not_limited"}"#, 200), NothingToReset);
        assert_eq!(outcome(r#"{"result":"ineligible","reason":"tier"}"#, 200), NoCredit);
        for reason in ["no_grant", "unknown_grant"] {
            assert_eq!(outcome(&format!(r#"{{"result":"unavailable","reason":"{reason}"}}"#), 200), NoCredit);
        }
        assert_eq!(outcome(r#"{"result":"unavailable","reason":"not_next_grant"}"#, 200), NotNextReset { next: None });
        assert_eq!(outcome(r#"{"result":"reset_unconfirmed"}"#, 200), Unconfirmed);
        assert_eq!(outcome(r#"{"result":"unavailable","reason":"stamp_indeterminate"}"#, 200), Unconfirmed);
        assert_eq!(outcome(r#"{"result":"cooldown","cooldown_until":"2026-10-18T13:00:00Z"}"#, 200), UnavailableNow { until: Some(cooldown) });
        assert_eq!(outcome(r#"{"result":"unavailable","reason":"paused"}"#, 200), UnavailableNow { until: None });
        assert_eq!(outcome(r#"{"result":"unavailable","reason":"low_priority_active"}"#, 200), UnavailableNow { until: None });
        for refused in [r#"{"result":"unavailable","reason":"not_authorized"}"#, r#"{"result":"something_new"}"#, "not json"] {
            assert_eq!(outcome(refused, 200), Failed(Refused), "{refused}");
        }
        assert_eq!(outcome(r#"{"result":"reset"}"#, 429), Failed(Refused));
        assert_eq!(outcome(r#"{"result":"reset"}"#, 500), Failed(Refused));
        assert_eq!(outcome("", 401), Failed(SignInExpired));
        assert_eq!(outcome("", 403), Failed(SignInExpired));
        assert_eq!(claim_outcome(403, true, b""), Failed(Refused));
        let scope = r#"{"type":"error","error":{"type":"permission_error","message":"OAuth token does not meet scope requirement user:inference"}}"#;
        assert_eq!(outcome(scope, 403), Failed(NotPermitted));
    }
}
