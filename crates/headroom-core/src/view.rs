//! Turns a `PanelModel` into the rows of the two tabs, without GTK. Times are relative to `model.now`.

use crate::claim::{when_label, ResetItem, ResetRef};
use crate::history::BreakdownView;
use crate::hub::Hub;
use crate::layout::Layout;
use crate::priority::{menu_bar_content, BurningFast, LineRef, MenuBarContent, Meter, PriorityBoard, PriorityItem, ReadStatus, Settings, Summary};
use crate::store::ProviderState;
use crate::{Account, ProviderSnapshot};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::HashMap;

mod unified;
mod usage;

pub use usage::usage_groups;

/// Everything the panel shows, rebuilt after each reading.
#[derive(Debug, Clone)]
pub struct PanelModel {
    /// In saved order.
    pub accounts: Vec<Account>,
    pub hubs: Vec<Hub>,
    pub snapshots: HashMap<String, ProviderSnapshot>,
    pub status: HashMap<String, ReadStatus>,
    pub board: PriorityBoard,
    pub layout: Layout,
    pub now: DateTime<Utc>,
    /// A reading at least this old shows "Last updated" under its account.
    pub old_after: Duration,
}

impl PanelModel {
    /// Ranks the saved readings with the user's thresholds; the one builder of the app and `headroom panel`.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        accounts: Vec<Account>,
        hubs: Vec<Hub>,
        snapshots: HashMap<String, ProviderSnapshot>,
        states: &HashMap<String, ProviderState>,
        latch: &BurningFast,
        layout: &Layout,
        settings: &Settings,
        now: DateTime<Utc>,
    ) -> Self {
        let status: HashMap<_, _> =
            accounts.iter().map(|a| (a.card_id(), read_status(snapshots.get(&a.card_id()), states.get(&a.card_id())))).collect();
        let board = PriorityBoard::make(&accounts, &snapshots, &status, &layout.pins.iter().cloned().collect(), latch, settings, now);
        Self { accounts, hubs, snapshots, status, board, layout: layout.clone(), now, old_after: Duration::minutes(settings.refresh_minutes) }
    }

    /// The top-bar text.
    pub fn label(&self) -> String {
        let settings = Settings::default();
        label_text(&menu_bar_content(&self.board, settings.menu_bar_limit, settings.always_show))
    }
}

/// What `headroom panel` prints.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PanelView {
    pub label: String,
    /// The newest reading among the accounts; `None` before anything was fetched.
    pub updated_at: Option<DateTime<Utc>>,
    pub priority: Vec<Group>,
    pub usage: Vec<Group>,
}

impl PanelView {
    pub fn new(model: &PanelModel) -> Self {
        let updated_at = model.accounts.iter().filter_map(|a| model.snapshots.get(&a.card_id())).map(|s| s.refreshed_at).max();
        Self { label: model.label(), updated_at, priority: priority_groups(model), usage: usage_groups(model) }
    }
}

/// A wait the provider asked for is not an error to show.
pub fn read_status(snapshot: Option<&ProviderSnapshot>, state: Option<&ProviderState>) -> ReadStatus {
    match state.and_then(|s| s.backoff.as_ref()).filter(|b| !b.provider_asked && !b.message.is_empty()) {
        Some(backoff) => ReadStatus::Unavailable(backoff.message.clone()),
        None if snapshot.is_some() => ReadStatus::Current,
        None => ReadStatus::Waiting,
    }
}

