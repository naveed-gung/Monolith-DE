#!/bin/sh
# File-layout regression only: uses a stub, never starts a player or downloads.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/monolith-install-check.XXXXXX")
trap 'rm -rf -- "$work"' EXIT HUP INT TERM
bundle="$work/bundle"
source="$work/source"
mkdir -p "$bundle" "$source/packaging/linux" "$source/target/release" "$source/assets"
printf '#!/bin/sh\nexit 0\n' > "$bundle/monolith-desktop"
chmod +x "$bundle/monolith-desktop"
cp "$root/packaging/linux/install.sh" "$bundle/install.sh"
cp "$root/packaging/linux/monolith-desktop.desktop" "$bundle/"
cp "$root/packaging/linux/monolith.desktop.metainfo.xml" "$bundle/"
cp "$root/assets/monolith.svg" "$bundle/monolith-desktop.svg"
cp "$bundle/monolith-desktop" "$source/target/release/"
cp "$bundle/install.sh" "$bundle/monolith-desktop.desktop" "$bundle/monolith.desktop.metainfo.xml" "$source/packaging/linux/"
cp "$bundle/monolith-desktop.svg" "$source/assets/monolith.svg"
export XDG_DATA_HOME="$work/data"
mkdir -p "$XDG_DATA_HOME/MonolithDesktop"
printf 'preserve library\n' > "$XDG_DATA_HOME/MonolithDesktop/library.sqlite3"
export PREFIX="$work/install & "'$music % folder'
for script in "$bundle/install.sh" "$source/packaging/linux/install.sh"; do
    sh "$script"
    cmp "$bundle/monolith-desktop" "$PREFIX/bin/monolith-desktop"
    test -f "$PREFIX/share/icons/hicolor/scalable/apps/monolith-desktop.svg"
    desktop="$PREFIX/share/applications/monolith-desktop.desktop"
    grep -Fx 'Icon=monolith-desktop' "$desktop"
    grep -F '\\$music %% folder/bin/monolith-desktop"' "$desktop"
    sh "$script" --uninstall
    test ! -e "$PREFIX/bin/monolith-desktop"
    test ! -e "$desktop"
    test ! -e "$PREFIX/share/icons/hicolor/scalable/apps/monolith-desktop.svg"
    grep -Fx 'preserve library' "$XDG_DATA_HOME/MonolithDesktop/library.sqlite3"
done
if PREFIX=relative sh "$bundle/install.sh" >/dev/null 2>&1; then
    echo 'FAIL: relative prefix accepted' >&2
    exit 1
fi
printf 'PASS: source/bundle install, icon lookup, quoted paths, uninstall and data preservation; stub only.\n'
