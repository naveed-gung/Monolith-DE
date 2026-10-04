#!/bin/sh
# Installs Monolith Desktop for the current user. No root, no package manager.
#
#   ./packaging/linux/install.sh            install or update
#   ./packaging/linux/install.sh --uninstall remove everything it wrote
#
# Run from the repository root. The binary must already be built:
#   cargo build --release
set -eu

APP=monolith-desktop
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
APP_DIR="$PREFIX/share/$APP"
ICON_DIR="$PREFIX/share/icons/hicolor/scalable/apps"
DESKTOP_DIR="$PREFIX/share/applications"
METAINFO_DIR="$PREFIX/share/metainfo"
ICON_NAME="$APP"

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/../.." && pwd)
BUILT="$ROOT/target/release/$APP"
ICON_SRC="$ROOT/assets/monolith.svg"
DESKTOP_SRC="$ROOT/packaging/linux/$APP.desktop"
META_SRC="$ROOT/packaging/linux/monolith.desktop.metainfo.xml"
# Docker's /out directory is also a self-contained install bundle.
if [ -f "$HERE/$APP" ]; then
    BUILT="$HERE/$APP"
    ICON_SRC="$HERE/$APP.svg"
    DESKTOP_SRC="$HERE/$APP.desktop"
    META_SRC="$HERE/monolith.desktop.metainfo.xml"
fi

# Every path is quoted and prefixed with -- so a space in $HOME cannot turn into
# a second argument.
die() { printf '%s\n' "$1" >&2; exit 1; }
case "$PREFIX" in /*) ;; *) die "PREFIX must be an absolute path" ;; esac
# Desktop Entry Exec paths cannot contain '=' or line breaks.
case "$PREFIX" in *'='*|*'
'*) die "PREFIX cannot contain an equals sign or newline" ;; esac

uninstall() {
    rm -f "$BIN_DIR/$APP"
    rm -rf "$APP_DIR"
    rm -f -- "$ICON_DIR/$ICON_NAME.svg" "$ICON_DIR/$APP-symbolic.svg"
    rm -f "$DESKTOP_DIR/$APP.desktop"
    rm -f "$METAINFO_DIR/monolith.desktop.metainfo.xml"
    rmdir "$ICON_DIR" "$DESKTOP_DIR" "$METAINFO_DIR" 2>/dev/null || true
    printf 'Monolith removed from %s\n' "$PREFIX"
    printf 'Your library in %s was left alone.\n' "${XDG_DATA_HOME:-$HOME/.local/share}/MonolithDesktop"
}

if [ "${1:-}" = "--uninstall" ]; then
    uninstall
    exit 0
fi

[ -x "$BUILT" ] || die "Build first: cargo build --release  (looked in $BUILT)"

[ -f "$ICON_SRC" ] || die "missing $ICON_SRC"

mkdir -p -- "$BIN_DIR" "$APP_DIR" "$ICON_DIR" "$DESKTOP_DIR" "$METAINFO_DIR"

install -m 0755 -- "$BUILT" "$BIN_DIR/$APP"
# Match Icon=monolith-desktop in the launcher and Debian package.
install -m 0644 -- "$ICON_SRC" "$ICON_DIR/$ICON_NAME.svg"

# Exec has its own quoting rules, including a second backslash-escape pass.
# https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html
executable=$(printf '%s' "$BIN_DIR/$APP" | sed 's/\\/\\\\\\\\/g; s/["`$]/\\\\&/g; s/%/%%/g')
while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
        Exec=*) printf 'Exec="%s"\n' "$executable" ;;
        *) printf '%s\n' "$line" ;;
    esac
done < "$DESKTOP_SRC" > "$DESKTOP_DIR/$APP.desktop"
chmod 0644 -- "$DESKTOP_DIR/$APP.desktop"
# Some shells need the desktop database refreshed before the launcher appears.
command -v update-desktop-database >/dev/null 2>&1 &&
    update-desktop-database "$DESKTOP_DIR" || true

cp -- "$META_SRC" \
    "$METAINFO_DIR/monolith.desktop.metainfo.xml"

printf 'Installed %s to %s\n' "$APP" "$BIN_DIR"
printf 'Run it with: %s\n' "$BIN_DIR/$APP"
