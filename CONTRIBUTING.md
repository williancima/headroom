# Contributing to Headroom

Headroom is a Rust and GTK4 app for Linux, plus a GNOME Shell extension. Its job is narrow: show the
usage limits of AI coding accounts. Please read this page before sending a pull request.

## Ground Rules

- For anything larger than a small fix, open an issue first and agree on the approach.
- Stay in scope. Anything unrelated to usage tracking will be declined.
- Verify your work. Changes to the panel or the extension need before and after screenshots.
- Prefer the simple solution over a clever one.
- Send one concern per pull request; do not mix unrelated changes.
- Follow the existing GNOME and libadwaita look.
- Never read credentials, files, or logs that belong to other tools. An account appears only when the user
  adds it.

[AGENTS.md](AGENTS.md) holds the full engineering rules: crate layout, conventions, and the details behind
the list above.

## License

Contributions are released under the project's [MIT License](LICENSE). Submitting a pull request means you
accept that.

## Workflow

1. Fork the repository and create a branch such as `feat/my-change` or `fix/some-bug`.
2. Make the change. When behavior changes, add a focused regression test.
3. Run the checks, one command at a time:

   ```sh
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   for t in extension/*.test.mjs; do node "$t"; done
   ```

4. Update `README.md` and the [docs/](docs/README.md) pages that cover the behavior you touched.
5. Open the pull request against `main`.

Build dependencies on Ubuntu: `libgtk-4-dev libadwaita-1-dev pkg-config`.

### Testing in a VM

Keep Headroom off your own desktop while developing. Run live checks in the QEMU VM:

```sh
scripts/vm.sh create   # once (needs qemu-system-x86, cloud-image-utils and KVM)
scripts/vm.sh start    # user and password: dev; the repo is at /mnt/headroom
```

For a newer GNOME, prefix both commands with a separate folder and Ubuntu codename, e.g.
`HEADROOM_VM_DIR=~/headroom-vm-51 HEADROOM_VM_RELEASE=stonking` (Ubuntu 26.10, GNOME 51). Only one VM runs at a
time (both use port 2222).

Compile on the host with `cargo build --release -j 2`; the VM runs that binary. More in
[docs/debugging.md](docs/debugging.md).

### Adding a Provider

Follow [docs/adding-a-provider.md](docs/adding-a-provider.md). If you would rather not write it yourself,
file a provider request issue.

### Fixing a Bug

Explain the root cause and the fix, attach before and after screenshots when the bug is visual, and add a
regression test.

## What We Merge

- Well-explained bug fixes
- Providers that fit the existing provider design
- Better documentation
- Speed-ups backed by measurements
- Accessibility improvements

## What We Decline

- Anything that widens the scope past usage tracking
- Changes that hurt performance, simplicity, or the current user experience
- Pull requests with no evidence of testing
- Code whose purpose is unclear
- Purely cosmetic changes nobody discussed first

## Questions

Open an issue in this repository.
