//! Tray label content.

use super::meter::{Meter, Unit};
use super::{LineRef, PriorityBoard};
use crate::models::MetricValue;

/// How many limits the tray label lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuBarLimit {
    IconOnly,
    UpToOne,
    UpToTwo,
    UpToThree,
    UpToFour,
    UpToFive,
}

impl MenuBarLimit {
    pub fn count(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MenuBarItem {
    pub id: String,
    pub card_id: String,
    pub label: String,
    pub short_label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MenuBarContent {
    pub items: Vec<MenuBarItem>,
    pub overflow: usize,
}

const GENERIC_WORDS: [&str; 3] = ["models", "bot", "usage"];

/// Drops a trailing generic word when another word remains.
pub fn short_label(label: &str) -> String {
    let mut words: Vec<&str> = label.split(' ').filter(|w| !w.is_empty()).collect();
    match words.last() {
        Some(last) if words.len() > 1 && GENERIC_WORDS.contains(&last.to_lowercase().as_str()) => {
            words.pop();
            words.join(" ")
        }
        _ => label.to_string(),
    }
}

fn item_lines(items: &[super::PriorityItem]) -> Vec<&LineRef> {
    items.iter().map(|i| &i.line).collect()
}

/// Only what's running out, or with `always_shows` every Priority section in its order, top to bottom.
pub fn menu_bar_content(board: &PriorityBoard, limit: MenuBarLimit, always_shows: bool) -> MenuBarContent {
    if limit == MenuBarLimit::IconOnly {
        return MenuBarContent::default();
    }
    let (counted, used_up): (Vec<&LineRef>, Vec<&LineRef>) = if always_shows {
        let mut counted: Vec<&LineRef> = board.pinned.iter().map(|p| &p.line).collect();
        counted.extend(item_lines(&board.attention));
        counted.extend(item_lines(&board.use_it_or_lose_it));
        (counted, item_lines(&board.used_up))
    } else {
        (item_lines(&board.running_out), vec![])
    };
    let total = counted.len();
    let items: Vec<MenuBarItem> = counted
        .into_iter()
        .chain(used_up)
        .take(limit.count())
        .map(|line| MenuBarItem {
            id: line.id.clone(),
            card_id: line.card_id.clone(),
            label: line.label.clone(),
            short_label: short_label(&line.label),
            value: line.menu_bar_value.clone(),
        })
        .collect();
    let overflow = total.saturating_sub(items.len());
    MenuBarContent { items, overflow }
}

pub(super) fn currency_symbol(code: &str) -> String {
    match code.to_uppercase().as_str() {
        "USD" => "$".into(),
        "EUR" => "€".into(),
        "GBP" => "£".into(),
        "BRL" => "R$".into(),
        "JPY" => "¥".into(),
        "CNY" => "CN¥".into(),
        "INR" => "₹".into(),
        "CAD" => "CA$".into(),
        "AUD" => "A$".into(),
        other => other.to_string(),
    }
}

/// `1.5K`, `12M` (0...1 fraction digits).
pub(super) fn compact(value: f64) -> String {
    let (scaled, suffix) = match value.abs() {
        v if v >= 1e12 => (value / 1e12, "T"),
        v if v >= 1e9 => (value / 1e9, "B"),
        v if v >= 1e6 => (value / 1e6, "M"),
        _ => (value / 1e3, "K"),
    };
    format!("{}{suffix}", trim_fraction(scaled, 1))
}

pub(super) fn trim_fraction(value: f64, digits: usize) -> String {
    let text = format!("{value:.digits$}");
    if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.').to_string() } else { text }
}

/// A number in the tray's style.
pub(super) fn tray_number(value: f64, unit: Unit, currency: Option<&str>) -> String {
    match unit {
        Unit::Percent => format!("{}%", value.clamp(0.0, 100.0).round() as i64),
        Unit::Dollars => {
            let symbol = currency_symbol(currency.unwrap_or("USD"));
            if value.abs() >= 1000.0 {
                return format!("{symbol}{}", compact(value));
            }
            let digits = if value != 0.0 && value.abs() < 1.0 { 2 } else { 0 };
            format!("{symbol}{value:.digits$}")
        }
        Unit::Count => {
            if value.abs() >= 1000.0 { compact(value) } else { trim_fraction(value, 1) }
        }
    }
}

impl Meter {
    /// The tray value of a bounded meter; `show_remaining` picks left over used.
    pub(super) fn menu_bar_value(&self, show_remaining: bool) -> String {
        let shown = if show_remaining { (self.limit - self.used).max(0.0) } else { self.used };
        if self.unit == Unit::Percent && self.limit > 0.0 {
            return tray_number(shown / self.limit * 100.0, Unit::Percent, None);
        }
        tray_number(shown, self.unit, self.currency.as_deref())
    }
}

/// A values line shows its first value.
pub(super) fn values_menu_bar_value(values: &[MetricValue]) -> String {
    match values.first() {
        Some(MetricValue { number, currency_code: Some(code), .. }) => tray_number(*number, Unit::Dollars, Some(code)),
        Some(MetricValue { number, currency_code: None, .. }) => tray_number(*number, Unit::Count, None),
        None => "—".to_string(),
    }
}
