//! A history row of the All Usage tab: an expander that opens its usage breakdown on click. Which rows are open
//! survives a redraw, since every model rebuilds the rows.

use crate::icons;
use headroom_core::history::BreakdownView;
use headroom_core::view::Row;
use relm4::adw::prelude::*;
use relm4::gtk::{self, glib};
use relm4::adw;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

/// What the open breakdown lists, decided apart from the widgets.
#[derive(Debug, PartialEq)]
pub struct Sections {
    pub header: String,
    pub nothing_to_show: bool,
    pub show_models: bool,
}

pub fn sections(breakdown: &BreakdownView) -> Sections {
    Sections {
        header: breakdown.figures.join(" · "),
        nothing_to_show: breakdown.empty,
        show_models: !breakdown.empty && !breakdown.models.is_empty(),
    }
}

#[derive(Clone, Default)]
pub struct History {
    open: Rc<RefCell<HashSet<String>>>,
}

impl History {
    /// A unified row: opens on click into one row per account, keyed by the row's id.
    pub fn members(&self, row: &Row) -> adw::ExpanderRow {
        let key = row.id.clone().unwrap_or_else(|| row.title.clone());
        let expander = adw::ExpanderRow::builder()
            .title(glib::markup_escape_text(&row.title))
            .subtitle(glib::markup_escape_text(&row.subtitle))
            .expanded(self.open.borrow().contains(&key))
            .build();
        if !row.kind.is_empty() {
            expander.add_prefix(&icons::provider_icon(&row.kind));
        }
        if let Some(value) = &row.value {
            expander.add_suffix(&gtk::Label::builder().label(value).css_classes(["dim-label"]).build());
        }
        if let Some(fraction) = row.meter {
            expander.add_suffix(&meter(fraction));
        }
        for member in &row.members {
            expander.add_row(&metric_row(member));
        }
        let open = self.open.clone();
        expander.connect_expanded_notify(move |e| {
            if e.is_expanded() {
                open.borrow_mut().insert(key.clone());
            } else {
                open.borrow_mut().remove(&key);
            }
        });
        expander
    }

    /// `card_id` and `title` tell one account's row apart from another account's row of the same provider.
    pub fn row(&self, provider: &str, kind: &str, card_id: &str, title: &str, breakdown: &BreakdownView) -> adw::ExpanderRow {
        let key = format!("{card_id}\u{1f}{title}");
        let sections = sections(breakdown);
        let expander = adw::ExpanderRow::builder()
            .title(glib::markup_escape_text(title))
            .subtitle(glib::markup_escape_text(&sections.header))
            .expanded(self.open.borrow().contains(&key))
            .build();
        let open = self.open.clone();
        expander.connect_expanded_notify(move |e| {
            if e.is_expanded() {
                open.borrow_mut().insert(key.clone());
            } else {
                open.borrow_mut().remove(&key);
            }
        });
        expander.add_row(&line(&breakdown.title, "", None));
        if !breakdown.empty {
            expander.add_row(&copy_row(provider, kind, breakdown));
        }
        if sections.nothing_to_show {
            expander.add_row(&line(&format!("{provider} didn't report any data for this range."), "", None));
            return expander;
        }
        for part in &breakdown.token_parts {
            expander.add_row(&line(&part.title, &part.value, part.fraction));
        }
        if sections.show_models {
            expander.add_row(&line("Top Models", breakdown.models_label.as_deref().unwrap_or(""), None));
            for model in &breakdown.models {
                expander.add_row(&line(&model.name, &model.value, None));
            }
        }
        expander
    }
}

/// A Copy button that puts this breakdown's image on the clipboard.
fn copy_row(provider: &str, kind: &str, breakdown: &BreakdownView) -> adw::ActionRow {
    let button = gtk::Button::builder().label("Copy").valign(gtk::Align::Center).build();
    button.update_property(&[gtk::accessible::Property::Label("Copy Usage Image")]);
    let msg = (provider.to_string(), kind.to_string(), Box::new(breakdown.clone()));
    button.connect_clicked(move |_| {
        let (provider, kind, breakdown) = msg.clone();
        let _ = crate::panel::BROKER.sender().send(crate::panel::PanelMsg::CopyBreakdown { provider, kind, breakdown });
    });
    let row = adw::ActionRow::new();
    row.add_suffix(&button);
    row
}

fn meter(fraction: f64) -> gtk::LevelBar {
    gtk::LevelBar::builder().min_value(0.0).max_value(1.0).value(fraction).width_request(72).valign(gtk::Align::Center).build()
}

/// A plain metric row: title, subtitle, meter and value.
pub fn metric_row(row: &Row) -> adw::ActionRow {
    let action = adw::ActionRow::builder().title(glib::markup_escape_text(&row.title)).subtitle(glib::markup_escape_text(&row.subtitle)).build();
    if !row.kind.is_empty() {
        action.add_prefix(&icons::provider_icon(&row.kind));
    }
    if let Some(fraction) = row.meter {
        action.add_suffix(&meter(fraction));
    }
    if let Some(value) = &row.value {
        action.add_suffix(&gtk::Label::builder().label(value).css_classes(["dim-label"]).build());
    }
    action
}

fn line(title: &str, value: &str, fraction: Option<f64>) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(glib::markup_escape_text(title)).build();
    if let Some(fraction) = fraction {
        row.add_suffix(&gtk::LevelBar::builder().min_value(0.0).max_value(1.0).value(fraction).width_request(72).valign(gtk::Align::Center).build());
    }
    if !value.is_empty() {
        row.add_suffix(&gtk::Label::builder().label(value).css_classes(["dim-label"]).build());
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use headroom_core::history::ModelLine;

    fn view(empty: bool, models: usize) -> BreakdownView {
        BreakdownView {
            title: "Today".into(),
            empty,
            figures: vec!["$1.50".into(), "3K tokens".into()],
            token_parts: vec![],
            models_label: Some("Cost".into()),
            models: (0..models).map(|i| ModelLine { name: format!("m{i}"), value: "$1".into() }).collect(),
        }
    }

    #[test]
    fn an_empty_breakdown_shows_only_the_notice_and_models_need_data() {
        let s = sections(&view(true, 2));
        assert!(s.nothing_to_show && !s.show_models);
        let s = sections(&view(false, 0));
        assert!(!s.nothing_to_show && !s.show_models);
        let s = sections(&view(false, 2));
        assert!(s.show_models);
        assert_eq!(s.header, "$1.50 · 3K tokens");
    }
}
