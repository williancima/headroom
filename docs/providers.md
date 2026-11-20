# Providers

For each provider: how you sign in and what its card displays.

- All values are taken from the provider's API. The single estimate is ChatGPT's credits, valued at 4¢ each.
- Headroom does not derive spend from logs or apply model pricing. When a provider does not report a metric,
  the metric stays hidden.
- Time ranges (Today, Yesterday, This Week, and similar) are always displayed, showing 0 when nothing is
  reported.
- Balances are displayed even when they read `$0 left`.
- Extra Usage is displayed while the provider has it turned on, even at `$0`, and hidden while it is off.

| Provider | Method of sign-in | Metrics on the card |
|---|---|---|
| Claude | Browser, via claude.ai OAuth | Session, Weekly, Sonnet, Fable, Extra Usage, Limit Resets |
| ChatGPT | Browser, via OpenAI OAuth; device code as a fallback | Session, Weekly, Spark, Spark Weekly, Extra Usage, Credit Limit, Limit Resets, Today, Yesterday, Last 30 Days |
| Cursor | Browser, at cursor.com | Total Usage, Cursor Models, Other Models, Extra Usage, Grok Bot, Requests, Credits, Today, Yesterday, Last 30 Days |
| Amp | Amp device code | Agent Usage, Orb Hours, Amp Free, Credits |
| Antigravity | Browser, via Google OAuth | Gemini 5h, Gemini, Argon, Claude 5h, Claude |
| Copilot | GitHub or GHE.com device code | Credits, Extra Usage, Chat, Completions |
| Copilot organization | An organization name plus a token | Org Credits, Org Spend |
| Devin | Browser, at devin.ai | Daily, Weekly, Extra Usage |
| Grok | xAI device code | Weekly, Extra Usage |
| Muse Code | Meta device code | Session, Weekly |
| Ollama | API key | Session, Weekly, Monthly, Extra Usage, Today, Yesterday, Last 30 Days, Requests |
| OpenCode | OpenCode Console device code | Session, Weekly, Monthly |
| OpenRouter | A Management API key, or a standard API key | Balance, Today, Yesterday, This Week, This Month, Prior 30 Days, Key Limit, Budget, Free Requests |
| Z.ai | API key | Session, Weekly, Web Searches, Limit Resets |

## Permissions Requested

Each OAuth sign-in requests the smallest set of permissions its card requires, and a later refresh never
goes beyond what the sign-in granted. Tokens are kept in the Secret Service.

| Provider | Scopes | Details |
|---|---|---|
| Claude | `user:profile`, `user:inference` | Spending a reset is possible at Anthropic only with a `user:inference` token, so Headroom requests it. The scope would also let the token run Claude on your plan, which Headroom never does. |
| ChatGPT | `openid profile email offline_access` | Standard OpenID Connect; the email is what names the account. |
| Cursor | none | Cursor's sign-in produces a dashboard session, which has no scopes. |
| Amp | none | Uses the device sign-in of the Amp CLI. Its token is valid for five minutes, so Headroom renews it ahead of every read. |
| Antigravity | `cloud-platform`, `userinfo.email` | Google's quota API demands `cloud-platform`. |
| Copilot | `read:user` (ignored by GitHub) | A GitHub App's requested scopes are ignored by GitHub, so the token cannot be limited. Headroom uses it solely for your login and quota. |
| Copilot organization | none; you create the token | A fine-grained personal access token with Organization permissions, Administration, Read-only, created by an organization owner or billing manager. |
| Devin | none | Reuses the Devin CLI's sign-in, so the returned session token can do whatever the CLI can. Headroom only uses it to read your quota. |
| Grok | `openid email offline_access grok-cli:access` | `api:access` is not requested, since it would allow the token to run Grok models. |
| OpenCode | none | Uses the device sign-in with which the OpenCode CLI links a Console account, limited to the workspace you choose. |
| Muse Code | none | Meta's device sign-in has no scopes. The usage response includes the account's Model API key, which Headroom never reads or stores. |

A tighter set of permissions takes effect the next time an account is added. To apply it to an account you
already have, remove the account and add it again.

## Claude

- Sign in with claude.ai in your browser.
- The plan (Pro, Max, or Ultra) is read once an hour. Ultra has no 5-hour limit, so it has no Session row.
- Extra Usage reports spend against the monthly limit, or the amount spent when no limit exists, in the
  account's billing currency. It is hidden while extra usage is disabled.
