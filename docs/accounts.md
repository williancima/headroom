# Accounts

You add accounts in the panel's **Accounts** screen (the people button in the header, or the accounts button in the
GNOME extension) or with the [`headroom` command](cli.md). Both use the same sign-ins. Headroom only knows
the accounts you add; it never looks for credentials of other tools.

## Add an account

1. Click **Add Account** and pick a provider. Copilot first asks for **Sign In with GitHub** (your own seat,
   with an optional GHE.com domain) or **Organization Billing** (an organization's credits and spend).
2. Finish the sign-in:

| Sign-in | Providers | What to do |
|---|---|---|
| Browser | Claude, ChatGPT, Cursor, Antigravity, Devin | Sign in in the browser that opens. |
| Device code | Amp, Copilot, Grok, Muse Code, OpenCode | Copy the code, click **Open Page**, and enter it there. |
| API key | Ollama, OpenRouter, Z.ai | Paste a key from the provider's page. It is checked with one usage read before it is saved. |
| Organization billing | Copilot | Enter the organization and a token from an owner or billing manager. |

A browser sign-in waits up to 5 minutes. If it fails, click **Try Again**. ChatGPT listens on port 1455 for
the browser's answer; if that port is busy, it offers a device code instead.

To add the logins of a CLIProxyAPI server, add a hub. See [CLIProxyAPI hubs](cliproxyapi.md).

- You can add several accounts per provider. A new account goes to the bottom of its provider's accounts.
- An account takes the email or name the provider reports. API-key accounts other than Ollama are named
  after the provider, such as "OpenRouter Account".
- A sign-in or key you already use isn't added twice. To sign in again, remove the account and add it back.
- A Copilot organization is added once, however you spell it.

## Hide and pin metrics

An account's entry on the Accounts screen lists the metrics it reported, with a **Show** and a **Pin** switch
each. See [Hiding, pinning, and ordering metrics](dashboard.md#hiding-pinning-and-ordering-metrics).

## Remove an account

**Remove** on the Accounts screen, or `headroom accounts remove <id>`, deletes the account, its saved usage,
and its credential in the Secret Service. An account from a CLIProxyAPI hub is removed with its hub.

## Where things are stored

- `~/.config/headroom/accounts.json`: each account's id, provider, name, and kind-specific fields
  (a Copilot domain or organization, a hub link). No credentials.
- Tokens and keys: the Secret Service (service `headroom`), one entry per account id.
- Layout (hidden and pinned metrics, order, unified providers): `layout.json` in the same folder.

See [Privacy](privacy.md).

## When a sign-in expires

The account shows the error, "Sign-in expired. Sign in again." Remove the account and add it again; it goes
to the bottom of its provider's accounts. A rejected API key works the same way.
