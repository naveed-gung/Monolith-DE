//! Monolith Music v1: one explicitly accepted song per encrypted connection.
//! Optional cover-art extension (backward compatible, see docs/nearby-music.md):
//! the offer may describe a JPEG/PNG/WebP sidecar; only a receiver that answers
//! `{"type":"accept","artwork":true}` gets those bytes, right after the audio.
use crate::{
    library::{self, Result},
    updates,
};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub const SERVICE_UUID: &str = "e8793c34-7c2e-49ad-88c2-16c9a0762c91";
pub const MAX_SONG_BYTES: u64 = 100_000_000;
const MAX_CONTROL: usize = 16 * 1024;
const CHUNK: usize = 64 * 1024;
/// Same cap the player uses for decoding covers.
pub const MAX_ARTWORK_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Artwork {
    pub size: u64,
    pub sha256: String,
    pub ext: String,
}
#[derive(Debug, Clone)]
pub struct Offer {
    pub name: String,
    pub title: String,
    pub size: u64,
    pub sha256: String,
    pub artwork: Option<Artwork>,
}
fn digest_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
/// Sniffs image magic bytes; the advertised extension is never trusted alone.
pub fn artwork_kind(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}
impl Artwork {
    pub fn describe(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() as u64 > MAX_ARTWORK_BYTES {
            return None;
        }
        Some(Self {
            size: bytes.len() as u64,
            sha256: digest_hex(bytes),
            ext: artwork_kind(bytes)?.into(),
        })
    }
    fn parse(value: &Value) -> Option<Self> {
        let size = value["artwork_size"]
            .as_u64()
            .filter(|s| *s > 0 && *s <= MAX_ARTWORK_BYTES)?;
        let sha256 = value["artwork_sha256"]
            .as_str()
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))?
            .to_ascii_lowercase();
        let ext = value["artwork_ext"]
            .as_str()
            .map(str::to_ascii_lowercase)
            .filter(|e| ["jpg", "png", "webp"].contains(&e.as_str()))?;
        Some(Self { size, sha256, ext })
    }
}
/// Cover to share for a library song: a sidecar image beside the file first,
/// then the first usable embedded picture. Unsupported or oversized images are skipped.
pub fn artwork_for(path: &Path) -> Option<Vec<u8>> {
    use lofty::file::TaggedFileExt;
    if let Some(sidecar) = library::sidecar_image(path) {
        if sidecar
            .metadata()
            .is_ok_and(|m| m.len() <= MAX_ARTWORK_BYTES)
        {
            if let Ok(bytes) = std::fs::read(&sidecar) {
                if Artwork::describe(&bytes).is_some() {
                    return Some(bytes);
                }
            }
        }
    }
    let file = lofty::read_from_path(path).ok()?;
    file.tags()
        .iter()
        .flat_map(|tag| tag.pictures())
        .map(|picture| picture.data())
        .find(|data| Artwork::describe(data).is_some())
        .map(<[u8]>::to_vec)
}
fn clean_text(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}
fn valid_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    clean_text(name, 180)
        && !name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        && !name.starts_with('.')
        && !name.ends_with(['.', ' '])
        && !["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        && !(stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
        && library::supported(Path::new(name))
}
/// Produces a wire-valid portable basename for any supported local audio file,
/// preserving already-valid names verbatim and sanitizing long Unicode or
/// Linux-legal punctuation (`:`, `?`, `"`, `|`, etc.) so valid tracks never fail preflight.
fn portable_offer_name(path: &Path) -> Result<String> {
    if !library::supported(path) {
        return Err("Invalid song filename.".into());
    }
    let raw = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid song filename.")?;
    if valid_name(raw) {
        return Ok(raw.to_owned());
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .ok_or("Invalid song filename.")?
        .to_ascii_lowercase();
    let os_stem = path.file_stem().and_then(|s| s.to_str());
    let stem_raw = os_stem.unwrap_or("track");
    let cleaned: String = stem_raw
        .chars()
        .map(|c| {
            if c.is_control() || ['/', '\\', ':', '*', '?', '"', '<', '>', '|'].contains(&c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut stem = cleaned.trim_matches(['.', ' ']).to_owned();
    if stem.is_empty() {
        stem = "track".to_owned();
    }
    let upper = stem.split('.').next().unwrap_or("").to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL"].contains(&upper.as_str())
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit())
    {
        stem.insert_str(0, "track_");
    }
    let max_stem = 180usize.saturating_sub(ext.len() + 1);
    if stem.len() > max_stem {
        let mut end = max_stem;
        while !stem.is_char_boundary(end) {
            end -= 1;
        }
        stem.truncate(end);
        stem = stem.trim_end_matches(['.', ' ']).to_owned();
        if stem.is_empty() {
            stem = "track".to_owned();
        }
    }
    let candidate = format!("{stem}.{ext}");
    if !valid_name(&candidate) {
        return Err("Invalid song filename.".into());
    }
    Ok(candidate)
}
impl Offer {
    pub fn from_file(path: &Path, title: &str) -> Result<Self> {
        let size = path.metadata()?.len();
        if size == 0 || size > MAX_SONG_BYTES {
            return Err("Choose a song under 100 MB.".into());
        }
        let name = portable_offer_name(path)?;
        let clean_title: String = title
            .chars()
            .filter(|c| !c.is_control())
            .take(120)
            .collect();
        let clean_title = if clean_title.trim().is_empty() {
            Path::new(&name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Track")
                .to_owned()
        } else {
            clean_title
        };
        let offer = Self {
            name,
            title: clean_title,
            size,
            sha256: updates::sha256(path)?,
            artwork: None,
        };
        offer.validate()?;
        Ok(offer)
    }
    /// Describes `bytes` as this offer's cover; unusable images are dropped.
    pub fn with_artwork(mut self, bytes: Option<&[u8]>) -> Self {
        self.artwork = bytes.and_then(Artwork::describe);
        self
    }
    fn validate(&self) -> Result<()> {
        if !valid_name(&self.name)
            || !clean_text(&self.title, 480)
            || self.size == 0
            || self.size > MAX_SONG_BYTES
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid song offer. Only audio files up to 100 MB are accepted.".into());
        }
        Ok(())
    }
    fn value(&self) -> Value {
        let mut value = json!({"type":"offer", "name":self.name, "title":self.title, "size":self.size, "sha256":self.sha256});
        if let Some(art) = &self.artwork {
            value["artwork_size"] = json!(art.size);
            value["artwork_sha256"] = json!(art.sha256);
            value["artwork_ext"] = json!(art.ext);
        }
        value
    }
    fn parse(value: Value) -> Result<Self> {
        if value["type"] != "offer" {
            return Err("Expected a song offer.".into());
        }
        let offer = Self {
            name: value["name"].as_str().unwrap_or("").into(),
            title: value["title"].as_str().unwrap_or("").into(),
            size: value["size"].as_u64().unwrap_or(0),
            sha256: value["sha256"].as_str().unwrap_or("").to_ascii_lowercase(),
            // Optional: a malformed description drops the cover, never the song.
            artwork: Artwork::parse(&value),
        };
        offer.validate()?;
        Ok(offer)
    }
}
fn control_write(stream: &mut impl Write, value: &Value) -> Result<()> {
    let data = serde_json::to_vec(value)?;
    if data.is_empty() || data.len() > MAX_CONTROL {
        return Err("Sharing message exceeds its limit.".into());
    }
    stream.write_all(&(data.len() as u32).to_be_bytes())?;
    stream.write_all(&data)?;
    stream.flush()?;
    Ok(())
}
fn control_read(stream: &mut impl Read) -> Result<Value> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_CONTROL {
        return Err("Invalid sharing message length.".into());
    }
    let mut data = vec![0; length];
    stream.read_exact(&mut data)?;
    Ok(serde_json::from_slice(&data)?)
}
fn hello() -> Value {
    json!({"type":"hello", "app":"monolith-music", "version":1})
}
fn check_hello(value: Value) -> Result<()> {
    if value != hello() {
        return Err("This device is not running a compatible Monolith sharing session.".into());
    }
    Ok(())
}
fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err("Sharing cancelled.".into())
    } else {
        Ok(())
    }
}
pub fn send(
    stream: &mut (impl Read + Write),
    path: &Path,
    offer: &Offer,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> Result<()> {
    send_with_artwork(stream, path, offer, None, cancel, progress)
}
/// `artwork` must be the exact bytes `offer.artwork` describes (see
/// [`Offer::with_artwork`]); they follow the audio only if the receiver asks.
pub fn send_with_artwork(
    stream: &mut (impl Read + Write),
    path: &Path,
    offer: &Offer,
    artwork: Option<&[u8]>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<()> {
    offer.validate()?;
    if offer.artwork.as_ref() != artwork.and_then(Artwork::describe).as_ref() {
        return Err("Cover art changed while preparing this song.".into());
    }
    check_cancel(cancel)?;
    control_write(stream, &hello())?;
    check_hello(control_read(stream)?)?;
    control_write(stream, &offer.value())?;
    let answer = control_read(stream)?;
    if answer["type"] != "accept" {
        return Err("The recipient declined or could not accept this song.".into());
    }
    let cover = artwork.filter(|_| answer["artwork"] == true && offer.artwork.is_some());
    let mut file = File::open(path)?;
    let mut buffer = [0; CHUNK];
    let mut sent = 0;
    while sent < offer.size {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer[..((offer.size - sent) as usize).min(CHUNK)])?;
        if count == 0 {
            return Err("The source song changed during sharing.".into());
        }
        stream.write_all(&buffer[..count])?;
        sent += count as u64;
        progress(sent, offer.size);
    }
    if let Some(cover) = cover {
        check_cancel(cancel)?;
        stream.write_all(cover)?;
    }
    stream.flush()?;
    let receipt = control_read(stream)?;
    if receipt["type"] != "saved" || receipt["sha256"] != offer.sha256 {
        return Err("No verified library receipt was received.".into());
    }
    Ok(())
}
/// Best-effort sidecar (`song.jpg` beside `song.mp3`) that the library and
/// player already read. Never replaces an existing image; errors are ignored.
fn save_cover(song: &Path, bytes: &[u8], ext: &str) {
    if library::sidecar_image(song).is_some() {
        return;
    }
    let path = song.with_extension(ext);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        if file.write_all(bytes).and_then(|_| file.sync_all()).is_err() {
            drop(file);
            let _ = std::fs::remove_file(&path);
        }
    }
}
// One bounded offer per connection; the sharing worker composes batch connections.
pub fn receive(
    stream: &mut (impl Read + Write),
    destination: &Path,
    cancel: &AtomicBool,
    mut approve: impl FnMut(&Offer) -> bool,
    mut progress: impl FnMut(u64, u64),
    mut import: impl FnMut(&Path) -> Result<()>,
) -> Result<PathBuf> {
    check_cancel(cancel)?;
    check_hello(control_read(stream)?)?;
    control_write(stream, &hello())?;
    let offer = Offer::parse(control_read(stream)?)?;
    if !approve(&offer) {
        control_write(stream, &json!({"type":"decline"}))?;
        return Err("Incoming song declined.".into());
    }
    check_cancel(cancel)?;
    std::fs::create_dir_all(destination)?;
    if destination.join(&offer.name).exists() {
        control_write(stream, &json!({"type":"decline"}))?;
        return Err("A song with this filename already exists. Nothing was overwritten.".into());
    }
    let mut staged = tempfile::Builder::new()
        .prefix(".monolith-sharing-")
        .suffix(".part")
        .tempfile_in(destination)?;
    // Always opt into advertised cover art; it is verified and saved best-effort.
    control_write(
        stream,
        &if offer.artwork.is_some() {
            json!({"type":"accept", "artwork":true})
        } else {
            json!({"type":"accept"})
        },
    )?;
    let mut buffer = [0; CHUNK];
    let mut received = 0;
    while received < offer.size {
        check_cancel(cancel)?;
        let count = stream.read(&mut buffer[..((offer.size - received) as usize).min(CHUNK)])?;
        if count == 0 {
            return Err("Connection ended before the song arrived.".into());
        }
        staged.write_all(&buffer[..count])?;
        received += count as u64;
        progress(received, offer.size);
    }
    staged.as_file().sync_all()?;
    // Drain exactly the advertised cover bytes (bounded by MAX_ARTWORK_BYTES).
    let cover = match &offer.artwork {
        Some(art) => {
            let mut bytes = vec![0; art.size as usize];
            stream.read_exact(&mut bytes)?;
            Artwork::describe(&bytes)
                .filter(|seen| seen == art)
                .map(|seen| (bytes, seen.ext))
        }
        None => None,
    };
    check_cancel(cancel)?;
    if updates::sha256(staged.path())? != offer.sha256 {
        return Err("Song integrity check failed. Incomplete file discarded.".into());
    }
    // Probe content, not the extension, before publishing anything to the library.
    // Trailing-moov or tagless M4A files from mobile can trip lofty while still
    // decoding cleanly, matching Library::import.
    let reader = std::io::BufReader::new(File::open(staged.path())?);
    let opts = lofty::config::ParseOptions::new().read_cover_art(false);
    let parsed = lofty::probe::Probe::new(reader)
        .guess_file_type()
        .ok()
        .and_then(|p| p.options(opts).read().ok());
    if parsed.is_none() {
        rodio::Decoder::try_from(File::open(staged.path())?)
            .map_err(|e| format!("Not a playable audio file: {e}"))?;
    }
    check_cancel(cancel)?;
    let target = destination.join(&offer.name);
    staged.persist_noclobber(&target).map_err(|e| e.error)?;
    if let Some((bytes, ext)) = cover {
        save_cover(&target, &bytes, &ext);
    }
    // Keep verified audio on disk if import fails; never claim it reached the library.
    import(&target)?;
    control_write(stream, &json!({"type":"saved", "sha256":offer.sha256}))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Cursor,
        net::{TcpListener, TcpStream},
    };
    #[test]
    fn rejects_untrusted_metadata_before_allocating_or_accepting() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../docs/bluetooth-wire-fixtures.json")).unwrap();
        assert_eq!(fixtures["serviceUuid"], SERVICE_UUID);
        assert_eq!(fixtures["maximumControlBytes"], MAX_CONTROL);
        assert_eq!(fixtures["maximumSongBytes"], MAX_SONG_BYTES);
        check_hello(fixtures["hello"].clone()).unwrap();
        Offer::parse(fixtures["offer"].clone()).unwrap();
        for message in ["hello", "offer", "accept", "decline", "saved"] {
            let mut stream = std::io::Cursor::new(Vec::new());
            control_write(&mut stream, &fixtures[message]).unwrap();
            stream.set_position(0);
            assert_eq!(control_read(&mut stream).unwrap(), fixtures[message]);
        }
        for name in [
            "../song.mp3",
            "C:\\song.mp3",
            "CON.mp3",
            "LPT1.wav",
            "song.exe",
            "song.mp3.",
        ] {
            assert!(!valid_name(name));
        }
        assert!(valid_name("أغنية.flac"));
        let dir = tempfile::tempdir().unwrap();
        let long_unicode = format!("{}.mp3", "𝘼𝙡𝙖_𝙜𝙤𝙯𝙡𝙪𝙢_".repeat(6));
        assert!(long_unicode.len() > 180);
        let tricky = dir.path().join(&long_unicode);
        std::fs::write(&tricky, audio()).unwrap();
        let sanitized = Offer::from_file(&tricky, "").unwrap();
        assert!(valid_name(&sanitized.name) && sanitized.name.len() <= 180);
        assert!(!sanitized.title.is_empty());
        assert!(control_read(&mut Cursor::new((MAX_CONTROL as u32 + 1).to_be_bytes())).is_err());
        assert!(check_hello(json!({"type":"hello", "app":"other", "version":1})).is_err());
        assert!(Offer::parse(json!({"type":"offer", "name":"song.mp3", "title":"Song", "size":MAX_SONG_BYTES+1, "sha256":"a".repeat(64)})).is_err());
    }
    fn audio() -> Vec<u8> {
        let mut wav = b"RIFF".to_vec();
        wav.extend(16036u32.to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(16000u32.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(16000u32.to_le_bytes());
        wav.resize(16044, 0);
        wav
    }
    fn sockets() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let b = listener.accept().unwrap().0;
        for s in [&a, &b] {
            s.set_read_timeout(Some(std::time::Duration::from_secs(8)))
                .unwrap();
        }
        (a, b)
    }
    #[test]
    fn transfers_real_audio_and_imports_before_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("song.wav");
        std::fs::write(&source, audio()).unwrap();
        let offer = Offer::from_file(&source, "Test song").unwrap();
        let dest = dir.path().join("received");
        let worker_dest = dest.clone();
        let (mut a, mut b) = sockets();
        let worker = std::thread::spawn(move || {
            let db = library::Library::open(&worker_dest.with_extension("sqlite3")).unwrap();
            receive(
                &mut b,
                &worker_dest,
                &AtomicBool::new(false),
                |_| true,
                |_, _| {},
                |p| {
                    db.import(p)?;
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(db.tracks(0).unwrap().len(), 1);
        });
        send(&mut a, &source, &offer, &AtomicBool::new(false), |_, _| {}).unwrap();
        worker.join().unwrap();
        assert_eq!(std::fs::read(dest.join("song.wav")).unwrap(), audio());
    }
    #[test]
    fn decline_cancel_partial_bad_hash_and_collision_leave_no_new_song() {
        for case in [
            "decline",
            "cancel",
            "partial",
            "hash",
            "not-audio",
            "collision",
            "import-failure",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("song.wav");
            std::fs::write(&source, audio()).unwrap();
            let mut offer = Offer::from_file(&source, "Song").unwrap();
            if case == "hash" {
                offer.sha256 = "a".repeat(64);
            }
            if case == "not-audio" {
                std::fs::write(&source, vec![42; 16044]).unwrap();
                offer.sha256 = updates::sha256(&source).unwrap();
            }
            let destination = dir.path().join("inbox");
            let worker_dest = destination.clone();
            let cancelled = case == "cancel";
            if case == "collision" {
                std::fs::create_dir(&destination).unwrap();
                std::fs::write(destination.join("song.wav"), b"original").unwrap();
            }
            let (mut a, mut b) = sockets();
            let worker = std::thread::spawn(move || {
                receive(
                    &mut b,
                    &worker_dest,
                    &AtomicBool::new(cancelled),
                    |_| case != "decline",
                    |_, _| {},
                    |_| {
                        if case == "import-failure" {
                            Err("Database unavailable".into())
                        } else {
                            Ok(())
                        }
                    },
                )
                .is_err()
            });
            if case == "partial" {
                control_write(&mut a, &hello()).unwrap();
                check_hello(control_read(&mut a).unwrap()).unwrap();
                control_write(&mut a, &offer.value()).unwrap();
                assert_eq!(control_read(&mut a).unwrap()["type"], "accept");
                a.write_all(b"short").unwrap();
                drop(a);
            } else {
                assert!(send(&mut a, &source, &offer, &AtomicBool::new(false), |_, _| {}).is_err());
            }
            assert!(worker.join().unwrap());
            if case == "collision" {
                assert_eq!(
                    std::fs::read(destination.join("song.wav")).unwrap(),
                    b"original"
                );
            } else if case == "import-failure" {
                assert_eq!(
                    std::fs::read(destination.join("song.wav")).unwrap(),
                    audio()
                );
            } else if destination.exists() {
                assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
            }
        }
    }
    fn jpeg() -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0];
        bytes.resize(2048, 7);
        bytes
    }
    #[test]
    fn artwork_description_is_optional_and_strict() {
        assert_eq!(artwork_kind(&jpeg()), Some("jpg"));
        assert_eq!(artwork_kind(b"GIF89a-not-allowed"), None);
        let art = Artwork::describe(&jpeg()).unwrap();
        let offer = Offer {
            name: "song.wav".into(),
            title: "Song".into(),
            size: 10,
            sha256: "a".repeat(64),
            artwork: Some(art.clone()),
        };
        assert_eq!(Offer::parse(offer.value()).unwrap().artwork, Some(art));
        for (key, value) in [
            ("artwork_size", json!(MAX_ARTWORK_BYTES + 1)),
            ("artwork_sha256", json!("bad")),
            ("artwork_ext", json!("gif")),
        ] {
            let mut value_with_bad_art = offer.value();
            value_with_bad_art[key] = value;
            let parsed = Offer::parse(value_with_bad_art).unwrap();
            assert!(parsed.artwork.is_none(), "{key} should drop artwork only");
        }
        assert!(
            Offer {
                artwork: None,
                ..offer
            }
            .value()
            .get("artwork_size")
            .is_none()
        );
    }
    #[test]
    fn cover_art_travels_after_audio_and_lands_as_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("song.wav");
        std::fs::write(&source, audio()).unwrap();
        let cover = jpeg();
        let offer = Offer::from_file(&source, "Song")
            .unwrap()
            .with_artwork(Some(&cover));
        // A sender refuses a description that does not match its bytes,
        // before anything touches the wire.
        let mut stale = offer.clone();
        stale.artwork.as_mut().unwrap().sha256 = "b".repeat(64);
        let mut sink = Cursor::new(Vec::new());
        let quiet = AtomicBool::new(false);
        assert!(
            send_with_artwork(&mut sink, &source, &stale, Some(&cover), &quiet, |_, _| {}).is_err()
        );
        assert!(sink.get_ref().is_empty());

        let dest = dir.path().join("inbox");
        let worker_dest = dest.clone();
        let (mut a, mut b) = sockets();
        let worker = std::thread::spawn(move || {
            receive(
                &mut b,
                &worker_dest,
                &AtomicBool::new(false),
                |offer| offer.artwork.is_some(),
                |_, _| {},
                |_| Ok(()),
            )
            .unwrap()
        });
        send_with_artwork(&mut a, &source, &offer, Some(&cover), &quiet, |_, _| {}).unwrap();
        let saved = worker.join().unwrap();
        assert_eq!(std::fs::read(&saved).unwrap(), audio());
        assert_eq!(std::fs::read(dest.join("song.jpg")).unwrap(), cover);
    }
}