- **Limit Resets** tallies the account's rate-limit resets. Expand it to see each reset by kind (Session,
  Weekly, Fable, or Full Reset), with a countdown and its expiry. Choose **Use** and confirm to spend the
  one you selected. **Use** appears only for a reset that Anthropic says is usable now. If Anthropic needs a
  different reset to be used first, the panel tells you which one, and nothing is spent.
- An account that was signed in before Headroom began requesting `user:inference` continues to read usage,
  but Anthropic rejects its reset claims with HTTP 401. Remove and re-add the account.

## ChatGPT

- Sign in with OpenAI in your browser. If Headroom cannot listen on port 1455, it falls back to a device
  code.
- On the command line this provider is `codex` (`chatgpt` also works when adding).
- Extra Usage reports your credits in dollars at 4¢ per credit, even at `$0`, and is hidden while ChatGPT
  reports credits as off.
- Rate-limit resets behave like Claude's, and each one is a Full Reset.
- Credit Limit is the amount of credits a workspace allows you to spend, with its reset time. It appears
  only when the workspace sets a limit.

Today, Yesterday, and Last 30 Days list the text tokens ChatGPT counted for each UTC day, taken from the
reports behind the Analytics view of the Codex app. ChatGPT gives no dollar amount for them. These reports
are read once an hour, at the start of a UTC day, and on a manual refresh. When today's or yesterday's
counts cannot be read, that range shows 0, while Last 30 Days still adds the earlier days.

## Cursor

- Sign in at cursor.com in your browser.
- Extra Usage is on-demand usage and follows the billing cycle. It is hidden when on-demand usage is off
  (no spend limit, or a limit of $0). If the limit is the team's shared pool rather than your own, the row
  is labeled Team Spend.
- Credits shows `$0 left` when Cursor reports no credits.
- A unified account (Cursor, X and Grok combined) has a single weekly included pool. It therefore shows
  Total Usage for that pool, without Cursor Models, Other Models, or Grok Bot rows. Credits is the balance
  on the dashboard's Credits card, and Extra Usage is the cost of this month's usage beyond the pool, paid
  from credits.

Today, Yesterday, and Last 30 Days display what Cursor billed and the tokens it reported, from the usage
history on Cursor's Usage page. Only on-demand usage counts toward the charge; included usage and errored or
free requests count as $0. Every event counts toward tokens.

- Days are read one by one, up to 4 at once, with a cap of 20 seconds per refresh.
- Today and yesterday are read on every refresh, and an older day is read until one read succeeds.
- A day that fails contributes nothing to a range.
- When Cursor rate-limits a read, the retry waits for its Retry-After, or for an hour.

## Amp

- Sign in with an Amp device code, on the page that `amp login` opens with the code pre-filled. The account
  is named with the email Amp reports.
- The card mirrors the output of `amp usage`: **Agent Usage** (agent dollars spent from the plan),
  **Orb Hours**, **Amp Free** (the daily free allowance), and **Credits** (your individual credits, shown
  even at `$0 left`).
- A row appears only if Amp reports it. Agent Usage and Orb Hours reset when the billing period Amp reports
  ends. Amp does not say when the daily Amp Free allowance resets.

## Antigravity

- Sign in with Google in your browser.
- The card has the Gemini and Claude pools (5-hour and weekly) and the weekly Argon window.
- Quota requests present themselves as the Antigravity hub client, which makes Google include the separate
  Argon window.
- A project without a quota subscription gets a card that explains the access problem. If the project lookup
  failed, refresh once more.

## Copilot

- Sign in with a GitHub device code. For GitHub Enterprise on GHE.com, enter the domain, for example
  `octocorp.ghe.com`. Pasting a link works, and a plain name such as `octocorp` is read as
  `octocorp.ghe.com`. Self-hosted GitHub Enterprise Server is unsupported. On the command line, pass
  `--domain`.
- A GHE.com sign-in verifies your Copilot seat once, and the account is rejected if you have none.
- Extra Usage is hidden when the plan does not permit it.

To view an organization's billing, add it as a separate account: **Organization Billing** in the panel, or
`--organization` on the command line, with a token from an owner or billing manager. The card has **Org
Credits** (the AI credits the organization has used this month) and **Org Spend** (dollars billed past the
included pool).

