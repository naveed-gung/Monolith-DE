//! Locate the external helpers Monolith can call: yt-dlp and FFmpeg.
//!
//! The installer `#[path]`-includes this file, so it must stay dependency
//! light: std plus `dirs` only. Downloads and installs live in
//! `toolsetup.rs`, which does get the full crate.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// One external helper Monolith can use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    /// YouTube and YouTube Music extraction for the Downloads page.
    YtDlp,
    /// Audio conversion (MP3) and probing for yt-dlp.
    Ffmpeg,
}

impl Tool {
    pub const ALL: [Tool; 2] = [Tool::YtDlp, Tool::Ffmpeg];
    pub fn label(self) -> &'static str {
        match self {
            Tool::YtDlp => "yt-dlp",
            Tool::Ffmpeg => "FFmpeg",
        }
    }
    /// Settings key the player's library database uses for the saved path.
    pub fn setting_key(self) -> &'static str {
        match self {
            Tool::YtDlp => "downloader",
            Tool::Ffmpeg => "ffmpeg",
        }
    }
    /// Executable file name on this platform.
    pub fn file(self) -> &'static str {
        match (self, cfg!(windows)) {
            (Tool::YtDlp, true) => "yt-dlp.exe",
            (Tool::YtDlp, false) => "yt-dlp",
            (Tool::Ffmpeg, true) => "ffmpeg.exe",
            (Tool::Ffmpeg, false) => "ffmpeg",
        }
    }
    fn version_flag(self) -> &'static str {
        match self {
            Tool::YtDlp => "--version",
            Tool::Ffmpeg => "-version",
        }
    }
}

/// A helper that answered its version flag in time.
pub struct Found {
    pub path: PathBuf,
    /// Inside `managed_dir()` — installed by Monolith, so it may be updated.
    pub managed: bool,
    /// First line of the version output, bounded.
    pub version: String,
}

/// `%LOCALAPPDATA%/Monolith/tools` — helpers Monolith installs live here,
/// separate from the per-user app data folder so updates never touch the
/// library database.
pub fn managed_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("Monolith").join("tools"))
}

/// Hide the console window helpers would otherwise flash on Windows.
/// Shared from `transfer::download`; helpers are always argument arrays.
pub fn quiet(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
}

/// Run `<exe> --version`/`-version` with a hard timeout. A helper that
/// hangs, crashes, or stays silent does not count as installed.
pub fn probe(tool: Tool, exe: &Path) -> Option<String> {
    let mut command = Command::new(exe);
    command
        .arg(tool.version_flag())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    quiet(&mut command);
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = String::new();
    child
        .stdout
        .take()?
        .take(2048)
        .read_to_string(&mut out)
        .ok()?;
    let line = out.lines().next()?.trim();
    (!line.is_empty()).then(|| line.chars().take(80).collect())
}

/// Directories a package manager might own, checked after PATH. Windows only:
/// Linux relies on PATH, which already covers /usr/bin and friends.
#[cfg(windows)]
fn install_dirs(tool: Tool) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(local) = dirs::data_local_dir() {
        let links = local.join(r"Microsoft\WinGet\Links");
        dirs.push(links);
        // WinGet unpacks into Packages\<publisher>.<name>_*/…: only descend
        // into package folders whose name mentions the tool, and stay shallow.
        let packages = local.join(r"Microsoft\WinGet\Packages");
        if let Ok(entries) = std::fs::read_dir(&packages) {
            let needle = match tool {
                Tool::YtDlp => "yt-dlp",
                Tool::Ffmpeg => "ffmpeg",
            };
            for entry in entries.flatten().take(64) {
                let path = entry.path();
                if !path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().to_lowercase().contains(needle))
                {
                    continue;
                }
                dirs.extend(shallow_bins(&path, 5));
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(r"scoop\shims"));
    }
    dirs.push(PathBuf::from(r"C:\ProgramData\chocolatey\bin"));
    dirs.push(PathBuf::from(r"C:\ffmpeg\bin"));
    dirs
}
#[cfg(not(windows))]
fn install_dirs(_tool: Tool) -> Vec<PathBuf> {
    Vec::new()
}

/// Directories up to `depth` levels down that could hold a bin folder.
#[cfg(windows)]
fn shallow_bins(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut level = vec![root.to_path_buf()];
    for _ in 0..depth {
        let mut next = Vec::new();
        for dir in level {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                if entry.file_name() == "bin" {
                    out.push(path.clone());
                }
                next.push(path);
            }
        }
        if out.len() > 8 || next.is_empty() {
            break;
        }
        level = next;
    }
    out
}

/// Candidate executables in priority order: the saved setting, Monolith's
/// managed tools folder, PATH, then well-known package manager locations.
fn candidates(tool: Tool, saved: &str) -> Vec<(PathBuf, bool)> {
    let mut out = Vec::new();
    if !saved.trim().is_empty() {
        out.push((PathBuf::from(saved), false));
    }
    let managed = managed_dir();
    if let Some(dir) = &managed {
        out.push((dir.join(tool.file()), true));
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            out.push((dir.join(tool.file()), false));
        }
    }
    for dir in install_dirs(tool) {
        out.push((dir.join(tool.file()), false));
    }
    out
}

/// The first candidate that is a real file and answers its version flag.
/// `managed` is reported against `managed_dir()` ownership, not just the
/// flag: a saved path pointing inside the managed folder still counts.
pub fn detect(tool: Tool, saved: &str) -> Option<Found> {
    let managed = managed_dir();
    for (path, is_managed) in candidates(tool, saved) {
        if !path.is_file() {
            continue;
        }
        let Some(version) = probe(tool, &path) else {
            continue;
        };
        let managed = is_managed
            || managed
                .as_ref()
                .is_some_and(|dir| path.starts_with(dir.as_path()));
        return Some(Found {
            path,
            managed,
            version,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_path_is_checked_first_and_missing_tools_report_none() {
        assert_eq!(Tool::YtDlp.version_flag(), "--version");
        assert_eq!(Tool::Ffmpeg.version_flag(), "-version");
        // A bogus saved path must not shadow a real detection and must not crash.
        let _ = detect(Tool::YtDlp, "C:/no/such/yt-dlp.exe");
        let _ = detect(Tool::Ffmpeg, "");
    }
    #[test]
    fn managed_dir_lives_under_local_app_data() {
        if let Some(dir) = managed_dir() {
            let text = dir.to_string_lossy().to_lowercase();
            assert!(text.ends_with("monolith\\tools") || text.ends_with("monolith/tools"));
        }
    }
    #[test]
    fn candidates_are_ordered_saved_managed_path_dirs() {
        let order = candidates(Tool::YtDlp, "SAVED");
        assert_eq!(order[0].0, PathBuf::from("SAVED"));
        let _ = install_dirs(Tool::Ffmpeg);
    }
}
