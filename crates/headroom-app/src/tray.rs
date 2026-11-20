//! The StatusNotifierItem (ksni), whose text label is the `XAyatanaLabel` property (vendored ksni patch).

use crate::panel::PanelMsg;
use crate::Command;
use ksni::menu::{MenuItem, StandardItem};
use ksni::{ToolTip, Tray};
use relm4::gtk::{gio, glib};
use relm4::gtk::prelude::*;
use relm4::Sender;
use tokio::sync::mpsc::UnboundedSender;

const ICON: &str = "dev.headroom.Headroom-symbolic";
const EXTENSION_NAME: &str = "dev.headroom.Extension";

/// True when the GNOME Shell extension owns its bus name, so it draws the label and the tray is skipped.
/// Blocking GDBus call (its own thread, not zbus), so run it with `spawn_blocking`.
pub fn extension_present() -> bool {
    let ask = || -> Result<bool, glib::Error> {
        let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
        let reply = bus.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(EXTENSION_NAME,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
        )?;
        Ok(reply.get::<(bool,)>().is_some_and(|(owned,)| owned))
    };
    ask().unwrap_or(false)
}

/// Icons next to the binary: `<prefix>/share/icons` (AppImage, /usr) or the source tree's `data/icons`
/// when running from `target/<profile>`. The tray host is another process, so it needs this path too.
pub fn icon_dir() -> Option<std::path::PathBuf> {
    let bin = std::env::current_exe().ok()?;
    let bin = bin.parent()?;
    [bin.join("../share/icons"), bin.join("../../data/icons")].into_iter().find(|d| d.join("hicolor").is_dir())
}

pub struct HeadroomTray {
    pub label: String,
    pub tooltip: String,
    pub commands: UnboundedSender<Command>,
    /// Straight to the panel, not via the worker.
    pub ui: Sender<PanelMsg>,
}

impl Tray for HeadroomTray {
    fn id(&self) -> String {
        "headroom".into()
    }

    fn title(&self) -> String {
        if self.label.is_empty() { "Headroom".into() } else { self.label.clone() }
    }

    fn icon_name(&self) -> String {
        ICON.into()
    }

    fn icon_theme_path(&self) -> String {
        icon_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()
    }

    fn x_ayatana_label(&self) -> String {
        self.label.clone()
    }

    fn x_ayatana_label_guide(&self) -> String {
        self.label.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip { icon_name: ICON.into(), title: self.title(), description: self.tooltip.clone(), ..Default::default() }
    }

    /// Left click, where the host supports it.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.ui.send(PanelMsg::Toggle);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let item = |label: &str, action: fn(&mut Self)| {
            StandardItem { label: label.into(), activate: Box::new(action), ..Default::default() }.into()
        };
        vec![
            item("Open Headroom", |t| {
                let _ = t.ui.send(PanelMsg::Show);
            }),
            item("Refresh Now", |t| {
                let _ = t.commands.send(Command::Refresh);
            }),
            item("Quit", |t| {
                let _ = t.commands.send(Command::Quit);
            }),
        ]
    }
}
