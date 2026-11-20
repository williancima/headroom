# Command Line

The `headroom` command can add accounts, print the most recently fetched usage as JSON, and fetch usage on
demand. Credentials stay in the Secret Service. A bare `headroom usage` only reads saved data: it does not
refresh, sign in, touch a credential, or start the app. While the app runs it fetches every 5 minutes (see
[Refreshing](refreshing.md)).

```sh
headroom usage                   # every account
headroom usage codex             # every ChatGPT account (the provider id stays codex)
headroom usage codex@1a2b3c4d    # one account, by its id
```

- Pass either an account id (the key under `providers` in the output) or a provider kind: `claude`, `codex`,
  `cursor`, `amp`, `antigravity`, `copilot`, `devin`, `grok`, `muse`, `ollama`, `opencode`, `openrouter`,
  `zai`. If nothing matches, the command fails.
- `--refresh` makes it fetch before printing (see below).

```sh
headroom accounts add claude                      # opens your browser to sign in
headroom accounts add opencode                    # prints a code, then opens the page to enter it
headroom accounts add copilot --domain octocorp   # GitHub Enterprise Cloud (octocorp.ghe.com)
headroom accounts add copilot --organization acme # an organization's billing, with a pasted token
headroom accounts add zai                         # asks for the API key (hidden prompt, or stdin)
headroom accounts list
headroom hubs add hub.example.com:8317            # a CLIProxyAPI hub; asks for its management key
headroom hubs list
headroom usage --refresh                          # fetch every account, then print the JSON
headroom panel                                    # what the panel shows, as JSON
```

- Sign-in through the browser: `claude`, `codex` (or `chatgpt`), `cursor`, `antigravity`, `devin`.
- Sign-in with a device code: `opencode`, `amp`, `grok`, `muse`, `copilot`. The command shows a code and
  opens the provider's page; type the code in there.
- Sign-in with an API key: `openrouter`, `ollama`, `zai`. The key comes from a hidden prompt, or from stdin
  when input is piped, and one test read validates it before the account is stored.
- For Copilot only, `--domain` accepts a GHE.com address (`octocorp` or `octocorp.ghe.com`), while
  `--organization` adds that organization's billing using the token of an owner or billing manager. Choose
  one of the two.
- CLIProxyAPI hubs: `hubs add` checks the address and key, stores the key in a separate Secret Service entry,
  and mirrors the hub's Claude, ChatGPT, Antigravity, Devin and Grok logins as accounts (labeled
  `(hub <host>)` in `accounts list`). `usage --refresh` re-reads each hub's login list first. `hubs key <id>`
  swaps in a new key (checked first) and `hubs remove <id>` deletes the hub's accounts and key; an id can be
  a unique prefix. Because the hub owns its accounts, `accounts remove` rejects them. Once a hub refuses the
  key or blocks this machine, nothing more is sent to it until the key is replaced. Plain `http://` is
  accepted only for this machine, the local network, and Tailscale.
- `headroom panel` prints, from saved usage, what the panel and tray display. It never fetches (the app
  does that) and exits 0. Its fields: `label` (the top-bar text), `updated_at` (the newest reading, or `null`
  if there is none), and `priority` and `usage` (the two tabs). Each tab is a list of groups shaped
  `{ "title", "rows" }`, and each row is `{ "title", "subtitle", "meter", "value" }`; `meter` (a bar fill
  between 0 and 1) and `value` are `null` for rows without them. A "Rate Limit Resets" row also carries
  `card_id` and `resets`, with one entry per reset: `{ "title", "expires", "usable", "confirmDetail", "claim" }`.
  The `claim` value identifies the reset and is passed back unchanged to spend it (see below). The app also
  records the limits burning fast in `burning_fast.json`, beside `snapshots.json`, so the command orders
  things the way the app does.
- Banked resets (Claude, Codex, Z.ai) appear in the output, but this command cannot claim them. The running
  app does it through its `claim-reset` action, which takes a JSON string
  `{"card_id": "<row card_id>", "claim": <entry claim>, "request_id": "<token>"}`. When a claim gets no clear
  reply, retry it with the same `request_id`. After a claim finishes, the app emits the session-bus signal
  `dev.headroom.Headroom.Resets.ClaimDone` on `/dev/headroom/Headroom` from `dev.headroom.Headroom`, carrying
  `(s request_id, s message, b settled)`. A `settled` of false means the claim might still go through, so
  send it again with the same `request_id`.

## The Background App

`headroom-app` stays in the background and refreshes every account on the schedule in
[Refreshing](refreshing.md), writing the same files that `headroom usage` reads. Its tray icon lists the
limits running out beside it (up to 3, then `+N`), and its menu has **Open Headroom**, **Refresh Now** and
**Quit**. The label next to the icon requires a StatusNotifier host that honors `XAyatanaLabel`, for example
Ubuntu's AppIndicator extension. Errors are written to stderr.

