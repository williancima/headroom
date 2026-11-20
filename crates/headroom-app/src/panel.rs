//! The GTK panel (relm4 component). Runs on the main thread and only draws what the worker sends.

use crate::accounts::AccountsPage;
use crate::add::AddFlow;
use crate::history_row::{self, History};
use crate::layout_logic::{self, Drag, Role};
use crate::{layout_menu, reorder};
use crate::resets::Resets;
use crate::share_logic::{self, Kind};
use crate::Command;
use headroom_core::login::LoginEvent;
use headroom_core::view::{priority_groups, usage_groups, Group, PanelModel};
use relm4::adw::prelude::*;
use relm4::gtk::{self, gio, glib};
use relm4::{adw, ComponentParts, ComponentSender, MessageBroker, SimpleComponent};
use std::cell::Cell;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// Lets the worker and the tray hold a sender before the GTK app has started.
pub static BROKER: MessageBroker<PanelMsg> = MessageBroker::new();

/// What an add in progress tells the panel.
#[derive(Debug)]
pub enum AddProgress {
    Event(LoginEvent),
    /// The name of the account or hub that was added.
    Done(String),
    Failed(String),
}

#[derive(Debug)]
pub enum PanelMsg {
    Add(AddProgress),
    Toast(String),
    ShowAccounts,
    AddAccount,
    Model(Box<PanelModel>),
    Refreshing(bool),
    /// A reset's confirm or spinner changed: draw the last model again.
    Redraw,
    /// The visible tab changed.
    TabChanged,
    /// The Priority copy button, or the Copy button of a history row's breakdown.
    CopyPriority,
    CopyBreakdown { provider: String, breakdown: Box<headroom_core::history::BreakdownView> },
    /// The `share-image` action's JSON.
    ShareImage(String),
    /// A claim answered. `settled` is false for a claim that may still land, which a retry resends.
    ClaimDone { request_id: String, message: String, settled: bool },
    Refresh,
    ShowSettings,
    /// Limits that crossed a level, for the desktop to show.
    Notify(Vec<headroom_core::notify::Notice>),
    Toggle,
    Show,
    Quit,
}

pub struct Panel {
    window: adw::ApplicationWindow,
    refresh: gtk::Button,
    copy: gtk::Button,
    stack: adw::ViewStack,
    priority: Page,
    usage: Page,
    nav: adw::NavigationView,
    accounts: AccountsPage,
    add: AddFlow,
    resets: Resets,
    history: History,
    model: Option<PanelModel>,
    toasts: adw::ToastOverlay,
    commands: UnboundedSender<Command>,
    /// Keeps the app alive while the window is hidden.
    _hold: gio::ApplicationHoldGuard,
}

/// "No Accounts" with an Add Account button, shared by the dashboard and the Accounts page.
pub fn empty_state(input: &relm4::Sender<PanelMsg>) -> adw::StatusPage {
    let empty = adw::StatusPage::builder()
        .icon_name("avatar-default-symbolic")
        .title("No Accounts")
        .description("Add an account to see its usage here.")
        .build();
    let add = gtk::Button::builder().label("Add Account").halign(gtk::Align::Center).css_classes(["suggested-action", "pill"]).build();
    let input = input.clone();
    add.connect_clicked(move |_| {
        let _ = input.send(PanelMsg::AddAccount);
    });
    empty.set_child(Some(&add));
    empty
}

/// A tab: its groups, or a status page while there are no accounts.
struct Page {
    root: gtk::Stack,
    groups: gtk::Box,
    scroller: gtk::ScrolledWindow,
}

