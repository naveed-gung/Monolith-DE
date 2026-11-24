# Changelog

## 0.7.1

- Fixed song downloads failing with `yt-dlp`: removed invalid `--no-part-overwrites`
  and `--no-write-thumbnail-to-console` flags, and prevented Linux builds from
  downloading Windows helper binaries in Settings.
- Music Share: keep Wi-Fi listener visible while Share is open, improve LAN peer
  discovery and cover-art transfer, and fix opaque canvas rendering on Linux.
- Linux: import M4A files with trailing-moov/non-standard tags via decoder
  fallback, and package full hicolor raster PNG icons in the `.deb`.

## 0.7.0

- Fixed a blank-window bug: after minimize→restore or maximize the software
  renderer could present a stale buffer and show only the backdrop until the
  mouse moved. `WM_SIZE` now forces a full repaint, and the tray restore uses
  `ShowWindow(SW_RESTORE)` directly so winit cannot skip the real transition.
- yt-dlp and FFmpeg are auto-detected at startup: saved setting, the managed
  tools folder, PATH, then WinGet/scoop/chocolatey locations, each verified by
  running its version flag. Found paths are saved automatically.
- Settings can download both tools into `%LOCALAPPDATA%/Monolith/tools`:
  yt-dlp verified against its release `SHA2-256SUMS`, FFmpeg (essentials
  build from gyan.dev, ffmpeg.org's recommended Windows builds) verified
  against its published `.sha256`. Downloads show real progress, can be
  cancelled, stage then swap in atomically, and re-probe before saving.
- The installer detects the same tools and offers to install any that are
  missing during setup; a helper failure never fails the install.
- yt-dlp is invoked with `--ffmpeg-location` when a managed or chosen FFmpeg
  is known, so MP3 conversion no longer requires FFmpeg on PATH.
