//! The add flow: a picker page, then one step page per provider kind. The worker does the work; this only
//! draws what the user must do and what went wrong.

use crate::accounts_logic::{parse_domain, picker, Choice};
use crate::icons;
use crate::panel::AddProgress;
use crate::Command;
use headroom_core::copilot::GitHubDomain;
use headroom_core::login::{is_oauth_kind, LoginEvent};
use headroom_core::ProviderKind;
use relm4::adw::prelude::*;
use relm4::gtk::{self, gio};
use relm4::adw;
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone)]
pub struct AddFlow {
    nav: adw::NavigationView,
    toasts: adw::ToastOverlay,
    commands: UnboundedSender<Command>,
    /// The step waiting for the worker, if any.
    step: Rc<RefCell<Option<Step>>>,
}

#[derive(Clone)]
struct Step {
    /// The browser hint; a device code brings its own text.
    message: gtk::Label,
    middle: gtk::Box,
    error: gtk::Label,
    spinner: gtk::Spinner,
    button: gtk::Button,
    /// Sign-in steps start by themselves, so the button only offers a retry after a failure.
    retry_only: bool,
}

impl Step {
    fn busy(&self, busy: bool) {
        self.spinner.set_visible(busy);
        self.spinner.set_spinning(busy);
        self.button.set_sensitive(!busy);
        self.button.set_visible(!(busy && self.retry_only));
        self.middle.set_sensitive(!busy);
    }
}

type Open = fn(&AddFlow);

fn label(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).wrap(true).xalign(0.0).build()
}

fn column() -> gtk::Box {
    gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).valign(gtk::Align::Start).build()
}

fn page(title: &str, body: &gtk::Box) -> adw::NavigationPage {
    let clamp = adw::Clamp::builder().child(body).margin_top(18).margin_bottom(18).margin_start(12).margin_end(12).build();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&gtk::ScrolledWindow::builder().child(&clamp).vexpand(true).build()));
    adw::NavigationPage::new(&view, title)
}

/// Rows in a boxed list, with the optional note under it.
fn form(rows: &[&gtk::Widget], note: Option<&str>) -> gtk::Box {
    let (middle, group) = (column(), adw::PreferencesGroup::new());
    for row in rows {
        group.add(*row);
    }
    middle.append(&group);
    if let Some(note) = note {
        middle.append(&gtk::Label::builder().label(note).wrap(true).xalign(0.0).css_classes(["dim-label", "caption"]).build());
    }
    middle
}

fn copy_button(text: &str, title: &str) -> gtk::Button {
    let button = gtk::Button::builder().label(title).halign(gtk::Align::Start).build();
    let text = text.to_string();
    button.connect_clicked(move |b| b.clipboard().set_text(&text));
    button
}

fn selectable(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder().label(text).selectable(true).wrap(true).xalign(0.0).css_classes(classes.to_vec()).build()
}

impl AddFlow {
    pub fn new(nav: adw::NavigationView, toasts: adw::ToastOverlay, commands: UnboundedSender<Command>) -> Self {
        // Leaving a step by any route abandons its sign-in.
        let cancel = commands.clone();
        nav.connect_popped(move |_, _| {
            let _ = cancel.send(Command::CancelAdd);
        });
        Self { nav, toasts, commands, step: Rc::default() }
    }

    fn window(&self) -> Option<gtk::Window> {
        self.nav.root().and_then(|r| r.downcast::<gtk::Window>().ok())
    }

    pub fn open_picker(&self) {
        let list = adw::PreferencesGroup::builder().description("Choose a provider to sign in to.").build();
        for choice in picker() {
            let row = adw::ActionRow::builder().title(choice.title()).activatable(true).build();
            row.add_prefix(&icons::provider_icon(choice.icon_id()));
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let flow = self.clone();
            row.connect_activated(move |_| flow.choose(choice));
            list.add(&row);
        }
        let body = column();
        body.append(&list);
        self.nav.push(&page("Add Account", &body));
    }

