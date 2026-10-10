# Building Monolith Desktop on Linux

The player uses Rust and Slint. A Linux build has not yet been verified. Only the
installer is Windows-only; nothing here needs it.

## What differs from Windows

| Area | Windows | Linux |
| --- | --- | --- |
| Tray icon and media keys | System Media Transport Controls | Not wired up yet |
| Translucent background | DWM acrylic | Not available, the toggle is disabled |
| Icon and version resource | `assets/windows.rc` via `rc.exe` | Not built; the launcher uses the SVG |
| System theme | `AppsUseLightTheme` in the registry | `org.gnome.desktop.interface color-scheme` |
| `smtc_check` example | built with `--features windows-smtc` | not built |

Dark is the default on both, so a fresh Linux install matches the Windows one.

## Build on Linux

Ubuntu 24.04 or newer.

```sh
sudo apt update
sudo apt install -y build-essential pkg-config libasound2-dev \
                    libxkbcommon-dev libwayland-dev libx11-dev libxrandr-dev \
                    libxi-dev libxcursor-dev libgl1-mesa-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

cargo build --release          # target/release/monolith-desktop
cargo test --lib                # no network or audio device needed
```

The first build compiles the bundled SQLite and the audio backend, so give it
several minutes. Playback of local files needs no network. Downloads are explicit user actions.

`rfd` 0.17.2 defaults to the XDG portal backend; GTK development headers are
not required by the current manifest. A running desktop portal is needed for
file dialogs on Linux.

## Build without a Linux machine

`packaging/linux/Dockerfile` defines a Linux build and Debian packaging step.
Linux compilation and package installation are pending verification.
The image and build prerequisites can require substantial downloads.
On a Linux build host or a working Docker installation:

```sh
docker build -f packaging/linux/Dockerfile -t monolith-linux .
docker create --name monolith-build monolith-linux
docker cp monolith-build:/out/. dist/
docker cp monolith-build:/out/monolith-desktop.desktop dist/
docker cp monolith-build:/out/monolith.desktop.metainfo.xml dist/
docker cp monolith-build:/out/monolith-desktop.svg dist/
docker rm monolith-build
```

The image also runs the test suite during the build, so a green image is the
evidence that the Linux build works and not just that it linked.

## Install for the current user

```sh
./packaging/linux/install.sh
```

Writes into `~/.local` only: the binary, an SVG icon, a `.desktop` launcher and
an AppStream `metainfo.xml`. No `sudo`, no package manager, nothing outside your
home directory. Override the root with `PREFIX=/opt ./install.sh` if you want a
system-wide copy.

Remove it again with `./packaging/linux/install.sh --uninstall`. Your library
file in `~/.local/share/MonolithDesktop` is left alone on purpose.

## Where the data lives

`$XDG_DATA_HOME/MonolithDesktop` (usually `~/.local/share/MonolithDesktop`),
holding `library.sqlite3` and any settings. Set `MONOLITH_DATA_DIR` to point the
app somewhere else, which is what the checks use to stay out of your real
library.

## Downloader

Monolith does not ship `yt-dlp` or FFmpeg. For the Downloads page:

```sh
sudo apt install yt-dlp ffmpeg
```

Select the yt-dlp executable in Settings; MP3 conversion requires FFmpeg on `PATH`.

## Checking it works

`cargo run --release --example visual_check -- /tmp/shot.ppm 1280 900 0` writes a
PPM of any page using a real window (requires a display server). Convert it to look at it:

```sh
ffmpeg -loglevel error -y -i /tmp/shot.ppm -frames:v 1 -update 1 /tmp/shot.png
```

`workflow_check` exercises import, playback control and persistence against a
throwaway database. `transfer_check` drives a fake child process and never
touches the network.

## Debian package (local build, no installer downloads)

After building on Debian 12 / Ubuntu 24.04 or newer:

```sh
sh packaging/linux/build-deb.sh
# Or provide an existing native executable and output directory:
sh packaging/linux/build-deb.sh target/release/monolith-desktop dist
# Then install the resulting package on a compatible Linux machine:
sudo apt install ./dist/monolith-desktop_0.3.0_amd64.deb
```

Requires `dpkg-dev`, `binutils` and the runtime development libraries on the
build machine. The script rejects non-ELF binaries, derives amd64/arm64 from the
ELF header, and uses `dpkg-shlibdeps` for runtime dependency versions. It does not
download anything, run the player, or modify the system. It bundles the launcher,
icon, AppStream metadata and license; personal library data is never packaged.
Build on the oldest Debian/Ubuntu release you intend to support. A successful
archive build still needs launch, audio and file-dialog checks on the target OS.

2026-10-02: shell syntax checked on Windows; no native Linux executable or `.deb`
produced here. Neither Docker nor an Ubuntu VMware guest was running.

## Shared interface and folder imports

Linux builds compile the same `ui/` components as Windows, including animated
navigation, compact layouts and the updated Downloads page. `src/inbox.rs` also
runs on both platforms: it scans Music/Monolith and the selected save folder on
the library worker, waits for stable copies, and avoids duplicate imports.
These source changes have Windows runtime coverage; Linux runtime remains unverified.

No verified `.deb` artifact is included. The per-user installer supports both a
source checkout and the self-contained Docker `/out` bundle; it quotes executable
paths and installs an icon matching the desktop entry.

File-layout regression (stub executable; no audio, downloads or native compilation):

```sh
sh packaging/linux/check-install.sh
```

This checks both source and bundle installs, icon lookup, quoted executable paths,
uninstall cleanup and preservation of the library directory.

The shared October 10 update adds three-step tours for each tab, full-brightness
spotlights, scroll-to-target positioning, and clearly labelled empty-library
examples. Player titles open the expanded player. Reimported moved songs can
reconnect to a unique old entry with a database backup and preserved collections.
These changes have Windows verification only; a native Linux build and `.deb`
remain deferred under the 100 MB download cap.
