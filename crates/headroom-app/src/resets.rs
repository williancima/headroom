//! The "Rate Limit Resets" row of the All Usage tab: an expander listing the account's resets, each with Use,
//! an inline confirm and a spinner while the claim runs. What survives a redraw lives in `resets_logic::Flow`.

use crate::panel::PanelMsg;
use crate::resets_logic::{keys, Flow, Key, Step};
use crate::Command;
use headroom_core::view::{Group, ResetEntry, Row};
use relm4::adw::prelude::*;
use relm4::gtk::{self, glib};
use relm4::{adw, Sender};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use tokio::sync::mpsc::UnboundedSender;

const GENERAL_DETAIL: &str = "Immediately reset your usage limits. This can't be undone.";

#[derive(Clone)]
pub struct Resets {
    flow: Rc<RefCell<Flow>>,
    commands: UnboundedSender<Command>,
    input: Sender<PanelMsg>,
}

impl Resets {
    pub fn new(input: Sender<PanelMsg>, commands: UnboundedSender<Command>) -> Self {
        Self { flow: Rc::default(), commands, input }
    }

    /// Cancels the confirms whose rows a new reading dropped.
    pub fn reconcile(&self, groups: &[Group]) {
        let live: HashSet<Key> =
            groups.iter().flat_map(|g| &g.rows).filter_map(|r| r.card_id.as_deref().map(|id| keys(id, &r.resets))).flatten().collect();
        self.flow.borrow_mut().reconcile(&live);
    }

    /// The claim answered: its row leaves the spinner.
    pub fn finish(&self, request_id: &str, settled: bool) {
        self.flow.borrow_mut().finish(request_id, settled);
    }

    pub fn row(&self, row: &Row) -> adw::ExpanderRow {
        let card_id = row.card_id.clone().unwrap_or_default();
        let expander = adw::ExpanderRow::builder().title(glib::markup_escape_text(&row.title)).expanded(self.flow.borrow().is_open(&card_id)).build();
        if let Some(value) = &row.value {
            expander.add_suffix(&gtk::Label::builder().label(value).css_classes(["dim-label"]).build());
        }
        let flow = self.flow.clone();
        let open_id = card_id.clone();
        expander.connect_expanded_notify(move |e| flow.borrow_mut().set_open(&open_id, e.is_expanded()));
        for (key, entry) in keys(&card_id, &row.resets).into_iter().zip(&row.resets) {
            expander.add_row(&self.reset_row(key, entry));
        }
        expander
    }

    fn reset_row(&self, key: Key, entry: &ResetEntry) -> adw::ActionRow {
        let step = self.flow.borrow().step(&key);
        let subtitle = match step {
            Some(Step::Confirming) => format!("Use this reset? {}", entry.confirm_detail.as_deref().unwrap_or(GENERAL_DETAIL)),
            _ => entry.expires.clone(),
        };
        let action = adw::ActionRow::builder().title(glib::markup_escape_text(&entry.title)).subtitle(glib::markup_escape_text(&subtitle)).build();
        let button = |label: &str, classes: &[&str]| gtk::Button::builder().label(label).valign(gtk::Align::Center).css_classes(classes.to_vec()).build();
        match step {
            None if entry.usable => {
                let use_it = button("Use", &[]);
                let this = self.clone();
                use_it.connect_clicked(move |_| this.change(|flow| flow.ask(key.clone())));
                action.add_suffix(&use_it);
            }
            None => {}
            Some(Step::Confirming) => {
                let (cancel, confirm) = (button("Cancel", &["flat"]), button("Use", &["suggested-action"]));
                let (this, k) = (self.clone(), key.clone());
                cancel.connect_clicked(move |_| this.change(|flow| flow.cancel(&k)));
                let (this, claim) = (self.clone(), entry.claim.clone());
                confirm.connect_clicked(move |_| {
                    let request_id = this.flow.borrow_mut().confirm(&key);
                    if let Some(request_id) = request_id {
                        let _ = this.commands.send(Command::ClaimReset { card_id: key.card_id.clone(), claim: claim.clone(), request_id });
                    }
                    let _ = this.input.send(PanelMsg::Redraw);
                });
                action.add_suffix(&cancel);
                action.add_suffix(&confirm);
            }
            Some(Step::Claiming) => action.add_suffix(&gtk::Spinner::builder().spinning(true).valign(gtk::Align::Center).build()),
        }
        action
    }

    fn change(&self, f: impl FnOnce(&mut Flow)) {
        f(&mut self.flow.borrow_mut());
        let _ = self.input.send(PanelMsg::Redraw);
    }
}