    fn choose(&self, choice: Choice) {
        match choice {
            Choice::Hub => self.hub_step(),
            Choice::Provider(ProviderKind::Copilot) => self.copilot_options(),
            Choice::Provider(kind) if is_oauth_kind(kind) => self.sign_in_step(kind, GitHubDomain::Github),
            Choice::Provider(kind) => self.api_key_step(kind),
        }
    }

    /// Shows a step and remembers it for the worker's answers. `submit` builds the command, or the message to show.
    fn push_step(&self, title: &str, message: &str, middle: gtk::Box, button_label: &str, retry_only: bool, submit: impl Fn() -> Result<Command, String> + 'static) -> gtk::Button {
        let error = label("");
        error.add_css_class("error");
        error.set_visible(false);
        let spinner = gtk::Spinner::builder().visible(false).build();
        let button = gtk::Button::builder().label(button_label).css_classes(["suggested-action", "pill"]).build();
        let actions = gtk::Box::builder().spacing(12).halign(gtk::Align::End).build();
        let cancel = gtk::Button::with_label("Cancel");
        let nav = self.nav.clone();
        cancel.connect_clicked(move |_| {
            nav.pop();
        });
        actions.append(&spinner);
        actions.append(&cancel);
        actions.append(&button);
        let body = column();
        let message = label(message);
        for widget in [message.upcast_ref::<gtk::Widget>(), middle.upcast_ref(), error.upcast_ref(), actions.upcast_ref()] {
            body.append(widget);
        }
        let step = Step { message, middle, error, spinner, button: button.clone(), retry_only };
        let (shown, commands) = (step.clone(), self.commands.clone());
        button.connect_clicked(move |_| match submit() {
            Ok(command) => {
                shown.error.set_visible(false);
                shown.busy(true);
                let _ = commands.send(command);
            }
            Err(message) => shown.fail(&message),
        });
        *self.step.borrow_mut() = Some(step);
        self.nav.push(&page(title, &body));
        button
    }

    fn sign_in_step(&self, kind: ProviderKind, domain: GitHubDomain) {
        let name = kind.display_name();
        let middle = column();
        middle.append(&label("Preparing sign-in…"));
        let button = self.push_step(&format!("Sign In to {name}"), "Continue in your browser. This page closes when you’re signed in.", middle, "Try Again", true, move || {
            Ok(Command::AddOAuth { kind, domain: domain.clone() })
        });
        button.emit_clicked();
    }

    fn api_key_step(&self, kind: ProviderKind) {
        let name = kind.display_name();
        let key = adw::PasswordEntryRow::builder().title("API Key").build();
        let middle = form(&[key.upcast_ref()], None);
        let message = format!("Enter an API key from your {name} account. It’s stored in the Secret Service keyring.");
        self.push_step(&format!("Add {name}"), &message, middle, "Add Account", false, move || {
            match key.text().trim() {
                "" => Err("Enter an API key.".into()),
                text => Ok(Command::AddApiKey { kind, key: text.to_string() }),
            }
        });
    }

    fn copilot_options(&self) {
        let list = adw::PreferencesGroup::builder().description("Sign in to your own seat, or add your organization’s billing with a token.").build();
        let options: [(&str, &str, Open); 3] = [
            ("Sign In with GitHub", "github.com", |f| f.sign_in_step(ProviderKind::Copilot, GitHubDomain::Github)),
            ("GitHub Enterprise", "A company address on GHE.com", AddFlow::domain_step),
            ("Organization Billing", "An organization’s Copilot credits, read with a token", AddFlow::organization_step),
        ];
        for (title, subtitle, open) in options {
            let row = adw::ActionRow::builder().title(title).subtitle(subtitle).activatable(true).build();
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let flow = self.clone();
            row.connect_activated(move |_| open(&flow));
            list.add(&row);
        }
        let body = column();
        body.append(&list);
        self.nav.push(&page("Add Copilot", &body));
    }

    fn domain_step(&self) {
        let domain = adw::EntryRow::builder().title("Domain, like octocorp.ghe.com").build();
        let middle = form(&[domain.upcast_ref()], Some("Only needed for GitHub Enterprise on GHE.com."));
        self.push_step("Sign In to Copilot", "Enter your company’s GitHub Enterprise address.", middle, "Continue", false, move || {
            parse_domain(&domain.text()).map(|domain| Command::AddOAuth { kind: ProviderKind::Copilot, domain }).map_err(String::from)
        });
    }

