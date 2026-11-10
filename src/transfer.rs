use crate::library::Result;
use std::{
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub fn validated_url(text: &str) -> Result<String> {
    let url = url::Url::parse(text.trim())?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || !matches!(
            url.host_str(),
            Some(
                "youtube.com"
                    | "www.youtube.com"
                    | "m.youtube.com"
                    | "music.youtube.com"
                    | "youtu.be"
            )
        )
    {
        return Err("Paste an HTTPS YouTube or YouTube Music link.".into());
    }
    Ok(url.to_string())
}
pub fn arguments(url: &str, directory: &Path, mp3: bool, ffmpeg: Option<&Path>) -> Vec<String> {
    let mut a = vec![
        "--ignore-config",
        "--no-plugin-dirs",
        "--no-playlist",
        "--no-simulate",
        "--newline",
        "--no-colors",
        "--no-cache-dir",
        "--no-overwrites",
        "--no-part-overwrites",
        "--socket-timeout",
        "20",
        "--retries",
        "3",
        "--fragment-retries",
        "3",
        "--progress",
        "--progress-delta",
        "1",
        "--progress-template",
        "download:PROGRESS:%(progress._percent_str)s",
        "--js-runtimes",
        "node",
        "--js-runtimes",
        "deno",
        // Keep the source thumbnail beside the audio so the artwork travels
        // with the file even before it is embedded in the player.
        "--write-thumbnail",
        "--no-write-thumbnail-to-console",
        "-P",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    a.push(directory.to_string_lossy().into_owned());
    if let Some(ffmpeg) = ffmpeg {
        // A managed or custom FFmpeg is not on PATH, so yt-dlp needs its
        // exact location for extraction and probing.
        a.push("--ffmpeg-location".to_owned());
        a.push(ffmpeg.to_string_lossy().into_owned());
    }
    a.extend(
        [
            "-o",
            "%(title).120B [%(id)s].%(ext)s",
            "-f",
            if mp3 {
                "bestaudio/best"
            } else {
                "bestaudio[ext=m4a]/bestaudio[ext=mp3]"
            },
        ]
        .map(str::to_owned),
    );
    if mp3 {
        a.extend(["-x", "--audio-format", "mp3", "--audio-quality", "0"].map(str::to_owned))
    } else {
        a.extend(["--fixup", "never"].map(str::to_owned))
    }
    a.extend(["--".into(), url.into()]);
    a
}
/// Keep helper console windows off the screen. Shared implementation lives
/// with the other tool-location code.
pub use crate::tools::quiet;
fn stop(child: &mut std::process::Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    #[cfg(windows)]
    {
        let tool = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32/taskkill.exe");
        let mut command = Command::new(tool);
        command
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        quiet(&mut command);
        let _ = command.status();
    }
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", "--", &format!("-{}", child.id())])
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}
pub fn publish(source: &Path, destination: &Path) -> Result<PathBuf> {
    let name = source.file_name().ok_or("Missing output filename")?;
    let target = destination.join(name);
    // A previous download of the same video already landed here. Say so in
    // plain language instead of surfacing a raw filesystem error.
    if target.exists() {
        return Err(format!(
            "\"{}\" is already in your downloads folder. Nothing was overwritten.",
            name.to_string_lossy()
        )
        .into());
    }
    let mut file = tempfile::NamedTempFile::new_in(destination)?;
    std::io::copy(&mut std::fs::File::open(source)?, &mut file)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(&target).map_err(|e| e.error)?;
    Ok(target)
}

/// Removes staging folders abandoned by a crash or power loss. `TempDir` only
/// cleans up on a graceful drop, so anything older than `max_age` is litter.
pub fn sweep_stale_staging(root: &Path, max_age: std::time::Duration) -> usize {
    let cutoff = std::time::SystemTime::now().checked_sub(max_age);
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(".monolith-transfer-") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let is_dir = if meta.is_dir() { true } else { false };
        if !is_dir {
            continue;
        }
        let stale = meta
            .modified()
            .ok()
            .zip(cutoff)
            .is_some_and(|(modified, cutoff)| modified < cutoff);
        if stale && std::fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}
pub fn download(
    executable: &Path,
    url: &str,
    destination: &Path,
    mp3: bool,
    ffmpeg: Option<&Path>,
    cancel: Arc<AtomicBool>,
    mut progress: impl FnMut(f32, String),
) -> Result<PathBuf> {
    let url = validated_url(url)?;
    if !executable.is_file() {
        return Err(
            "yt-dlp was not found. Install it or choose the executable in Settings.".into(),
        );
    }
    if !destination.is_dir() {
        return Err("Choose an existing download folder in Settings.".into());
    }
    let destination = destination.canonicalize()?;
    let staging = tempfile::Builder::new()
        .prefix(".monolith-transfer-")
        .tempdir_in(&destination)?;
    let mut command = Command::new(executable);
    command
        .args(arguments(&url, staging.path(), mp3, ffmpeg))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    quiet(&mut command);
    let mut child = command.spawn()?;
    let (tx, rx) = mpsc::sync_channel::<String>(32);
    let readers: Vec<_> = [
        Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
        Box::new(child.stderr.take().unwrap()),
    ]
    .into_iter()
    .map(|pipe| {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(pipe);
            let mut bytes = Vec::new();
            loop {
                bytes.clear();
                // Bound each helper log entry; don't buffer an unbounded line.
                match (&mut reader).take(8192).read_until(b'\n', &mut bytes) {
                    Ok(0) | Err(_) => break,
                    _ => {}
                }
                if tx
                    .send(String::from_utf8_lossy(&bytes).trim().to_owned())
                    .is_err()
                {
                    break;
                }
            }
        })
    })
    .collect();
    drop(tx);
    let start = Instant::now();
    let mut tail = String::new();
    let mut result = None;
    let mut failure = None;
    loop {
        if cancel.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(7200) {
            stop(&mut child);
            failure = Some(
                if cancel.load(Ordering::Relaxed) {
                    "Download cancelled."
                } else {
                    "Download timed out after two hours."
                }
                .to_owned(),
            );
            break;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                if let Some(p) = line
                    .strip_prefix("PROGRESS:")
                    .and_then(|s| s.trim().trim_end_matches('%').trim().parse::<f32>().ok())
                    .filter(|p| p.is_finite())
                {
                    progress(p.clamp(0., 100.), "Downloading audio…".into());
                } else if !line.is_empty() {
                    tail.push_str(&line);
                    tail.push('\n');
                    if tail.len() > 8192 {
                        let mut cut = tail.len() - 4096;
                        while !tail.is_char_boundary(cut) {
                            cut += 1
                        }
                        tail.drain(..cut);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(exit) = child.try_wait()? {
                    result = Some(exit);
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Err(error) = child.try_wait() {
            stop(&mut child);
            failure = Some(error.to_string());
            break;
        }
    }
    drop(rx);
    for reader in readers {
        let _ = reader.join();
    }
    if let Some(error) = failure {
        return Err(error.into());
    }
    if !result.is_some_and(|s| s.success()) {
        return Err(format!("Download failed. {}", tail.trim()).into());
    }
    let mut outputs = Vec::new();
    let mut artwork = Vec::new();
    for entry in std::fs::read_dir(staging.path())? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        match entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("m4a" | "mp3") => outputs.push(entry.path()),
            Some("jpg" | "jpeg" | "png" | "webp") => artwork.push(entry.path()),
            _ => {}
        }
    }
    if outputs.len() != 1 {
        return Err("Downloader did not produce one finished audio file.".into());
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("Download cancelled.".into());
    }
    progress(100., "Saving to your library…".into());
    let published = publish(&outputs[0], &destination)?;
    // The thumbnail is a companion, not the deliverable: a collision there
    // must never fail a download whose audio already saved.
    for image in artwork {
        if image
            .file_stem()
            .is_some_and(|stem| stem == published.file_stem().unwrap_or_default())
        {
            let _ = publish(&image, &destination);
            break;
        }
    }
    Ok(published)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sweep_removes_only_stale_staging_folders() -> Result<()> {
        use std::time::Duration;
        let dir = tempfile::tempdir()?;
        let one = dir.path().join(".monolith-transfer-one");
        let two = dir.path().join(".monolith-transfer-two");
        let keep = dir.path().join("Music");
        for path in [&one, &two, &keep] {
            std::fs::create_dir(path)?;
        }
        // A day-old cutoff leaves brand new folders alone.
        assert_eq!(
            sweep_stale_staging(dir.path(), Duration::from_secs(24 * 60 * 60)),
            0
        );
        assert!(one.exists() && two.exists());
        // A zero window treats them as abandoned.
        assert_eq!(sweep_stale_staging(dir.path(), Duration::ZERO), 2);
        assert!(!one.exists() && !two.exists());
        assert!(keep.exists(), "unrelated folders must survive");
        Ok(())
    }
    #[test]
    fn validates_links_and_publishes_without_overwriting() -> Result<()> {
        for bad in [
            "file:///etc/passwd",
            "http://youtube.com/watch?v=x",
            "https://youtube.com.evil.test/x",
            "https://user:pass@youtube.com/x",
            "--exec anything",
        ] {
            assert!(validated_url(bad).is_err(), "{bad}")
        }
        let link = validated_url("https://youtu.be/abc")?;
        let root = tempfile::tempdir()?;
        let args = arguments(&link, root.path(), false, None);
        assert!(!args.contains(&"--ffmpeg-location".into()));
        let args = arguments(
            &link,
            root.path(),
            true,
            Some(Path::new("C:/tools/ffmpeg.exe")),
        );
        let slot = args.iter().position(|a| a == "--ffmpeg-location").unwrap();
        assert_eq!(args[slot + 1], "C:/tools/ffmpeg.exe");
        assert_eq!(args[args.len() - 2], "--");
        assert_eq!(args.last().unwrap(), &link);
        assert!(args.contains(&"--ignore-config".into()));
        assert!(args.contains(&"--write-thumbnail".into()));
        let source = root.path().join("source.m4a");
        std::fs::write(&source, b"audio")?;
        let destination = root.path().join("destination");
        std::fs::create_dir(&destination)?;
        let target = publish(&source, &destination)?;
        assert_eq!(std::fs::read(&target)?, b"audio");
        std::fs::write(&source, b"replacement")?;
        assert!(publish(&source, &destination).is_err());
        assert_eq!(std::fs::read(target)?, b"audio");
        Ok(())
    }
}
