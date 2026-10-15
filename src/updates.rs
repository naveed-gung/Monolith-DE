//! Explicit GitHub release checks. Native HTTPS and streaming SHA-256 keep the
//! updater out of the audio/UI threads and reuse the existing installer.
use crate::library::Result;
use serde_json::Value;
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub const REPOSITORY: &str = "naveed-gung/Monolith-DE";
pub const DOWNLOAD_LIMIT: u64 = 100_000_000;
const METADATA_LIMIT: u64 = 1_048_576;

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

fn version(value: &str) -> Option<[u64; 3]> {
    let parts = value
        .strip_prefix('v')
        .unwrap_or(value)
        .split('.')
        .collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    }
    let mut result = [0; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty()
            || !part.bytes().all(|b| b.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return None;
        }
        result[index] = part.parse().ok()?;
    }
    Some(result)
}

pub fn parse_release(
    body: &[u8],
    current: &str,
    platform: &str,
    repo: &str,
) -> Result<Option<Release>> {
    if body.len() as u64 > METADATA_LIMIT {
        return Err("Release metadata is too large.".into());
    }
    let raw: Value = serde_json::from_slice(body)?;
    if raw["draft"] != false || raw["prerelease"] != false {
        return Ok(None);
    }
    let tag = raw["tag_name"].as_str().ok_or("Release has no version.")?;
    let latest = version(tag).ok_or("Invalid release version.")?;
    let installed = version(current).ok_or("Invalid installed version.")?;
    if latest <= installed {
        return Ok(None);
    }
    let app_version = tag.strip_prefix('v').unwrap_or(tag);
    let expected = match platform {
        "windows-x86_64" => "monolith-setup.exe".to_owned(),
        "linux-amd64" => format!("monolith-desktop_{app_version}_amd64.deb"),
        _ => return Err("No update package for this platform.".into()),
    };
    let assets = raw["assets"].as_array().ok_or("Release has no packages.")?;
    let mut matching = assets
        .iter()
        .filter(|a| a["name"].as_str() == Some(expected.as_str()));
    let asset = matching
        .next()
        .ok_or("No package for this platform in the latest release.")?;
    if matching.next().is_some() {
        return Err("Release contains ambiguous packages.".into());
    }
    let address = asset["browser_download_url"]
        .as_str()
        .ok_or("Package has no download address.")?;
    let uri = url::Url::parse(address)?;
    let expected_path = format!("/{repo}/releases/download/{tag}/{expected}");
    if uri.scheme() != "https"
        || uri.host_str() != Some("github.com")
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.port_or_known_default() != Some(443)
        || uri.path() != expected_path
        || uri.query().is_some()
        || uri.fragment().is_some()
    {
        return Err("Update address does not belong to Monolith's release source.".into());
    }
    let size = asset["size"].as_u64().ok_or("Package size is missing.")?;
    if size == 0 || size > DOWNLOAD_LIMIT {
        return Err("Update exceeds the 100 MB download limit.".into());
    }
    let hash = asset["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("Release package has no valid SHA-256 digest; refusing to install it.")?;
    Ok(Some(Release {
        version: app_version.to_owned(),
        name: expected,
        url: address.to_owned(),
        size,
        sha256: hash.to_ascii_lowercase(),
    }))
}

fn curl(address: &str, destination: &Path, limit: u64, cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        return Err("Update cancelled.".into());
    }
    let stderr = tempfile::tempfile()?;
    let program = if cfg!(windows) { "curl.exe" } else { "curl" };
    let mut command = Command::new(program);
    command
        .args([
            "--disable",
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "10",
            "--max-time",
            "600",
            "--max-redirs",
            "5",
            "--max-filesize",
        ])
        .arg(limit.to_string())
        .arg("--output")
        .arg(destination)
        .arg(address)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr.try_clone()?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("HTTPS download tool unavailable: {e}"))?;
    let outcome = loop {
        if cancelled.load(Ordering::Relaxed)
            || fs::metadata(destination).is_ok_and(|m| m.len() > limit)
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Update cancelled or download limit reached.".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.into());
            }
        }
    };
    if !outcome.success() {
        let mut stderr = stderr;
        stderr.seek(SeekFrom::Start(0))?;
        let mut error = String::new();
        stderr.take(2048).read_to_string(&mut error)?;
        return Err(format!("GitHub download failed. No public release may be available; private releases need authentication. {}", error.trim()).into());
    }
    Ok(())
}

pub fn check(cancelled: &AtomicBool) -> Result<Option<Release>> {
    let address = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let metadata = tempfile::NamedTempFile::new()?;
    curl(&address, metadata.path(), METADATA_LIMIT, cancelled)?;
    let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x86_64",
        ("linux", "x86_64") => "linux-amd64",
        _ => return Err("No update package for this platform.".into()),
    };
    parse_release(
        &fs::read(metadata.path())?,
        crate::version::VERSION,
        platform,
        REPOSITORY,
    )
}

