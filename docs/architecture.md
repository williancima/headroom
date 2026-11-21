# Architecture

Headroom is a Cargo workspace (Rust 2021) plus a GNOME Shell extension.

| Part | What it is |
|---|---|
| `crates/headroom-core` | The library: providers, sign-in, stores, scheduling, ranking, history, and the view model. No GTK. |
| `crates/headroom-app` | `headroom-app`: the GTK4/libadwaita panel (relm4), the `ksni` tray, the D-Bus actions, and the worker. |
| `crates/headroom-cli` | `headroom`: accounts, hubs, usage JSON, and the panel JSON. |
| `extension/` | The GNOME Shell 46-51 extension. It runs `headroom panel` and calls the app's D-Bus actions. |
| `vendor/ksni` | A patched `ksni` that adds the `XAyatanaLabel` property the tray text uses. |

## Core (`headroom-core`)

- `models.rs`: `ProviderKind`, `Account`, `OAuthTokens`/`Credential`, `MetricLine`, `ProviderSnapshot`.
- `store.rs`: `Store` reads and writes the JSON files and the Secret Service entries. See
  [Privacy](privacy.md).
- `login.rs`: adding and removing accounts, shared by the CLI and the panel. It reports progress as
  `LoginEvent` values and prints nothing.
- One module per provider (`claude/`, `codex/`, `cursor/`, `amp/`, `antigravity/`, `copilot/`, `devin/`,
  `grok/`, `muse/`, `ollama/`, `opencode/`, `openrouter/`, `zai/`). Each has a client for HTTP and sign-in
  and a mapper from the response to metric lines. See [Adding a provider](adding-a-provider.md).
- `oauth/`: PKCE, the loopback callback server, device-code polling, and token parsing.
- `refresh.rs`: `fetch_account` picks the provider by account kind, loads its credential, keeps the hourly
  secondary reads, and saves renewed tokens. `sync_hubs` mirrors CLIProxyAPI hubs.
- `backoff.rs`: when an account is due and what a failure costs. See [Refreshing](refreshing.md).
- `hub/`: CLIProxyAPI hubs. A hub is not a provider; its logins are ordinary accounts whose requests go
  through a `Transport::Hub`, so the provider code runs unchanged.
- `claim.rs`: spending a rate-limit reset.
- `history/`: the usage-history ledger and the rows and breakdowns built from it.
- `priority/`: ranks limits into the Priority sections and builds the tray label.
- `layout.rs`: hidden and pinned metrics, order, and unified providers (`layout.json`).
- `view.rs`, `view/`: turns the saved data into the rows of both tabs (`PanelModel`, `PanelView`).
- `envelope.rs`: the `headroom.limits.v1` JSON of `headroom usage`. See [Command line](cli.md).

Accounts are the source of truth. Provider and account order is the order in `accounts.json`; nothing sorts
them.

## The app (`headroom-app`)

- GTK runs on the main thread (`panel.rs` and the `accounts`, `add`, `resets`, and `history_row` widgets). It
  only draws what it is sent.
- The worker (`worker.rs`) runs on its own thread with a Tokio runtime. All store, Secret Service, and
  network calls happen there, never on the GTK thread. It refreshes accounts, runs adds and removals,
  applies layout changes, keeps the tray label current, and sends a new `PanelModel` to the panel.
- The panel and the tray send `Command` values to the worker; the worker sends `PanelMsg` values to the panel.
- `tray.rs` is the `ksni` icon. It is skipped when the extension is running. See [Tray](tray.md).
- The app is single-instance (`dev.headroom.Headroom`). A second launch only shows the running panel.
- It exports GApplication actions on the session bus at `/dev/headroom/Headroom`: `refresh`, `accounts`,
  `quit`, `claim-reset`, `set-hidden`, `set-pinned`, `set-unified`, `move-metric`, `move-account`, and
  `move-provider`. The extension uses them. See [Command line](cli.md) and
  [GNOME Shell extension](gnome-extension.md).
- It sets `GSK_RENDERER=cairo` unless you set it, to keep memory low.

## The command line (`headroom-cli`)

`headroom` reads the same files the app writes. `accounts add`, `hubs add`, and `usage --refresh` call into
core directly, so they work with the app closed. `panel` and plain `usage` never fetch.

## Provider icons

`extension/icons/headroom-provider-<id>-symbolic.svg` (one per `ProviderKind::id()`, plus `cliproxyapi` for a hub)
is the only copy. The extension ships the files; `crates/headroom-app/build.rs` bundles them as a GResource that
`icons::register` adds to the icon theme. `Row.kind` in the panel JSON tells which icon a row shows.

## The extension

It never fetches. It runs `headroom panel` and draws the JSON, and its buttons call the app's actions. The
`*_logic.js` files hold the pure parts, tested by `node extension/*_logic.test.mjs`.
