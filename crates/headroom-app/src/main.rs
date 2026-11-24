//! `headroom-app`: refreshes every account in the background, shows what is running out in the tray and
//! opens a GTK4 panel. GTK runs on the main thread; the refresh loop and the tray run on a worker thread.

mod accounts;
mod accounts_logic;
mod add;
mod history_row;
mod icons;
mod layout_apply;
mod layout_logic;
mod layout_menu;
mod logic;
mod panel;
mod reorder;
mod resets;
mod resets_logic;
mod settings;
mod settings_logic;
mod setup;
mod setup_logic;
mod share;
mod share_logic;
mod shortcut;
mod tray;
mod worker;

use headroom_core::claim::ResetRef;
use headroom_core::copilot::GitHubDomain;
use headroom_core::ProviderKind;
use panel::{Panel, PanelMsg, BROKER};
use relm4::gtk::{gio, glib, prelude::*};
use relm4::RelmApp;
use std::ops::ControlFlow;
use std::panic::AssertUnwindSafe;
use uuid::Uuid;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

const NAME: &str = "headroom-app";
const APP_ID: &str = "dev.headroom.Headroom";

pub enum Command {
    Refresh,
    Quit,
    /// The Settings window saved: reread them, apply the proxy and redraw.
    SettingsChanged,
    AddOAuth { kind: ProviderKind, domain: GitHubDomain },
    AddApiKey { kind: ProviderKind, key: String },
    AddCopilotOrganization { organization: String, token: String },
    AddHub { address: String, key: String },
    RemoveAccount(String),
    RemoveHub(Uuid),
    /// Aborts the add in progress, if any.
    CancelAdd,
    /// Sent by a finished add task: show it and refresh.
    Added(String),
    /// Spends one reset; `request_id` stays the same across retries of an unsettled claim.
    ClaimReset { card_id: String, claim: ResetRef, request_id: String },
    /// Line ids to hide or show; hiding unpins.
    SetHidden { ids: Vec<String>, hidden: bool },
    SetPinned { ids: Vec<String>, pinned: bool },
    SetUnified { kind: ProviderKind, on: bool },
    MoveMetric { card_id: String, dragged: String, target: String },
    MoveAccount { dragged: String, target: String },
    MoveProvider { dragged: ProviderKind, target: ProviderKind },
}

/// Started from `Panel::init`, which only runs in the primary instance: a second launch just activates the first.
fn start_worker(commands: UnboundedSender<Command>, inbox: UnboundedReceiver<Command>) {
    setup::run_startup();
    let ui = BROKER.sender().clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("could not start the async runtime");
        let ran = std::panic::catch_unwind(AssertUnwindSafe(|| runtime.block_on(worker::run(commands, inbox, ui.clone()))));
        match ran {
            Ok(Ok(())) => {
                let _ = ui.send(PanelMsg::Quit);
            }
            Ok(Err(message)) => {
                eprintln!("{NAME}: {message}");
                std::process::exit(1);
            }
            // The panic message is already printed; without the worker the panel would show stale data forever.
            Err(_) => std::process::exit(1),
        }
    });
}

fn main() {
    // The .deb's login autostart: GNOME only loads a system extension it saw at login, and the app may not have run yet.
    if std::env::args().nth(1).as_deref() == Some("--enable-extension") {
        return setup::enable_extension_once();
    }
    // The GL/Vulkan renderer keeps ~70 MB after the panel is first shown, even hidden; cairo keeps ~15 MB and
    // is plenty for a few lists. Set before GTK starts and before any other thread exists.
    if std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "cairo");
    }
    // The first launch starts hidden; `Panel` shows the window when a later launch activates this process.
    let relm = RelmApp::new(APP_ID).with_broker(&BROKER).visible_on_activate(false);
    // Login autostart and the extension's D-Bus start race at login. The loser would activate the winner, which
    // opens its window, so a `--background` start that finds Headroom running exits instead.
    let app = relm4::main_application();
    app.add_main_option("background", glib::Char::from(0), glib::OptionFlags::NONE, glib::OptionArg::None, "Start without the window", None);
    app.connect_handle_local_options(|app, options| {
        if !options.contains("background") {
            return ControlFlow::Continue(());
        }
        match app.register(gio::Cancellable::NONE) {
            Ok(()) if app.is_remote() => ControlFlow::Break(glib::ExitCode::SUCCESS),
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => {
                eprintln!("{NAME}: could not register on the session bus: {error}");
                ControlFlow::Break(glib::ExitCode::FAILURE)
            }
        }
    });
    relm.run::<Panel>(unbounded_channel());
}