    fn organization_step(&self) {
        let org = adw::EntryRow::builder().title("Organization").build();
        let token = adw::PasswordEntryRow::builder().title("Token").build();
        let note = "Shows your organization’s Copilot credits and spend. Use a personal access token from an organization owner or billing manager: fine-grained, with Organization permissions → Administration → Read-only.";
        let middle = form(&[org.upcast_ref(), token.upcast_ref()], Some(note));
        let message = "Enter your organization and a token that can read its billing. It’s stored in the Secret Service keyring.";
        self.push_step("Add Copilot", message, middle, "Add Account", false, move || {
            let (organization, token) = (org.text().trim().to_string(), token.text().trim().to_string());
            if organization.is_empty() || token.is_empty() {
                return Err("Enter the organization and its token.".into());
            }
            Ok(Command::AddCopilotOrganization { organization, token })
        });
    }

    fn hub_step(&self) {
        let url = adw::EntryRow::builder().title("URL, like https://hub.example.com").build();
        let key = adw::PasswordEntryRow::builder().title("Management Key").build();
        let note = "The hub needs remote management turned on. Headroom uses https unless you type http://, which works only for this computer, your local network, or Tailscale.";
        let middle = form(&[url.upcast_ref(), key.upcast_ref()], Some(note));
        let message = "Enter your CLIProxyAPI server’s address and management key. Its Claude, ChatGPT, Antigravity, Devin, and Grok logins appear as accounts. The key is stored in the Secret Service keyring.";
        self.push_step("Connect CLIProxyAPI", message, middle, "Connect", false, move || {
            let (address, key) = (url.text().trim().to_string(), key.text().trim().to_string());
            if address.is_empty() || key.is_empty() {
                return Err("Enter the hub’s address and management key.".into());
            }
            Ok(Command::AddHub { address, key })
        });
    }

    pub fn progress(&self, progress: AddProgress) {
        let step = self.step.borrow().clone();
        match progress {
            AddProgress::Event(event) => {
                if let Some(step) = step {
                    self.show_event(&step, event);
                }
            }
            AddProgress::Failed(message) => {
                if let Some(step) = step {
                    step.fail(&message);
                }
            }
            AddProgress::Done(name) => {
                *self.step.borrow_mut() = None;
                self.nav.pop_to_tag(crate::accounts::TAG);
                self.toasts.add_toast(adw::Toast::builder().title(format!("Added {name}")).use_markup(false).build());
            }
        }
    }

    fn show_event(&self, step: &Step, event: LoginEvent) {
        while let Some(child) = step.middle.first_child() {
            step.middle.remove(&child);
        }
        let window = self.window();
        let open = move |url: &str| gtk::UriLauncher::new(url).launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
        match event {
            LoginEvent::OpenUrl(url) => {
                step.message.set_visible(true);
                open(&url);
                step.middle.append(&label("Waiting for your browser…"));
                step.middle.append(&selectable(&url, &["dim-label", "caption"]));
                step.middle.append(&copy_button(&url, "Copy Link"));
            }
            LoginEvent::DeviceCode { code, url } | LoginEvent::PortBusy { code, url } => {
                step.message.set_visible(false);
                open(&url);
                step.middle.append(&label("Open the verification page in your browser and enter this code."));
                step.middle.append(&selectable(&code, &["title-1"]));
                let buttons = gtk::Box::builder().spacing(12).build();
                buttons.append(&copy_button(&code, "Copy Code"));
                let page = gtk::Button::with_label("Open Page");
                page.connect_clicked(move |_| open(&url));
                buttons.append(&page);
                step.middle.append(&buttons);
            }
        }
        step.middle.set_sensitive(true);
    }
}

impl Step {
    fn fail(&self, message: &str) {
        self.busy(false);
        // A sign-in's code or link is spent; Try Again brings a new one.
        if self.retry_only {
            while let Some(child) = self.middle.first_child() {
                self.middle.remove(&child);
            }
        }
        self.error.set_label(message);
        self.error.set_visible(true);
    }
}
