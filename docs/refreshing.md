# Refreshing

`headroom-app` refreshes every account at the interval set in [Settings](settings.md#general) while it runs:
5, 10, 15, 30 or 60 minutes, 5 by default. A new interval applies from the next check; it doesn't start a pass.
The command line never refreshes unless you ask (`headroom usage --refresh`).

## When data updates

| Trigger | What refreshes |
| --- | --- |
| Launch | Accounts with no saved values, or values one interval old or more |
| Every interval | Every account that is not waiting after a failure. The app checks the clock every 30 seconds, so a pass missed during sleep runs on wake |
| Adding an account | Every account, as a normal pass |
| The panel's refresh button, the tray's **Refresh Now**, the extension's refresh button, or `headroom usage --refresh` | Every account, skipping the cache and the wait after a failure |
| Claiming a rate-limit reset | Every account, like a refresh |
| Launch, hourly, and every manual refresh | Each [CLIProxyAPI hub](cliproxyapi.md)'s login list, read before usage |

A wait a provider asked for (HTTP 429) holds even for a manual refresh. Accounts fetch in parallel, each with a
two-minute deadline.

## Caching

Usage is read on every refresh. These reads happen at most once an hour:

- The plan name (Claude, Antigravity, Grok, OpenCode, Ollama).
- ChatGPT's reset credits, whose count is still updated on every refresh.
- Claude's reset grants. The last answer is kept, so each reset shows between reads.
- Z.ai's banked resets and plan.

Usage history (Today, Yesterday, Last 30 Days) is read per UTC or local day, depending on the provider:

- [Cursor](providers.md#cursor) reads today and yesterday every refresh, and each older day until one read of
  it succeeds.
- [Ollama](providers.md#ollama), [ChatGPT](providers.md#chatgpt), and [OpenRouter](providers.md#openrouter) read
  it hourly, at the start of a UTC day, and on a manual refresh.
- The days already read are saved with each account's refresh state, so the CLI and the app share them.

The last values are saved in `~/.local/state/headroom/snapshots.json` and shown at launch.

## When a fetch fails

A failed refresh keeps the last good values. The account shows its error under its name, and under **Needs
Attention** in Priority. The next successful refresh clears it.

After a failure, an account waits before it is tried again:

- 5 minutes, doubling with each failure in a row, up to an hour. This covers a refused key or sign-in and
  provider errors.
- If the request never reached the provider (no connection, or a hub that can't be reached), the wait stays
  at 5 minutes and doesn't grow.
- A manual refresh skips the wait, except one the provider asked for.

When a provider rate-limits Headroom (HTTP 429, or GitHub's HTTP 403), the account waits as long as
`Retry-After` asks, up to an hour, or on the doubling schedule. Claude's wait covers every card signed in to
the same Anthropic login.

The wait is saved in `~/.local/state/headroom/provider_state.json`, so it survives a restart.

An account that doesn't answer within two minutes shows "Refresh timed out after 120s" and is tried again on
the doubling schedule.

Values one refresh interval old or more show "Last updated … ago" under the account's name. In the
[`headroom usage` output](cli.md), `"stale": true` means two refresh intervals have passed. Both follow the
interval in [Settings](settings.md); the waits after a failure above start at 5 minutes whatever it is.

## Expired sign-ins

OAuth accounts renew their token 5 minutes before it expires. Cursor renews in the last 7 days of its token.
If a provider refuses the token (HTTP 401), Headroom renews once and retries.

If the provider still refuses the token or the renewal, the account shows "Sign-in expired. Sign in again."
Remove the account and add it again.

Other renewal failures, such as an outage or no connection, don't mean the sign-in expired. The account keeps
its current token while it works and retries on the next pass. Once the token expires, the refresh fails like
any [failed fetch](#when-a-fetch-fails).
