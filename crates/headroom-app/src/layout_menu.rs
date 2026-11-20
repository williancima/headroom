//! The customization actions and the right-click menus. Every change goes through an app action, the same
//! ones the GNOME Shell extension calls over D-Bus, so the worker is the only writer.

use crate::layout_logic::{command, menu_ids, ACTIONS};
use crate::Command;
use headroom_core::view::{Group, Row};
use relm4::gtk::{self, gdk, gio, glib};
use relm4::gtk::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

/// Registers `set-hidden`, `set-pinned`, `set-unified`, `move-metric`, `move-account` and `move-provider`.
pub fn register(app: &gtk::Application, commands: &UnboundedSender<Command>) {
    for (name, signature) in ACTIONS {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::new(signature).expect("a valid variant type")));
        let commands = commands.clone();
        action.connect_activate(move |_, parameter| {
            let Some(parameter) = parameter else { return eprintln!("{}: ignoring {name}: no parameter", crate::NAME) };
            match command(name, parameter) {
                Ok(command) => drop(commands.send(command)),
                Err(error) => eprintln!("{}: ignoring {name}: {error}", crate::NAME),
            }
        });
        app.add_action(&action);
    }
}

/// Calls an app action from a widget.
pub fn activate(name: &str, parameter: glib::Variant) {
    relm4::main_application().activate_action(name, Some(&parameter));
}

/// Right click on a metric row: Pin or Unpin, and Hide. A row without an id has no menu.
pub fn attach_row_menu(widget: &impl IsA<gtk::Widget>, row: &Row) {
    let Some(ids) = menu_ids(row) else { return };
    let pin = if row.pinned { "Unpin" } else { "Pin" };
    let (pin_ids, hide_ids, pinned) = (ids.clone(), ids, !row.pinned);
    attach(widget, &[
        item(pin, move || activate("set-pinned", (pin_ids.clone(), pinned).to_variant())),
        item("Hide", move || activate("set-hidden", (hide_ids.clone(), true).to_variant())),
    ]);
}

/// Right click in a provider group: Unify Multiple Accounts, when the provider has two or more accounts.
pub fn attach_group_menu(widget: &impl IsA<gtk::Widget>, group: &Group) {
    if !group.unifiable {
        return;
    }
    let (kind, on) = (group.kind.clone(), !group.unified);
    let check = gtk::CheckButton::builder().label("Unify Multiple Accounts").active(group.unified).build();
    check.connect_toggled(move |_| activate("set-unified", (kind.as_str(), on).to_variant()));
    attach(widget, &[check.upcast()]);
}

fn item(label: &str, on_click: impl Fn() + 'static) -> gtk::Widget {
    let button = gtk::Button::builder().label(label).css_classes(["flat"]).build();
    button.connect_clicked(move |_| on_click());
    button.upcast()
}

fn attach(widget: &impl IsA<gtk::Widget>, items: &[gtk::Widget]) {
    let list = gtk::Box::builder().orientation(gtk::Orientation::Vertical).margin_top(2).margin_bottom(2).margin_start(2).margin_end(2).build();
    let popover = gtk::Popover::builder().child(&list).has_arrow(false).halign(gtk::Align::Start).build();
    for item in items {
        list.append(item);
        // A pick closes the menu; the check toggles first, then the model redraws the rows.
        if let Some(button) = item.downcast_ref::<gtk::Button>() {
            let popover = popover.clone();
            button.connect_clicked(move |_| popover.popdown());
        } else if let Some(check) = item.downcast_ref::<gtk::CheckButton>() {
            let popover = popover.clone();
            check.connect_toggled(move |_| popover.popdown());
        }
    }
    popover.set_parent(widget);
    let gesture = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
    let shown = popover.clone();
    gesture.connect_pressed(move |gesture, _, x, y| {
        // The row's own menu wins over the group's: the group's gesture sees a claimed sequence.
        gesture.set_state(gtk::EventSequenceState::Claimed);
        shown.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        shown.popup();
    });
    widget.add_controller(gesture);
    // Rows are rebuilt on every model; a popover left parented would warn when its row is finalized.
    widget.connect_destroy(move |_| popover.unparent());
}
