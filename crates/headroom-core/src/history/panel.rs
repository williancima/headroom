//! A breakdown as the panels draw it: formatted text only, so GTK and the extension just lay it out.

use super::Breakdown;
use crate::priority::values_text;
use crate::MetricValue;
use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BreakdownView {
    pub title: String,
    pub empty: bool,
    /// "$12.50", "3K tokens"; what the window charged.
    pub figures: Vec<String>,
    pub token_parts: Vec<TokenPartLine>,
    /// What the models are ranked by ("Cost"); `None` when the provider names no models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_label: Option<String>,
    pub models: Vec<ModelLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TokenPartLine {
    pub title: String,
    pub value: String,
    /// The part's share of the total, only when the parts add up to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelLine {
    pub name: String,
    pub value: String,
}

impl Breakdown {
    pub fn view(&self, now: DateTime<Utc>) -> BreakdownView {
        let fills = self.token_parts_fill_total();
        let total = self.totals.get(super::Measure::Tokens).unwrap_or(0.0);
        let figure = self.model_figures().first().copied();
        BreakdownView {
            title: self.archived_title(now).unwrap_or_else(|| self.window.title()),
            empty: self.is_empty(),
            figures: self.figures(),
            token_parts: self
                .token_parts()
                .into_iter()
                .map(|part| TokenPartLine {
                    title: part.title.to_string(),
                    value: values_text(&[MetricValue { number: part.number, currency_code: None, label: None }]),
                    fraction: fills.then(|| part.number / total),
                })
                .collect(),
            models_label: figure.map(|f| f.label().to_string()),
            models: figure
                .map(|f| self.models_by(f).iter().filter_map(|m| m.number(f).map(|n| ModelLine { name: m.name.clone(), value: f.text(n) })).collect())
                .unwrap_or_default(),
        }
    }
}
