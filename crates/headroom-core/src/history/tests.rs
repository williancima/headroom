use super::*;
use crate::models::MetricLine;
use chrono::Duration;
use std::sync::Mutex;

const DUBAI: Calendar = Calendar::Fixed(4 * 3600);

/// 00:30 on 31 October in Dubai, still 30 October in UTC.
fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_793_392_200, 0).unwrap()
}

fn at(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn history(days: Vec<(usize, Day)>) -> History {
    let starts = History::day_starts(0..30, now(), DUBAI);
    History::new(DUBAI, now(), days.into_iter().map(|(offset, day)| (starts[offset], day)).collect())
}

fn every_day(totals: Totals) -> Vec<(usize, Day)> {
    (0..30).map(|offset| (offset, Day::new(totals.clone()))).collect()
}

fn tokens(number: f64) -> MetricValue {
    Measure::Tokens.metric_value(number)
}

#[test]
fn day_starts_count_back_across_month_and_year_ends() {
    let jan = at("2027-01-02T12:00:00Z");
    let days: Vec<String> = History::day_starts(0..=3, jan, Calendar::Utc).iter().map(|d| d.to_string()).collect();
    assert_eq!(days, ["2027-01-02", "2027-01-01", "2026-12-31", "2026-12-30"]);
    assert_eq!(History::day_starts(0..=1, now(), Calendar::Utc)[0].to_string(), "2026-10-30");
    assert_eq!(History::day_starts(0..=1, now(), DUBAI)[0].to_string(), "2026-10-31");
}

#[test]
fn a_zero_from_read_days_confirms_no_usage_only_when_no_day_failed() {
    let row = HistoryRow::Window(Window::LastDays(2), Measure::Tokens, None);
    let title = row.title();
    let expected = MetricLine::Values { label: title.clone(), values: vec![tokens(0.0)] };
    let mut days = vec![(0, Day::new(Totals::zero(&[Measure::Tokens]))), (1, Day::new(Totals::default()))];

    let sparse = history(days.clone());
    assert_eq!(row.line(&title, &sparse), Some(expected.clone()));
    assert!(row.breakdown(&sparse).unwrap().is_empty(), "both days were successfully read");

    days.pop();
    let partial = history(days);
    assert_eq!(row.line(&title, &partial), Some(expected), "keep the figure from the day that was read");
    assert!(!row.breakdown(&partial).unwrap().is_empty(), "the other day's activity is unknown");
    assert_eq!(row.line(&title, &history(vec![])), None);
}

#[test]
fn a_row_sums_the_days_that_report_a_figure_and_hides_when_none_do() {
    let mut days = every_day(Totals::from([(Measure::Spend, 1.0), (Measure::UsageEvents, 2.0), (Measure::Tokens, 100.0)]));
    days[3].1 = Day::new(Totals::from([(Measure::Spend, 1.0), (Measure::UsageEvents, 2.0)]));
    let history = history(days);

    let two = HistoryRow::Window(Window::LastDays(30), Measure::Spend, Some(Measure::Tokens));
    assert_eq!(
        two.line("Last 30 Days", &history),
        Some(MetricLine::Values { label: "Last 30 Days".into(), values: vec![Measure::Spend.metric_value(30.0), tokens(2_900.0)] })
    );
    assert_eq!(HistoryRow::Window(Window::LastDays(30), Measure::Requests, None).line("Last 30 Days", &history), None);
    let events = HistoryRow::Measure("Usage Events", Measure::UsageEvents, Window::LastDays(30));
    let MetricValue { number, label, .. } = match events.line("Usage Events", &history) {
        Some(MetricLine::Values { values, .. }) => values[0].clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!((number, label.as_deref()), (60.0, Some("in 30 days")));
}

#[test]
fn restored_windows_use_the_original_reporting_day_after_provider_midnight() {
    let before = at("2026-10-31T23:59:00Z");
    let after = at("2026-11-01T00:01:00Z");
    let restored = |window: Window, calendar: Calendar| {
        let mut history = History::new(calendar, before, BTreeMap::new());
        history.reported.insert(window, Totals::zero(&[Measure::Tokens]));
        let detail = Breakdown::new(window, &history);
        serde_json::from_str::<Breakdown>(&serde_json::to_string(&detail).unwrap()).unwrap()
    };

    let today = restored(Window::Today, Calendar::Utc);
    assert_eq!(today.archived_title(before), None);
    assert_eq!(today.archived_title(after).as_deref(), Some("Oct 31, 2026"));
    assert_eq!(restored(Window::Yesterday, Calendar::Utc).archived_title(after).as_deref(), Some("Oct 30, 2026"));
    assert_eq!(restored(Window::Today, DUBAI).archived_title(after), None, "both instants are Nov 1 in Dubai");
    assert_eq!(restored(Window::LastDays(7), Calendar::Utc).archived_title(after).as_deref(), Some("7 Days Ending Oct 31, 2026"));
}

#[test]
fn encoded_rankings_keep_only_the_top_models_and_other_for_each_figure() {
    let models: BTreeMap<String, Totals> = (1..=15)
        .map(|i| (format!("model-{i}"), Totals::from([(Measure::Spend, f64::from(i)), (Measure::Tokens, f64::from(16 - i))])))
        .collect();
    let day = Day { totals: Totals::from([(Measure::Spend, 120.0), (Measure::Tokens, 120.0)]), by_model: Some(models) };
    let detail = Breakdown::new(Window::Today, &history(vec![(0, day)]));
    let encoded = serde_json::to_string(&detail).unwrap();
    let saved: Breakdown = serde_json::from_str(&encoded).unwrap();

    assert_eq!(saved, detail);
    let names = |figure| saved.models_by(figure).iter().map(|m| m.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(ModelFigure::Cost), ["model-15", "model-14", "model-13", "model-12", "model-11", "Other"]);
    assert_eq!(names(ModelFigure::Tokens), ["model-1", "model-2", "model-3", "model-4", "model-5", "Other"]);
    assert_eq!(saved.models_by(ModelFigure::Cost).last().unwrap().dollars, Some(55.0));
    assert_eq!(saved.models_by(ModelFigure::Tokens).last().unwrap().tokens, Some(55.0));
    for i in 6..=10 {
        assert!(!encoded.contains(&format!("\"model-{i}\"")));
    }
}

#[test]
fn models_no_one_was_charged_for_rank_by_tokens_with_no_dollars() {
    let by_model = BTreeMap::from([
        ("a".to_string(), Totals::from([(Measure::Spend, 0.0), (Measure::Tokens, 100.0)])),
        ("b".to_string(), Totals::from([(Measure::Spend, 0.0), (Measure::Tokens, 900.0)])),
    ]);
    let day = Day { totals: Totals::from([(Measure::Spend, 0.0), (Measure::Tokens, 1_000.0)]), by_model: Some(by_model) };
    let today = Breakdown::new(Window::Today, &history(vec![(0, day)]));

    assert_eq!(today.model_figures(), [ModelFigure::Tokens]);
    assert_eq!(today.models().iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["b", "a"]);
    assert!(today.models().iter().all(|m| m.dollars.is_none()));
}

#[test]
fn unattributed_usage_counts_toward_totals_while_model_figures_show_only_reported_amounts() {
    let attributed = Day {
        totals: Totals::from([(Measure::Spend, 10.0), (Measure::Tokens, 100.0)]),
        by_model: Some(BTreeMap::from([
            ("a".to_string(), Totals::from([(Measure::Spend, 8.0), (Measure::Tokens, 40.0)])),
            ("b".to_string(), Totals::from([(Measure::Spend, 2.0)])),
        ])),
    };
    let unsplit = Day::new(Totals::from([(Measure::Spend, 100.0), (Measure::Tokens, 100_000.0)]));
    let range = Breakdown::new(Window::LastDays(2), &history(vec![(0, attributed), (1, unsplit)]));

    assert_eq!(range.totals.get(Measure::Spend), Some(110.0));
    assert_eq!(range.totals.get(Measure::Tokens), Some(100_100.0));
    let cost = range.models_by(ModelFigure::Cost);
    assert_eq!(cost.iter().map(|m| (m.name.as_str(), m.dollars)).collect::<Vec<_>>(), [("a", Some(8.0)), ("b", Some(2.0))]);
    let by_tokens = range.models_by(ModelFigure::Tokens);
    assert_eq!(by_tokens.iter().map(|m| (m.name.as_str(), m.tokens)).collect::<Vec<_>>(), [("a", Some(40.0))]);
}

#[test]
fn model_shares_use_reported_credits_and_fall_back_to_turns_without_a_positive_credit_total() {
    let mut charged = Day {
        totals: Totals::from([(Measure::Credits, 8.0), (Measure::Turns, 10.0)]),
        by_model: Some(BTreeMap::from([
            ("billed".to_string(), Totals::from([(Measure::Credits, 2.0)])),
            ("turns-only".to_string(), Totals::from([(Measure::Turns, 10.0)])),
        ])),
    };
    let recent = Day {
        totals: Totals::from([(Measure::Turns, 4.0)]),
        by_model: Some(BTreeMap::from([("turns-only".to_string(), Totals::from([(Measure::Turns, 4.0)]))])),
    };
    let range = |charged: &Day| Breakdown::new(Window::LastDays(2), &history(vec![(0, recent.clone()), (1, charged.clone())]));

    let credits = range(&charged);
    assert_eq!(credits.totals.get(Measure::Credits), Some(8.0), "today reported only turns");
    assert_eq!(credits.models().iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["billed"]);
    assert_eq!(credits.models()[0].share, Some(0.25), "two of the eight reported credits were attributed");

    charged.by_model.as_mut().unwrap().insert("refund".into(), Totals::from([(Measure::Credits, -1.0)]));
    assert_eq!(range(&charged).models().last().unwrap().share, Some(-0.125), "a credit adjustment keeps its sign");

    for total in [None, Some(0.0)] {
        match total {
            Some(number) => charged.totals.values.insert(Measure::Credits, number),
            None => charged.totals.values.remove(&Measure::Credits),
        };
        let turns = range(&charged);
        assert_eq!(turns.totals.get(Measure::Credits), total);
        assert_eq!(turns.models().iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["turns-only"]);
        assert_eq!(turns.models()[0].share, Some(1.0));
    }
}

#[test]
fn the_figures_lead_with_dollars_and_byok_shows_only_when_spent() {
    let figures = |values: &[(Measure, f64)]| {
        let totals = Totals { values: values.iter().copied().collect() };
        Breakdown::new(Window::Today, &history(vec![(0, Day::new(totals))])).figures()
    };
    use Measure::*;
    assert_eq!(figures(&[(Spend, 12.5), (UsageEvents, 9.0), (Tokens, 3_000.0), (ByokSpend, 0.0)]), ["$12.50", "3K tokens"]);
    assert_eq!(figures(&[(Spend, 12.5), (ByokSpend, 2.0), (Tokens, 3_000.0)]), ["$12.50", "3K tokens", "$2 BYOK"]);
    assert_eq!(figures(&[(Tokens, 3_000.0)]), ["3K tokens"]);
    assert_eq!(figures(&[(Requests, 8.0)]), ["8 requests"], "a legacy plan reports nothing else");
}

#[test]
fn token_parts_are_largest_first_and_a_bar_needs_them_to_add_up_to_the_total() {
    use Measure::*;
    let breakdown = |values: &[(Measure, f64)]| {
        let totals = Totals { values: values.iter().copied().collect() };
        Breakdown::new(Window::Today, &history(vec![(0, Day::new(totals))]))
    };
    let split = breakdown(&[(Tokens, 100.0), (InputTokens, 10.0), (CacheReadTokens, 80.0), (CacheWriteTokens, 0.0), (OutputTokens, 10.0)]);
    assert_eq!(split.token_parts().iter().map(|p| p.title).collect::<Vec<_>>(), ["Cache Read", "Input", "Output"]);
    assert!(split.token_parts_fill_total());

    let cached_inside_input = breakdown(&[(Tokens, 660.0), (InputTokens, 600.0), (CacheReadTokens, 200.0), (OutputTokens, 60.0)]);
    assert!(!cached_inside_input.token_parts_fill_total(), "cached tokens are already part of input");
}

#[test]
fn this_week_starts_on_sunday_and_this_month_on_the_first() {
    let wednesday = at("2026-11-18T12:00:00Z");
    let day = |text: &str| text.parse::<NaiveDate>().unwrap();
    let days = ["2026-11-14", "2026-11-15", "2026-11-18", "2026-11-01", "2026-10-31"]
        .into_iter()
        .map(|d| (day(d), Day::new(Totals::from([(Measure::Requests, 1.0)]))))
        .collect();
    let history = History::new(Calendar::Utc, wednesday, days);
    assert_eq!(history.totals(Window::ThisWeek).unwrap().get(Measure::Requests), Some(2.0));
    assert_eq!(history.totals(Window::ThisMonth).unwrap().get(Measure::Requests), Some(4.0));
}

struct Fake {
    interval: Option<Duration>,
    wait: Option<Duration>,
    fail: Vec<NaiveDate>,
    asked: Mutex<Vec<Vec<NaiveDate>>>,
}

impl Fake {
    fn new() -> Self {
        Self { interval: None, wait: None, fail: vec![], asked: Mutex::new(vec![]) }
    }

    fn asked(&self) -> Vec<Vec<NaiveDate>> {
        self.asked.lock().unwrap().clone()
    }
}

impl Source for Fake {
    fn clock(&self) -> Calendar {
        Calendar::Utc
    }

    fn reread_interval(&self) -> Option<Duration> {
        self.interval
    }

    async fn read(&self, days: &[NaiveDate], _as_of: DateTime<Utc>, _clock: Calendar) -> Read {
        self.asked.lock().unwrap().push(days.to_vec());
        let read = days.iter().filter(|d| !self.fail.contains(d)).map(|d| (*d, Day::new(Totals::from([(Measure::Requests, 1.0)])))).collect();
        Read { days: read, rate_limited_for: self.wait }
    }
}

fn rows() -> Vec<(String, HistoryRow)> {
    vec![("Last 4 Days".into(), HistoryRow::Window(Window::LastDays(4), Measure::Requests, None))]
}

fn span(now: DateTime<Utc>) -> Vec<NaiveDate> {
    History::day_starts(0..4, now, Calendar::Utc)
}

#[tokio::test]
async fn the_ledger_reads_old_days_once_and_today_and_yesterday_every_time() {
    let mut ledger = Ledger::default();
    let source = Fake::new();
    let first = at("2026-11-10T10:00:00Z");
    let days = span(first);

    let out = history_lines(&source, &mut ledger, &rows(), first, false).await;
    assert_eq!(source.asked()[0], days);
    assert!(matches!(&out.lines[0], MetricLine::Values { values, .. } if values[0].number == 4.0));
    assert!(out.breakdowns.contains_key("Last 4 Days"));

    // The oldest two were read after their end and stay; today's partial read is read again.
    history_lines(&source, &mut ledger, &rows(), first + Duration::hours(1), false).await;
    assert_eq!(source.asked()[1], days[..2]);
}

#[tokio::test]
async fn a_failed_reread_drops_the_day_but_old_days_stay() {
    let mut ledger = Ledger::default();
    let mut source = Fake::new();
    let now = at("2026-11-10T10:00:00Z");
    let days = span(now);
    history_lines(&source, &mut ledger, &rows(), now, false).await;

    source.fail = vec![days[1]];
    let out = history_lines(&source, &mut ledger, &rows(), now, false).await;
    assert!(matches!(&out.lines[0], MetricLine::Values { values, .. } if values[0].number == 3.0));
    assert!(!ledger.days().contains_key(&days[1]) && ledger.days().contains_key(&days[3]));
}

#[tokio::test]
async fn a_new_day_drops_the_days_that_left_the_span_and_reads_the_new_one() {
    let mut ledger = Ledger::default();
    let source = Fake::new();
    let now = at("2026-11-10T10:00:00Z");
    history_lines(&source, &mut ledger, &rows(), now, false).await;
    let next = now + Duration::days(1);
    history_lines(&source, &mut ledger, &rows(), next, false).await;
    let days = span(next);
    assert_eq!(source.asked()[1], days[..2]);
    assert_eq!(ledger.days().keys().copied().collect::<Vec<_>>(), days.iter().rev().copied().collect::<Vec<_>>());
}

#[tokio::test]
async fn a_rate_limit_holds_reads_until_it_ends_and_keeps_old_days() {
    let mut ledger = Ledger::default();
    let mut source = Fake::new();
    let now = at("2026-11-10T10:00:00Z");
    history_lines(&source, &mut ledger, &rows(), now, false).await;
    source.wait = Some(Duration::hours(1));
    history_lines(&source, &mut ledger, &rows(), now, false).await;
    assert!(ledger.is_held(now + Duration::minutes(59)) && !ledger.is_held(now + Duration::minutes(61)));

    let calls = source.asked().len();
    let out = history_lines(&source, &mut ledger, &rows(), now + Duration::minutes(30), false).await;
    assert_eq!(source.asked().len(), calls, "held: nothing is sent");
    assert!(matches!(&out.lines[0], MetricLine::Values { values, .. } if values[0].number == 2.0), "today and yesterday are unread, older days stay");
}

#[tokio::test]
async fn a_source_with_a_reread_interval_rests_on_the_same_day_and_reads_at_once_on_a_new_one() {
    let mut ledger = Ledger::default();
    let mut source = Fake::new();
    source.interval = Some(Duration::hours(1));
    let now = at("2026-11-10T10:00:00Z");
    history_lines(&source, &mut ledger, &rows(), now, false).await;

    let rested = history_lines(&source, &mut ledger, &rows(), now + Duration::minutes(30), false).await;
    assert_eq!(source.asked().len(), 1);
    assert!(matches!(&rested.lines[0], MetricLine::Values { values, .. } if values[0].number == 4.0));

    history_lines(&source, &mut ledger, &rows(), now + Duration::minutes(61), false).await;
    assert_eq!(source.asked().len(), 2, "the interval ended");
    history_lines(&source, &mut ledger, &rows(), now + Duration::minutes(62), false).await;
    assert_eq!(source.asked().len(), 2);
    history_lines(&source, &mut ledger, &rows(), now + Duration::days(1), false).await;
    assert_eq!(source.asked().len(), 3, "a new day is read at once");
}

#[tokio::test]
async fn a_forced_refresh_rereads_today_and_yesterday_inside_the_interval() {
    let mut ledger = Ledger::default();
    let mut source = Fake::new();
    source.interval = Some(Duration::hours(1));
    let now = at("2026-11-10T10:00:00Z");
    history_lines(&source, &mut ledger, &rows(), now, false).await;
    history_lines(&source, &mut ledger, &rows(), now + Duration::minutes(5), true).await;
    assert_eq!(source.asked().len(), 2);
    assert_eq!(source.asked()[1], span(now)[..2]);
}

#[tokio::test]
async fn no_history_rows_read_nothing() {
    let mut ledger = Ledger::default();
    let source = Fake::new();
    let out = history_lines(&source, &mut ledger, &[], at("2026-11-10T10:00:00Z"), false).await;
    assert!(out.lines.is_empty() && out.breakdowns.is_empty() && source.asked().is_empty());
}

#[tokio::test]
async fn the_ledger_survives_a_save_and_old_provider_state_still_loads() {
    let mut ledger = Ledger::default();
    let source = Fake::new();
    let now = at("2026-11-10T10:00:00Z");
    history_lines(&source, &mut ledger, &rows(), now, false).await;
    ledger.hold(Duration::hours(1), now);

    let state = crate::store::ProviderState { history: Some(ledger.clone()), ..Default::default() };
    let json = serde_json::to_string(&state).unwrap();
    let loaded: crate::store::ProviderState = serde_json::from_str(&json).unwrap();
    assert_eq!(loaded.history, Some(ledger));

    let old: crate::store::ProviderState = serde_json::from_str(r#"{"last_secondary_read":null,"cached_body":null}"#).unwrap();
    assert_eq!(old.history, None);
    assert!(!serde_json::to_string(&old).unwrap().contains("history"));
}
