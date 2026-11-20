//! The worker thread: refreshes every account, keeps the tray label current and feeds the panel.
//! Store, keyring and network calls all happen here, never on the GTK thread.

use crate::logic;
use crate::panel::PanelMsg;
use crate::tray::HeadroomTray;
use crate::{Command, NAME};
use chrono::{DateTime, Utc};
use headroom_core::backoff;
use headroom_core::claim::{self, ResetRef};
use headroom_core::http;
use headroom_core::notify::NotifyState;
use headroom_core::priority::BurningFast;
use headroom_core::settings::Settings;
use headroom_core::view::PanelModel;
use crate::panel::AddProgress;
use headroom_core::login::{self, LoginError};
use headroom_core::refresh::{fetch_account, forget, sync_hubs, Fetched, Hubs};
use headroom_core::store::{ProviderState, Store};
use headroom_core::{Account, ProviderSnapshot};
use ksni::{Handle, TrayMethods};
use relm4::Sender;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::{AbortHandle, JoinSet};
use uuid::Uuid;

/// Runs a blocking store or keyring call off the async workers.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("blocking task panicked")
}

struct App {
    store: Store,
    hubs: Arc<Hubs>,
    latch: BurningFast,
    settings: Settings,
    notify: NotifyState,
    last_hub_sync: Option<DateTime<Utc>>,
    /// `None` while the GNOME Shell extension draws the label instead.
    tray: Option<Handle<HeadroomTray>>,
    ui: Sender<PanelMsg>,
    commands: UnboundedSender<Command>,
    /// The add in progress; dropped on cancel, or when another add starts.
    adding: Option<AbortHandle>,
    /// The last board needs re-ranking as time passes.
    reranks: bool,
}

type Loaded = (Vec<Account>, HashMap<String, ProviderSnapshot>, HashMap<String, ProviderState>);

impl App {
    async fn load(&self) -> std::io::Result<Loaded> {
        let store = self.store.clone();
        blocking(move || Ok((store.accounts()?, store.snapshots()?, store.provider_state()?))).await
    }

    fn interval(&self) -> chrono::Duration {
        chrono::Duration::minutes(i64::from(self.settings.refresh_minutes))
    }

    /// Rereads the settings. A changed proxy applies to every client built from now on, and the hub client
    /// lives for the whole process, so it is rebuilt.
    async fn reload_settings(&mut self) {
        let store = self.store.clone();
        let settings = match blocking(move || store.settings()).await {
            Ok(settings) => settings,
            Err(error) => return eprintln!("{NAME}: could not read the settings: {error}"),
        };
        if settings.proxy != self.settings.proxy {
            http::set_proxy(settings.parsed_proxy());
            self.hubs = Arc::new(Hubs::new(&self.store));
        }
        self.settings = settings;
    }

    /// Rebuilds the tray and the panel from what is on disk. `reading` is true after a fetch, which updates the burning-fast latch.
    async fn render(&mut self, reading: bool) {
        let (accounts, snapshots, states) = match self.load().await {
            Ok(loaded) => loaded,
            Err(error) => return eprintln!("{NAME}: could not read the saved usage: {error}"),
        };
        let now = Utc::now();
        if reading {
            let latch = self.latch.after_reading(&accounts, &snapshots, now);
            if latch != self.latch {
                self.latch = latch;
                let (store, latch) = (self.store.clone(), self.latch.clone());
                if let Err(error) = blocking(move || store.save_burning_fast(&latch)).await {
                    eprintln!("{NAME}: could not save what is burning fast: {error}");
                }
            }
        }
        let store = self.store.clone();
        let hubs = blocking(move || store.hubs()).await.unwrap_or_else(|e| {
            eprintln!("{NAME}: could not read the hubs: {e}");
            Vec::new()
        });
        let store = self.store.clone();
        let layout = blocking(move || store.layout()).await.unwrap_or_else(|e| {
            eprintln!("{NAME}: could not read the layout: {e}");
            Default::default()
        });
        if reading {
            let notices = self.notify.evaluate(&accounts, &snapshots, &self.settings, now);
            if !notices.is_empty() {
                let _ = self.ui.send(PanelMsg::Notify(notices));
            }
        }
        let model = PanelModel::build(accounts, hubs, snapshots, &states, &self.latch, &layout, &self.settings.priority(), now);
        let (label, tooltip) = (model.label(), logic::tooltip_text(&model.board, &model.accounts));
        self.reranks = model.board.requires_ranking_clock(now);
        if let Some(tray) = &self.tray {
            tray.update(move |t| (t.label, t.tooltip) = (label, tooltip)).await;
        }
        let _ = self.ui.send(PanelMsg::Model(Box::new(model)));
    }