impl Page {
    fn new(input: &relm4::Sender<PanelMsg>) -> Self {
        let groups = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).valign(gtk::Align::Start).build();
        let clamp = adw::Clamp::builder().child(&groups).margin_top(12).margin_bottom(12).margin_start(12).margin_end(12).build();
        let list = gtk::ScrolledWindow::builder().child(&clamp).vexpand(true).build();
        let empty = empty_state(input);
        let root = gtk::Stack::new();
        root.add_named(&list, Some("list"));
        root.add_named(&empty, Some("empty"));
        Self { root, groups, scroller: list }
    }

    fn show(&self, empty: bool, groups: Vec<Group>, resets: &Resets, history: &History) {
        let adjustment = self.scroller.vadjustment();
        let scrolled = adjustment.value();
        self.root.set_visible_child_name(if empty { "empty" } else { "list" });
        while let Some(child) = self.groups.first_child() {
            self.groups.remove(&child);
        }
        for group in groups {
            let roles = layout_logic::roles(&group);
            let widget = adw::PreferencesGroup::builder().title(glib::markup_escape_text(&group.title)).build();
            // The Usage tab's groups drag by a grip in the header and open the Unify menu on a right click.
            if !group.kind.is_empty() {
                let grip = gtk::Image::from_icon_name("list-drag-handle-symbolic");
                reorder::make_draggable(&grip, Drag::Provider { kind: group.kind.clone() });
                widget.set_header_suffix(Some(&grip));
                reorder::make_target(&widget, Drag::Provider { kind: group.kind.clone() });
                layout_menu::attach_group_menu(&widget, &group);
            }
            for (row, role) in group.rows.iter().zip(roles) {
                let built: gtk::Widget = if !row.resets.is_empty() {
                    resets.row(row).upcast()
                } else if let Some(breakdown) = &row.breakdown {
                    history.row(&group.title, row.card_id.as_deref().unwrap_or_default(), &row.title, breakdown).upcast()
                } else if !row.members.is_empty() {
                    history.members(row).upcast()
                } else {
                    history_row::metric_row(row).upcast()
                };
                layout_menu::attach_row_menu(&built, row);
                match role {
                    Role::Header(card_id) => reorder::make_reorderable(&built, Drag::Account { card_id }),
                    Role::Metric(Some(card_id)) => reorder::make_reorderable(&built, Drag::Metric { card_id, label: row.title.clone() }),
                    _ => {}
                }
                widget.add(&built);
            }
            self.groups.append(&widget);
        }
        // The rebuilt rows start at the top; put the list back where the reader was.
        if scrolled > 0.0 {
            glib::idle_add_local_once(move || adjustment.set_value(scrolled));
        }
    }
}

impl Panel {
    fn draw(&self, model: &PanelModel) {
        let (empty, usage) = (model.accounts.is_empty(), usage_groups(model));
        self.resets.reconcile(&usage);
        self.priority.show(empty, priority_groups(model), &self.resets, &self.history);
        self.usage.show(empty, usage, &self.resets, &self.history);
        self.accounts.show(model);
    }

    /// The copy button shows on the Priority tab while it has rows.
    fn sync_copy(&self) {
        let rows = self.model.as_ref().is_some_and(|m| !priority_groups(m).is_empty());
        self.copy.set_visible(rows && self.stack.visible_child_name().as_deref() == Some("priority"));
    }

    fn copy_image(&self, card: Option<share_logic::ShareCard>) {
        let Some(card) = card else { return };
        let message = match crate::share::copy_card(&card, &self.window) {
            Ok(()) => "Copied to Clipboard".to_string(),
            Err(message) => message,
        };
        self.toasts.add_toast(adw::Toast::builder().title(message).use_markup(false).build());
    }

    /// Renders what the extension asked for to its PNG and tells it on `ShareReady`.
    fn share_image(&self, text: &str) {
        let request = match share_logic::parse_share_request(text) {
            Ok(r) => r,
            Err(error) => return eprintln!("{}: ignoring share-image: {error}", crate::NAME),
        };
        let card = match (&self.model, &request.kind) {
            (None, _) => None,
            (Some(m), Kind::Priority) => Some(share_logic::priority_card(&priority_groups(m))),
            (Some(m), Kind::Breakdown { row_id }) => usage_groups(m).iter().find_map(|g| {
                let row = g.rows.iter().find(|r| r.id.as_deref() == Some(row_id) && r.breakdown.is_some())?;
                Some(share_logic::breakdown_card(&g.title, row.breakdown.as_ref()?))
            }),
        };
        let done = match card {
            Some(card) => crate::share::save_card(&card, &self.window, &request.request_id),
            None => Err("That usage is no longer shown.".to_string()),
        };
        match done {
            Ok(path) => emit_share_ready(&request.request_id, &path, ""),
            Err(error) => emit_share_ready(&request.request_id, "", &error),
        }
    }

    /// Pushes the Accounts page unless it is already in the stack.
    fn show_accounts(&self) {
        if self.nav.find_page(crate::accounts::TAG).is_none() {
            self.nav.push(&self.accounts.page);
        }
    }
}

impl SimpleComponent for Panel {
    type Input = PanelMsg;
    type Output = ();
    type Init = (UnboundedSender<Command>, UnboundedReceiver<Command>);
    type Root = adw::ApplicationWindow;
    type Widgets = ();

    fn init_root() -> Self::Root {
        if let (Some(dir), Some(display)) = (crate::tray::icon_dir(), gtk::gdk::Display::default()) {
            gtk::IconTheme::for_display(&display).add_search_path(dir);
        }
        gtk::Window::set_default_icon_name("dev.headroom.Headroom");
        adw::ApplicationWindow::builder().title("Headroom").default_width(480).width_request(460).default_height(640).hide_on_close(true).visible(false).build()
    }

