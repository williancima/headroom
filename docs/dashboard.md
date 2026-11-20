# Dashboard

The panel opens from the [tray icon](tray.md) or from the GNOME top-bar dropdown. It contains two tabs
(Priority and All Usage), a refresh button, a copy button, and buttons for **Accounts** and **Settings**.
See [Settings](settings.md).

- Without accounts, each tab offers **Add Account**.
- Closing the panel only hides it; the app continues to run.

## Priority

This tab lists just the items that need your attention, with the most urgent first.

- **Pinned** is at the top and holds every pinned metric that has data. A pinned limit that is running
  out, burning fast, or about to reset with room to spare remains here, keeping the color and note it
  would get in that section, and is not repeated further down. Only a pinned limit that is fully used
  moves out, into Used Up, until it resets.
- **Running Out** lists each unpinned limit at or below its Warning level (30% left by default, changed in
  [Settings](settings.md#thresholds)). It also lists a limit that, at the current pace, would be exhausted
  before it resets, whatever amount remains; it then shows the time it would run out, such as `Out in 1h 8m`,
  where the reset time would normally be. It stays in the list until the pace falls to 90% of the limit.
  Items that would run out soonest at the current pace come first, followed by limits without a pace,
  ordered by least remaining.
- **Use It or Lose It** lists an unpinned limit that resets in under 24 hours and still has more than 50%
  left. Only windows longer than one day qualify; the 5-hour session and dollar spending caps never do. Each
  entry gives its reset, like `Resets in 18h`, with the soonest first. These are never part of the tray
  label.
- **All Clear** appears under Running Out once every account has a current reading and none of the limits,
  pinned or not, is low. It holds back while some accounts have not reported.
- **Needs Attention** names an account whose refresh failed, or whose provider returned a notice. **Fix**
  opens that account's page, explaining the cause and the remedy.
- **Used Up** is always last. It lists exhausted limits, with the earliest reset first. A balance without a
  limit, like Cursor credits, appears here only when it is exactly zero.

Equal items keep your dashboard order. When no account has a limit to watch, **No Reported Limits** links
to All Usage.

## All Usage

Each provider gets a card containing its accounts, and each account lists its name, plan, and metrics. A card
includes all metrics that have data and that you have not hidden, in your chosen order.

- Time ranges are always displayed, showing 0 when the provider reports nothing.
- Balances are displayed even at `$0 left`.
- Extra Usage is displayed while the provider has it enabled, even at `$0`, and is hidden when disabled.
- A metric with no limit (a balance, one day's spend, a status) displays its value, for example
  `$12.40 left`.

### Limits

A limit row contains its label, a bar, the amount used, and the time left until it resets.

- A countdown uses the biggest unit plus the one below it, such as `45m`, `4h 12m`, `2d 5h`, `17d`,
  `3mo 17d`, or `1y 2mo`; a following unit of zero is dropped. `m` stands for minutes and `mo` for calendar
  months. The same format also expresses how long ago an account was last updated and when a limit that is
  burning fast would run out.

### Resets

While rate-limit resets are available, Claude, ChatGPT, and Z.ai accounts have a **Rate Limit Resets** row in
All Usage. Expand it to see each reset and its expiry. To use one, pick **Use**, review what it refills,
then pick **Use** again (or **Cancel**). A message reports the result, and the account refreshes so the
reset you spent disappears. If a claim does not complete, for instance because the connection dropped, it
is retried as the same request, so a second reset can never be spent by accident. On Z.ai, only the earliest
Session reset and the earliest Weekly reset are usable. For Claude, **Use** is offered only when Anthropic
says the reset can be used right now.

### Unify Multiple Accounts

Right-click a Claude or ChatGPT group that has two or more accounts, then choose **Unify Multiple Accounts**
to merge them into a single card. The choice is stored per provider in `layout.json`, shows a checkmark
while active, and is undone by choosing it again.

- The card states how many accounts it covers and the plan if all accounts share it, or **Multiple** if they
  differ. Account status and errors are visible only while unify is off.
- Each limit appears once. Its bar adds up what each account has used, capped at that account's own limit,
  and compares it with the total of the limits, so percentages are averaged and an account lacking the
  limit is ignored. The bar is full only when every account is exhausted. Amounts in differing units sum
  what each account spent, without a cap.
- The reset shown is the earliest upcoming reset among the accounts.
- Rate Limit Resets gathers the resets of all accounts, and **Use** spends the one belonging to that
  account.
- Clicking a row expands a list of each account with its own bar, amount, and reset; clicking again
  collapses it.
- Pin and Hide on a unified row apply to the limit in every account.
- Unified rows offer no usage details, and Priority continues to list each account separately.

### Copying as an Image

- The copy button in the panel header (Priority tab only, and only when it has rows) places the Priority tab
  on the clipboard as an image.
- Inside an expanded usage breakdown (see [Usage details](#usage-details)), a **Copy** button copies that
  breakdown; it appears only if the breakdown has data. The GNOME extension provides both copy actions under
  the same condition.
- Both images omit email addresses. A toast, or the extension's notice, confirms with "Copied to Clipboard".

### Account Status

Beneath an account's name the panel gives its plan and, if relevant, a status: the error or notice from the
latest refresh, "Loading…" until the first reading arrives, or "Last updated … ago" once its values are at
least one refresh interval old (see [Settings](settings.md)).

## Hiding, Pinning, and Ordering Metrics

The layout is stored in `~/.config/headroom/layout.json`, and the order of accounts and providers follows
`accounts.json`. Both persist across restarts.

- Right-click a metric in All Usage and choose **Hide** to remove it from All Usage, Priority and the
  top-bar label. Hiding a pinned metric also unpins it.
- Right-click a metric and choose **Pin** to place it under Pinned in Priority. A hidden metric cannot be
  pinned.
- In the GTK panel's **Accounts** screen, each account expands to list its reported metrics, each with
  **Show** and **Pin** switches. Switch **Show** on again to restore a hidden metric.
- A newly reported metric is added at the bottom of its account.

## Context Menus

- On a metric: **Pin** or **Unpin**, and **Hide**.
- On a Claude or ChatGPT group with two or more accounts: **Unify Multiple Accounts**.

The GNOME extension shows these choices as buttons beneath the row or title; right-click once more to hide
them.

## Reordering

- In All Usage, drag a metric within its account, an account by its name within its provider, or a provider
  (GTK: using the handle in its title; extension: by its title).
- An account can move beyond its own provider only if both providers hold a single account.
- New providers are added at the bottom, and so are new accounts within their provider. The app never sorts
  providers or accounts on its own.
- Reordering cannot be undone.

## Usage Details

In All Usage, click a Today, Yesterday, or similar row to open its details, in the GTK panel and in the GNOME
extension (the extension marks rows that open this way with a chevron). Click again to close.

- The details list charges, tokens by kind, and the five leading models, with all others under **Other**.
- Models are ranked by cost when the provider reports it, otherwise by tokens, otherwise by share.
- For ChatGPT, each percentage is the model's portion of the credits or turns in that range.
- A range totals the days that supplied each figure. A day that was quiet or unreadable contributes nothing.