    /// One refresh pass, with the panel told while it runs.
    async fn pass(&mut self, forced: bool, only_stale: bool) {
        let _ = self.ui.send(PanelMsg::Refreshing(true));
        self.fetch_all(forced, only_stale).await;
        let _ = self.ui.send(PanelMsg::Refreshing(false));
    }

    /// `only_stale` is the launch pass, which leaves fresh saved values alone.
    async fn fetch_all(&mut self, forced: bool, only_stale: bool) {
        self.reload_settings().await;
        let now = Utc::now();
        if logic::hub_sync_due(self.last_hub_sync, now, forced) {
            self.last_hub_sync = Some(now);
            self.sync_hubs(None).await;
        }
        let (accounts, mut snapshots, mut states) = match self.load().await {
            Ok(loaded) => loaded,
            Err(error) => return eprintln!("{NAME}: could not read the saved usage: {error}"),
        };
        let interval = self.interval();
        let mut tasks = JoinSet::new();
        for account in &accounts {
            let card_id = account.card_id();
            let mut state = states.get(&card_id).cloned().unwrap_or_default();
            let previous = snapshots.get(&card_id).cloned();
            if !backoff::due(&state, now, forced) || (only_stale && !logic::stale(previous.as_ref(), now, interval)) {
                continue;
            }
            let (account, hubs) = (account.clone(), self.hubs.clone());
            tasks.spawn(async move {
                let fetch = fetch_account(&account, &hubs, previous.as_ref(), &mut state, now, forced);
                let fetched = tokio::time::timeout(backoff::REFRESH_TIMEOUT, fetch)
                    .await
                    .unwrap_or_else(|_| Fetched { snapshot: Err(backoff::timeout_error()), warning: None });
                (account, state, fetched)
            });
        }
        while let Some(joined) = tasks.join_next().await {
            let (account, mut state, fetched) = match joined {
                Ok(done) => done,
                Err(error) => {
                    eprintln!("{NAME}: a refresh task failed: {error}");
                    continue;
                }
            };
            let card_id = account.card_id();
            if let Some(warning) = fetched.warning {
                eprintln!("{NAME}: {warning}");
            }
            backoff::record(&mut state, fetched.snapshot.as_ref().map(|_| ()), now);
            states.insert(card_id.clone(), state);
            backoff::share_claude_wait(&account, &accounts, &mut states, now);
            match fetched.snapshot {
                Ok(snapshot) => {
                    snapshots.insert(card_id, snapshot);
                }
                Err(error) => eprintln!("{NAME}: {}: {error}", account.full_name()),
            }
        }
        let store = self.store.clone();
        let saved = blocking(move || store.save_snapshots(&snapshots).and_then(|_| store.save_provider_state(&states))).await;
        if let Err(error) = saved {
            eprintln!("{NAME}: could not save the usage: {error}");
        }
        // A hub that said a login is gone is read again at once; the account leaves with it.
        let missing = self.hubs.client.take_login_missing();
        if !missing.is_empty() {
            self.sync_hubs(Some(missing)).await;
        }
        self.render(true).await;
    }

