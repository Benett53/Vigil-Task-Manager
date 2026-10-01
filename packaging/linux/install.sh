#!/bin/sh
# Installs Vigil from a release archive.
#   ./install.sh            -> installs for the current user into ~/.local
#   sudo ./install.sh --system  -> installs system wide into /usr/local
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
if [ "${1:-}" = "--system" ]; then
    PREFIX=/usr/local
else
    PREFIX="$HOME/.local"
fi
APP=io.github.baba537.Vigil

install -Dm755 "$HERE/vigil" "$PREFIX/bin/vigil"
install -Dm644 "$HERE/$APP.desktop" "$PREFIX/share/applications/$APP.desktop"
install -Dm644 "$HERE/$APP.metainfo.xml" "$PREFIX/share/metainfo/$APP.metainfo.xml"
install -Dm644 "$HERE/icons/$APP.svg" "$PREFIX/share/icons/hicolor/scalable/apps/$APP.svg"
for size in 32 48 64 128 256; do
    install -Dm644 "$HERE/icons/vigil-$size.png" "$PREFIX/share/icons/hicolor/${size}x${size}/apps/$APP.png"
done

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$PREFIX/share/applications" || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" || true

echo "Vigil installed to $PREFIX/bin/vigil"
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "Note: $PREFIX/bin is not in your PATH." ;;
esac