**Open Headroom** (or a left click, on hosts that pass it through) opens the panel, with a **Priority** tab
for what is running out and an **All Usage** tab for every account's limits. The panel's **Accounts** screen
adds and removes accounts and CLIProxyAPI hubs, using the same sign-in flows as `headroom accounts add` and
`headroom hubs add`. Closing the panel only hides it, and starting `headroom-app` again brings the running
instance back. GNOME Shell also gets a top-bar dropdown, described in
[GNOME Shell Extension](gnome-extension.md).

The app publishes GApplication actions on the session bus (`dev.headroom.Headroom`, object
`/dev/headroom/Headroom`): `refresh`, `accounts` (opens the panel on its Accounts screen), `quit`, plus
`claim-reset`, `set-hidden`, `set-pinned`, `set-unified`, `move-metric`, `move-account` and `move-provider`,
which the extension calls. Example:

```sh
gdbus call --session --dest dev.headroom.Headroom --object-path /dev/headroom/Headroom \
  --method org.gtk.Actions.Activate refresh [] {}
```

When the GNOME Shell extension already owns `dev.headroom.Extension` as the app starts, the app skips its
tray icon and the extension renders the label.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Output was printed. |
| 1 | An error, reported on stderr. |
| 2 | Bad arguments, or a provider argument that matches no account. |
| 4 | `headroom usage` found no fetched data for the named accounts, or there are no accounts. Add an account or refresh. The empty envelope is still printed. |

## Output of `headroom usage`

```json
{
  "schema": "headroom.limits.v1",
  "generatedAt": "2026-07-13T01:40:00.000Z",
  "providers": {
    "codex@1a2b3c4d": {
      "displayName": "ChatGPT · Work",
      "plan": "Pro 200",
      "fetchedAt": "2026-07-13T01:39:30.000Z",
      "expiresAt": "2026-07-13T01:49:30.000Z",
      "stale": false,
      "resources": {
        "session": {
          "kind": "consumption",
          "unit": "percent",
          "used": 42,
          "limit": 100,
          "remaining": 58,
          "utilization": 0.42,
          "resetsAt": "2026-07-13T06:00:00.000Z",
          "windowSeconds": 18000
        },
        "credits": {
          "kind": "balance",
          "unit": "credits",
          "available": 821
        }
      }
    }
  }
}
```

| Field | Meaning |
| --- | --- |
| `providers` | Entries keyed by account id, one per account. An account that has not been fetched has none. |
| `displayName` | The provider and account name, as the app labels them. |
| `plan` | The plan name given by the provider. |
| `fetchedAt` | The last successful fetch of the account. |
| `expiresAt` | `fetchedAt` plus two refresh intervals (10 minutes by default; the interval is set in [Settings](settings.md)). |
| `stale` | True once `expiresAt` is in the past: the app is not running, or its refreshes of this account fail. |
| `notice` | Present only sometimes: a message the provider returns instead of usage, for instance a Grok team account that has no personal quota. |

Every resource is a single value with a `unit`:

- `kind` is `consumption` (carries `used`) or `balance` (carries `available`).
- Consumption with a bound also has `limit`, `remaining`, and `utilization` from 0 to 1.
- `resetsAt`, `windowSeconds`, and `expiresAt` (a list of instants, for a balance of resets) are included
  only when the provider provides them.
- A resource with no present value is omitted rather than reported as zero.
- `unit` mirrors the provider's live format. Cursor `totalUsage` uses `percent`, or `requests` on
  request-based Enterprise plans. Claude `extraUsage` uses the account's billing currency, such as `usd` or
  `eur`.

A refresh that fails leaves the output as it was; the app shows the error under the account's name.

### Resources

| Provider | Resource keys |
| --- | --- |
| Claude | `session`, `weekly`, `sonnet`, `fable`, `extraUsage`, `rateLimitResets` |
| ChatGPT (`codex`) | `session`, `weekly`, `spark`, `sparkWeekly`, `credits`, `rateLimitResets` |
| Cursor | `totalUsage`, `grokBot`, `autoUsage`, `apiUsage`, `onDemand`, `requests`, `credits` |
| Antigravity | `geminiSession`, `geminiWeekly`, `argonWeekly`, `nonGeminiSession`, `nonGeminiWeekly` |
| Devin | `daily`, `weekly`, `extraUsageBalance` |
| OpenRouter | `balance`, `keyLimit` |

- Accounts for Amp, Copilot, Grok, Muse Code, Ollama, OpenCode, and Z.ai list a plan and `fetchedAt` but an
  empty `resources`. Their limits are visible in the panel and in `headroom panel`.
- ChatGPT `credits` is `0` for an empty balance, and missing when ChatGPT reports none.
- Antigravity `argonWeekly` is the weekly quota of Gemini 4 Argon; it is missing for accounts without one.
- Charts, subtitles, badges, layout, and spend periods (like OpenRouter's today, week, and month) are not
  part of the output.
