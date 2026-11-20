//! The Toggle Shortcut: a GNOME custom keybinding that calls the app's `toggle` action, and the dialog that records a key.

use crate::settings_logic::{with_path, without_path, KEYBINDING_PATH, TOGGLE_COMMAND};
use relm4::adw::prelude::*;
use relm4::gtk::{self, gdk, gio};
use relm4::adw;

const LIST_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const BINDING_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const LIST_KEY: &str = "custom-keybindings";

/// False outside GNOME, where there is nothing to write the keybinding to.
pub fn available() -> bool {
    gio::SettingsSchemaSource::default().is_some_and(|s| s.lookup(LIST_SCHEMA, true).is_some() && s.lookup(BINDING_SCHEMA, true).is_some())
}

fn binding_settings() -> gio::Settings {
    gio::Settings::with_path(BINDING_SCHEMA, KEYBINDING_PATH)
}

/// The accelerator bound now, if our keybinding is registered.
pub fn current() -> Option<String> {
    let list: Vec<String> = gio::Settings::new(LIST_SCHEMA).strv(LIST_KEY).iter().map(|s| s.to_string()).collect();
    if !list.iter().any(|p| p == KEYBINDING_PATH) {
        return None;
    }
    Some(binding_settings().string("binding").to_string()).filter(|b| !b.is_empty())
}

/// What the row shows for an accelerator.
pub fn label(accelerator: Option<&str>) -> String {
    accelerator
        .and_then(gtk::accelerator_parse)
        .map(|(key, mods)| gtk::accelerator_get_label(key, mods).to_string())
        .unwrap_or_else(|| "Not Set".into())
}

/// Binds the key, or with `None` removes the keybinding. Appends to the list and never touches other entries.
pub fn apply(accelerator: Option<&str>) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("Could not save the shortcut: {e}");
    let (list, custom) = (gio::Settings::new(LIST_SCHEMA), binding_settings());
    let paths: Vec<String> = list.strv(LIST_KEY).iter().map(|s| s.to_string()).collect();
    match accelerator {
        Some(accelerator) => {
            custom.set_string("name", "Headroom Toggle").map_err(|e| fail(&e))?;
            custom.set_string("command", TOGGLE_COMMAND).map_err(|e| fail(&e))?;
            custom.set_string("binding", accelerator).map_err(|e| fail(&e))?;
            if let Some(next) = with_path(&paths, KEYBINDING_PATH) {
                list.set_strv(LIST_KEY, next).map_err(|e| fail(&e))?;
            }
        }
        None => {
            if let Some(next) = without_path(&paths, KEYBINDING_PATH) {
                list.set_strv(LIST_KEY, next).map_err(|e| fail(&e))?;
            }
            for key in ["name", "command", "binding"] {
                custom.reset(key);
            }
        }
    }
    gio::Settings::sync();
    Ok(())
}

/// Asks for a key. Escape cancels, Backspace clears (`Some(None)`), a key with a modifier is `Some(Some(accelerator))`.
pub fn capture(parent: &impl IsA<gtk::Widget>, done: impl Fn(Option<String>) + 'static) {
    let dialog = adw::Dialog::builder().title("Toggle Shortcut").content_width(320).build();
    let page = adw::StatusPage::builder()
        .icon_name("input-keyboard-symbolic")
        .title("Press a Shortcut")
        .description("Escape cancels, Backspace clears.")
        .build();
    dialog.set_child(Some(&page));
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let closing = dialog.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        let mods = state & gtk::accelerator_get_default_mod_mask();
        if key == gdk::Key::Escape {
            closing.close();
        } else if key == gdk::Key::BackSpace && mods.is_empty() {
            done(None);
            closing.close();
        } else if !mods.is_empty() && gtk::accelerator_valid(key, mods) {
            done(Some(gtk::accelerator_name(key.to_lower(), mods).to_string()));
            closing.close();
        }
        gtk::glib::Propagation::Stop
    });
    dialog.add_controller(keys);
    dialog.present(Some(parent));
}
