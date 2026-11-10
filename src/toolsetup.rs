//! Managed installs of the helpers Downloads uses: fetch, verify, install.
//!
//! yt-dlp comes from the project's own GitHub release page and is checked
//! against its published `SHA2-256SUMS`. FFmpeg comes from gyan.dev, one of
//! the Windows builds the FFmpeg project links itself, checked against the
//! digest the site publishes beside the archive. Everything lands in
//! `tools::managed_dir()`, staged first and swapped in atomically.

use crate::{
    library::Result,
    tools::{self, Tool},
    updates,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};

/// Bound on helper downloads: larger than the app-update cap because an
/// FFmpeg archive legitimately is, but still finite.
const TOOL_LIMIT: u64 = 250_000_000;
const METADATA_LIMIT: u64 = 1_048_576;

const YTDLP_EXE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
const YTDLP_SUMS: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS";
const FFMPEG_ZIP: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";
const FFMPEG_SUM: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip.sha256";

/// `<hex>  <name>` lines (yt-dlp) or a bare digest (gyan.dev).
fn checksum_for(body: &[u8], name: &str) -> Result<String> {
    let text = std::str::from_utf8(body).map_err(|_| "Checksum file is not text.")?;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(file)) = (parts.next(), parts.next()) else {
            continue;
        };
        if file.trim_start_matches('*') == name
            && hash.len() == 64
            && hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Ok(hash.to_ascii_lowercase());
        }
    }
    let bare = text.trim();
    if bare.len() == 64 && bare.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(bare.to_ascii_lowercase());
    }
    Err(format!("No SHA-256 checksum for {name}.").into())
}

/// Fetch metadata and the payload, verify the digest, return the staged file.
/// `TempPath` deletes itself on a failure return, so a rejected download
/// never lingers beside the managed tools.
fn verified_download(
    url: &str,
    sums_url: &str,
    name: &str,
    staging: &Path,
    cancelled: &AtomicBool,
    progress: &mut dyn FnMut(f32, String),
) -> Result<tempfile::TempPath> {
    progress(0., "Fetching checksums…".into());
    let sums = tempfile::NamedTempFile::new()?;
    updates::curl(
        sums_url,
        sums.path(),
        METADATA_LIMIT,
        cancelled,
        &mut |_| {},
        "",
    )?;
    let expected = checksum_for(&fs::read(sums.path())?, name)?;
    let total = updates::head_length(url, cancelled)?.unwrap_or(0);
    if cancelled.load(Ordering::Relaxed) {
        return Err("Download cancelled.".into());
    }
    let staged = tempfile::Builder::new()
        .prefix(".monolith-tool-")
        .tempfile_in(staging)?;
    let label = name.to_owned();
    updates::curl(
        url,
        staged.path(),
        TOOL_LIMIT,
        cancelled,
        &mut |bytes| {
            if total > 0 {
                progress(
                    (bytes as f32 / total as f32 * 88.).clamp(0., 88.),
                    format!("Downloading {label}…"),
                );
            } else {
                progress(
                    1.,
                    format!("Downloading {label}… {:.1} MB", bytes as f64 / 1_000_000.),
                );
            }
        },
        "",
    )?;
    if cancelled.load(Ordering::Relaxed) {
        return Err("Download cancelled.".into());
    }
    progress(90., format!("Verifying {name}…"));
    let actual = updates::sha256(staged.path())?;
    if actual != expected {
        return Err(format!("{name} failed checksum verification; discarded.").into());
    }
    Ok(staged.into_temp_path())
}

/// Move `staged` onto `target`, keeping the old file until the new one is in.
fn swap_in(staged: &Path, target: &Path) -> Result<()> {
    let backup = target.with_extension("old");
    let _ = fs::remove_file(&backup);
    let had_old = target.is_file();
    if had_old && let Err(error) = fs::rename(target, &backup) {
        return Err(format!("Could not set the current tool aside: {error}").into());
    }
    if let Err(error) = fs::rename(staged, target) {
        if had_old {
            let _ = fs::rename(&backup, target);
        }
        return Err(format!("Could not install the tool: {error}").into());
    }
    let _ = fs::remove_file(&backup);
    Ok(())
}

/// System bsdtar — not `tar` on PATH, which can be a GNU build that cannot
/// read zip archives.
fn bsdtar() -> Result<Command> {
    if cfg!(windows) {
        let tool = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join(r"System32\tar.exe");
        Ok(Command::new(tool))
    } else {
        Ok(Command::new("tar"))
    }
}

