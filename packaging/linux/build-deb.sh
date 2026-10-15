#!/bin/sh
# Package an already-built native Linux executable; no downloads or sudo.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
binary=${1:-"$root/target/release/monolith-desktop"}
out=${2:-"$root/dist"}
for tool in dpkg-deb dpkg-shlibdeps readelf; do
    command -v "$tool" >/dev/null || { echo "Required Linux build tool: $tool" >&2; exit 1; }
done
[ -f "$binary" ] || { echo "Build the Linux executable first: $binary" >&2; exit 1; }
# Never wrap a Windows .exe in a Debian archive or mislabel a foreign binary.
machine=$(LC_ALL=C readelf -h "$binary" | sed -n 's/.*Machine: *//p')
case "$machine" in
    'Advanced Micro Devices X86-64') arch=amd64 ;;
    AArch64) arch=arm64 ;;
    *) echo "Unsupported Linux ELF architecture: $machine" >&2; exit 1 ;;
esac
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT HUP INT TERM
stage="$work/package"
install -Dm755 "$binary" "$stage/usr/bin/monolith-desktop"
install -Dm644 "$root/packaging/linux/monolith-desktop.desktop" "$stage/usr/share/applications/monolith-desktop.desktop"
install -Dm644 "$root/packaging/linux/monolith.desktop.metainfo.xml" "$stage/usr/share/metainfo/monolith.desktop.metainfo.xml"
install -Dm644 "$root/assets/monolith.svg" "$stage/usr/share/icons/hicolor/scalable/apps/monolith-desktop.svg"
install -Dm644 "$root/LICENSE" "$stage/usr/share/doc/monolith-desktop/copyright"
mkdir -p "$stage/DEBIAN" "$work/debian" "$out"
cat > "$work/debian/control" <<EOF
Source: monolith-desktop
Section: sound
Priority: optional
Maintainer: Monolith Desktop maintainers <maintainers@localhost>

Package: monolith-desktop
Architecture: $arch
Description: Native local music player
EOF
# Resolve linked runtime versions from the ELF. Control also lists the display
# libraries loaded dynamically by winit/Slint, which shlibdeps cannot detect.
deps=$(cd "$work" && dpkg-shlibdeps -O -e"$stage/usr/bin/monolith-desktop" | sed -n 's/^shlibs:Depends=//p')
[ -n "$deps" ] || { echo "Could not determine runtime dependencies" >&2; exit 1; }
cat > "$stage/DEBIAN/control" <<EOF
Package: monolith-desktop
Version: $version
Architecture: $arch
Section: sound
Priority: optional
Maintainer: Monolith Desktop maintainers <maintainers@localhost>
Depends: $deps, libxkbcommon0, libx11-6, libxcursor1, libxrandr2, libxi6, libwayland-client0, libwayland-cursor0, libegl1, libgl1, curl, ca-certificates, xdg-utils
Recommends: xdg-desktop-portal
Installed-Size: $(du -sk "$stage/usr" | cut -f1)
Description: Native local music player
 Import local music and playlists, search your collection and listen offline.
 Uses Rust and Slint. External download tools are optional and not bundled.
EOF
package="$out/monolith-desktop_${version}_${arch}.deb"
dpkg-deb --root-owner-group --build "$stage" "$package"
dpkg-deb --info "$package"
dpkg-deb --contents "$package"