    /// Runs an add as its own task, so refreshes go on during a long sign-in. Progress goes to the panel.
    fn start_add(&mut self, command: Command) {
        if let Some(previous) = self.adding.take() {
            previous.abort();
        }
        let (store, hubs, ui, commands) = (self.store.clone(), self.hubs.clone(), self.ui.clone(), self.commands.clone());
        let task = tokio::spawn(async move {
            let events = ui.clone();
            let on_event = move |e| {
                let _ = events.send(PanelMsg::Add(AddProgress::Event(e)));
            };
            let added: Result<String, String> = match command {
                Command::AddOAuth { kind, domain } => login::add_oauth(&store, kind, domain, on_event).await.map(|a| a.full_name()).map_err(|e| login_error(&e)),
                Command::AddApiKey { kind, key } => login::add_api_key(&store, kind, &key).await.map(|a| a.full_name()).map_err(|e| login_error(&e)),
                Command::AddCopilotOrganization { organization, token } => {
                    login::add_copilot_organization(&store, &organization, &token).await.map(|a| a.full_name()).map_err(|e| login_error(&e))
                }
                Command::AddHub { address, key } => {
                    headroom_core::hub::connect(&hubs.client, &hubs.store, &hubs.keys, &address, &key).await.map(|(hub, _)| hub.host()).map_err(|e| e.to_string())
                }
                _ => return,
            };
            match added {
                Ok(name) => {
                    let _ = commands.send(Command::Added(name));
                }
                Err(message) => {
                    let _ = ui.send(PanelMsg::Add(AddProgress::Failed(message)));
                }
            }
        });
        self.adding = Some(task.abort_handle());
    }

    async fn remove(&mut self, command: Command) {
        let (store, hubs) = (self.store.clone(), self.hubs.clone());
        let result = blocking(move || match command {
            Command::RemoveAccount(card_id) => login::remove_account(&store, &card_id).map(drop),
            Command::RemoveHub(id) => login::remove_hub(&store, &hubs.keys, id).map(drop),
            _ => Ok(()),
        })
        .await;
        if let Err(error) = result {
            let _ = self.ui.send(PanelMsg::Toast(login_error(&error)));
        }
        self.render(false).await;
    }

    /// Applies a layout or order change and redraws from what is saved, with no fetch.
    async fn customize(&mut self, command: Command) {
        let store = self.store.clone();
        match blocking(move || crate::layout_apply::apply(&store, &command)).await {
            Ok(true) => self.render(false).await,
            Ok(false) => {}
            Err(error) => eprintln!("{NAME}: could not save the layout: {error}"),
        }
    }

    /// Spends one reset, tells the panel the outcome, then reads the accounts again unless the claim failed.
    async fn claim_reset(&mut self, card_id: String, claim: ResetRef, request_id: String) -> bool {
        let now = Utc::now();
        let outcome = claim::claim_reset(&self.store, &card_id, &claim, &request_id, now).await;
        let _ = self.ui.send(PanelMsg::ClaimDone { request_id, message: outcome.message(now), settled: outcome.settled() });
        if outcome.failed() {
            return false;
        }
        self.pass(true, false).await;
        true
    }

    async fn sync_hubs(&self, only: Option<Vec<Uuid>>) {
        match sync_hubs(&self.hubs, only.as_deref()).await {
            Ok((gone, warnings)) => {
                for warning in warnings {
                    eprintln!("{NAME}: {warning}");
                }
                let store = self.store.clone();
                if let Err(error) = blocking(move || forget(&store, &gone)).await {
                    eprintln!("{NAME}: could not forget removed accounts: {error}");
                }
            }
            Err(error) => eprintln!("{NAME}: could not sync the hubs: {error}"),
        }
    }
}

fn login_error(error: &LoginError) -> String {
    crate::accounts_logic::login_error_text(error)
}

