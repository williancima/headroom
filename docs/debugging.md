# Debugging

## Run a local build

Test builds in the QEMU VM from `scripts/vm.sh`, not on your own desktop. It is an Ubuntu 24.04 VM with a
minimal GNOME, so it runs the binaries you build on an Ubuntu 24.04 host.

```sh
scripts/vm.sh create                  # once: downloads the cloud image, disk goes to ~/headroom-vm
scripts/vm.sh start                   # first boot installs GNOME and reboots; user and password: dev
cargo build --release -j 2            # on the host
ssh -p 2222 dev@127.0.0.1             # or use the VM window
```

- The repo is mounted read-only at `/mnt/headroom` in the VM, and `target/release` is on the VM's `PATH`.
  Install the extension from `/mnt/headroom/extension` as in [GNOME Shell extension](gnome-extension.md).
- Sign in to accounts from the Accounts window inside the VM.
- Only one `headroom-app` runs at a time (`dev.headroom.Headroom`). Launching it again shows the running
  panel instead.
- A rebuild needs a restart of the app. There is no hot reload. On Wayland, a changed extension only loads
  after logging out and in.

## Read the output

Headroom has no logging framework and no log level: the app and the CLI print one line per problem to stderr,
prefixed with `headroom-app:` or `headroom:`. `RUST_LOG` does nothing.

- In a terminal, read it there.
- When the app runs under systemd (`systemd-run --user` or a user unit), stderr goes to the journal:
  `journalctl --user -f -t headroom-app`, or `journalctl --user -u <unit> -f`.
- Every failed account refresh prints `headroom-app: <account name>: <message>`. The same message shows under
  the account in the panel.
- A message never holds a credential, a URL query, or a response body.

The extension's own errors show as a red card in its dropdown and in the Shell log
(`journalctl -f -o cat /usr/bin/gnome-shell`). See [GNOME Shell extension](gnome-extension.md).

## Look at the saved state

```sh
headroom usage            # the last fetched usage, as JSON
headroom panel            # what the panel shows, as JSON
ls ~/.config/headroom ~/.local/state/headroom
```

`provider_state.json` holds each account's wait after a failure (see [Refreshing](refreshing.md)), its hourly
read times, and the usage-history days already read. Deleting it clears all of that; the app reads it again.

Check that the app's D-Bus actions are up:

```sh
gdbus introspect --session --dest dev.headroom.Headroom --object-path /dev/headroom/Headroom
```

## Tests

```sh
cargo test --workspace -j 2     # core, app logic, and CLI
cargo clippy --workspace
node extension/breakdown_logic.test.mjs
node extension/layout_logic.test.mjs
node extension/resets_logic.test.mjs
```

Provider tests run against a local `wiremock` server and never reach the real services. Run heavy cargo
commands one at a time on a low-memory machine.
