//! Monolith Music v1: one explicitly accepted song per encrypted connection.
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

#[derive(Debug, Clone)]
pub struct Offer {
    pub name: String,
    pub title: String,
    pub size: u64,
    pub sha256: String,
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
impl Offer {
    pub fn from_file(path: &Path, title: &str) -> Result<Self> {
        let size = path.metadata()?.len();
        if size == 0 || size > MAX_SONG_BYTES {
            return Err("Choose a song under 100 MB.".into());
        }
        let offer = Self {
            name: path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or("Invalid song filename.")?
                .into(),
            title: title
                .chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect(),
            size,
            sha256: updates::sha256(path)?,
        };
        offer.validate()?;
        Ok(offer)
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
        json!({"type":"offer", "name":self.name, "title":self.title, "size":self.size, "sha256":self.sha256})
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
    mut progress: impl FnMut(u64, u64),
) -> Result<()> {
    offer.validate()?;
    check_cancel(cancel)?;
    control_write(stream, &hello())?;
    check_hello(control_read(stream)?)?;
    control_write(stream, &offer.value())?;
    if control_read(stream)?["type"] != "accept" {
        return Err("The recipient declined or could not accept this song.".into());
    }
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
    stream.flush()?;
    let receipt = control_read(stream)?;
    if receipt["type"] != "saved" || receipt["sha256"] != offer.sha256 {
        return Err("No verified library receipt was received.".into());
    }
    Ok(())
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
    control_write(stream, &json!({"type":"accept"}))?;
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
    check_cancel(cancel)?;
    if updates::sha256(staged.path())? != offer.sha256 {
        return Err("Song integrity check failed. Incomplete file discarded.".into());
    }
    // Probe content, not the extension, before publishing anything to the library.
    lofty::probe::Probe::new(std::io::BufReader::new(File::open(staged.path())?))
        .guess_file_type()?
        .options(lofty::config::ParseOptions::new().read_cover_art(false))
        .read()?;
    check_cancel(cancel)?;
    let target = destination.join(&offer.name);
    staged.persist_noclobber(&target).map_err(|e| e.error)?;
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
}
