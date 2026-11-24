# Settings

The **Settings** window holds the options of `headroom-app`. Open it from the main menu in the panel's
header or the settings button in the [GNOME extension](gnome-extension.md) dropdown (which opens the panel
window on top of it). Every change saves at once and applies without a restart.

The values are saved in `~/.config/headroom/settings.json` (see [Privacy](privacy.md#files)). A value in the
file that is out of range or not one of the choices is corrected on read, and a note goes to stderr.

## General

- **Launch at Login**: starts `headroom-app` when you log in. It writes
  `~/.config/autostart/dev.headroom.Headroom.desktop`, running the binary that is open now (the AppImage
  file when you run one, not its temporary mount), and turning it off deletes the file. On start the app
  rewrites the file if its path no longer matches, for example after the AppImage moved or a `.deb` replaced
  a source build. The switch shows whether that file exists; it is not saved in `settings.json`.
- **Refresh Every**: how often the app [refreshes](refreshing.md) every account. Choices are 5, 10, 15, 30
  and 60 minutes; the default is 5.
- **Toggle Shortcut**: see [below](#toggle-shortcut).

## Thresholds

The percent left at or under which a limit changes level. They set what [Priority](dashboard.md#priority)
lists as running out, and when the Warning and Critical [notifications](notifications.md) are sent.

| Setting | Range | Step | Default |
| --- | --- | --- | --- |
| **Warning** | 20 to 70 | 5 | 30 |
| **Critical** | 5 to 25 | 5 | 10 |

Critical always stays below Warning: its highest value is 5 under Warning (up to 25). Lowering Warning
lowers Critical with it if needed.

## Notifications

Four switches, all off by default: **Critical**, **Warning**, **Burning Fast**, and **Use It or Lose It**.
See [Notifications](notifications.md).

## Network

**Proxy** sends the app's requests through a proxy. It takes effect when you apply the row.

- The format is `scheme://host[:port]`, with `socks5`, `http`, or `https` as the scheme. Without a port, the
  scheme's usual one is used (1080, 80, 443). Example: `socks5://localhost:9050`.
- A proxy with a user name or password is refused with a message, and the address you typed is never shown
  in it. Headroom stores no proxy credentials.
- Leave it empty to connect directly.
- It covers every request `headroom-app` and the [`headroom` command](cli.md) make: providers,
  [CLIProxyAPI hubs](cliproxyapi.md) and sign-ins. The command reads it each time it runs.

## Toggle Shortcut

Records a key that opens and closes Headroom from anywhere. Click the button, press a key with a modifier
(**Escape** cancels, **Backspace** clears). The row is greyed out when GNOME Settings is not available.

- It adds a GNOME custom keybinding named "Headroom Toggle" to the existing ones. Other shortcuts are never
  changed or reordered, and clearing the shortcut removes only this one.
- The keybinding runs the app's `toggle` action over D-Bus (`gdbus call --session --dest dev.headroom.Headroom
  ...`). With the GNOME extension running, its dropdown opens or closes. Otherwise the panel window toggles.
- The shortcut lives in GNOME's own settings, not in `settings.json`.
