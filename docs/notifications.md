# Notifications

Headroom can send a desktop notification when a limit crosses a level. All four kinds are off by default;
turn them on in [Settings](settings.md#notifications). They work for every limit that has a bound (a bar with
a limit), not for balances or spend figures. Clicking a notification opens the panel.

| Notification | Sent when |
| --- | --- |
| **Critical** | A limit drops to or under the Critical threshold |
| **Warning** | A limit drops to or under the Warning threshold |
| **Burning Fast** | A limit that was not running out would now run out before it resets, at its current pace |
| **Use It or Lose It** | A limit resets within 24 hours with over 50% left (windows longer than a day only) |

The title names the kind, the account and the limit, like `Warning: Claude · work Weekly`. The thresholds
are the ones in [Settings](settings.md#thresholds).

## Rules

- The first reading of a limit sends nothing. It only records where the limit stands, so starting the app
  doesn't send a burst. A limit that is already unused near its reset counts as already told.
- Each kind is sent once per window. A new window (the reset has passed) allows it again. Warning and
  Critical also allow it again once the limit climbs back above their level. Burning Fast allows it again once the
  pace eases, which is when Priority stops listing the limit as burning fast.
- A drop past both levels at once sends one notification: the lowest level that is switched on.
- What was already sent is kept in memory only. After a restart, the first reading is silent again.

## Showing them

GNOME shows notifications only from apps it can match to a desktop file, so
`data/dev.headroom.Headroom.desktop` must be installed (for example in `~/.local/share/applications/`) with
`headroom-app` on your PATH. Packaging does not install it yet.