## Devin

- Sign in to Devin in the browser window that opens.
- Usage is read from `server.codeium.com`, the default server of the Devin CLI.
- Extra Usage reports the balance whenever Windsurf gives one, including `$0 left`. A zero balance is
  omitted by Windsurf, so the row is hidden in that case.
- If a plan conceals its daily quota and has no weekly one, the daily quota appears in the Weekly row.
- An account with neither quota nor balance has nothing to display, and the card states that.

## Grok

- Sign in with an xAI device code.
- Weekly is the shared pool. Extra Usage reports the pay-as-you-go cap, and is hidden when there is no cap
  or the cap is $0.
- A team account has no personal quota; its card shows the plan and says so.

## Muse Code

- Sign in with a Meta device code. The account is named with the email Muse reports.
- The card has the Muse Code plan, the 5-hour Session, and the Weekly limit.
- Muse reports nothing until the plan's first prompt, so both limits read 0% until then.
- While the 5-hour window is idle, Muse omits usage, so the card holds the last values it read. A window
  whose reset time has passed reads 0%.
- A Meta account lacking a Muse Code subscription has no quota, and the card says so.
- This sign-in cannot be renewed. Once Meta stops accepting it, remove the account and add it again.

## Ollama

- Paste an API key from [ollama.com/settings/keys](https://ollama.com/settings/keys).
- The card has whichever of the session, weekly, and monthly windows your plan includes. A free plan has
  none, so its card has Extra Usage and the usage rows below.
- Extra Usage reports purchased credits whenever Ollama gives them, including `$0 left`.

Today, Yesterday, and Last 30 Days give Ollama's usage by UTC day: its dollar value (the worth of the
requests, whichever of plan or credits covered them) and the input and output tokens. Requests covers the
last 30 days. A legacy plan reports only requests, so its card has the Requests row alone. The first read
covers all 30 days; after that, today and yesterday are read once an hour, at the start of a UTC day, and on
a manual refresh.

## OpenCode

- Sign in with an OpenCode Console device code, the same sign-in the OpenCode CLI uses. Choose the workspace
  on the page that opens. The account is named with your Console email, and every workspace becomes its own
  account.
- The card has the session, weekly, and monthly caps, along with the plan the Console reports: Go, Go Plus,
  or Go Teams. If reading the plan fails, the caps remain and the plan is left out until the next attempt
  an hour later.

## OpenRouter

Paste a **Management** key from
[openrouter.ai/settings/management-keys](https://openrouter.ai/settings/management-keys) to see the entire
account.

- **Balance** is credits purchased minus credits spent, and it has no bar.
- Today, This Week, and This Month total the spend of all standard keys in your default workspace,
  including disabled and expired ones.
- Yesterday and Prior 30 Days come from OpenRouter's activity data, which spans the 30 UTC days before
  today. The first read covers all 30 days, then yesterday is read once an hour.
- A Management key limited to one workspace has no Balance, and its ranges total that workspace's keys.
- Spend through your own provider keys (BYOK) is never counted in the credit spend.

A standard API key reports its own spend: Today, This Week, This Month, **Key Limit** if the key has one,
**Budget** if a guardrail or workspace budget applies, and **Free Requests** for the UTC day. Daily, weekly,
and monthly limits reset at midnight UTC, and weeks begin on Monday.

## Z.ai

- Paste an API key from the [Z.ai key list](https://z.ai/manage-apikey/apikey-list).
- The card has the session and weekly windows of the GLM Coding Plan, plus the web-search quota.
- A key without a GLM Coding Plan has no quota, and the card says so.
- **Limit Resets** tallies the banked Session and Weekly resets. Expand it for a single list, each entry
  titled Session Reset or Weekly Reset. **Use** spends the earliest one of its kind.

## Errors

When the latest refresh failed, or the provider has no usage to report, the status line under the account's
name gives the reason.

- **Sign-in expired.** Remove the account, then add it again.
- **Rejected the API key.** Remove the account, then add it with a new key.
- **Rate limited** and HTTP errors go away on a later refresh. See [Refreshing](refreshing.md).
- Accounts that come from a CLIProxyAPI hub have messages of their own. See
  [CLIProxyAPI hubs](cliproxyapi.md#when-something-goes-wrong).
