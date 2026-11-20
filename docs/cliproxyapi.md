# CLIProxyAPI hubs

[CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) is a server that keeps sign-ins for several
people or machines. Connect a hub, and its Claude, ChatGPT, Antigravity, Devin, and Grok logins show in
Headroom as ordinary accounts.

- The hub owns the list of logins. Headroom mirrors it.
- Several hubs can be connected at once.
- The hub must allow remote management (`allow-remote` in its config), unless it runs on this machine.

## Connect a hub

In the panel: **Accounts**, then **Add Account**, then **CLIProxyAPI**. Enter the hub's URL, such as
`https://hub.example.com`, and its management key, then click **Connect**.

From a terminal:

```sh
headroom hubs add hub.example.com:8317   # asks for the management key
headroom hubs list
headroom hubs key <id>                   # replace the key (tested first)
headroom hubs remove <id>                # ids may be a unique prefix
```

A pasted panel address (`…/management.html`) or API address (`…/v0/management`) works too. If the address or
key is wrong, nothing is saved and the message says why. Every supported login on the hub appears at once,
named after its email.

An address without a scheme uses https. Plain `http://` works only for this machine, the local network
(`localhost`, private or link-local IPs, `.local` names), and Tailscale (`….ts.net`, `100.64.0.0/10`,
`fd7a:115c:a1e0::/48`).

## Hub accounts

A hub account works like any other account, except that the hub manages it: `headroom accounts remove`
refuses it, and the Accounts screen has no **Remove** for it. A login that is also signed in directly shows
twice.

## Staying in sync

Headroom reads the hub's login list at launch, about once an hour, and on every manual refresh. A manual
refresh reads the list before usage, so a login turned off on the hub is gone right away.

- If a read fails, it is retried on the following update, roughly 5 minutes on.
- When the hub tells an account its login is gone, Headroom reads the list again at once.
- A login newly added on the hub is placed last among its provider's accounts.
- A login removed or disabled on the hub disappears at the next read. Enabled again, it comes back as a new
  account.
- A renamed login, or a ChatGPT login on another workspace, keeps its account, name, and pins.

Logins for unsupported providers don't show. That includes Cursor, because the hub holds a Cursor API key and
Cursor's usage reads need Headroom's own sign-in.

## In Accounts

Each hub has a row under the accounts, with its host and subtitle. **Remove** deletes the hub's accounts and
key from this machine; nothing changes on the hub. To replace the key, use `headroom hubs key`.

## When something goes wrong

| The account says | Fix |
|---|---|
| Can’t reach the CLIProxyAPI hub at <host> | Start the hub or check the address |
| … rejected the management key | `headroom hubs key <id>` |
| … blocked this machine after too many wrong keys | Wait out the 30-minute ban, then replace the key |
| … only allows management from its own machine | Enable `allow-remote` in the hub's config |
| … has its management API turned off | Define a management key in the hub's config, or correct the address |
| … no working sign-in for this login | Sign the login in again on the hub |
| … no longer on the CLIProxyAPI hub | Nothing. The account leaves once the hub's list drops it |
| … couldn’t reach Claude / ChatGPT | Check the hub's network |
| Use https for <host> | Connect over https |
| … doesn’t use https. Try <address> | Connect with the http address it gives |
| The management key for the CLIProxyAPI hub at <host> is missing | `headroom hubs key <id>`, or remove the hub |
| … isn’t the address its management key was saved for | Remove the hub and connect it again |

After the hub rejects the key or blocks this machine, Headroom sends it nothing until you replace the key.

## What the management key can do

The management key acts as the hub's administrator credential: it can both read and modify the hub's logins and settings.

- Headroom keeps it in the Secret Service under the service `headroom-cliproxy-hubs`, apart from account
  credentials, together with the address it was saved for.
- It sends the key only to that address.
- It only lists logins and sends usage reads and reset claims through the hub's `api-call`. The hub adds each
  login's token, so no provider token reaches this machine.