    fn init((commands, inbox): Self::Init, window: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        crate::start_worker(commands.clone(), inbox);
        let (priority, usage) = (Page::new(sender.input_sender()), Page::new(sender.input_sender()));
        let stack = adw::ViewStack::new();
        stack.add_titled(&priority.root, Some("priority"), "Priority");
        stack.add_titled(&usage.root, Some("usage"), "All Usage");
        stack.set_visible_child_name("priority");

        let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh.set_tooltip_text(Some("Refresh Now"));
        let input = sender.input_sender().clone();
        refresh.connect_clicked(move |_| {
            let _ = input.send(PanelMsg::Refresh);
        });
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build()));
        header.pack_start(&refresh);
        let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
        copy.update_property(&[gtk::accessible::Property::Label("Copy Priority Image")]);
        copy.set_visible(false);
        let input = sender.input_sender().clone();
        copy.connect_clicked(move |_| {
            let _ = input.send(PanelMsg::CopyPriority);
        });
        header.pack_start(&copy);
        let input = sender.input_sender().clone();
        stack.connect_visible_child_name_notify(move |_| {
            let _ = input.send(PanelMsg::TabChanged);
        });
        let people = gtk::Button::from_icon_name("system-users-symbolic");
        people.update_property(&[gtk::accessible::Property::Label("Accounts")]);
        let input = sender.input_sender().clone();
        people.connect_clicked(move |_| {
            let _ = input.send(PanelMsg::ShowAccounts);
        });
        header.pack_end(&people);
        let gear = gtk::Button::from_icon_name("emblem-system-symbolic");
        gear.update_property(&[gtk::accessible::Property::Label("Settings")]);
        let input = sender.input_sender().clone();
        gear.connect_clicked(move |_| {
            let _ = input.send(PanelMsg::ShowSettings);
        });
        header.pack_end(&gear);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&stack));
        let nav = adw::NavigationView::new();
        nav.add(&adw::NavigationPage::new(&view, "Headroom"));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&nav));
        window.set_content(Some(&toasts));
        let accounts = AccountsPage::new(sender.input_sender().clone(), commands.clone());
        let add = AddFlow::new(nav.clone(), toasts.clone(), commands.clone());

        // The first activation is the launch itself and stays hidden; a later one is a second launch.
        let launched = Cell::new(false);
        let input = sender.input_sender().clone();
        relm4::main_application().connect_activate(move |_| {
            if launched.replace(true) {
                let _ = input.send(PanelMsg::Show);
            }
        });

        let app = relm4::main_application();
        let action = |name: &str, msgs: &'static [fn() -> PanelMsg]| {
            let (input, simple) = (sender.input_sender().clone(), gio::SimpleAction::new(name, None));
            simple.connect_activate(move |_, _| msgs.iter().for_each(|msg| drop(input.send(msg()))));
            app.add_action(&simple);
        };
        action("refresh", &[|| PanelMsg::Refresh]);
        action("accounts", &[|| PanelMsg::Show, || PanelMsg::ShowAccounts]);
        action("quit", &[|| PanelMsg::Quit]);
        action("show", &[|| PanelMsg::Show]);
        action("settings", &[|| PanelMsg::Show, || PanelMsg::ShowSettings]);
        // The Toggle Shortcut's keybinding calls this. With the GNOME Shell extension, its menu opens instead.
        let (toggle_input, toggle) = (sender.input_sender().clone(), gio::SimpleAction::new("toggle", None));
        toggle.connect_activate(move |_, _| toggle_extension_menu(toggle_input.clone()));
        app.add_action(&toggle);
        // The GNOME Shell extension claims through this: org.gtk.Actions.Activate with the JSON of `parse_claim_request`.
        let claim = gio::SimpleAction::new("claim-reset", Some(glib::VariantTy::STRING));
        let claim_commands = commands.clone();
        claim.connect_activate(move |_, parameter| {
            let text = parameter.and_then(|p| p.str()).unwrap_or_default();
            match crate::resets_logic::parse_claim_request(text) {
                Ok(r) => drop(claim_commands.send(Command::ClaimReset { card_id: r.card_id, claim: r.claim, request_id: r.request_id })),
                Err(error) => eprintln!("{}: ignoring claim-reset: {error}", crate::NAME),
            }
        });
        app.add_action(&claim);
        // The extension asks for a share image through this: the JSON of `share_logic::parse_share_request`.
        let share = gio::SimpleAction::new("share-image", Some(glib::VariantTy::STRING));
        let share_input = sender.input_sender().clone();
        share.connect_activate(move |_, parameter| {
            let _ = share_input.send(PanelMsg::ShareImage(parameter.and_then(|p| p.str()).unwrap_or_default().to_string()));
        });
        app.add_action(&share);
        layout_menu::register(&app, &commands);

        let hold = app.hold();
        let resets = Resets::new(sender.input_sender().clone(), commands.clone());
        ComponentParts { model: Panel { window, refresh, copy, stack, priority, usage, nav, accounts, add, resets, history: History::default(), model: None, toasts, commands, _hold: hold }, widgets: () }
    }

    fn update(&mut self, msg: PanelMsg, _sender: ComponentSender<Self>) {
        match msg {
            PanelMsg::Model(model) => {
                self.draw(&model);
                self.model = Some(*model);
                self.sync_copy();
            }
            PanelMsg::TabChanged => self.sync_copy(),
            PanelMsg::CopyPriority => {
                let card = self.model.as_ref().map(|m| share_logic::priority_card(&priority_groups(m)));
                self.copy_image(card);
            }
            PanelMsg::CopyBreakdown { provider, breakdown } => self.copy_image(Some(share_logic::breakdown_card(&provider, &breakdown))),
            PanelMsg::ShareImage(text) => self.share_image(&text),
            PanelMsg::Redraw => {
                if let Some(model) = &self.model {
                    self.draw(model);
                }
            }
            PanelMsg::ClaimDone { request_id, message, settled } => {
                self.resets.finish(&request_id, settled);
                emit_claim_done(&request_id, &message, settled);
                self.toasts.add_toast(adw::Toast::builder().title(message).use_markup(false).build());
                if let Some(model) = &self.model {
                    self.draw(model);
                }
            }
            PanelMsg::Add(progress) => self.add.progress(progress),
            PanelMsg::Toast(text) => self.toasts.add_toast(adw::Toast::builder().title(text).use_markup(false).build()),
            PanelMsg::ShowAccounts => self.show_accounts(),
            PanelMsg::AddAccount => {
                self.show_accounts();
                self.add.open_picker();
            }
            PanelMsg::Refreshing(busy) => self.refresh.set_sensitive(!busy),
            PanelMsg::Refresh => {
                let _ = self.commands.send(Command::Refresh);
            }
            PanelMsg::ShowSettings => crate::settings::open(&self.window, self.commands.clone()),
            PanelMsg::Notify(notices) => {
                let app = relm4::main_application();
                for notice in notices {
                    let note = gio::Notification::new(&notice.title);
                    note.set_body(Some(&notice.body));
                    note.set_default_action("app.show");
                    app.send_notification(Some(&notice.id), &note);
                }
            }
            PanelMsg::Toggle if self.window.is_visible() => self.window.set_visible(false),
            PanelMsg::Toggle | PanelMsg::Show => self.window.present(),
            PanelMsg::Quit => relm4::main_application().quit(),
        }
    }
}

