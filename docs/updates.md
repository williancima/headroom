# Updates

Nothing asks you anything: updates install on their own.

## AppImage

About a minute after the app starts, and every 6 hours after that, it reads the latest
[GitHub release](https://github.com/williancima/headroom/releases). If the version is newer than the running
one (pre-releases are ignored), it downloads the AppImage, the release's `SHA256SUMS` and its minisign signature
`SHA256SUMS.minisig`. It checks that the list is signed by the Headroom release key for that exact version, then
checks the file's SHA-256, replaces the AppImage file in place and restarts. A download larger than expected
(1 MiB for the release answer and the checksum files, 512 MiB for the AppImage) is refused before any check. A release without the signature
is skipped until a later check. The request uses the [proxy](settings.md#network)
if one is set.

- The folder holding the AppImage must be writable; otherwise the app logs that and does not check.
- A failed check or download (network, bad signature or checksum) deletes the partial `.part` file, logs a short line and
  tries again in 6 hours. Nothing is shown in the window.

## .deb

The package adds the Headroom APT repository (`https://williancima.github.io/headroom/`) with its signing
key, and lets unattended-upgrades install from it. Ubuntu's daily automatic upgrade then installs new versions
like any other package; `sudo apt upgrade` does the same by hand. Files it adds:

- `/etc/apt/sources.list.d/headroom.sources`
- `/etc/apt/keyrings/headroom-archive-keyring.gpg`
- `/etc/apt/apt.conf.d/52headroom-unattended`

`sudo apt purge headroom` deletes them; `sudo apt remove headroom` keeps them, like any configuration file.

## Restart after an update

Every minute the app checks whether its own file was replaced on disk (what a package upgrade does). If so,
it starts a new copy once the old one has exited, and quits. The same restart follows an AppImage update.

## Launch at Login

If the autostart file points to a path that is no longer the running one, the app rewrites it at start. See
[Settings](settings.md#general).
