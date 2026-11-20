//! Drag to reorder: a `DragSource` carries a `Drag` as a string, a `DropTarget` turns a drop on its own kind
//! into a move action. The core refuses the moves that are not allowed.

use crate::layout_logic::Drag;
use crate::layout_menu::activate;
use relm4::gtk::prelude::*;
use relm4::gtk::{self, gdk, glib};

/// Makes the widget draggable and a drop target for the same kind of drag.
pub fn make_reorderable(widget: &impl IsA<gtk::Widget>, drag: Drag) {
    make_draggable(widget, drag.clone());
    make_target(widget, drag);
}

/// `handle` starts the drag (a provider group drags by its grip).
pub fn make_draggable(handle: &impl IsA<gtk::Widget>, drag: Drag) {
    let source = gtk::DragSource::builder().actions(gdk::DragAction::MOVE).build();
    source.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&drag.encode().to_value())));
    handle.add_controller(source);
}

pub fn make_target(widget: &impl IsA<gtk::Widget>, target: Drag) {
    let drop = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    drop.connect_drop(move |_, value, _, _| {
        let Some(dragged) = value.get::<String>().ok().and_then(|text| Drag::decode(&text)) else { return false };
        match dragged.dropped_on(&target) {
            Some((name, parameter)) => {
                activate(name, parameter);
                true
            }
            None => false,
        }
    });
    widget.add_controller(drop);
}