/// Tells the GNOME Shell extension how a claim ended: signal `dev.headroom.Headroom.Resets.ClaimDone(s, s, b)`
/// on `/dev/headroom/Headroom`. Emitted for every claim, whoever asked.
fn emit_claim_done(request_id: &str, message: &str, settled: bool) {
    let Some(bus) = relm4::main_application().dbus_connection() else { return };
    let args = (request_id, message, settled).to_variant();
    if let Err(error) = bus.emit_signal(None, "/dev/headroom/Headroom", "dev.headroom.Headroom.Resets", "ClaimDone", Some(&args)) {
        eprintln!("{}: could not emit ClaimDone: {error}", crate::NAME);
    }
}

/// Tells the extension a share image is ready: signal `dev.headroom.Headroom.Share.ShareReady(s, s, s)` (request id,
/// PNG path, error; the path is empty on failure) on `/dev/headroom/Headroom`.
fn emit_share_ready(request_id: &str, path: &str, error: &str) {
    let Some(bus) = relm4::main_application().dbus_connection() else { return };
    let args = (request_id, path, error).to_variant();
    if let Err(error) = bus.emit_signal(None, "/dev/headroom/Headroom", "dev.headroom.Headroom.Share", "ShareReady", Some(&args)) {
        eprintln!("{}: could not emit ShareReady: {error}", crate::NAME);
    }
}

/// Asks the GNOME Shell extension to open or close its menu (`dev.headroom.Extension.ToggleMenu()`). Without the
/// extension, or with an older one that lacks the method, the call fails and the panel window toggles instead.
fn toggle_extension_menu(input: relm4::Sender<PanelMsg>) {
    let Some(bus) = relm4::main_application().dbus_connection() else {
        return drop(input.send(PanelMsg::Toggle));
    };
    bus.call(
        Some("dev.headroom.Extension"),
        "/dev/headroom/Extension",
        "dev.headroom.Extension",
        "ToggleMenu",
        None,
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        2000,
        gio::Cancellable::NONE,
        move |reply| {
            if reply.is_err() {
                let _ = input.send(PanelMsg::Toggle);
            }
        },
    );
}
