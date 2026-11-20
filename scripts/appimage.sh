#!/usr/bin/env bash
# Builds dist/Headroom-x86_64.AppImage from target/release (run `cargo build --release --workspace` first).
# ponytail: does not bundle GTK4/libadwaita; the host needs gtk4 and libadwaita >= 1.5 (GNOME 46+).
# Bundle them via linuxdeploy-plugin-gtk if non-GNOME hosts matter.
set -euo pipefail

cd "$(dirname "$0")/.."
APPID=dev.headroom.Headroom
EXT=usr/share/gnome-shell/extensions/headroom@headroom.dev
TOOL_VERSION=1.9.0
# The release publishes no checksum; this is the 1.9.0 file first downloaded on 2026-11-20. Update both together.
TOOL_SHA256=46fdd785094c7f6e545b61afcfb0f3d98d8eab243f644b4b17698c01d06083d1
CACHE=.cache/appimage
TOOL="$CACHE/appimagetool-$TOOL_VERSION-x86_64.AppImage"
APPDIR=dist/Headroom.AppDir

if [ ! -x "$TOOL" ]; then
  mkdir -p "$CACHE"
  curl -fsSL -o "$TOOL.part" \
    "https://github.com/AppImage/appimagetool/releases/download/$TOOL_VERSION/appimagetool-x86_64.AppImage"
  mv "$TOOL.part" "$TOOL"
fi
echo "$TOOL_SHA256  $TOOL" | sha256sum -c --quiet - || { echo "appimagetool checksum mismatch; delete $TOOL" >&2; exit 1; }
chmod +x "$TOOL"

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/metainfo" \
  "$APPDIR/usr/share/icons/hicolor/scalable/apps" "$APPDIR/usr/share/icons/hicolor/symbolic/apps" \
  "$APPDIR/$EXT/icons"

install -m755 target/release/headroom-app target/release/headroom "$APPDIR/usr/bin/"
install -m644 data/$APPID.desktop "$APPDIR/"
install -m644 data/$APPID.desktop "$APPDIR/usr/share/applications/"
install -m644 data/icons/hicolor/scalable/apps/$APPID.svg "$APPDIR/"
install -m644 data/icons/hicolor/scalable/apps/*.svg "$APPDIR/usr/share/icons/hicolor/scalable/apps/"
install -m644 data/icons/hicolor/symbolic/apps/*.svg "$APPDIR/usr/share/icons/hicolor/symbolic/apps/"
install -m644 data/$APPID.metainfo.xml "$APPDIR/usr/share/metainfo/"
# Same file set as the .deb: every non-test .js, metadata, stylesheet and icons.
install -m644 extension/*.js extension/metadata.json extension/stylesheet.css "$APPDIR/$EXT/"
install -m644 extension/icons/* "$APPDIR/$EXT/icons/"

cat > "$APPDIR/AppRun" <<'RUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
export PATH="$HERE/usr/bin:$PATH"
# `--cli` runs the `headroom` command (the wrapper the app writes to ~/.local/bin calls it).
if [ "${1:-}" = "--cli" ]; then
  shift
  exec "$HERE/usr/bin/headroom" "$@"
fi
exec "$HERE/usr/bin/headroom-app" "$@"
RUN
chmod +x "$APPDIR/AppRun"

mkdir -p dist
# The tool is itself an AppImage; extract-and-run avoids needing FUSE (CI runners lack it).
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" dist/Headroom-x86_64.AppImage
rm -rf "$APPDIR"
