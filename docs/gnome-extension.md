# GNOME Shell Extension

A top-bar panel for GNOME Shell 46 to 51 (Ubuntu 24.04 and later). It shows the same limits as the app's
panel: the tray text becomes the top-bar label, and clicking it opens a dropdown with **Priority** and
**All Usage**, "Updated X ago", and a row of round icon buttons: copy (Priority tab only), refresh, accounts and settings.

## Install

The `.deb` installs the extension system-wide. The AppImage copies it to
`~/.local/share/gnome-shell/extensions/headroom@headroom.dev/` every time it starts (changed files are
replaced, files that are no longer shipped are removed) and writes `~/.local/bin/headroom`, a script that runs
the AppImage in command-line mode. A `headroom` file there that is not that script is left alone.

The first time the app starts with the extension installed, it adds `headroom@headroom.dev` to GNOME's
enabled extensions and remembers that (`~/.local/state/headroom/extension_enabled`). It never enables it
again, so turning the extension off sticks. Log out and back in once so GNOME loads a new extension. To
install it from source instead:

Put `headroom` on your PATH or in `~/.local/bin`, run `headroom-app`, then:

```sh
gnome-extensions pack extension/ --force --out-dir /tmp \
  --extra-source=resets.js --extra-source=resets_logic.js \
  --extra-source=breakdown.js --extra-source=breakdown_logic.js \
  --extra-source=layout.js --extra-source=layout_logic.js \
  --extra-source=share.js --extra-source=share_logic.js \
  --extra-source=icons
gnome-extensions install --force /tmp/headroom@headroom.dev.shell-extension.zip
```

On Wayland, log out and back in, then `gnome-extensions enable headroom@headroom.dev` (a source install
is not enabled by the app until it has run once with the extension in place).

## How it works

- On **All Usage**, click a **Rate Limit Resets** card to list its resets. **Use** asks "Use This Reset?" with
  what it refills; confirming sends the claim to the app (`claim-reset` action) and the card shows "Using…"
  until the app answers with the `ClaimDone` signal. The outcome shows at the top of the list, and the panel
  reloads so the spent reset leaves it. Needs `headroom-app` running.
- Right-click a metric for **Pin**/**Unpin** and **Hide**, or a Claude or ChatGPT title for **Unify Multiple
  Accounts**. Drag metrics, accounts and provider titles on **All Usage** to reorder them. These call the app's
  `set-hidden`, `set-pinned`, `set-unified`, `move-metric`, `move-account` and `move-provider` actions; the app
  saves the change and the panel reloads. See [Dashboard](dashboard.md#hiding-pinning-and-ordering-metrics).

- The extension never fetches. It runs `headroom panel` (JSON) when it starts, when the dropdown opens, every
  60 seconds while it is open, and when files in `~/.local/state/headroom` or `~/.config/headroom` change.
- The footer's copy button (Priority tab, shown only while it has rows) and the **Copy** button in an open
  usage breakdown (only when it has data) copy an image. Wayland lets only the focused client set the
  clipboard, so the extension asks `headroom-app` for a PNG (`share-image` action, `ShareReady` signal), puts
  it on the clipboard, deletes the file, and shows "Copied to Clipboard". Email addresses are left out. See
  [Dashboard](dashboard.md#copying-as-an-image).
- The refresh, accounts and settings buttons call the `refresh`, `accounts` and `settings` actions of the
  running `headroom-app` (see [CLI](cli.md)); settings opens the [Settings](settings.md) window. The
  extension does not start the app.
- Rows that open on click (usage details, unified accounts, rate-limit resets) show a chevron.
- The extension exports `ToggleMenu` on `dev.headroom.Extension`, so the [Toggle Shortcut](settings.md#toggle-shortcut)
  opens or closes the dropdown.
- While the extension is enabled it owns the session bus name `dev.headroom.Extension`, and `headroom-app`
  skips its own tray icon if it sees that name at startup. Restart the app after enabling or disabling the
  extension to switch between the two.

## Troubleshooting

Errors show as a red card in the dropdown (and once in the Shell log: `journalctl -f -o cat /usr/bin/gnome-shell`).

- "The headroom command was not found": install `headroom` on your PATH or in `~/.local/bin`.
- "Headroom isn't running": start `headroom-app`. A button clicked before the app is up (for example right after
  login) waits up to 10 seconds for it, then shows this.
- "headroom panel failed": run `headroom panel` in a terminal to see the same error.
