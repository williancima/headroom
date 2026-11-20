//! The Settings dialog. Every change is saved to `settings.json` at once and the worker is told to reread it.

use crate::settings_logic::{autostart_contents, autostart_path, critical_max, proxy_input};
use crate::{shortcut, Command, NAME};
use headroom_core::settings::{Settings, REFRESH_CHOICES};
use headroom_core::store::Store;
use relm4::adw::prelude::*;
use relm4::gtk::{self, glib};
use relm4::adw;
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::mpsc::UnboundedSender;

/// Edits the shared settings, saves them and tells the worker.
type Save = Rc<dyn Fn(&dyn Fn(&mut Settings))>;

fn switch_row(title: &str, active: bool, changed: impl Fn(bool) + 'static) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).active(active).build();
    row.connect_active_notify(move |row| changed(row.is_active()));
    row
}

fn spin_row(title: &str, (lo, hi): (i64, i64), value: i64) -> adw::SpinRow {
    let adjustment = gtk::Adjustment::new(value as f64, lo as f64, hi as f64, 5.0, 5.0, 0.0);
    adw::SpinRow::builder().title(title).adjustment(&adjustment).build()
}

pub fn open(parent: &impl IsA<gtk::Widget>, commands: UnboundedSender<Command>) {
    let store = match Store::default_paths() {
        Ok(store) => store,
        Err(error) => return eprintln!("{NAME}: could not open the settings: {error}"),
    };
    let current = store.settings().unwrap_or_else(|error| {
        eprintln!("{NAME}: could not read the settings: {error}");
        Settings::default()
    });
    let shared = Rc::new(RefCell::new(current.clone()));
    let save: Save = Rc::new(move |edit| {
        let mut settings = shared.borrow_mut();
        edit(&mut settings);
        match store.save_settings(&settings) {
            Ok(()) => drop(commands.send(Command::SettingsChanged)),
            Err(error) => eprintln!("{NAME}: could not save the settings: {error}"),
        }
    });

    let dialog = adw::PreferencesDialog::builder().title("Settings").build();
    let page = adw::PreferencesPage::new();
    dialog.add(&page);

    let general = adw::PreferencesGroup::builder().title("General").build();
    general.add(&autostart_row());
    let choices: Vec<String> = REFRESH_CHOICES.iter().map(|m| format!("{m} Minutes")).collect();
    let refresh = adw::ComboRow::builder().title("Refresh Every").model(&gtk::StringList::new(&choices.iter().map(String::as_str).collect::<Vec<_>>())).build();
    refresh.set_selected(REFRESH_CHOICES.iter().position(|m| *m == current.refresh_minutes).unwrap_or(0) as u32);
    let on_refresh = save.clone();
    refresh.connect_selected_notify(move |row| {
        if let Some(minutes) = REFRESH_CHOICES.get(row.selected() as usize) {
            on_refresh(&|s| s.refresh_minutes = *minutes);
        }
    });
    general.add(&refresh);
    general.add(&shortcut_row(dialog.clone()));
    page.add(&general);

    let thresholds = adw::PreferencesGroup::builder().title("Thresholds").description("Percent left at or under which a limit changes level.").build();
    let warning = spin_row("Warning", headroom_core::settings::WARNING_RANGE, current.warning);
    let critical = spin_row("Critical", (headroom_core::settings::CRITICAL_RANGE.0, critical_max(current.warning)), current.critical);
    let (on_warning, critical_row) = (save.clone(), critical.clone());
    warning.connect_value_notify(move |row| {
        let value = row.value() as i64;
        // Critical's ceiling follows; setting the upper bound clamps its value, which saves through its own handler.
        critical_row.adjustment().set_upper(critical_max(value) as f64);
        on_warning(&|s| s.warning = value);
    });
    let on_critical = save.clone();
    critical.connect_value_notify(move |row| {
        let value = row.value() as i64;
        on_critical(&|s| s.critical = value);
    });
    thresholds.add(&warning);
    thresholds.add(&critical);
    page.add(&thresholds);

    let notifications = adw::PreferencesGroup::builder().title("Notifications").build();
    let n = current.notify;
    for (title, on, set) in [
        ("Critical", n.critical, (|s: &mut Settings, v| s.notify.critical = v) as fn(&mut Settings, bool)),
        ("Warning", n.warning, |s, v| s.notify.warning = v),
        ("Burning Fast", n.burning_fast, |s, v| s.notify.burning_fast = v),
        ("Use It or Lose It", n.use_it_or_lose_it, |s, v| s.notify.use_it_or_lose_it = v),
    ] {
        let save = save.clone();
        notifications.add(&switch_row(title, on, move |v| save(&|s| set(s, v))));
    }
    page.add(&notifications);

    let network = adw::PreferencesGroup::builder().title("Network").description("Example: socks5://localhost:9050. No user name or password.").build();
    let proxy = adw::EntryRow::builder().title("Proxy").text(current.proxy.clone().unwrap_or_default()).show_apply_button(true).build();
    let (on_proxy, toaster) = (save.clone(), dialog.clone());
    proxy.connect_apply(move |row| match proxy_input(&row.text()) {
        Ok(value) => {
            row.remove_css_class("error");
            on_proxy(&|s| s.proxy = value.clone());
        }
        Err(message) => {
            row.add_css_class("error");
            toaster.add_toast(adw::Toast::builder().title(message).use_markup(false).build());
        }
    });
    network.add(&proxy);
    page.add(&network);

    dialog.present(Some(parent));
}

/// Launch at Login: its state is whether the autostart file exists.
fn autostart_row() -> adw::SwitchRow {
    let path = autostart_path(&glib::user_config_dir());
    let row = adw::SwitchRow::builder().title("Launch at Login").active(path.exists()).build();
    row.connect_active_notify(move |row| {
        let result = if row.is_active() {
            crate::setup::app_exe().and_then(|exe| {
                std::fs::create_dir_all(path.parent().unwrap_or(&path))?;
                std::fs::write(&path, autostart_contents(&exe))
            })
        } else {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        };
        if let Err(error) = result {
            eprintln!("{NAME}: could not change Launch at Login: {error}");
            // Show what is on disk.
            let on_disk = path.exists();
            if on_disk != row.is_active() {
                row.set_active(on_disk);
            }
        }
    });
    row
}

fn shortcut_row(dialog: adw::PreferencesDialog) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title("Toggle Shortcut").build();
    if !shortcut::available() {
        row.set_subtitle("Shortcuts need GNOME Settings");
        row.set_sensitive(false);
        return row;
    }
    let button = gtk::Button::builder().label(shortcut::label(shortcut::current().as_deref())).valign(gtk::Align::Center).build();
    let shown = button.clone();
    button.connect_clicked(move |button| {
        let (shown, dialog) = (shown.clone(), dialog.clone());
        shortcut::capture(button, move |accelerator| match shortcut::apply(accelerator.as_deref()) {
            Ok(()) => shown.set_label(&shortcut::label(accelerator.as_deref())),
            Err(message) => dialog.add_toast(adw::Toast::builder().title(message).use_markup(false).build()),
        });
    });
    row.add_suffix(&button);
    row
}