pub async fn run(
    commands: UnboundedSender<Command>,
    mut inbox: UnboundedReceiver<Command>,
    ui: Sender<PanelMsg>,
) -> Result<(), String> {
    let store = Store::default_paths().map_err(|e| format!("could not find the data folders: {e}"))?;
    let tray = if blocking(crate::tray::extension_present).await {
        eprintln!("{NAME}: the GNOME Shell extension is running, so the tray is skipped");
        None
    } else {
        match (HeadroomTray { label: String::new(), tooltip: String::new(), commands: commands.clone(), ui: ui.clone() }).spawn().await {
            Ok(tray) => Some(tray),
            Err(error) => {
                // The extension may not be loaded yet (a fresh install needs a re-login): run with the panel alone.
                eprintln!("{NAME}: no GNOME Shell extension or tray found ({error}); running with the panel only, open it by launching Headroom again");
                // Launched by hand: show something. At login (gnome-session sets this) the extension may just be late.
                if std::env::var_os("DESKTOP_AUTOSTART_ID").is_none() {
                    let _ = ui.send(PanelMsg::Show);
                }
                None
            }
        }
    };
    let settings = store.settings().unwrap_or_else(|e| {
        eprintln!("{NAME}: could not read the settings: {e}");
        Settings::default()
    });
    http::set_proxy(settings.parsed_proxy());
    crate::setup::spawn_background(commands.clone());
    let hubs = Arc::new(Hubs::new(&store));
    let latch = store.burning_fast().unwrap_or_else(|e| {
        eprintln!("{NAME}: could not read what is burning fast: {e}");
        BurningFast::default()
    });
    let mut app = App { store, hubs, latch, settings, notify: NotifyState::default(), last_hub_sync: None, tray, ui, commands: commands.clone(), adding: None, reranks: false };

    app.render(false).await;
    let mut last_pass = Utc::now();
    app.pass(false, true).await;
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let now = Utc::now();
                if logic::pass_due(last_pass, now, app.interval()) {
                    last_pass = now;
                    app.pass(false, false).await;
                } else if app.reranks {
                    app.render(false).await;
                }
            }
            command = inbox.recv() => match command {
                Some(Command::Refresh) => {
                    last_pass = Utc::now();
                    app.pass(true, false).await;
                }
                Some(Command::SettingsChanged) => {
                    app.reload_settings().await;
                    app.render(false).await;
                }
                Some(Command::Added(name)) => {
                    // Only the finished task's handle: a newer add may have started since.
                    if app.adding.as_ref().is_some_and(|task| task.is_finished()) {
                        app.adding = None;
                    }
                    app.render(false).await;
                    let _ = app.ui.send(PanelMsg::Add(AddProgress::Done(name)));
                    last_pass = Utc::now();
                    app.pass(false, false).await;
                }
                Some(Command::ClaimReset { card_id, claim, request_id }) => {
                    if app.claim_reset(card_id, claim, request_id).await {
                        last_pass = Utc::now();
                    }
                }
                Some(Command::CancelAdd) => {
                    if let Some(task) = app.adding.take() {
                        task.abort();
                    }
                }
                Some(command @ (Command::AddOAuth { .. } | Command::AddApiKey { .. } | Command::AddCopilotOrganization { .. } | Command::AddHub { .. })) => {
                    app.start_add(command);
                }
                Some(
                    command @ (Command::SetHidden { .. }
                    | Command::SetPinned { .. }
                    | Command::SetUnified { .. }
                    | Command::MoveMetric { .. }
                    | Command::MoveAccount { .. }
                    | Command::MoveProvider { .. }),
                ) => app.customize(command).await,
                Some(command @ (Command::RemoveAccount(_) | Command::RemoveHub(_))) => app.remove(command).await,
                Some(Command::Quit) | None => return Ok(()),
            },
        }
        if app.tray.as_ref().is_some_and(|tray| tray.is_closed()) {
            return Err("the tray service stopped".into());
        }
    }
}
