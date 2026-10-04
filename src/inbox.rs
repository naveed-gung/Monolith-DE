//! Incremental imports from Monolith's music/save folders, on the library worker.
use crate::library::{self, Library, Result};
use std::{collections::HashMap, path::PathBuf, time::UNIX_EPOCH};

#[derive(Default)]
pub struct Inbox {
    previous: HashMap<PathBuf, String>,
    handled: HashMap<PathBuf, String>,
}
impl Inbox {
    pub fn new(db: &Library) -> Result<Self> {
        // Remember completed observations across restarts: removing a library
        // entry must not resurrect an unchanged file on the next scan.
        db.0.execute_batch(
            "CREATE TABLE IF NOT EXISTS inbox_files(path TEXT PRIMARY KEY, stamp TEXT NOT NULL)",
        )?;
        let mut query = db.0.prepare("SELECT path,stamp FROM inbox_files")?;
        let rows = query.query_map([], |r| {
            Ok((PathBuf::from(r.get::<_, String>(0)?), r.get(1)?))
        })?;
        Ok(Self {
            previous: HashMap::new(),
            handled: rows.collect::<rusqlite::Result<_>>()?,
        })
    }

    /// Called every three seconds. Two equal observations let file copies settle.
    /// Uses the existing bounded, symlink-skipping walk; never removes audio.
    pub fn scan(&mut self, db: &Library, roots: &[PathBuf]) -> Result<usize> {
        let mut observed = HashMap::new();
        let mut added = 0;
        for root in roots {
            if !root.is_dir() {
                continue;
            }
            for path in library::folder_files(root)? {
                // Ignore yt-dlp's private staging files until publication.
                if path.components().any(|part| {
                    part.as_os_str()
                        .to_string_lossy()
                        .starts_with(".monolith-transfer-")
                }) {
                    continue;
                }
                let Ok(path) = path.canonicalize() else {
                    continue;
                };
                let Ok(meta) = path.metadata() else { continue };
                let stamp = format!(
                    "{}:{}",
                    meta.len(),
                    meta.modified()?.duration_since(UNIX_EPOCH)?.as_nanos()
                );
                if meta.len() == 0 || self.handled.get(&path) == Some(&stamp) {
                    continue;
                }
                if self.previous.get(&path) == Some(&stamp) {
                    match db.import(&path) {
                        Ok(imported) => {
                            db.0.execute("INSERT INTO inbox_files(path,stamp) VALUES(?1,?2) ON CONFLICT(path) DO UPDATE SET stamp=excluded.stamp",
                                rusqlite::params![path.to_string_lossy().as_ref(), stamp])?;
                            added += usize::from(imported);
                        }
                        Err(error) => {
                            eprintln!("Automatic import skipped {}: {error}", path.display())
                        }
                    }
                    // Failed/partial files retry when their size or timestamp changes.
                    self.handled.insert(path, stamp);
                } else {
                    observed.insert(path, stamp);
                }
            }
        }
        self.previous = observed;
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wave(path: &std::path::Path) {
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend(38u32.to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(8000u32.to_le_bytes());
        bytes.extend(16000u32.to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(2u32.to_le_bytes());
        bytes.extend(0i16.to_le_bytes());
        std::fs::write(path, bytes).unwrap();
    }
    #[test]
    fn detects_startup_live_copies_retries_and_respects_removal() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let music = temp.path().join("Music");
        std::fs::create_dir(&music)?;
        let db = Library::open(&temp.path().join("library.db"))?;
        let first = music.join("first.wav");
        wave(&first);
        let roots = [music.clone()];
        let mut inbox = Inbox::new(&db)?;
        assert_eq!(inbox.scan(&db, &roots)?, 0); // not yet settled
        assert_eq!(inbox.scan(&db, &roots)?, 1); // already there at startup
        assert_eq!(inbox.scan(&db, &roots)?, 0);
        let nested = music.join("Album");
        std::fs::create_dir(&nested)?;
        let next = nested.join("copy.wav");
        std::fs::write(&next, b"partial")?;
        inbox.scan(&db, &roots)?;
        assert_eq!(inbox.scan(&db, &roots)?, 0); // invalid audio isn't an entry
        wave(&next);
        assert_eq!(inbox.scan(&db, &roots)?, 0); // changed file settles again
        assert_eq!(inbox.scan(&db, &roots)?, 1);
        db.0.execute("DELETE FROM tracks", [])?;
        let mut reopened = Inbox::new(&db)?;
        reopened.scan(&db, &roots)?;
        assert_eq!(reopened.scan(&db, &roots)?, 0); // user's removals survive restart
        assert!(first.exists() && next.exists());
        let staging = music.join(".monolith-transfer-test");
        std::fs::create_dir(&staging)?;
        wave(&staging.join("unfinished.wav"));
        reopened.scan(&db, &roots)?;
        assert_eq!(reopened.scan(&db, &roots)?, 0);
        let other = temp.path().join("Other");
        std::fs::create_dir(&other)?;
        wave(&other.join("new.wav"));
        reopened.scan(&db, std::slice::from_ref(&other))?;
        assert_eq!(reopened.scan(&db, &[other])?, 1);
        Ok(())
    }
}