pub(crate) fn sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        context.update(&buffer[..n]);
    }
    Ok(context
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

pub fn download(release: &Release, directory: &Path, cancelled: &AtomicBool) -> Result<PathBuf> {
    if cancelled.load(Ordering::Relaxed) {
        return Err("Update cancelled.".into());
    }
    if release.size == 0 || release.size > DOWNLOAD_LIMIT {
        return Err("Update exceeds the 100 MB download limit.".into());
    }
    let path = directory.join(format!(
        "monolith-update-{}{}",
        release.version,
        if release.name.ends_with(".exe") {
            ".exe"
        } else {
            ".deb"
        }
    ));
    if path.exists() {
        if fs::metadata(&path)?.len() == release.size && sha256(&path)? == release.sha256 {
            return Ok(path);
        }
        return Err("An earlier update failed verification. Remove that update package and retry; your installed app is unchanged.".into());
    }
    fs::create_dir_all(directory)?;
    let suffix = if release.name.ends_with(".exe") {
        ".exe"
    } else {
        ".deb"
    };
    let staged = tempfile::Builder::new()
        .prefix(".monolith-update-")
        .suffix(suffix)
        .tempfile_in(directory)?;
    curl(&release.url, staged.path(), release.size, cancelled)?;
    if cancelled.load(Ordering::Relaxed) {
        return Err("Update cancelled.".into());
    }
    if staged.as_file().metadata()?.len() != release.size
        || sha256(staged.path())? != release.sha256
    {
        return Err("Update failed size or checksum verification; package discarded.".into());
    }
    let path = directory.join(format!("monolith-update-{}{}", release.version, suffix));
    if cancelled.load(Ordering::Relaxed) {
        return Err("Update cancelled.".into());
    }
    // Never overwrite an existing executable, even in the app's update folder.
    let kept = staged.persist_noclobber(&path).map_err(|e| e.error)?;
    kept.sync_all()?;
    Ok(path)
}

pub fn open_installer(path: &Path, release: &Release, cancelled: &AtomicBool) -> Result<()> {
    // Recheck immediately before launching; a previously checked path is not a
    // guarantee that its contents have remained unchanged on disk.
    if fs::metadata(path)?.len() != release.size || sha256(path)? != release.sha256 {
        return Err("Downloaded installer changed; refusing to open it.".into());
    }
    if cancelled.load(Ordering::Relaxed) {
        return Err("Update cancelled.".into());
    }
    if cfg!(windows) {
        Command::new(path).spawn()?;
    } else {
        Command::new("xdg-open").arg(path).spawn()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release() -> Value {
        serde_json::json!({"draft":false,"prerelease":false,"tag_name":"v0.4.0","assets":[{
            "name":"monolith-setup.exe", "size":42, "digest":format!("sha256:{}", "a".repeat(64)),
            "browser_download_url":"https://github.com/naveed-gung/Monolith-DE/releases/download/v0.4.0/monolith-setup.exe"
        }]})
    }
    fn parse(raw: Value) -> Result<Option<Release>> {
        parse_release(
            &serde_json::to_vec(&raw)?,
            "0.3.0",
            "windows-x86_64",
            REPOSITORY,
        )
    }
    #[test]
    fn stable_versions_only_and_no_downgrades() {
        assert!(parse(release()).unwrap().is_some());
        let mut raw = release();
        raw["draft"] = true.into();
        assert!(parse(raw).unwrap().is_none());
        let mut raw = release();
        raw["tag_name"] = "v0.2.9".into();
        assert!(parse(raw).unwrap().is_none());
        assert!(version("0.3.0-rc1").is_none());
        assert!(version("0.03.0").is_none());
        assert!(version("0.10.0") > version("0.9.0"));
    }
    #[test]
    fn reject_untrusted_or_unverified_packages() {
        for address in [
            "http://github.com/naveed-gung/Monolith-DE/releases/download/v0.4.0/monolith-setup.exe",
            "https://github.com.evil.test/monolith-setup.exe",
            "https://github.com/other/repo/releases/download/v0.4.0/monolith-setup.exe",
        ] {
            let mut raw = release();
            raw["assets"][0]["browser_download_url"] = address.into();
            assert!(parse(raw).is_err());
        }
        let mut raw = release();
        raw["assets"][0]["digest"] = Value::Null;
        assert!(parse(raw).is_err());
        let mut raw = release();
        raw["assets"][0]["size"] = (DOWNLOAD_LIMIT + 1).into();
        assert!(parse(raw).is_err());
        let mut raw = release();
        let extra = raw["assets"][0].clone();
        raw["assets"].as_array_mut().unwrap().push(extra);
        assert!(parse(raw).is_err());
    }

    #[test]
    fn streaming_hash_reuse_and_tamper_refusal() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("monolith-update-0.4.0.exe");
        fs::write(&package, b"hello").unwrap();
        let digest = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert_eq!(sha256(&package).unwrap(), digest);
        let release = Release {
            version: "0.4.0".into(),
            name: "monolith-setup.exe".into(),
            url: "unused:existing-local-fixture".into(),
            size: 5,
            sha256: digest.into(),
        };
        let cancelled = AtomicBool::new(false);
        assert_eq!(
            download(&release, root.path(), &cancelled).unwrap(),
            package
        );
        fs::write(&package, b"HELLO").unwrap();
        assert!(download(&release, root.path(), &cancelled).is_err());
        assert!(open_installer(&package, &release, &cancelled).is_err());
        assert_eq!(fs::read(&package).unwrap(), b"HELLO");
    }

    #[test]
    fn linux_package_and_cancelled_work_are_checked() {
        let mut raw = release();
        raw["assets"][0]["name"] = "monolith-desktop_0.4.0_amd64.deb".into();
        raw["assets"][0]["browser_download_url"] = "https://github.com/naveed-gung/Monolith-DE/releases/download/v0.4.0/monolith-desktop_0.4.0_amd64.deb".into();
        let body = serde_json::to_vec(&raw).unwrap();
        assert!(
            parse_release(&body, "0.3.0", "linux-amd64", REPOSITORY)
                .unwrap()
                .is_some()
        );
        assert!(parse_release(&body, "0.3.0", "windows-x86_64", REPOSITORY).is_err());
        let destination = tempfile::NamedTempFile::new().unwrap();
        assert!(
            curl(
                "https://example.invalid",
                destination.path(),
                10,
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert_eq!(destination.as_file().metadata().unwrap().len(), 0);
    }
}