fn install_ytdlp(
    dir: &Path,
    cancelled: &AtomicBool,
    progress: &mut dyn FnMut(f32, String),
) -> Result<()> {
    let staged = verified_download(
        YTDLP_EXE,
        YTDLP_SUMS,
        "yt-dlp.exe",
        dir,
        cancelled,
        progress,
    )?;
    let staged: &Path = staged.as_ref();
    if cancelled.load(Ordering::Relaxed) {
        return Err("Download cancelled.".into());
    }
    progress(95., "Installing yt-dlp…".into());
    let target = dir.join(Tool::YtDlp.file());
    swap_in(staged, &target)?;
    let _ = fs::remove_file(staged);
    progress(100., "yt-dlp installed.".into());
    Ok(())
}

fn install_ffmpeg(
    dir: &Path,
    cancelled: &AtomicBool,
    progress: &mut dyn FnMut(f32, String),
) -> Result<()> {
    let staged = verified_download(
        FFMPEG_ZIP,
        FFMPEG_SUM,
        "ffmpeg-release-essentials.zip",
        dir,
        cancelled,
        progress,
    )?;
    let staged: &Path = staged.as_ref();
    if cancelled.load(Ordering::Relaxed) {
        return Err("Download cancelled.".into());
    }
    progress(92., "Extracting FFmpeg…".into());
    let unpacked = tempfile::Builder::new()
        .prefix(".monolith-ffmpeg-")
        .tempdir_in(dir)?;
    let mut tar = bsdtar()?;
    tar.arg("-xf").arg(staged).arg("-C").arg(unpacked.path());
    tools::quiet(&mut tar);
    let status = tar
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    let _ = fs::remove_file(staged);
    if !status.success() {
        return Err("Could not unpack the FFmpeg archive.".into());
    }
    // essentials layout: ffmpeg-<ver>-essentials_build/bin/{ffmpeg,ffprobe}.exe
    let names: [&str; 2] = [
        Tool::Ffmpeg.file(),
        if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        },
    ];
    for name in names {
        let mut found = None;
        for entry in fs::read_dir(unpacked.path())? {
            let candidate = entry?.path().join("bin").join(name);
            if candidate.is_file() {
                found = Some(candidate);
                break;
            }
        }
        let source = found.ok_or_else(|| format!("{name} was not in the FFmpeg archive."))?;
        // fs::copy then rename: the source tree vanishes with the staging dir.
        let temp = dir.join(format!(".{name}.new"));
        fs::copy(&source, &temp)?;
        swap_in(&temp, &dir.join(name))?;
        let _ = fs::remove_file(&temp);
    }
    progress(100., "FFmpeg installed.".into());
    Ok(())
}

/// Download and install `tool` into the managed folder, then re-probe it so
/// the caller can show the version it actually got.
pub fn install(
    tool: Tool,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(f32, String),
) -> Result<tools::Found> {
    let dir = tools::managed_dir().ok_or("No folder for managed tools.")?;
    fs::create_dir_all(&dir)?;
    match tool {
        Tool::YtDlp => install_ytdlp(&dir, cancelled, &mut progress)?,
        Tool::Ffmpeg => install_ffmpeg(&dir, cancelled, &mut progress)?,
    }
    tools::detect(tool, &dir.join(tool.file()).to_string_lossy()).ok_or_else(|| {
        format!(
            "{} installed but did not answer its version check.",
            tool.label()
        )
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checksum_lines_and_bare_digests_parse() {
        let sums = b"abc123  other.exe\n\
            f5389d3698ecde4ef06f4e6b1ce0294fe9ac1396228c8990b8d3416d6d177a7d  yt-dlp.exe\n\
            def456  yt-dlp\n";
        assert_eq!(
            checksum_for(sums, "yt-dlp.exe").unwrap(),
            "f5389d3698ecde4ef06f4e6b1ce0294fe9ac1396228c8990b8d3416d6d177a7d"
        );
        let bare = b"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824\n";
        assert_eq!(
            checksum_for(bare, "ffmpeg-release-essentials.zip").unwrap(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(checksum_for(b"garbage", "yt-dlp.exe").is_err());
        assert!(checksum_for(b"1234  yt-dlp.exe", "yt-dlp.exe").is_err());
    }
    #[test]
    fn swap_in_replaces_without_losing_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("yt-dlp.exe");
        fs::write(&target, b"old").unwrap();
        let staged = dir.path().join("staged");
        fs::write(&staged, b"new").unwrap();
        swap_in(&staged, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(!dir.path().join("yt-dlp.old").exists());
    }
}
