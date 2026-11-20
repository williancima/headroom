# AGENTS.md

Headroom is a Linux app (Rust, GTK4/libadwaita, plus a GNOME Shell extension) that shows the usage
limits of the AI coding accounts a user adds within the app (Claude, Codex, Cursor, Amp, Copilot,
Antigravity, Devin, Grok, Muse Code, Ollama, OpenCode, OpenRouter, Z.ai, and CLIProxyAPI hubs).

These are the engineering conventions for Headroom. Read them before you contribute.

## Agent Instructions

Agent instructions for this repository are defined in AGENTS.md alone. A CLAUDE.md file may do nothing
except reference the closest AGENTS.md through `@AGENTS.md`; never put guidance, repeated instructions, or
project rules in CLAUDE.md.

## Releases

- **Never raise the version number on your own initiative; always get explicit approval first.**
  Suggest the number, then wait for sign-off before tagging or cutting a release.
- Never leave a release in Draft, and never ship blank notes.

Steps, once the owner has approved the number (`X.Y.Z`):

1. Set `version = "X.Y.Z"` in the three `crates/*/Cargo.toml` files and in their `headroom-core` path
   dependencies, then run `cargo check -j 2` so `Cargo.lock` follows.
2. Add `<release version="X.Y.Z" date="YYYY-MM-DD"/>` at the top of `<releases>` in
   `data/dev.headroom.Headroom.metainfo.xml`.
3. Run the checks under Running / Testing Changes, then commit the bump on `main`.
4. Tag with notes as the message: `git tag -a vX.Y.Z` (the editor opens; write user-facing notes, never
   blank), then push `main` and the tag.
5. `.github/workflows/release.yml` refuses a tag that doesn't match `crates/headroom-app`, isn't annotated,
   or has an empty message. It builds the .deb, the AppImage and the extension zip, writes `SHA256SUMS`,
   and publishes the release with the tag message as its notes (never as a Draft).
6. The same workflow signs `SHA256SUMS` with minisign (trusted comment `headroom X.Y.Z`) and publishes
   `SHA256SUMS.minisig` with the other files; the AppImage self-update rejects a release without it. The
   key and its password are the repository secrets `MINISIGN_SECRET_KEY` and `MINISIGN_PASSWORD`.
7. It also builds a flat APT repository holding the new .deb, signs it with the GPG key in the secrets
   `APT_GPG_KEY` and `APT_GPG_PASSPHRASE` (public half: `data/apt/headroom-archive-keyring.gpg`, shipped in
   the .deb), and deploys it to GitHub Pages (`https://williancima.github.io/headroom/`). The `github-pages`
   environment allows `v*` tags.
8. Check the release page lists all five files and the Pages deploy succeeded, then install the .deb or
   AppImage in the test VM.

## Architecture

- `crates/headroom-core`: providers (one module per provider under `src/<name>/`), sign-in (`oauth`,
  `login`), storage (`store`), Priority ranking, usage history, layout (hidden/pinned/ordered metrics,
  unified accounts) and the view model shared by every UI.
- `crates/headroom-app`: the long-lived app (`headroom-app`): refresh worker, GTK4 panel, Accounts
  window, ksni tray icon, and D-Bus GActions on `dev.headroom.Headroom` at `/dev/headroom/Headroom`.
- `crates/headroom-cli`: the `headroom` CLI; `headroom panel` prints the view model as JSON for the
  GNOME extension.
- `extension/`: GNOME Shell extension. Logic that can run under node lives in `*_logic.js` with a
  `*_logic.test.mjs` next to it.
- Account metadata lives in `$XDG_CONFIG_HOME/headroom/accounts.json`; credentials live only in the
  Secret Service. State (snapshots, backoffs, history) lives in `$XDG_STATE_HOME/headroom/`.
- Users initiate all provider sign-ins from the Accounts window. Never discover credentials or
  accounts from other tools, and never read their files, keyrings, SQLite databases, or environment
  variables.
- Each provider calls its API and maps the response to dashboard metrics. Do not add log scanning,
  spend estimates, or external pricing; the one exception is ChatGPT's credits, priced at 4¢ each in
  the Codex mapper.
- The dashboard displays only limits and dollar values that the provider supplies. Providers and
  accounts appear in the user's saved order (`accounts.json`); a new account is placed at the bottom of
  its provider's accounts. Never sort providers or accounts by catalog order or by name.
- The one exception to provider-supplied values: a unified multi-account row may average provider
  percents, at the owner's request.
- See `docs/` for behavior docs and the developer docs (`architecture.md`, `adding-a-provider.md`).

## Providers

Provider sign-in and usage code lives under `crates/headroom-core/src/<name>/`. Provider API failures
surface as friendly account-specific errors without logging credentials or response bodies. See
`docs/adding-a-provider.md`.

**Metric order default:** every metric that has data is shown. When you add or change a metric, check
its order with the owner before deciding; never choose it silently.

## Running / Testing Changes

The development machine has 7.7 GB of RAM. Run one cargo command at a time, never in parallel, with
low parallelism:

```sh
CARGO_INCREMENTAL=0 cargo test --workspace -j 2
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets -j 2 -- -D warnings
for t in extension/*.test.mjs; do node "$t"; done
```

**Never run development builds on the host.** No `headroom-app` or `headroom` from `target/`, no extension
install from the repo, no `~/.local/bin` link to a build. A released .deb or AppImage the owner installs on
the host is normal use: leave it, its settings and its credentials alone, and never stop or restart it.
Building and the commands above are fine on the host. Every live check (app, panel, extension, sign-in,
shortcut, notifications) runs inside the QEMU VM from `scripts/vm.sh` (`create`, then `start`; it
mounts the repo at `/mnt/headroom`, so the VM runs the host's `target/release` build). Sign in to
accounts from the Accounts window inside the VM; never copy credentials into it from the host.

The app is a long-lived process with no hot reload: rebuild and restart `headroom-app` in the VM after
every change. The extension is reinstalled with `gnome-extensions pack` + `install --force` (see
`docs/gnome-extension.md`); on Wayland a changed extension only loads after logging out and in.

## Pull Requests

Each PR description must use the structure below, so reviewers can skim it fast:

- **TL;DR** — begin with a one- or two-sentence plain-English summary of the change.
- **What was happening** — plain-English bullets on the earlier behavior, bug, or gap that prompted the
  change.
- **What this changes** — bullets on what the PR really changes.
- **Heads-up** (optional) — risks, follow-ups, trade-offs.
- **Tests** (optional) — how the change was checked.
- **Screenshots** (required for any PR that makes a visual change).

## Documentation

- A logic change must also update `README.md` and every page in `docs/` that describes the affected
  behavior.
- Write docs that are plain, brief, and easy to skim, leaving out visual design details. Delete the
  documentation of features that no longer exist.

## Code Conventions

- Keep each file under roughly 500 lines; split out focused concerns when needed.
- Follow existing Rust formatting (`cargo fmt`) and keep clippy clean.
- Validate user input and provider API responses at the boundary; trust internal code.
- Every behavior change gets a focused regression test.
- Do not add dependencies without a clear need. When adding a zbus-based crate, check
  `cargo tree -i zbus -e features`: zbus must not get its `tokio` feature.

## Error Handling

Fail loudly: log the error and show a friendly, account-specific message to the user. Do not add silent
fallbacks that hide real problems. Never log credentials or response bodies.

## UI

- Hardcoded copy that serves as a title uses title case.
- Match the existing GNOME/libadwaita design language.
- Only add tooltips when explicitly asked to.
