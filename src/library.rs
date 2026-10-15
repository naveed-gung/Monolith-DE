use lofty::{
    file::{AudioFile, TaggedFileExt},
    tag::Accessor,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const EXTENSIONS: &[&str] = &["mp3", "m4a", "aac", "flac", "wav", "ogg", "aiff", "aif"];
// M3U and PLS are what Windows players actually export, so those are the only
// playlist formats Monolith reads.
pub const PLAYLIST_EXTENSIONS: &[&str] = &["m3u", "m3u8", "pls"];
const MAX_PLAYLIST_ENTRIES: usize = 20_000;
const MAX_PLAYLIST_BYTES: u64 = 4 * 1024 * 1024;

/// What an import actually resolved against the local library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistImport {
    pub name: String,
    pub matched: usize,
    pub missing: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistEntry {
    pub location: String,
    pub title: Option<String>,
}
/// Parses M3U/M3U8 and PLS text into a playlist name plus its entries.
///
/// PLS is an INI file whose section header identifies it, so the format is
/// detected up front. Otherwise `[playlist]` and `NumberOfEntries=` style
/// metadata would be mistaken for track paths.
pub fn parse_playlist(text: &str) -> (Option<String>, Vec<PlaylistEntry>) {
    let mut name = None;
    let mut entries = Vec::new();
    let mut pending_title: Option<String> = None;
    let pls = text
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case("[playlist]"));
    for raw in text.lines() {
        if entries.len() >= MAX_PLAYLIST_ENTRIES {
            break;
        }
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#PLAYLIST:") {
            let value = rest.trim();
            if !value.is_empty() {
                name = Some(value.to_owned());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let value = rest.trim();
            let title = value
                .split_once(',')
                .map(|(_, title)| title.trim())
                .unwrap_or(value)
                .trim();
            pending_title = (!title.is_empty()).then(|| title.to_owned());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if pls {
            // Only File<n> keys hold tracks; everything else is PLS metadata.
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let is_track = key.len() > 4
                    && key[..4].eq_ignore_ascii_case("file")
                    && key[4..].chars().all(|c| c.is_ascii_digit());
                if is_track && !value.trim().is_empty() {
                    entries.push(PlaylistEntry {
                        location: value.trim().to_owned(),
                        title: pending_title.take(),
                    });
                }
            }
            continue;
        }
        entries.push(PlaylistEntry {
            location: line.to_owned(),
            title: pending_title.take(),
        });
    }
    (name, entries)
}
/// Playlist names can arrive from a file header such as `#PLAYLIST:`.
/// Control characters are stripped so a crafted file cannot push stray
/// newlines into the UI or the status line.
fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_owned()
}
/// Case- and separator-insensitive key so the same track matches on Windows.
fn path_key(text: &str) -> String {
    text.trim_start_matches(r"\\?\")
        .replace('/', "\\")
        .to_lowercase()
}
#[derive(Clone, Debug)]
pub struct Track {
    pub id: i64,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub seconds: f64,
    pub favorite: bool,
}
pub struct Library(pub Connection);
/// Database-only bulk edits. Audio files are never deleted or moved.
pub enum BatchAction {
    Remove,
    AddTo(i64),
    RemoveFrom(i64),
    CreatePlaylist(String),
}
impl Library {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(3))?;
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS tracks(id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL, artist TEXT NOT NULL, album TEXT NOT NULL, seconds REAL NOT NULL, favorite INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS playlists(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
            CREATE TABLE IF NOT EXISTS playlist_tracks(playlist_id INTEGER REFERENCES playlists(id) ON DELETE CASCADE,
                track_id INTEGER REFERENCES tracks(id) ON DELETE CASCADE, PRIMARY KEY(playlist_id,track_id));
            CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS transfers(id INTEGER PRIMARY KEY, url TEXT NOT NULL, path TEXT NOT NULL, finished TEXT DEFAULT CURRENT_TIMESTAMP);")?;
        Ok(Self(db))
    }
    pub fn setting(&self, key: &str, fallback: &str) -> String {
        self.0
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .unwrap_or_else(|_| fallback.into())
    }
    pub fn save(&self, key: &str, value: &str) -> Result<()> {
        self.0.execute("INSERT INTO settings VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [key,value])?;
        Ok(())
    }
    pub fn import(&self, path: &Path) -> Result<bool> {
        let path = path.canonicalize()?;
        if !path.is_file() || !supported(&path) {
            return Err(
                "Unsupported file. Choose MP3, M4A, AAC, FLAC, WAV, AIFF, or Ogg Vorbis.".into(),
            );
        }
        let text = path.to_str().ok_or("File path is not valid Unicode")?;
        if self
            .0
            .query_row("SELECT 1 FROM tracks WHERE path=?1", [text], |_| Ok(()))
            .optional()?
            .is_some()
        {
            return Ok(false);
        }
        let file = lofty::probe::Probe::open(&path)?
            .options(lofty::config::ParseOptions::new().read_cover_art(false))
            .read()?;
        let tag = file.primary_tag().or_else(|| file.first_tag());
        let title = tag
            .and_then(|t| t.title())
            .map(|v| v.into_owned())
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| {
                path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        let artist = tag
            .and_then(|t| t.artist())
            .map(|v| v.into_owned())
            .unwrap_or_else(|| "Unknown artist".into());
        let album = tag
            .and_then(|t| t.album())
            .map(|v| v.into_owned())
            .unwrap_or_else(|| "Singles".into());
        self.0.execute(
            "INSERT INTO tracks(path,title,artist,album,seconds) VALUES(?1,?2,?3,?4,?5)",
            params![
                text,
                title,
                artist,
                album,
                file.properties().duration().as_secs_f64()
            ],
        )?;
        Ok(true)
    }
    /// Reconnect an explicitly chosen missing file without changing track identity.
    pub fn relink(&self, id: i64, path: &Path) -> Result<()> {
        let old: String = self
            .0
            .query_row("SELECT path FROM tracks WHERE id=?1", [id], |r| r.get(0))?;
        if Path::new(&old).is_file() {
            return Err("This song's file is already available. Play it from the library.".into());
        }
        let path = path.canonicalize()?;
        if !path.is_file() || !supported(&path) {
            return Err("Choose a supported audio file for this song.".into());
        }
        let text = path.to_str().ok_or("File path is not valid Unicode")?;
        if self
            .0
            .query_row(
                "SELECT 1 FROM tracks WHERE path=?1 AND id!=?2",
                params![text, id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err("That file already belongs to another song in your library.".into());
        }
        let file = lofty::probe::Probe::open(&path)?
            .options(lofty::config::ParseOptions::new().read_cover_art(false))
            .read()?;
        self.0.execute(
            "UPDATE tracks SET path=?1,seconds=?2 WHERE id=?3",
            params![text, file.properties().duration().as_secs_f64(), id],
        )?;
        Ok(())
    }
    pub fn tracks(&self, playlist: i64) -> Result<Vec<Track>> {
        let mut q=self.0.prepare("SELECT id,path,title,artist,album,seconds,favorite FROM tracks
            WHERE ?1=0 OR (?1=-1 AND favorite=1) OR id IN(SELECT track_id FROM playlist_tracks WHERE playlist_id=?1)
            ORDER BY title COLLATE NOCASE,id")?;
        let rows = q.query_map([playlist], |r| {
            Ok(Track {
                id: r.get(0)?,
                path: r.get(1)?,
                title: r.get(2)?,
                artist: r.get(3)?,
                album: r.get(4)?,
                seconds: r.get(5)?,
                favorite: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
    /// Reimported, moved songs keep their original identity. Only one missing
    /// and one available entry with the same filename and tags can be joined.
    /// Ambiguous matches and different recordings remain separate.
    pub fn reconnect_imports(&self) -> Result<Vec<(i64, i64)>> {
        let mut groups = std::collections::HashMap::new();
        for track in self.tracks(0)? {
            let name = Path::new(&track.path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let name = if cfg!(windows) {
                name.to_lowercase()
            } else {
                name
            };
            groups
                .entry((
                    name,
                    track.title.clone(),
                    track.artist.clone(),
                    track.album.clone(),
                ))
                .or_insert_with(Vec::new)
                .push(track);
        }
        let mut matches = Vec::new();
        for group in groups.values() {
            if group.len() != 2 {
                continue;
            }
            let present: Vec<_> = group
                .iter()
                .filter(|t| Path::new(&t.path).is_file())
                .collect();
            if present.len() != 1 {
                continue;
            }
            let restored = present[0];
            let missing = group.iter().find(|t| t.id != restored.id).unwrap();
            let valid = lofty::probe::Probe::open(&restored.path).and_then(|p| {
                p.options(lofty::config::ParseOptions::new().read_cover_art(false))
                    .read()
            });
            if (missing.seconds - restored.seconds).abs() <= 0.5
                && valid.is_ok_and(|file| {
                    (file.properties().duration().as_secs_f64() - restored.seconds).abs() <= 0.5
                })
            {
                matches.push((missing.clone(), restored.clone()));
            }
        }
        if matches.is_empty() {
            return Ok(Vec::new());
        }
        // SQLite creates a consistent snapshot before the metadata-only repair.
        if let Some(path) = self.0.path().filter(|p| !p.is_empty() && *p != ":memory:") {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            let backup =
                Path::new(path).with_file_name(format!("library-before-reconnect-{stamp}.sqlite3"));
            self.0.execute(
                "VACUUM INTO ?1",
                [backup.to_str().ok_or("Invalid backup path")?],
            )?;
        }
        let tx = self.0.unchecked_transaction()?;
        let mut aliases = Vec::new();
        for (kept, restored) in matches {
            tx.execute("INSERT OR IGNORE INTO playlist_tracks SELECT playlist_id,?1 FROM playlist_tracks WHERE track_id=?2", params![kept.id, restored.id])?;
            tx.execute("DELETE FROM tracks WHERE id=?1", [restored.id])?;
            tx.execute(
                "UPDATE tracks SET path=?1,seconds=?2,favorite=?3 WHERE id=?4",
                params![
                    restored.path,
                    restored.seconds,
                    kept.favorite || restored.favorite,
                    kept.id
                ],
            )?;
            aliases.push((restored.id, kept.id));
        }
        for key in ["current", "queue"] {
            let saved = self.setting(key, "");
            let mut seen = std::collections::HashSet::new();
            let value = saved
                .split(',')
                .filter_map(|s| s.parse::<i64>().ok())
                .map(|id| {
                    aliases
                        .iter()
                        .find(|(from, _)| *from == id)
                        .map_or(id, |(_, to)| *to)
                })
                .filter(|id| seen.insert(*id))
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            if !saved.is_empty() {
                tx.execute(
                    "UPDATE settings SET value=?1 WHERE key=?2",
                    params![value, key],
                )?;
            }
        }
        tx.commit()?;
        Ok(aliases)
    }
    pub fn playlists(&self) -> Result<Vec<(i64, String)>> {
        Ok(self
            .0
            .prepare("SELECT id,name FROM playlists ORDER BY name COLLATE NOCASE")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?)
    }
    pub fn create_playlist(&self, name: &str) -> Result<()> {
        let name = clean_name(name);
        if name.is_empty() || name.chars().count() > 80 {
            return Err("Playlist name must be 1–80 characters.".into());
        }
        self.0
            .execute("INSERT INTO playlists(name) VALUES(?1)", [name])?;
        Ok(())
    }
    /// Validate the entire selection, then commit once. A stale row or playlist
    /// rolls back the whole operation instead of partially editing a collection.
    pub fn apply_batch(&mut self, ids: &[i64], action: BatchAction) -> Result<usize> {
        let ids: std::collections::BTreeSet<i64> = ids.iter().copied().collect();
        if ids.is_empty() || ids.len() > 20_000 {
            return Err("Select between 1 and 20,000 songs.".into());
        }
        let tx = self.0.transaction()?;
        for id in &ids {
            if *id <= 0
                || !tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM tracks WHERE id=?1)",
                    [id],
                    |r| r.get::<_, bool>(0),
                )?
            {
                return Err("A selected song no longer exists. Refresh your selection.".into());
            }
        }
        let (sql, playlist) = match action {
            BatchAction::Remove => ("DELETE FROM tracks WHERE id=?2", 0),
            BatchAction::AddTo(id) | BatchAction::RemoveFrom(id) => {
                if id <= 0
                    || !tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM playlists WHERE id=?1)",
                        [id],
                        |r| r.get::<_, bool>(0),
                    )?
                {
                    return Err("Choose an existing playlist.".into());
                }
                (
                    if matches!(action, BatchAction::AddTo(_)) {
                        "INSERT OR IGNORE INTO playlist_tracks VALUES(?1,?2)"
                    } else {
                        "DELETE FROM playlist_tracks WHERE playlist_id=?1 AND track_id=?2"
                    },
                    id,
                )
            }
            BatchAction::CreatePlaylist(name) => {
                let name = clean_name(&name);
                if name.is_empty() || name.chars().count() > 80 {
                    return Err("Playlist name must be 1–80 characters.".into());
                }
                tx.execute("INSERT INTO playlists(name) VALUES(?1)", [name])?;
                (
                    "INSERT INTO playlist_tracks VALUES(?1,?2)",
                    tx.last_insert_rowid(),
                )
            }
        };
        for id in &ids {
            tx.execute(sql, params![playlist, id])?;
        }
        tx.commit()?;
        Ok(ids.len())
    }
    /// Track ids belonging to a custom playlist, used to filter the in-memory
    /// library without hitting the database on every search keystroke.
    pub fn playlist_track_ids(&self, playlist: i64) -> Result<Vec<i64>> {
        let mut q = self
            .0
            .prepare("SELECT track_id FROM playlist_tracks WHERE playlist_id=?1")?;
        Ok(q.query_map([playlist], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?)
    }
    /// Drops rows whose audio file has disappeared. Originals are never
    /// touched, so this only forgets library entries.
    pub fn prune_missing(&self) -> Result<usize> {
        let rows = self.tracks(0)?;
        let mut missing = Vec::new();
        for track in rows {
            if !Path::new(&track.path).is_file() {
                missing.push(track.id);
            }
        }
        let mut removed = 0;
        for id in missing {
            removed += self.0.execute("DELETE FROM tracks WHERE id=?1", [id])?;
        }
        Ok(removed)
    }
    pub fn favorite(&self, id: i64) -> Result<()> {
        self.0
            .execute("UPDATE tracks SET favorite=NOT favorite WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn remove(&self, id: i64) -> Result<()> {
        self.0.execute("DELETE FROM tracks WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn add_to_playlist(&self, playlist: i64, id: i64) -> Result<()> {
        self.0.execute(
            "INSERT OR IGNORE INTO playlist_tracks VALUES(?1,?2)",
            params![playlist, id],
        )?;
        Ok(())
    }
    pub fn remove_from_playlist(&self, playlist: i64, id: i64) -> Result<()> {
        self.0.execute(
            "DELETE FROM playlist_tracks WHERE playlist_id=?1 AND track_id=?2",
            params![playlist, id],
        )?;
        Ok(())
    }
    pub fn delete_playlist(&self, id: i64) -> Result<()> {
        self.0.execute("DELETE FROM playlists WHERE id=?1", [id])?;
        Ok(())
    }
    /// Creates a playlist from an M3U or PLS file, matching each entry against
    /// tracks already in the library by path, then title, then file name.
    pub fn import_playlist(&self, path: &Path) -> Result<PlaylistImport> {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !PLAYLIST_EXTENSIONS.contains(&extension.as_str()) {
            return Err("Choose an .m3u, .m3u8, or .pls playlist file.".into());
        }
        let bytes = std::fs::read(path)?;
        if bytes.len() as u64 > MAX_PLAYLIST_BYTES {
            return Err("Playlist file is larger than 4 MB.".into());
        }
        let (declared, entries) = parse_playlist(&String::from_utf8_lossy(&bytes));
        if entries.is_empty() {
            return Err("That playlist file has no track entries.".into());
        }
        let wanted = clean_name(&declared.unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }));
        let name = unique_playlist_name(&self.playlists()?, &wanted);
        self.create_playlist(&name)?;
        let id = self
            .playlists()?
            .into_iter()
            .find(|(_, existing)| existing == &name)
            .map(|(id, _)| id)
            .ok_or("Could not create the imported playlist")?;
        let library = self.tracks(0)?;
        let by_path: std::collections::HashMap<String, i64> =
            library.iter().map(|t| (path_key(&t.path), t.id)).collect();
        // First entry wins so two tracks sharing a file name cannot silently
        // resolve to whichever row happened to be read last.
        let by_stem: std::collections::HashMap<String, i64> = library
            .iter()
            .filter_map(|t| {
                Path::new(&t.path)
                    .file_stem()
                    .map(|s| (s.to_string_lossy().to_lowercase(), t.id))
            })
            .fold(std::collections::HashMap::new(), |mut map, (key, id)| {
                map.entry(key).or_insert(id);
                map
            });
        let by_title: std::collections::HashMap<String, i64> = library
            .iter()
            .map(|t| (t.title.to_lowercase(), t.id))
            .fold(std::collections::HashMap::new(), |mut map, (key, id)| {
                map.entry(key).or_insert(id);
                map
            });
        let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut seen = std::collections::HashSet::new();
        let mut matched = 0;
        let mut missing = 0;
        for entry in entries {
            let raw = Path::new(&entry.location);
            let resolved = if raw.is_absolute() {
                raw.to_path_buf()
            } else {
                base.join(raw)
            };
            let stem = resolved
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            // Exact path is authoritative, then the declared title, and only
            // then a loose file-name match.
            let found = by_path
                .get(&path_key(&resolved.to_string_lossy()))
                .copied()
                .or_else(|| {
                    entry
                        .title
                        .as_deref()
                        .and_then(|t| by_title.get(&t.to_lowercase()).copied())
                })
                .or_else(|| by_stem.get(&stem).copied());
            match found {
                Some(track) if seen.insert(track) => {
                    self.0.execute(
                        "INSERT OR IGNORE INTO playlist_tracks VALUES(?1,?2)",
                        params![id, track],
                    )?;
                    matched += 1;
                }
                Some(_) => {}
                None => missing += 1,
            }
        }
        if matched == 0 {
            // Do not leave an empty playlist behind when the file was wrong.
            self.0.execute("DELETE FROM playlists WHERE id=?1", [id])?;
            return Err(format!(
                "No tracks in \"{name}\" are in your library yet. Import the music first, then retry."
            )
            .into());
        }
        Ok(PlaylistImport {
            name,
            matched,
            missing,
        })
    }
    pub fn history(&self) -> Result<String> {
        let mut q = self
            .0
            .prepare("SELECT path FROM transfers ORDER BY id DESC LIMIT 30")?;
        Ok(q.query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .join("\n"))
    }
}
pub fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| EXTENSIONS.contains(&s.to_ascii_lowercase().as_str()))
}
/// Artwork saved beside a track, such as "Song.jpg" next to "Song.m4a".
/// Downloads keep their thumbnail here so it travels with the music file.
pub fn sidecar_image(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?;
    let parent = path.parent()?;
    for extension in ["jpg", "jpeg", "png", "webp"] {
        let candidate = parent.join(format!("{}.{extension}", stem.to_string_lossy()));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
fn unique_playlist_name(existing: &[(i64, String)], wanted: &str) -> String {
    let base: String = wanted.trim().chars().take(80).collect();
    let base = if base.is_empty() {
        "Imported playlist".to_owned()
    } else {
        base
    };
    let taken = |name: &str| {
        existing
            .iter()
            .any(|(_, other)| other.eq_ignore_ascii_case(name))
    };
    if !taken(&base) {
        return base;
    }
    for suffix in 2..1000 {
        let candidate = format!("{base} ({suffix})");
        if !taken(&candidate) {
            return candidate;
        }
    }
    base
}
pub fn folder_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    let mut visited = 0;
    while let Some(dir) = pending.pop() {
        for item in std::fs::read_dir(dir)? {
            let item = item?;
            let kind = item.file_type()?;
            visited += 1;
            if visited > 50_000 {
                return Err(
                    "Folder is too large. Import smaller folders (under 50,000 entries).".into(),
                );
            }
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                pending.push(item.path())
            } else if kind.is_file() && supported(&item.path()) {
                files.push(item.path())
            }
        }
    }
    Ok(files)
}
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn restored_song(dir: &Path, db: &Library) -> Result<PathBuf> {
        let path = dir.join("song.wav");
        let mut wav = b"RIFF".to_vec();
        wav.extend(32036u32.to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(16000u32.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(32000u32.to_le_bytes());
        wav.resize(32044, 0);
        std::fs::write(&path, wav)?;
        db.0.execute("INSERT INTO tracks(path,title,artist,album,seconds,favorite) VALUES(?1,'song','Unknown artist','Singles',2,1)", [dir.join("old/song.wav").to_str().unwrap()])?;
        db.import(&path)?;
        Ok(path.canonicalize()?)
    }
    #[test]
    fn reimported_song_keeps_identity_collections_and_has_a_recoverable_backup() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Library::open(&dir.path().join("library.sqlite3"))?;
        let audio = restored_song(dir.path(), &db)?;
        let bytes = std::fs::read(&audio)?;
        db.create_playlist("Original collection")?;
        db.create_playlist("New collection")?;
        let lists = db.playlists()?;
        db.add_to_playlist(lists[0].0, 1)?;
        db.add_to_playlist(lists[1].0, 2)?;
        db.save("current", "2")?;
        db.save("queue", "1,2")?;
        assert_eq!(db.reconnect_imports()?, vec![(2, 1)]);
        let tracks = db.tracks(0)?;
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].id, 1);
        assert!(tracks[0].favorite);
        assert_eq!(Path::new(&tracks[0].path), audio);
        for (id, _) in lists {
            assert_eq!(db.playlist_track_ids(id)?, vec![1]);
        }
        assert_eq!(db.setting("current", ""), "1");
        assert_eq!(db.setting("queue", ""), "1");
        assert_eq!(std::fs::read(audio)?, bytes);
        let backup = std::fs::read_dir(dir.path())?
            .filter_map(|e| e.ok())
            .find(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("library-before-reconnect-")
            })
            .unwrap();
        let snapshot = Connection::open(backup.path())?;
        assert_eq!(
            snapshot.query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get::<_, i32>(0))?,
            2
        );
        assert!(db.reconnect_imports()?.is_empty());
        Ok(())
    }
    #[test]
    fn ambiguous_or_different_recordings_are_not_reconnected() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Library::open(&dir.path().join("library.sqlite3"))?;
        restored_song(dir.path(), &db)?;
        db.0.execute("UPDATE tracks SET seconds=9 WHERE id=1", [])?;
        assert!(db.reconnect_imports()?.is_empty());
        db.0.execute("UPDATE tracks SET seconds=2 WHERE id=1", [])?;
        db.0.execute("INSERT INTO tracks(path,title,artist,album,seconds) VALUES(?1,'song','Unknown artist','Singles',2)", [dir.path().join("another/song.wav").to_str().unwrap()])?;
        assert!(db.reconnect_imports()?.is_empty());
        assert_eq!(db.tracks(0)?.len(), 3);
        Ok(())
    }
    #[test]
    fn parses_m3u_and_pls_playlist_text() {
        let (name, entries) = parse_playlist(
            "#EXTM3U\n#PLAYLIST:Road Trip\n#EXTINF:214,Artist - Song One\nC:\\Music\\one.mp3\n\n# comment\nD:/Music/two.flac\n",
        );
        assert_eq!(name.as_deref(), Some("Road Trip"));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].location, r"C:\Music\one.mp3");
        assert_eq!(entries[0].title.as_deref(), Some("Artist - Song One"));
        assert_eq!(entries[1].location, "D:/Music/two.flac");
        assert_eq!(entries[1].title, None);
        let (name, entries) =
            parse_playlist("[playlist]\nNumberOfEntries=1\nFile1=C:\\Music\\pls.mp3\n");
        assert_eq!(name, None);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].location, r"C:\Music\pls.mp3");
        assert_eq!(parse_playlist("").1.len(), 0);
    }
    #[test]
    fn imports_playlist_entries_against_the_library() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Library::open(&dir.path().join("library.sqlite3"))?;
        let track = dir.path().join("Night Drive.mp3");
        std::fs::write(&track, b"audio")?;
        db.0.execute(
            "INSERT INTO tracks(path,title,artist,album,seconds) VALUES(?1,'Night Drive','Nova','LP',180)",
            [track.to_str().unwrap()],
        )?;
        let list = dir.path().join("Evening.m3u");
        std::fs::write(
            &list,
            "#EXTM3U\nNight Drive.mp3\nmissing-track.mp3\nNight Drive.mp3\n",
        )?;
        let summary = db.import_playlist(&list)?;
        assert_eq!(summary.name, "Evening");
        assert_eq!(summary.matched, 1, "duplicates collapse into one entry");
        assert_eq!(summary.missing, 1);
        let id = db.playlists()?[0].0;
        assert_eq!(db.tracks(id)?.len(), 1);
        let again = db.import_playlist(&list)?;
        assert_eq!(again.name, "Evening (2)");
        assert!(db.import_playlist(&dir.path().join("nope.txt")).is_err());
        Ok(())
    }
    #[test]
    fn playlist_names_strip_control_characters() {
        assert_eq!(clean_name("Road\nTrip\u{7}"), "RoadTrip");
        assert_eq!(clean_name("  Evening  "), "Evening");
    }
    #[test]
    fn empty_import_is_rolled_back() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Library::open(&dir.path().join("library.sqlite3"))?;
        let list = dir.path().join("Ghost.m3u");
        std::fs::write(&list, "#EXTM3U\nnowhere-at-all.mp3\n")?;
        assert!(db.import_playlist(&list).is_err());
        assert!(
            db.playlists()?.is_empty(),
            "a playlist with no matches must not be left behind"
        );
        Ok(())
    }
    #[test]
    fn relink_rejects_bad_or_duplicate_files_without_changing_the_entry() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Library::open(&dir.path().join("library.sqlite3"))?;
        let missing = dir.path().join("missing.wav");
        let other = dir.path().join("other.wav");
        std::fs::write(&other, b"audio")?;
        let other = other.canonicalize()?;
        db.0.execute("INSERT INTO tracks(path,title,artist,album,seconds,favorite) VALUES(?1,'Kept','A','LP',2,1)", [missing.to_str().unwrap()])?;
        db.0.execute(
            "INSERT INTO tracks(path,title,artist,album,seconds) VALUES(?1,'Other','A','LP',2)",
            [other.to_str().unwrap()],
        )?;
        db.create_playlist("Evening")?;
        let playlist = db.playlists()?[0].0;
        db.add_to_playlist(playlist, 1)?;
        let invalid = dir.path().join("invalid.wav");
        std::fs::write(&invalid, b"not audio")?;
        for path in [&missing, &other, &invalid] {
            assert!(db.relink(1, path).is_err());
        }
        let track = &db.tracks(playlist)?[0];
        assert_eq!(track.id, 1);
        assert_eq!(track.path, missing.to_str().unwrap());
        assert!(track.favorite);
        assert_eq!(std::fs::read(other)?, b"audio");
        Ok(())
    }
    #[test]
    fn prune_missing_forgets_rows_without_touching_files() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Library::open(&dir.path().join("library.sqlite3"))?;
        let present = dir.path().join("Here.wav");
        std::fs::write(&present, b"audio")?;
        db.0.execute(
            "INSERT INTO tracks(path,title,artist,album,seconds) VALUES(?1,'Here','A','B',1)",
            [present.to_str().unwrap()],
        )?;
        db.0.execute(
            "INSERT INTO tracks(path,title,artist,album,seconds) VALUES(?1,'Gone','A','B',1)",
            [dir.path().join("Gone.wav").to_str().unwrap()],
        )?;
        assert_eq!(db.prune_missing()?, 1);
        assert_eq!(db.tracks(0)?.len(), 1);
        assert_eq!(std::fs::read(&present)?, b"audio");
        Ok(())
    }
    #[test]
    fn finds_sidecar_artwork_next_to_a_track() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let track = dir.path().join("Track.m4a");
        std::fs::write(&track, b"audio")?;
        assert!(sidecar_image(&track).is_none());
        let art = dir.path().join("Track.jpg");
        std::fs::write(&art, b"jpeg")?;
        assert_eq!(sidecar_image(&track), Some(art));
        Ok(())
    }
    #[test]
    fn persistence_and_playlist_cascade_preserve_original() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("library.sqlite3");
        let original = dir.path().join("original.wav");
        std::fs::write(&original, b"untouched")?;
        let db = Library::open(&path)?;
        db.0.execute("INSERT INTO tracks(path,title,artist,album,seconds) VALUES(?1,'One','Artist','Album',60)",[original.to_str().unwrap()])?;
        let id = db.tracks(0)?[0].id;
        db.create_playlist("Night")?;
        let playlist = db.playlists()?[0].0;
        db.add_to_playlist(playlist, id)?;
        db.add_to_playlist(playlist, id)?;
        db.favorite(id)?;
        db.save("volume", "0.4")?;
        drop(db);
        let db = Library::open(&path)?;
        assert_eq!(db.setting("volume", "1"), "0.4");
        assert_eq!(db.tracks(playlist)?.len(), 1);
        assert_eq!(db.tracks(-1)?.len(), 1);
        assert!(db.create_playlist(" ").is_err());
        assert!(db.create_playlist("Night").is_err());
        db.remove(id)?;
        assert!(db.tracks(playlist)?.is_empty());
        assert_eq!(std::fs::read(original)?, b"untouched");
        Ok(())
    }
    #[test]
    fn batch_edits_are_atomic_and_keep_audio_files() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut db = Library::open(&dir.path().join("library.sqlite3"))?;
        let mut ids = Vec::new();
        for name in ["one.wav", "two.wav"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"original")?;
            db.0.execute("INSERT INTO tracks(path,title,artist,album,seconds) VALUES(?1,?2,'Artist','Album',60)",params![path.to_str(),name])?;
            ids.push(db.0.last_insert_rowid());
        }
        assert!(db.apply_batch(&[ids[0], 999], BatchAction::Remove).is_err());
        assert_eq!(db.tracks(0)?.len(), 2);
        assert!(
            db.apply_batch(&ids, BatchAction::CreatePlaylist(" ".into()))
                .is_err()
        );
        assert!(db.playlists()?.is_empty());
        assert_eq!(
            db.apply_batch(&ids, BatchAction::CreatePlaylist("Evening".into()))?,
            2
        );
        let playlist = db.playlists()?[0].0;
        assert_eq!(db.tracks(playlist)?.len(), 2);
        assert!(
            db.apply_batch(&ids, BatchAction::CreatePlaylist("Evening".into()))
                .is_err()
        );
        assert_eq!(db.playlists()?.len(), 1);
        assert!(db.apply_batch(&ids, BatchAction::AddTo(999)).is_err());
        db.apply_batch(&ids, BatchAction::RemoveFrom(playlist))?;
        assert!(db.tracks(playlist)?.is_empty());
        db.apply_batch(&[ids[0], ids[0], ids[1]], BatchAction::AddTo(playlist))?;
        assert_eq!(db.tracks(playlist)?.len(), 2);
        db.apply_batch(&ids, BatchAction::Remove)?;
        assert!(db.tracks(0)?.is_empty());
        assert!(db.tracks(playlist)?.is_empty());
        for name in ["one.wav", "two.wav"] {
            assert_eq!(std::fs::read(dir.path().join(name))?, b"original");
        }
        Ok(())
    }
}