/// `"{short label} {value}"` per item, joined with spaces, then `+N` for the rest.
pub fn label_text(content: &MenuBarContent) -> String {
    let mut text = content.items.iter().map(|i| format!("{} {}", i.short_label, i.value)).collect::<Vec<_>>().join(" · ");
    if content.overflow > 0 && !text.is_empty() {
        text.push_str(&format!(" +{}", content.overflow));
    }
    text
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Row {
    pub title: String,
    pub subtitle: String,
    /// Bar fill, 0 to 1.
    pub meter: Option<f64>,
    pub value: Option<String>,
    /// The account a "Rate Limit Resets" or history row belongs to (a unified card's resets row: the first
    /// account's, each entry names its own); the resets row lists one entry per reset. An account's header row
    /// carries it too, with no resets or breakdown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_id: Option<String>,
    /// The provider's `ProviderKind::id()`, on the row that opens a card (an account's or a unified card's header,
    /// a Priority row); empty on the metric rows under a header.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// The metric's line id; a unified row's own id (`"{kind}.unified/{label}"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// A unified row: every account's line id for the label, for Pin and Hide.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub copy_ids: Vec<String>,
    /// Pinned; on a unified row, any copy is pinned.
    #[serde(skip_serializing_if = "is_false")]
    pub pinned: bool,
    /// A unified row: one row per account that reports the label, for the detail that opens on click.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<Row>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resets: Vec<ResetEntry>,
    /// A history row: what opens when it is clicked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<BreakdownView>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One reset of a "Rate Limit Resets" row. `claim` goes back unchanged in the `claim-reset` action.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetEntry {
    pub title: String,
    /// "Expires in 3d 4h".
    pub expires: String,
    pub usable: bool,
    pub confirm_detail: Option<String>,
    pub claim: ResetRef,
    /// The account this reset belongs to; set only on a unified card's row, whose own `card_id` is one account's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_id: Option<String>,
}

impl ResetEntry {
    pub fn new(item: &ResetItem, now: DateTime<Utc>) -> Self {
        let when = when_label(item.expires_at, now).map_or("Expiring".to_string(), |w| if w == "soon" { "Expires soon".into() } else { format!("Expires in {w}") });
        Self { title: item.kind.title().into(), expires: when, usable: item.usable, confirm_detail: item.confirm_detail.clone(), claim: item.claim.clone(), card_id: None }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Group {
    pub title: String,
    /// The provider's `ProviderKind::id()`.
    pub kind: String,
    /// Claude or ChatGPT with 2+ accounts: Unify Multiple Accounts applies.
    pub unifiable: bool,
    pub unified: bool,
    pub rows: Vec<Row>,
}

fn join(parts: impl IntoIterator<Item = Option<String>>) -> String {
    parts.into_iter().flatten().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// The provider id of the account with this card id; empty when there is none.
fn kind_of(model: &PanelModel, card_id: &str) -> String {
    model.accounts.iter().find(|a| a.card_id() == card_id).map_or(String::new(), |a| a.kind.id().into())
}

fn name(model: &PanelModel, card_id: &str) -> String {
    model.accounts.iter().find(|a| a.card_id() == card_id).map_or(card_id.to_string(), Account::full_name)
}

/// The line's meter, read from its snapshot (a pinned metric that isn't listed has no item to carry one).
fn meter_of(model: &PanelModel, line: &LineRef) -> Option<Meter> {
    let account = model.accounts.iter().find(|a| a.card_id() == line.card_id)?;
    Meter::from_line(account.kind, model.snapshots.get(&line.card_id)?.line(&line.label)?)
}

/// The callout (run out or resets in) takes the reset's place.
fn limit_row(model: &PanelModel, line: &LineRef, item: Option<&PriorityItem>) -> Row {
    let meter = item.and_then(|i| i.meter.clone()).or_else(|| meter_of(model, line));
    let callout = item
        .and_then(|i| i.run_out_text(model.now).or_else(|| i.resets_in_text(model.now)))
        .or_else(|| meter.as_ref().and_then(|m| m.reset_text(model.now)));
    Row {
        title: line.label.clone(),
        subtitle: join([Some(name(model, &line.card_id)), callout]),
        meter: meter.as_ref().map(|m| m.fill_fraction(false)),
        value: Some(meter.as_ref().map_or(line.menu_bar_value.clone(), |m| format!("{} used", m.value_text(false)))),
        kind: kind_of(model, &line.card_id),
        ..Row::default()
    }
}

fn group(title: &str, rows: Vec<Row>) -> Option<Group> {
    (!rows.is_empty()).then(|| Group { title: title.into(), kind: String::new(), unifiable: false, unified: false, rows })
}

/// Sections in order: Pinned, then by the summary, then Waiting and Used Up.
pub fn priority_groups(model: &PanelModel) -> Vec<Group> {
    let board: &PriorityBoard = &model.board;
    let items = |list: &[PriorityItem]| list.iter().map(|i| limit_row(model, &i.line, Some(i))).collect::<Vec<_>>();
    let notices = || {
        let rows = board.notices.iter().map(|n| Row { title: name(model, &n.card_id), subtitle: n.message.clone(), kind: kind_of(model, &n.card_id), ..Row::default() });
        group("Needs Attention", rows.collect())
    };
    let mut groups = vec![];
    groups.extend(group("Pinned", board.pinned.iter().map(|p| limit_row(model, &p.line, p.item.as_ref())).collect()));
    match board.summary {
        Summary::WaitingForUsage => {}
        Summary::NoReportedLimits => groups.extend(group(
            "",
            vec![Row { title: "No Reported Limits".into(), subtitle: "Open All Usage to see what each account reports.".into(), ..Row::default() }],
        )),
        Summary::Unavailable => groups.extend(notices()),
        Summary::AllClear | Summary::Listed => {
            let mut rows = items(&board.attention);
            if rows.is_empty() && board.shows_clear_row {
                rows.push(Row { title: "All Clear".into(), subtitle: "Nothing is running out.".into(), ..Row::default() });
            }
            groups.extend(group("Running Out", rows));
            groups.extend(group("Use It or Lose It", items(&board.use_it_or_lose_it)));
            groups.extend(notices());
        }
    }
    let waiting = board.waiting.iter().map(|id| Row { title: "Waiting for Usage".into(), subtitle: name(model, id), kind: kind_of(model, id), ..Row::default() });
    groups.extend(group("Waiting", waiting.collect()));
    let used_up = board.used_up.iter().map(|i| Row {
        title: i.line.label.clone(),
        subtitle: name(model, &i.line.card_id),
        value: i.used_up_note(model.now),
        kind: kind_of(model, &i.line.card_id),
        ..Row::default()
    });
    groups.extend(group("Used Up", used_up.collect()));
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::resets::LINE_LABEL;
    use crate::{MetricLine, ProviderKind};
    use chrono::Duration;
    use crate::backoff::{self, Backoff};
    use crate::priority::MenuBarItem;
    use crate::provider::ProviderError;
    use crate::{MetricValue, ProgressFormat};

    fn now() -> DateTime<Utc> {
        "2027-01-15T08:00:00Z".parse().unwrap()
    }

    fn percent(label: &str, used: f64, left_hours: i64) -> MetricLine {
        MetricLine::Progress {
            label: label.into(),
            used,
            limit: 100.0,
            format: ProgressFormat::Percent,
            resets_at: Some(now() + Duration::hours(left_hours)),
            period_ms: Some(7 * 24 * 3600 * 1000),
        }
    }

    fn snap(lines: Vec<MetricLine>, age: Duration) -> ProviderSnapshot {
        ProviderSnapshot { plan: Some("Max".into()), lines, refreshed_at: now() - age, reset_expiries: vec![], reset_items: vec![], notice: None, breakdowns: Default::default() }
    }

    fn model(accounts: Vec<Account>, snapshots: HashMap<String, ProviderSnapshot>, status: HashMap<String, ReadStatus>) -> PanelModel {
        let board = PriorityBoard::make(&accounts, &snapshots, &status, &Default::default(), &BurningFast::default(), &Settings::default(), now());
        PanelModel { accounts, hubs: vec![], snapshots, status, board, layout: Layout::default(), now: now(), old_after: Duration::minutes(5) }
    }

    fn titles(groups: &[Group]) -> Vec<&str> {
        groups.iter().map(|g| g.title.as_str()).collect()
    }

    #[test]
    fn priority_sections_follow_their_order_and_titles() {
        let a = Account::new(ProviderKind::Claude, "Work");
        let lines = vec![percent("Weekly", 85.0, 100), percent("Session", 100.0, 3), percent("Idle", 10.0, 18)];
        let m = model(vec![a.clone()], HashMap::from([(a.card_id(), snap(lines, Duration::zero()))]), HashMap::new());
        let groups = priority_groups(&m);
        assert_eq!(titles(&groups), ["Running Out", "Use It or Lose It", "Used Up"]);
        assert_eq!(groups[0].rows[0].title, "Weekly");
        assert_eq!(groups[0].rows[0].subtitle, "Claude · Work · 4d 4h");
        assert_eq!(groups[0].rows[0].value.as_deref(), Some("85% used"));
        assert_eq!(groups[0].rows[0].meter, Some(0.85));
        assert_eq!(groups[1].rows[0].subtitle, "Claude · Work · Resets in 18h");
        assert_eq!(groups[2].rows[0].value.as_deref(), Some("3h"));
    }

    #[test]
    fn a_healthy_board_shows_the_clear_row_and_waiting_accounts_wait() {
        let (a, b) = (Account::new(ProviderKind::Claude, "Work"), Account::new(ProviderKind::Codex, ""));
        let m = model(vec![a.clone()], HashMap::from([(a.card_id(), snap(vec![percent("Weekly", 10.0, 100)], Duration::zero()))]), HashMap::new());
        let groups = priority_groups(&m);
        assert_eq!((titles(&groups), groups[0].rows[0].title.as_str()), (vec!["Running Out"], "All Clear"));

        let m = model(vec![b.clone()], HashMap::new(), HashMap::new());
        let groups = priority_groups(&m);
        assert_eq!(titles(&groups), ["Waiting"]);
        assert_eq!(groups[0].rows[0].subtitle, "ChatGPT");
    }

    #[test]
    fn priority_rows_carry_the_accounts_kind_and_json_omits_an_empty_one() {
        let a = Account::new(ProviderKind::Codex, "");
        let m = model(vec![a.clone()], HashMap::from([(a.card_id(), snap(vec![percent("Weekly", 85.0, 100)], Duration::zero()))]), HashMap::new());
        let groups = priority_groups(&m);
        assert_eq!(groups[0].rows[0].kind, "codex");
        let json = serde_json::to_value(&groups[0].rows[0]).unwrap();
        assert_eq!(json["kind"], "codex");
        assert!(serde_json::to_value(Row::default()).unwrap().get("kind").is_none());
        let waiting = priority_groups(&model(vec![a], HashMap::new(), HashMap::new()));
        assert_eq!(waiting[0].rows[0].kind, "codex");
    }

    #[test]
    fn an_unreadable_account_is_a_notice_on_priority() {
        let a = Account::new(ProviderKind::Claude, "Work");
        let status = HashMap::from([(a.card_id(), ReadStatus::Unavailable("Sign in again".into()))]);
        let m = model(vec![a], HashMap::new(), status);
        let groups = priority_groups(&m);
        assert_eq!(titles(&groups), ["Needs Attention"]);
        assert_eq!(groups[0].rows[0].subtitle, "Sign in again");
    }

    #[test]
    fn providers_group_in_saved_order_with_status_notice_and_placeholder() {
        let (c1, x, c2) = (
            Account::new(ProviderKind::Claude, "A"),
            Account::new(ProviderKind::Grok, ""),
            Account::new(ProviderKind::Claude, "B"),
        );
        let mut grok = snap(vec![], Duration::minutes(12));
        grok.notice = Some("Team account".into());
        let balance = MetricLine::Values { label: "Balance".into(), values: vec![MetricValue { number: 5.0, currency_code: Some("usd".into()), label: None }] };
        let snapshots = HashMap::from([
            (c1.card_id(), snap(vec![percent("Weekly", 40.0, 100), balance], Duration::zero())),
            (x.card_id(), grok),
        ]);
        let c1_id = c1.card_id();
        let m = model(vec![c1, x, c2], snapshots, HashMap::new());
        let groups = usage_groups(&m);
        assert_eq!(titles(&groups), ["Claude", "Grok"]);
        let claude = &groups[0].rows;
        assert_eq!(claude[0], Row { title: "Claude · A".into(), subtitle: "Max".into(), card_id: Some(c1_id), kind: "claude".into(), ..Row::default() });
        assert!(claude[1].kind.is_empty() && claude[2].kind.is_empty());
        assert_eq!(claude[3].kind, "claude");
        assert_eq!(claude[1].value.as_deref(), Some("40%"));
        assert_eq!(claude[1].meter, Some(0.4));
        assert_eq!(claude[2].value.as_deref(), Some("$5"));
        assert_eq!(claude[3].title, "Claude · B");
        assert_eq!(claude[3].subtitle, "Loading…");
        let grok = &groups[1].rows;
        assert_eq!(grok[0].subtitle, "Max · Last updated 12m ago");
        assert_eq!(grok[1].title, "Team account");
    }

    #[test]
    fn the_resets_row_lists_the_accounts_resets_and_exports_the_claim() {
        let a = Account::new(ProviderKind::Codex, "");
        let at = now() + Duration::hours(30);
        let mut s = snap(vec![MetricLine::Values { label: LINE_LABEL.into(), values: vec![MetricValue { number: 1.0, currency_code: None, label: None }] }], Duration::zero());
        s.reset_items = ResetItem::codex(&[at]);
        let m = model(vec![a.clone()], HashMap::from([(a.card_id(), s)]), HashMap::new());
        let row = &usage_groups(&m)[0].rows[1];
        assert_eq!(row.card_id.as_deref(), Some(a.card_id().as_str()));
        assert_eq!((row.resets[0].title.as_str(), row.resets[0].expires.as_str(), row.resets[0].usable), ("Full Reset", "Expires in 1d 6h", true));
        let json = serde_json::to_value(row).unwrap();
        assert_eq!(json["resets"][0]["claim"], serde_json::json!({"codexCredit": at}));
        assert!(serde_json::to_value(&usage_groups(&m)[0].rows[0]).unwrap().get("resets").is_none());
    }

    #[test]
    fn last_updated_follows_the_refresh_interval() {
        let a = Account::new(ProviderKind::Claude, "Work");
        let subtitle = |age: i64| {
            let snapshots = HashMap::from([(a.card_id(), snap(vec![percent("Weekly", 85.0, 100)], Duration::minutes(age)))]);
            let settings = Settings { refresh_minutes: 30, ..Settings::default() };
            let m = PanelModel::build(vec![a.clone()], vec![], snapshots, &HashMap::new(), &BurningFast::default(), &Layout::default(), &settings, now());
            usage_groups(&m)[0].rows[0].subtitle.clone()
        };
        assert!(!subtitle(10).contains("Last updated"));
        assert!(subtitle(31).contains("Last updated 31m ago"));
    }

    #[test]
    fn an_error_replaces_the_staleness_text() {
        let a = Account::new(ProviderKind::Claude, "A");
        let status = HashMap::from([(a.card_id(), ReadStatus::Unavailable("Rate limited".into()))]);
        let m = model(vec![a.clone()], HashMap::from([(a.card_id(), snap(vec![], Duration::hours(1)))]), status);
        assert_eq!(usage_groups(&m)[0].rows[0].subtitle, "Max · Rate limited");
    }

    fn snapshot_aged(age: Duration) -> ProviderSnapshot {
        snap(vec![], age)
    }

    fn item(label: &str, value: &str) -> MenuBarItem {
        MenuBarItem { id: label.into(), card_id: "c".into(), label: label.into(), short_label: label.into(), value: value.into() }
    }

    #[test]
    fn read_status_follows_the_snapshot_and_the_wait() {
        let snap = snapshot_aged(Duration::zero());
        let mut state = ProviderState::default();
        assert_eq!(read_status(Some(&snap), Some(&state)), ReadStatus::Current);
        assert_eq!(read_status(None, None), ReadStatus::Waiting);

        backoff::record(&mut state, Err(&ProviderError::SignInExpired), now());
        let message = state.backoff.as_ref().unwrap().message.clone();
        assert_eq!(read_status(Some(&snap), Some(&state)), ReadStatus::Unavailable(message.clone()));
        assert_eq!(read_status(None, Some(&state)), ReadStatus::Unavailable(message));

        // A wait the provider asked for stays hidden.
        state.backoff = Some(Backoff { failures: 1, retry_at: now(), provider_asked: true, message: "slow down".into() });
        assert_eq!(read_status(Some(&snap), Some(&state)), ReadStatus::Current);
        assert_eq!(read_status(None, Some(&state)), ReadStatus::Waiting);
    }

    #[test]
    fn the_label_lists_items_and_the_overflow() {
        assert_eq!(label_text(&MenuBarContent::default()), "");
        let one = MenuBarContent { items: vec![item("Session", "92%")], overflow: 0 };
        assert_eq!(label_text(&one), "Session 92%");
        let more = MenuBarContent { items: vec![item("Session", "92%"), item("Weekly", "80%")], overflow: 2 };
        assert_eq!(label_text(&more), "Session 92% · Weekly 80% +2");
    }

    #[test]
    fn the_panel_json_has_the_label_the_time_and_both_tabs() {
        let a = Account::new(ProviderKind::Claude, "Work");
        let snapshots = HashMap::from([(a.card_id(), snap(vec![percent("Weekly", 85.0, 100)], Duration::minutes(2)))]);
        let m = PanelModel::build(vec![a.clone()], vec![], snapshots, &HashMap::new(), &BurningFast::default(), &Layout::default(), &Settings::default(), now());
        let json = serde_json::to_value(PanelView::new(&m)).unwrap();
        let keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["label", "priority", "updated_at", "usage"]);
        assert_eq!(json["label"], m.label());
        assert_eq!(json["updated_at"], "2027-01-15T07:58:00Z");
        let row = &json["priority"][0]["rows"][0];
        assert_eq!(row["title"], "Weekly");
        assert_eq!(row["meter"], 0.85);
        let account_row = &json["usage"][0]["rows"][0];
        assert_eq!(account_row["title"], "Claude · Work");
        assert!(account_row["meter"].is_null() && account_row["value"].is_null());

        let empty = PanelModel::build(vec![a], vec![], HashMap::new(), &HashMap::new(), &BurningFast::default(), &Layout::default(), &Settings::default(), now());
        assert!(serde_json::to_value(PanelView::new(&empty)).unwrap()["updated_at"].is_null());
    }

    #[test]
    fn a_history_row_carries_its_formatted_breakdown() {
        use crate::history::{Breakdown, Calendar, Day, History, Measure, Totals, Window};
        let a = Account::new(ProviderKind::Openrouter, "Work");
        let day = Day {
            totals: Totals::from([(Measure::Spend, 1.5), (Measure::Tokens, 3_000.0), (Measure::InputTokens, 1_000.0), (Measure::OutputTokens, 2_000.0)]),
            by_model: Some(std::collections::BTreeMap::from([("gpt".to_string(), Totals::from([(Measure::Spend, 1.5), (Measure::Tokens, 3_000.0)]))])),
        };
        let today = Calendar::Utc.today(now());
        let history = History::new(Calendar::Utc, now(), std::collections::BTreeMap::from([(today, day)]));
        let mut snapshot = snap(vec![MetricLine::Values { label: "Today".into(), values: vec![MetricValue { number: 1.5, currency_code: Some("USD".into()), label: None }] }], Duration::minutes(1));
        snapshot.breakdowns.insert("Today".into(), Breakdown::new(Window::Today, &history));
        let m = PanelModel::build(vec![a.clone()], vec![], HashMap::from([(a.card_id(), snapshot)]), &HashMap::new(), &BurningFast::default(), &Layout::default(), &Settings::default(), now());

        let json = serde_json::to_value(PanelView::new(&m)).unwrap();
        let rows = json["usage"][0]["rows"].as_array().unwrap();
        assert!(rows[0].get("breakdown").is_none());
        assert_eq!(rows[1]["card_id"], a.card_id());
        let breakdown = &rows[1]["breakdown"];
        assert_eq!(breakdown["title"], "Today");
        assert_eq!(breakdown["empty"], false);
        assert_eq!(breakdown["figures"], serde_json::json!(["$1.50", "3K tokens"]));
        assert_eq!(breakdown["token_parts"], serde_json::json!([{ "title": "Output", "value": "2K", "fraction": 2.0 / 3.0 }, { "title": "Input", "value": "1K", "fraction": 1.0 / 3.0 }]));
        assert_eq!(breakdown["models_label"], "Cost");
        assert_eq!(breakdown["models"], serde_json::json!([{ "name": "gpt", "value": "$1.50" }]));

        let later = Breakdown::new(Window::Today, &history).view(now() + Duration::days(1));
        assert_eq!(later.title, "Jan 15, 2027");
    }
}
