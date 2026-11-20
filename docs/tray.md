# Tray

`headroom-app` shows a StatusNotifierItem (through `ksni`). The text next to the icon lists the limits
[running out](dashboard.md#priority), most urgent first, like `Session 82% · Weekly 71% +2`.

- The text needs a StatusNotifier host that reads `XAyatanaLabel`, such as Ubuntu's AppIndicator extension.
  Hosts that ignore it show only the icon; the tooltip lists the same limits and any account that could not
  be read.
- It lists up to 3 limits, then a count of the rest (`+2`). Used Up limits never count. With nothing running
  out, the label is empty.
- Each limit shows a short name (a trailing "Models", "Bot", or "Usage" is dropped) and its used value.
- **Left click** opens the panel, where the host supports it. The menu has **Open Headroom**,
  **Refresh Now**, and **Quit**.

## With the GNOME Shell extension

At startup the app asks the session bus whether `dev.headroom.Extension` has an owner. If the
[extension](gnome-extension.md) is running, the app creates no tray icon and logs
"the GNOME Shell extension is running, so the tray is skipped"; the extension draws the same label in the top
bar. Restart the app after enabling or disabling the extension to switch.

If the tray service can't start (no StatusNotifier host) and the extension is not running, the app keeps
running with its panel only, opens the panel, and logs a note on stderr. Launching Headroom again shows the
panel.
