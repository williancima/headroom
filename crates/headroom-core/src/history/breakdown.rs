//! What a window's detail shows: what it charged, its tokens by kind, and its top models.

use super::{Calendar, History, Measure, Totals, Window};
use crate::priority::values_text;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What Top Models shows and ranks by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelFigure {
    Cost,
    Tokens,
    /// A model's part of the range's credits or turns, for a provider with no dollars or tokens by model.
    Share,
}

impl ModelFigure {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cost => "Cost",
            Self::Tokens => "Tokens",
            Self::Share => "Share",
        }
    }

    pub fn text(self, number: f64) -> String {
        match self {
            Self::Cost => values_text(&[Measure::Spend.metric_value(number)]),
            Self::Tokens => values_text(&[crate::MetricValue { number, currency_code: None, label: None }]),
            Self::Share => format!("{:.0}%", number * 100.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub name: String,
    pub dollars: Option<f64>,
    pub tokens: Option<f64>,
    pub share: Option<f64>,
}

impl Model {
    pub fn number(&self, figure: ModelFigure) -> Option<f64> {
        match figure {
            ModelFigure::Cost => self.dollars,
            ModelFigure::Tokens => self.tokens,
            ModelFigure::Share => self.share,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TokenPart {
    pub title: &'static str,
    pub number: f64,
}

pub const SHOWN_MODELS: usize = 5;
const DOLLAR_MEASURES: [Measure; 2] = [Measure::Spend, Measure::UsageValue];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Breakdown {
    pub window: Window,
    reporting_day: NaiveDate,
    calendar: Calendar,
    pub totals: Totals,
    unread_days: usize,
    /// Only the names and figures shown in the panel are kept with the snapshot.
    rankings: BTreeMap<ModelFigure, Vec<Model>>,
}

impl Breakdown {
    pub fn new(window: Window, history: &History) -> Self {
        let totals = history.totals(window).unwrap_or_default();
        let models = history.models(window);
        let is_reported = |measure: Measure| models.values().any(|m| m.get(measure).unwrap_or(0.0) > 0.0);
        let dollars = DOLLAR_MEASURES.into_iter().find(|m| is_reported(*m));
        let has_tokens = is_reported(Measure::Tokens);
        let share = [Measure::Credits, Measure::Turns].into_iter().find(|m| totals.get(*m).unwrap_or(0.0) > 0.0 && is_reported(*m));

        let every_model: Vec<Model> = models
            .iter()
            .map(|(name, model)| {
                let fraction = share.and_then(|measure| {
                    let contribution = model.get(measure)? / totals.get(measure)?;
                    contribution.is_finite().then_some(contribution)
                });
                Model {
                    name: name.clone(),
                    dollars: dollars.and_then(|m| model.get(m)),
                    tokens: if has_tokens { model.get(Measure::Tokens) } else { None },
                    share: fraction,
                }
            })
            .collect();
        let mut available = vec![];
        if dollars.is_some() {
            available.push(ModelFigure::Cost);
        }
        if has_tokens {
            available.push(ModelFigure::Tokens);
        }
        if available.is_empty() && share.is_some() {
            available.push(ModelFigure::Share);
        }
        Self {
            window,
            reporting_day: history.calendar.today(history.as_of),
            calendar: history.calendar,
            totals,
            unread_days: history.unread_days(window),
            rankings: available.into_iter().map(|figure| (figure, rank(&every_model, figure))).collect(),
        }
    }

    /// What the models report, in order of preference.
    pub fn model_figures(&self) -> Vec<ModelFigure> {
        self.rankings.keys().copied().collect()
    }

    pub fn models_by(&self, figure: ModelFigure) -> &[Model] {
        self.rankings.get(&figure).map_or(&[], Vec::as_slice)
    }

    /// The ranking of the first figure the models report.
    pub fn models(&self) -> &[Model] {
        self.rankings.values().next().map_or(&[], Vec::as_slice)
    }

    /// Relative window names expire at midnight on the source's clock.
    pub fn archived_title(&self, now: DateTime<Utc>) -> Option<String> {
        if self.reporting_day == self.calendar.today(now) {
            return None;
        }
        let back = self.window.day_offsets().map_or(0, |offsets| *offsets.start());
        let end = self.reporting_day - Duration::days(back);
        let date = format!("{} {}, {}", MONTHS[end.month0() as usize], end.day(), end.year());
        Some(match self.window {
            Window::Today | Window::Yesterday => date,
            Window::LastDays(count) | Window::PriorDays(count) => format!("{count} Days Ending {date}"),
            Window::ThisWeek => format!("Week Through {date}"),
            Window::ThisMonth => format!("Month Through {date}"),
        })
    }

    /// Every figure is zero, so there is nothing to show beyond a zero total. Unread days can still have
    /// activity, unless nothing at all was reported for the range.
    pub fn is_empty(&self) -> bool {
        (self.unread_days == 0 || self.totals.values.is_empty()) && self.totals.values.values().all(|n| *n == 0.0)
    }

    /// What the window charged, then its tokens and any spend on the user's own keys. A window that
    /// reports neither dollars nor tokens falls back to the first thing it does report.
    pub fn figures(&self) -> Vec<String> {
        let charged = DOLLAR_MEASURES.into_iter().find(|m| self.totals.get(*m).is_some());
        let mut shown: Vec<Measure> = charged.into_iter().chain([Measure::Tokens, Measure::ByokSpend]).filter(|m| {
            self.totals.get(*m).is_some_and(|n| *m != Measure::ByokSpend || n > 0.0)
        }).collect();
        if shown.is_empty() {
            shown.extend(self.totals.values.keys().next().copied());
        }
        shown.iter().filter_map(|m| self.totals.get(*m).map(|n| values_text(&[m.metric_value(n)]))).collect()
    }

    /// The kinds of token that have any, largest first.
    pub fn token_parts(&self) -> Vec<TokenPart> {
        let mut parts: Vec<TokenPart> = Measure::TOKEN_PARTS
            .iter()
            .filter_map(|(measure, title)| self.totals.get(*measure).map(|number| TokenPart { title, number }))
            .filter(|part| part.number > 0.0)
            .collect();
        parts.sort_by(|a, b| b.number.total_cmp(&a.number));
        parts
    }

    /// Some providers count cached tokens inside input, so only parts that add up to the total can be
    /// drawn as shares of it.
    pub fn token_parts_fill_total(&self) -> bool {
        let Some(total) = self.totals.get(Measure::Tokens).filter(|t| *t > 0.0) else { return false };
        (self.token_parts().iter().map(|p| p.number).sum::<f64>() - total).abs() < 1.0
    }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// The top models by `figure`, the rest as Other.
fn rank(models: &[Model], figure: ModelFigure) -> Vec<Model> {
    let mut ranked: Vec<&Model> = models.iter().filter(|m| m.number(figure).is_some()).collect();
    ranked.sort_by(|a, b| {
        let (left, right) = (a.number(figure).unwrap_or(0.0), b.number(figure).unwrap_or(0.0));
        right.total_cmp(&left).then_with(|| a.name.cmp(&b.name))
    });
    let mut top: Vec<Model> = ranked.iter().take(SHOWN_MODELS).map(|m| (*m).clone()).collect();
    let rest = &ranked[ranked.len().min(SHOWN_MODELS)..];
    if !rest.is_empty() {
        let sum = |number: fn(&Model) -> Option<f64>| {
            let values: Vec<f64> = rest.iter().filter_map(|m| number(m)).collect();
            (!values.is_empty()).then(|| values.iter().sum())
        };
        top.push(Model { name: "Other".into(), dollars: sum(|m| m.dollars), tokens: sum(|m| m.tokens), share: sum(|m| m.share) });
    }
    top
}
