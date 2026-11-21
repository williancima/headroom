//! The Accounts page: saved accounts and hubs, each with a Remove that asks first.

use crate::accounts_logic::{account_rows, hub_removal_body, hub_rows, count_text, HUB_ICON};
use crate::icons;
use crate::layout_logic::{metric_settings, Drag, MetricSetting};
use crate::layout_menu::activate;
use crate::reorder::make_reorderable;
use crate::panel::PanelMsg;
use headroom_core::view::PanelModel;
use crate::Command;
use relm4::adw::prelude::*;
use relm4::gtk::{self, glib};
use relm4::{adw, Sender};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use tokio::sync::mpsc::UnboundedSender;

pub const TAG: &str = "accounts";

pub struct AccountsPage {
    pub page: adw::NavigationPage,
    content: gtk::Stack,
    accounts: adw::PreferencesGroup,
    hubs: adw::PreferencesGroup,
    account_rows: RefCell<Vec<adw::PreferencesRow>>,
    hub_rows: RefCell<Vec<adw::PreferencesRow>>,
    /// Accounts whose metrics are showing; kept across the rebuild on every refresh.
    expanded: Rc<RefCell<HashSet<String>>>,
    commands: UnboundedSender<Command>,
}

impl AccountsPage {
    pub fn new(input: Sender<PanelMsg>, commands: UnboundedSender<Command>) -> Self {
        let accounts = adw::PreferencesGroup::new();
        let hubs = adw::PreferencesGroup::builder().title("CLIProxyAPI Hubs").build();
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).valign(gtk::Align::Start).build();
        body.append(&accounts);
        body.append(&hubs);
        let clamp = adw::Clamp::builder().child(&body).margin_top(12).margin_bottom(12).margin_start(12).margin_end(12).build();
        // With nothing saved the page would be blank (e.g. Back from the picker opened by the dashboard).
        let content = gtk::Stack::new();
        content.add_named(&gtk::ScrolledWindow::builder().child(&clamp).vexpand(true).build(), Some("list"));
        content.add_named(&crate::panel::empty_state(&input), Some("empty"));
        let add = gtk::Button::from_icon_name("list-add-symbolic");
        add.update_property(&[gtk::accessible::Property::Label("Add Account")]);
        add.connect_clicked(move |_| {
            let _ = input.send(PanelMsg::AddAccount);
        });
        let header = adw::HeaderBar::new();
        header.pack_end(&add);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&content));
        let page = adw::NavigationPage::builder().child(&view).title("Accounts").tag(TAG).build();
        Self { page, content, accounts, hubs, account_rows: RefCell::default(), hub_rows: RefCell::default(), expanded: Rc::default(), commands }
    }

    pub fn show(&self, model: &PanelModel) {
        let rows = account_rows(&model.accounts);
        let mut built = Vec::new();
        for row in rows {
            let expander = adw::ExpanderRow::builder()
                .title(glib::markup_escape_text(&row.title))
                .subtitle(glib::markup_escape_text(&row.subtitle))
                .expanded(self.expanded.borrow().contains(&row.card_id))
                .build();
            expander.add_prefix(&icons::provider_icon(row.kind));
            let (expanded, id) = (self.expanded.clone(), row.card_id.clone());
            expander.connect_expanded_notify(move |r| {
                if r.is_expanded() {
                    expanded.borrow_mut().insert(id.clone());
                } else {
                    expanded.borrow_mut().remove(&id);
                }
            });
            let card_id = row.card_id.clone();
            if row.removable {
                let (commands, card_id) = (self.commands.clone(), row.card_id);
                let button = remove_button(
                    format!("Remove {}?", row.title),
                    "Its sign-in is deleted from this computer. Nothing changes at the provider.".into(),
                    "Remove".into(),
                    move || {
                        let _ = commands.send(Command::RemoveAccount(card_id.clone()));
                    },
                );
                expander.add_suffix(&button);
            }
            if let Some(account) = model.accounts.iter().find(|a| a.card_id() == card_id) {
                for m in metric_settings(account, &model.snapshots, &model.layout) {
                    expander.add_row(&metric_row(&card_id, &m));
                }
            }
            built.push(expander.upcast());
        }
        replace(&self.accounts, &self.account_rows, built);
        let rows = hub_rows(&model.hubs, &model.accounts);
        let mut built = Vec::new();
        for row in rows {
            let action = adw::ActionRow::builder().title(glib::markup_escape_text(&row.host)).subtitle(glib::markup_escape_text(&row.subtitle)).build();
            action.add_prefix(&icons::provider_icon(HUB_ICON));
            let commands = self.commands.clone();
            let (heading, body, label, id) = (
                format!("Remove the Hub at {}?", row.host),
                hub_removal_body(&row),
                format!("Remove Hub and {}", count_text(row.accounts)),
                row.id,
            );
            action.add_suffix(&remove_button(heading, body, label, move || {
                let _ = commands.send(Command::RemoveHub(id));
            }));
            built.push(action.upcast());
        }
        replace(&self.hubs, &self.hub_rows, built);
        let empty = self.account_rows.borrow().is_empty() && self.hub_rows.borrow().is_empty();
        self.content.set_visible_child_name(if empty { "empty" } else { "list" });
    }
}

/// A metric under its account: Show and Pin switches (Pin waits while hidden), and a drag to reorder.
fn metric_row(card_id: &str, metric: &MetricSetting) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(glib::markup_escape_text(&metric.label)).build();
    // `flip` is for Show, which the `set-hidden` action takes the other way round.
    let switch = |label: &str, active: bool, sensitive: bool, action: &'static str, flip: bool| {
        let toggle = gtk::Switch::builder().active(active).sensitive(sensitive).valign(gtk::Align::Center).build();
        let id = metric.id.clone();
        toggle.connect_active_notify(move |t| {
            if t.is_active() != active {
                activate(action, (vec![id.clone()], t.is_active() != flip).to_variant());
            }
        });
        let cell = gtk::Box::builder().spacing(6).build();
        cell.append(&gtk::Label::builder().label(label).css_classes(["dim-label"]).build());
        cell.append(&toggle);
        cell
    };
    row.add_suffix(&switch("Pin", metric.pinned, metric.shown, "set-pinned", false));
    row.add_suffix(&switch("Show", metric.shown, true, "set-hidden", true));
    make_reorderable(&row, Drag::Metric { card_id: card_id.into(), label: metric.label.clone() });
    row
}

/// Swaps the rows of a group for new ones; the group is hidden while it has none.
fn replace(group: &adw::PreferencesGroup, old: &RefCell<Vec<adw::PreferencesRow>>, new: Vec<adw::PreferencesRow>) {
    for row in old.borrow().iter() {
        group.remove(row);
    }
    for row in &new {
        group.add(row);
    }
    group.set_visible(!new.is_empty());
    *old.borrow_mut() = new;
}

fn remove_button(heading: String, body: String, action: String, confirmed: impl Fn() + Clone + 'static) -> gtk::Button {
    let button = gtk::Button::builder().label("Remove").valign(gtk::Align::Center).css_classes(["flat"]).build();
    button.connect_clicked(move |button| {
        let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
        dialog.add_responses(&[("cancel", "Cancel"), ("remove", &action)]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let confirmed = confirmed.clone();
        dialog.connect_response(Some("remove"), move |_, _| confirmed());
        dialog.present(Some(button));
    });
    button
}
