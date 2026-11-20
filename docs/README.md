# Headroom Documentation

These pages describe what the app does. Where the app and a page differ, treat it as a bug.

## Using Headroom

- [Dashboard](dashboard.md): the Priority and All Usage tabs, context menus, and reordering
- [Accounts](accounts.md): adding and removing accounts, and how signing in works
- [CLIProxyAPI hubs](cliproxyapi.md): mirroring the Claude, ChatGPT, Antigravity, Devin, and Grok logins
  held by a CLIProxyAPI server
- [Tray](tray.md): the tray icon and its menu
- [GNOME Shell extension](gnome-extension.md): the dropdown in the top bar
- [Command line](cli.md): the `headroom` command and its JSON output
- [Settings](settings.md): the Settings window, thresholds, proxy, and the toggle shortcut
- [Notifications](notifications.md): when Headroom notifies you, and the wording
- [Updates](updates.md): AppImage self-update and the restart that follows
- [Refreshing](refreshing.md): the refresh schedule and what a failed fetch does
- [Providers](providers.md): how each provider signs in and the metrics it shows
- [Privacy](privacy.md): where data is stored and what leaves your machine

## Development

- [Architecture](architecture.md): crates, the refresh worker, and the stores
- [Adding a provider](adding-a-provider.md): the sign-in and usage contract, plus the steps to follow
- [Debugging](debugging.md): running a local build and reading what it prints
