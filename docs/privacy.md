# Privacy

Headroom reads nothing that belongs to other tools: no other app's keyring entries, auth files, logs,
databases, or environment variables. It holds only the credentials you add in the app or with `headroom
accounts add`, and it calls only the provider APIs those accounts need. It sends no telemetry and makes no
update check.

## Credentials

- OAuth tokens and API keys live in the Secret Service (GNOME Keyring, KWallet, or any provider of
  `org.freedesktop.secrets`), through the `keyring` crate. Service `headroom`, one entry per account id.
  Removing an account deletes its entry.
- A [CLIProxyAPI hub](cliproxyapi.md)'s management key lives in its own service, `headroom-cliproxy-hubs`,
  with the address it was saved for, and is deleted with the hub. Hub accounts hold no token.
- Nothing is written to disk in plain text. If no Secret Service is running, adding an account fails with a
  message that says so.

## Files

| File | Holds |
|---|---|
| `~/.config/headroom/accounts.json` | Accounts: id, provider, name, a hash of the key (to reject duplicates), and a Copilot domain or organization or hub link |
| `~/.config/headroom/layout.json` | Hidden and pinned metrics, order, unified providers |
| `~/.config/headroom/hubs.json` | Hub addresses, never their keys |
| `~/.config/headroom/settings.json` | [Settings](settings.md): thresholds, refresh interval, notification switches, and the proxy address (never with a user name or password) |
| `~/.local/state/headroom/snapshots.json` | The last mapped usage of each account |
| `~/.local/state/headroom/provider_state.json` | Waits after failures, hourly read times, a few small provider bodies kept between hourly reads (a plan or reset list), and usage-history days |
| `~/.local/state/headroom/burning_fast.json` | Which limits are burning fast |

Both folders follow `XDG_CONFIG_HOME` and `XDG_STATE_HOME`. Headroom creates them readable only by you (mode
0700) and writes each file readable only by you (mode 0600). Nothing syncs anywhere.

## What leaves your machine

Requests go only to the provider of each account (and to a hub you connected, with its management key). A
ChatGPT sign-in listens on port 1455 of `127.0.0.1` and `[::1]` only while it waits for the browser. Headroom sends no analytics or
crash reports. Update checks are described in [Updates](updates.md).

The [`headroom` command](cli.md) `usage` and `panel` read only the saved files: no network request, no
Secret Service access. `usage --refresh` and `accounts add` do both.

If you set a [proxy](settings.md#network), the requests of the app and the `headroom` command go through it, so the proxy sees the same
traffic. Headroom refuses a proxy address with a user name or password and stores no proxy credentials.

[Notifications](notifications.md) are local desktop notices; nothing is sent anywhere, and what was already
notified is kept in memory only.

## Share images

Copying an image of the Priority tab or a usage breakdown renders it on your machine and leaves email
addresses out. The GTK panel puts it straight on the clipboard. For the GNOME extension, the app writes a
temporary PNG under `$XDG_RUNTIME_DIR/headroom/` (a private folder); the extension copies it to the clipboard
and deletes it.
