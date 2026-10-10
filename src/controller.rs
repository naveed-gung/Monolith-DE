use crate::{AppWindow, TrackRow};
use monolith_desktop::{
    library::{self, Library, Result, Track},
    transfer,
};
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink, Source};
use slint::{Model, ModelRc, VecModel};
use std::{
    hash::BuildHasher,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

pub enum Command {
    Import(Vec<PathBuf>),
    ImportPlaylist(PathBuf),
    PruneMissing,
    Folder(PathBuf),
    Filter(usize, String),
    /// Which of the three shared pages is showing, so the Songs view can stop
    /// applying the playlist filter and sort instead.
    Page(i32),
    /// 0 title, 1 artist, 2 album, 3 recently added.
    Sort(i32),
    Play(i64),
    Relink(i64, PathBuf),
    Thumbnail(i64),
    Toggle,
    Resume,
    Pause,
    Stop,
    Next(i32),
    Seek(f32),
    Volume(f32),
    Favorite(i64),
    Remove(i64),
    NewPlaylist(String),
    DeletePlaylist(i64),
    AddToPlaylist(i64, usize),
    RemoveFromPlaylist(i64, i64),
    Save(String, String),
    Shuffle(bool),
    Repeat(i32),
    Download(String, bool),
    Downloaded(String, std::result::Result<PathBuf, String>),
    Shutdown,
}
fn update(ui: &slint::Weak<AppWindow>, f: impl FnOnce(AppWindow) + Send + 'static) {
    let _ = ui.upgrade_in_event_loop(f);
}
fn status(ui: &slint::Weak<AppWindow>, s: impl Into<String>) {
    let s = s.into();
    update(ui, move |u| u.set_status(s.into()));
}
/// How long typing must pause before the library is re-filtered.
const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);
struct Audio {
    _stream: OutputStream,
    sink: Sink,
}
struct Worker {
    db: Library,
    ui: slint::Weak<AppWindow>,
    all: Vec<Track>,
    visible: Vec<i64>,
    playlists: Vec<(i64, String)>,
    /// Membership per playlist id, so filtering never has to re-query.
    members: std::collections::HashMap<i64, std::collections::HashSet<i64>>,
    filter: usize,
    query: String,
    page: i32,
    sort: i32,
    /// Search text waiting for the debounce window to close.
    pending_filter: Option<(usize, String)>,
    filter_due: Option<std::time::Instant>,
    /// Decoded artwork, keyed by track id, most-recently-used last.
    art: std::collections::HashMap<i64, (Art, Art)>,
    art_order: std::collections::VecDeque<i64>,
    thumbnails: std::collections::VecDeque<(i64, Option<Art>)>,
    queue: Vec<i64>,
    current: Option<i64>,
    position: f64,
    audio: Option<Audio>,
    volume: f32,
    shuffle: bool,
    repeat: i32,
    last_second: u64,
}

/// How many tracks keep decoded artwork in memory. Each entry holds a bar and
/// a hero buffer, so this stays a few megabytes.
const ART_CACHE_LIMIT: usize = 8;

impl Worker {
    /// Virtualized rows request 64px covers as they appear. Cache both hits and
    /// misses; evict UI buffers too, so scrolling cannot accumulate decoded art.
    fn thumbnail(&mut self, id: i64) {
        if !self.visible.contains(&id) {
            return;
        }
        let cached = self
            .thumbnails
            .iter()
            .position(|(key, _)| *key == id)
            .and_then(|index| self.thumbnails.remove(index));
        let art = cached.map(|(_, art)| art).unwrap_or_else(|| {
            let track = self.all.iter().find(|track| track.id == id)?;
            row_cover(&track.path)
        });
        self.thumbnails.push_back((id, art.clone()));
        let evicted = if self.thumbnails.len() > 128 {
            self.thumbnails.pop_front().map(|(key, _)| key)
        } else {
            None
        };
        update(&self.ui, move |ui| {
            let rows = ui.get_tracks();
            for index in 0..rows.row_count() {
                let Some(mut row) = rows.row_data(index) else {
                    continue;
                };
                if Some(row.id as i64) == evicted {
                    row.thumbnail = slint::Image::default();
                    rows.set_row_data(index, row);
                } else if row.id as i64 == id {
                    row.thumbnail = art
                        .as_ref()
                        .map(|art| {
                            slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
                                &art.bytes, art.width, art.height,
                            ))
                        })
                        .unwrap_or_default();
                    rows.set_row_data(index, row);
                }
            }
        });
    }
    /// Reloads library rows and playlist metadata from disk. Everything else
    /// filters from the in-memory copy.
    fn reload(&mut self) -> Result<()> {
        self.all = self.db.tracks(0)?;
        self.thumbnails.clear();
        self.playlists = self.db.playlists()?;
        self.members.clear();
        for (id, _) in &self.playlists {
            self.members
                .insert(*id, self.db.playlist_track_ids(*id)?.into_iter().collect());
        }
        // Artwork for tracks that no longer exist would never be evicted.
        let live: std::collections::HashSet<i64> = self.all.iter().map(|t| t.id).collect();
        self.art.retain(|id, _| live.contains(id));
        self.art_order.retain(|id| live.contains(id));
        Ok(())
    }
    /// Applies the current filter to the already-loaded library and pushes the
    /// result to the UI. No database access.
    fn apply_filter(&mut self) -> Result<()> {
        let mut ids = vec![0, -1];
        ids.extend(self.playlists.iter().map(|p| p.0));
        if self.filter >= ids.len() {
            self.filter = 0
        }
        // Songs is the flat view of the whole library: the playlist chips do
        // not apply there, so the selection is forced to "All music".
        let songs = self.page == 1;
        let selected = if songs { 0 } else { ids[self.filter] };
        let q = self.query.to_lowercase();
        let favorites: std::collections::HashSet<i64> = self
            .all
            .iter()
            .filter(|t| t.favorite)
            .map(|t| t.id)
            .collect();
        let members = self.members.get(&selected);
        let tracks = self
            .all
            .iter()
            .filter(|t| match selected {
                0 => true,
                -1 => favorites.contains(&t.id),
                _ => members.is_some_and(|set| set.contains(&t.id)),
            })
            .filter(|t| {
                q.is_empty()
                    || format!("{} {} {}", t.title, t.artist, t.album)
                        .to_lowercase()
                        .contains(&q)
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut tracks = tracks;
        if songs {
            // `Track` carries no import date, and ids autoincrement, so a
            // descending id is the honest stand-in for "most recently added".
            match self.sort {
                1 => tracks.sort_by(|a, b| {
                    a.artist
                        .to_lowercase()
                        .cmp(&b.artist.to_lowercase())
                        .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
                }),
                2 => tracks.sort_by(|a, b| {
                    a.album
                        .to_lowercase()
                        .cmp(&b.album.to_lowercase())
                        .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
                }),
                3 => tracks.sort_by(|a, b| b.id.cmp(&a.id)),
                _ => tracks.sort_by(|a, b| {
                    a.title
                        .to_lowercase()
                        .cmp(&b.title.to_lowercase())
                        .then_with(|| a.artist.to_lowercase().cmp(&b.artist.to_lowercase()))
                }),
            }
        }
        self.visible = tracks.iter().map(|t| t.id).collect();
        let mut names = vec!["All music".to_owned(), "Favorites".to_owned()];
        names.extend(self.playlists.iter().map(|p| p.1.clone()));
        // The header always reports the whole library; the Songs page shows a
        // count of its own because it is never filtered down to a playlist.
        let count = if songs {
            tracks.len() as i32
        } else {
            self.all.len() as i32
        };
        let sort = self.sort;
        let favorites = self.all.iter().filter(|t| t.favorite).count() as i32;
        let albums = self
            .all
            .iter()
            .map(|t| (&t.artist, &t.album))
            .collect::<std::collections::HashSet<_>>()
            .len() as i32;
        let playlist_ids = ids.iter().map(|id| *id as i32).collect::<Vec<_>>();
        let filter = self.filter as i32;
        let history = self.db.history()?;
        update(&self.ui, move |ui| {
            ui.set_tracks(ModelRc::new(VecModel::from(
                tracks
                    .into_iter()
                    .map(|t| TrackRow {
                        id: t.id as i32,
                        title: t.title.into(),
                        detail: format!("{} · {}", t.artist, t.album).into(),
                        duration: library::clock(t.seconds).into(),
                        favorite: t.favorite,
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            )));
            ui.set_playlists(ModelRc::new(VecModel::from(
                names
                    .into_iter()
                    .map(Into::into)
                    .collect::<Vec<slint::SharedString>>(),
            )));
            ui.set_playlist_ids(ModelRc::new(VecModel::from(playlist_ids)));
            ui.set_playlist_index(filter);
            ui.set_songs_sort(sort);
            ui.set_total_tracks(count);
            ui.set_album_count(albums);
            ui.set_favorite_count(favorites);
            ui.set_download_history(history.into());
        });
        Ok(())
    }
    /// Full reload followed by a filter, for changes that alter library rows.
    fn refresh(&mut self) -> Result<()> {
        self.reload()?;
        self.apply_filter()
    }
    fn persist(&self) -> Result<()> {
        self.db
            .save("current", &self.current.unwrap_or(-1).to_string())?;
        self.db.save(
            "position",
            &self
                .audio
                .as_ref()
                .map_or(self.position, |a| a.sink.get_pos().as_secs_f64())
                .to_string(),
        )?;
        self.db.save(
            "queue",
            &self
                .queue
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        )?;
        Ok(())
    }
    fn show_player(&self) {
        let track = self
            .current
            .and_then(|id| self.all.iter().find(|t| t.id == id))
            .cloned();
        let playing = self.audio.is_some();
        let position = self.position;
        update(&self.ui, move |ui| {
            ui.set_playing(playing);
            ui.set_position(position as f32);
            ui.set_elapsed(library::clock(position).into());
            if let Some(t) = track {
                ui.set_current_id(t.id as i32);
                ui.set_current_title(t.title.into());
                ui.set_current_artist(t.artist.into());
                ui.set_duration(t.seconds as f32);
                ui.set_total_time(library::clock(t.seconds).into());
            } else {
                ui.set_current_id(-1);
                ui.set_current_title("Nothing playing".into());
                ui.set_current_artist("Choose a track to begin".into());
                ui.set_duration(0.);
                ui.set_total_time("0:00".into());
            }
        });
    }
    fn start(&mut self, id: i64, position: f64) -> Result<()> {
        let t = self
            .all
            .iter()
            .find(|t| t.id == id)
            .ok_or("Track is no longer in the library")?;
        let file = std::fs::File::open(&t.path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                let title = t.title.clone();
                let path = t.path.clone();
                update(&self.ui, move |u| {
                    u.set_unavailable_title(title.into());
                    u.set_unavailable_path(path.into());
                    u.set_unavailable_track_id(id as i32);
                });
                format!(
                    "\"{}\" is unavailable. Restore the file or import its new folder. Saved path: {}",
                    t.title,
                    t.path
                )
            } else {
                format!("Cannot open \"{}\": {error}", t.title)
            }
        })?;
        let decoder = Decoder::try_from(file)?;
        let duration = decoder
            .total_duration()
            .map_or(t.seconds, |d| d.as_secs_f64());
        let mut stream = OutputStreamBuilder::open_default_stream()?;
        stream.log_on_drop(false);
        let sink = Sink::connect_new(stream.mixer());
        sink.pause();
        sink.set_volume(self.volume);
        sink.append(decoder);
        let position = if duration > 0. && position >= duration {
            0.
        } else {
            position.clamp(0., duration.max(0.))
        };
        if position > 0. {
            sink.try_seek(Duration::from_secs_f64(position))
                .map_err(|e| e.to_string())?;
        }
        // Decode/open succeeds before replacing the current track.
        self.audio.take();
        sink.play();
        self.audio = Some(Audio {
            _stream: stream,
            sink,
        });
        self.current = Some(id);
        self.position = position;
        self.last_second = u64::MAX;
        if let Some(t) = self.all.iter_mut().find(|t| t.id == id) {
            t.seconds = duration;
        }
        self.show_player();
        self.persist()?;
        if let Some(t) = self.all.iter().find(|t| t.id == id) {
            // Reuse decoded artwork before hitting the disk again.
            let art = match self.art.remove(&id) {
                Some(cached) => {
                    self.art_order.retain(|cached_id| *cached_id != id);
                    self.art_order.push_back(id);
                    self.art.insert(id, cached.clone());
                    Some(cached)
                }
                None => {
                    let decoded = cover(&t.path);
                    if let Some(art) = &decoded {
                        self.art.insert(id, art.clone());
                        self.art_order.push_back(id);
                        while self.art_order.len() > ART_CACHE_LIMIT {
                            if let Some(oldest) = self.art_order.pop_front() {
                                self.art.remove(&oldest);
                            }
                        }
                    }
                    decoded
                }
            };
            update(&self.ui, move |u| {
                let present = art.is_some();
                u.set_has_cover(present);
                u.set_has_cover_expanded(present);
                if let Some((bar, expanded)) = art {
                    u.set_cover(slint::Image::from_rgba8(
                        slint::SharedPixelBuffer::clone_from_slice(
                            &bar.bytes, bar.width, bar.height,
                        ),
                    ));
                    u.set_cover_expanded(slint::Image::from_rgba8(
                        slint::SharedPixelBuffer::clone_from_slice(
                            &expanded.bytes,
                            expanded.width,
                            expanded.height,
                        ),
                    ));
                }
            });
        }
        status(&self.ui, "Playing from your library.");
        Ok(())
    }
    fn pause(&mut self) -> Result<()> {
        if let Some(audio) = self.audio.take() {
            self.position = audio.sink.get_pos().as_secs_f64();
            drop(audio);
        }
        self.show_player();
        self.persist()?;
        Ok(())
    }
    fn next(&mut self, direction: i32, ended: bool) -> Result<()> {
        if self.queue.is_empty() {
            return Ok(());
        }
        if direction < 0
            && self
                .audio
                .as_ref()
                .map_or(self.position, |a| a.sink.get_pos().as_secs_f64())
                > 3.
        {
            if let Some(id) = self.current {
                return self.start(id, 0.);
            }
        }
        let index = self
            .current
            .and_then(|id| self.queue.iter().position(|v| *v == id))
            .unwrap_or(0);
        let next = if ended && self.repeat == 2 {
            index as i32
        } else {
            index as i32 + direction
        };
        if next < 0 || next >= self.queue.len() as i32 {
            if self.repeat == 1 {
                return self.start(
                    self.queue[next.rem_euclid(self.queue.len() as i32) as usize],
                    0.,
                );
            }
            self.pause()?;
            return Ok(());
        }
        self.start(self.queue[next as usize], 0.)
    }
    fn import(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        update(&self.ui, |u| u.set_busy(true));
        status(&self.ui, "Reading your music…");
        let mut count = 0;
        let mut duplicate = 0;
        let mut errors = Vec::new();
        let mut failed = 0;
        for path in paths {
            match self.db.import(&path) {
                Ok(true) => count += 1,
                Ok(false) => duplicate += 1,
                Err(e) => {
                    failed += 1;
                    if errors.len() < 2 {
                        errors.push(format!(
                            "{}: {e}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ))
                    }
                }
            }
        }
        self.refresh()?;
        update(&self.ui, |u| u.set_busy(false));
        status(
            &self.ui,
            format!(
                "Added {count} · Already in library {duplicate} · Failed {failed}{}",
                if errors.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", errors.join("; "))
                }
            ),
        );
        Ok(())
    }
    fn step(&mut self, cmd: Command) -> Result<()> {
        match cmd {
            Command::Import(paths) => self.import(paths)?,
            Command::PruneMissing => {
                update(&self.ui, |u| u.set_busy(true));
                let removed = self.db.prune_missing();
                let removed = match removed {
                    Ok(n) => n,
                    Err(e) => {
                        update(&self.ui, |u| u.set_busy(false));
                        return Err(e);
                    }
                };
                self.refresh()?;
                update(&self.ui, |u| u.set_busy(false));
                status(
                    &self.ui,
                    if removed == 0 {
                        "Every track in your library is still on disk.".to_owned()
                    } else {
                        format!(
                            "Removed {removed} missing {} from the library. Files on disk were not touched.",
                            if removed == 1 { "track" } else { "tracks" }
                        )
                    },
                );
            }
            Command::Folder(path) => {
                update(&self.ui, |u| u.set_busy(true));
                self.import(library::folder_files(&path)?)?
            }
            Command::Filter(index, query) => {
                // Debounce: typing must not re-filter on every keystroke.
                self.pending_filter = Some((index, query));
                self.filter_due = Some(std::time::Instant::now() + SEARCH_DEBOUNCE);
            }
            Command::Page(page) => {
                // Only the Songs view changes what the list means, so a page
                // switch is a re-filter just for that one case.
                let next = page.clamp(0, 2);
                if next != self.page {
                    self.page = next;
                    self.apply_filter()?;
                }
            }
            Command::Sort(order) => {
                let next = order.clamp(0, 3);
                if next != self.sort {
                    self.sort = next;
                    self.apply_filter()?;
                }
            }
            Command::Thumbnail(id) => self.thumbnail(id),
            Command::Relink(id, path) => {
                // Validate the decoder before changing the saved path. A bad
                // selection cannot replace the entry or interrupt current audio.
                let _ = Decoder::try_from(std::fs::File::open(&path)?)?;
                self.db.relink(id, &path)?;
                self.art.remove(&id);
                self.art_order.retain(|cached| *cached != id);
                self.refresh()?;
                self.start(id, 0.)?;
                update(&self.ui, |u| u.set_unavailable_track_id(-1));
                status(&self.ui, "Song reconnected. Favorites and playlists kept.");
            }
            Command::Play(id) => {
                self.queue = self.visible.clone();
                if self.shuffle {
                    let hash = std::collections::hash_map::RandomState::new();
                    self.queue.sort_by_cached_key(|candidate| {
                        (*candidate != id, hash.hash_one(candidate))
                    });
                }
                self.start(id, 0.)?
            }
            Command::Pause => self.pause()?,
            Command::Resume => {
                if self.audio.is_none() {
                    if let Some(id) = self.current {
                        self.start(id, self.position)?
                    } else if let Some(id) = self.visible.first().copied() {
                        self.queue = self.visible.clone();
                        self.start(id, 0.)?
                    }
                }
            }
            Command::Toggle => {
                if self.audio.is_some() {
                    self.pause()?
                } else if let Some(id) = self.current {
                    self.start(id, self.position)?
                } else if let Some(id) = self.visible.first().copied() {
                    self.queue = self.visible.clone();
                    self.start(id, 0.)?
                }
            }
            Command::Stop => {
                self.audio.take();
                self.position = 0.;
                self.show_player();
                self.persist()?;
            }
            Command::Next(d) => self.next(d, false)?,
            Command::Seek(position) => {
                if !position.is_finite() {
                    return Err("Invalid seek position".into());
                }
                let end = self
                    .current
                    .and_then(|id| self.all.iter().find(|t| t.id == id))
                    .map_or(0., |t| t.seconds);
                let pos = (position as f64).clamp(0., end.max(0.));
                if let Some(a) = &self.audio {
                    a.sink
                        .try_seek(Duration::from_secs_f64(pos))
                        .map_err(|e| e.to_string())?;
                }
                self.position = pos;
                self.show_player();
                self.persist()?;
            }
            Command::Volume(value) => {
                if !value.is_finite() {
                    return Err("Invalid volume".into());
                }
                self.volume = (value / 100.).clamp(0., 1.);
                if let Some(a) = &self.audio {
                    a.sink.set_volume(self.volume);
                }
                self.db.save("volume", &self.volume.to_string())?
            }
            Command::Favorite(id) => {
                self.db.favorite(id)?;
                self.refresh()?
            }
            Command::Remove(id) => {
                self.db.remove(id)?;
                self.queue.retain(|v| *v != id);
                if self.current == Some(id) {
                    self.audio.take();
                    self.current = None;
                    self.position = 0.;
                }
                self.persist()?;
                self.refresh()?;
                self.show_player();
                status(&self.ui, "Removed from library. Original file kept.");
            }
            Command::NewPlaylist(name) => {
                self.db.create_playlist(&name)?;
                self.refresh()?;
                update(&self.ui, |u| u.set_playlist_name("".into()));
                status(
                    &self.ui,
                    "Playlist created. Use + beside a track to add it.",
                );
            }
            Command::ImportPlaylist(path) => {
                update(&self.ui, |u| u.set_busy(true));
                let imported = self.db.import_playlist(&path);
                self.refresh()?;
                update(&self.ui, |u| u.set_busy(false));
                status(
                    &self.ui,
                    match &imported {
                        Ok(summary) => format!(
                            "Imported \"{}\" with {} of your tracks · {} not in the library",
                            summary.name, summary.matched, summary.missing
                        ),
                        Err(e) => e.to_string(),
                    },
                );
            }
            Command::DeletePlaylist(id) => {
                self.db.delete_playlist(id)?;
                self.filter = 0;
                self.refresh()?;
            }
            Command::AddToPlaylist(id, index) => {
                let playlist = self
                    .playlists
                    .get(index.checked_sub(2).ok_or("Choose a playlist")?)
                    .ok_or("Playlist no longer exists")?
                    .0;
                self.db.add_to_playlist(playlist, id)?;
                self.refresh()?;
                status(&self.ui, "Added to playlist.");
            }
            Command::RemoveFromPlaylist(id, playlist) => {
                if playlist <= 0 {
                    return Err("Choose a playlist".into());
                }
                self.db.remove_from_playlist(playlist, id)?;
                self.refresh()?;
            }
            Command::Save(key, value) => {
                self.db.save(&key, &value)?;
                let downloader = self.db.setting("downloader", "");
                let folder = self.db.setting("download_folder", "");
                update(&self.ui, move |u| {
                    u.set_downloader_path(downloader.into());
                    u.set_download_folder(folder.into());
                });
            }
            Command::Shuffle(value) => {
                self.shuffle = value;
                self.db.save("shuffle", if value { "1" } else { "0" })?;
                if value {
                    let hash = std::collections::hash_map::RandomState::new();
                    self.queue
                        .sort_by_cached_key(|id| (Some(*id) != self.current, hash.hash_one(id)));
                } else {
                    self.queue = self
                        .all
                        .iter()
                        .map(|t| t.id)
                        .filter(|id| self.queue.contains(id))
                        .collect();
                }
                self.persist()?;
            }
            Command::Repeat(value) => {
                self.repeat = value.clamp(0, 2);
                self.db.save("repeat", &self.repeat.to_string())?
            }
            _ => {}
        }
        Ok(())
    }
}
pub fn spawn(
    db: Library,
    ui: slint::Weak<AppWindow>,
    rx: mpsc::Receiver<Command>,
    tx: mpsc::Sender<Command>,
    cancel: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let volume = db
            .setting("volume", "0.7")
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .unwrap_or(0.7)
            .clamp(0., 1.);
        let current = db
            .setting("current", "-1")
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0);
        let position = db
            .setting("position", "0")
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .unwrap_or(0.)
            .max(0.);
        let queue = db
            .setting("queue", "")
            .split(',')
            .filter_map(|s| s.parse().ok())
            .collect();
        let shuffle = db.setting("shuffle", "0") == "1";
        let repeat = db
            .setting("repeat", "0")
            .parse::<i32>()
            .unwrap_or(0)
            .clamp(0, 2);
        let mut state = Worker {
            db,
            ui,
            all: vec![],
            visible: vec![],
            playlists: vec![],
            members: std::collections::HashMap::new(),
            filter: 0,
            query: String::new(),
            page: 0,
            sort: 0,
            pending_filter: None,
            filter_due: None,
            art: std::collections::HashMap::new(),
            art_order: std::collections::VecDeque::new(),
            thumbnails: std::collections::VecDeque::new(),
            queue,
            current,
            position,
            audio: None,
            volume,
            shuffle,
            repeat,
            last_second: u64::MAX,
        };
        if let Err(e) = state.refresh() {
            status(&state.ui, format!("Library error: {e}"));
        }
        state
            .queue
            .retain(|id| state.all.iter().any(|t| t.id == *id));
        if state
            .current
            .is_some_and(|id| !state.all.iter().any(|t| t.id == id))
        {
            state.current = None;
            state.position = 0.;
        }
        state.show_player();
        update(&state.ui, move |u| {
            u.set_volume(volume * 100.);
            u.set_shuffle(shuffle);
            u.set_repeat_mode(repeat);
        });
        let mut inbox = match monolith_desktop::inbox::Inbox::new(&state.db) {
            Ok(inbox) => Some(inbox),
            Err(error) => {
                status(&state.ui, format!("Automatic import unavailable: {error}"));
                None
            }
        };
        let mut next_scan = std::time::Instant::now();
        let mut inbox_error = String::new();
        let mut download_job: Option<std::thread::JoinHandle<()>> = None;
        loop {
            // Always time out so a pending debounce can fire even while idle.
            let command = match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => break,
            };
            if state
                .filter_due
                .is_some_and(|due| std::time::Instant::now() >= due)
            {
                state.filter_due = None;
                if let Some((index, query)) = state.pending_filter.take() {
                    state.filter = index;
                    state.query = query;
                    if let Err(e) = state.apply_filter() {
                        status(&state.ui, e.to_string());
                    }
                }
            }
            if let Some(command) = command {
                match command {
                    Command::Shutdown => break,
                    Command::Download(url, mp3) => {
                        if download_job.as_ref().is_some_and(|job| !job.is_finished()) {
                            status(&state.ui, "A download is already active.");
                            continue;
                        }
                        if let Some(job) = download_job.take() {
                            let _ = job.join();
                        }
                        if let Err(e) = transfer::validated_url(&url) {
                            let text = e.to_string();
                            update(&state.ui, move |u| u.set_download_status(text.into()));
                            continue;
                        }
                        cancel.store(false, Ordering::Relaxed);
                        let executable = PathBuf::from(state.db.setting("downloader", ""));
                        let destination = PathBuf::from(state.db.setting("download_folder", ""));
                        let weak = state.ui.clone();
                        let sender = tx.clone();
                        let stop = cancel.clone();
                        update(&weak, |u| {
                            u.set_downloading(true);
                            u.set_download_progress(0.);
                            u.set_download_status("Starting download…".into());
                        });
                        download_job = Some(std::thread::spawn(move || {
                            let result = transfer::download(
                                &executable,
                                &url,
                                &destination,
                                mp3,
                                stop,
                                |percent, message| {
                                    update(&weak, move |u| {
                                        u.set_download_progress(percent);
                                        u.set_download_status(message.into());
                                    });
                                },
                            )
                            .map_err(|e| e.to_string());
                            let _ = sender.send(Command::Downloaded(url, result));
                        }));
                    }
                    Command::Downloaded(url, result) => {
                        let message = match result {
                            Ok(path) => {
                                let import = state.import(vec![path.clone()]);
                                let history = state.db.0.execute(
                                    "INSERT INTO transfers(url,path) VALUES(?1,?2)",
                                    rusqlite::params![url, path.to_string_lossy().as_ref()],
                                );
                                let _ = state.refresh();
                                match (import, history) {
                                    (Ok(_), Ok(_)) => format!("Saved: {}", path.display()),
                                    (Err(e), _) => {
                                        format!("File saved; library update failed: {e}")
                                    }
                                    (_, Err(e)) => {
                                        format!("File saved; history update failed: {e}")
                                    }
                                }
                            }
                            Err(e) => e,
                        };
                        update(&state.ui, move |u| {
                            u.set_downloading(false);
                            u.set_download_status(message.into());
                        });
                    }
                    other => {
                        if let Err(e) = state.step(other) {
                            status(&state.ui, e.to_string());
                            update(&state.ui, |u| u.set_busy(false));
                        }
                    }
                }
            }
            if std::time::Instant::now() >= next_scan {
                if let Some(inbox) = &mut inbox {
                    let roots: Vec<_> = ["music_folder", "download_folder"]
                        .into_iter()
                        .map(|key| state.db.setting(key, ""))
                        .filter(|path| !path.is_empty())
                        .map(PathBuf::from)
                        .collect();
                    match inbox.scan(&state.db, &roots) {
                        Ok(count) => {
                            inbox_error.clear();
                            if count > 0 {
                                if let Err(error) = state.refresh() {
                                    status(&state.ui, error.to_string());
                                } else {
                                    status(
                                        &state.ui,
                                        format!(
                                            "Automatically added {count} from your music folders."
                                        ),
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            let message = format!("Automatic folder import: {error}");
                            if message != inbox_error {
                                status(&state.ui, &message);
                                inbox_error = message;
                            }
                        }
                    }
                }
                next_scan = std::time::Instant::now() + Duration::from_secs(3);
            }
            if let Some(audio) = &state.audio {
                if audio.sink.empty() {
                    if let Err(e) = state.next(1, true) {
                        let _ = state.pause();
                        status(&state.ui, e.to_string());
                    }
                } else {
                    let seconds = audio.sink.get_pos().as_secs();
                    if seconds != state.last_second {
                        state.last_second = seconds;
                        state.position = audio.sink.get_pos().as_secs_f64();
                        let position = state.position;
                        update(&state.ui, move |u| {
                            u.set_position(position as f32);
                            u.set_elapsed(library::clock(position).into());
                        });
                    }
                }
            }
        }
        cancel.store(true, Ordering::Relaxed);
        if let Some(job) = download_job {
            let _ = job.join();
        }
        // Record a transfer that completed just before the window closed.
        for pending in rx.try_iter() {
            if let Command::Downloaded(url, Ok(path)) = pending {
                if let Err(e) = state.db.import(&path) {
                    eprintln!("Saved download could not be imported: {e}");
                }
                if let Err(e) = state.db.0.execute(
                    "INSERT INTO transfers(url,path) VALUES(?1,?2)",
                    rusqlite::params![url, path.to_string_lossy().as_ref()],
                ) {
                    eprintln!("Could not record download: {e}");
                }
            }
        }
        if let Err(e) = state.pause() {
            eprintln!("Could not save playback state: {e}");
        }
    })
}

/// Artwork prepared for one display slot, with the corner radius already in
/// the pixels.
#[derive(Clone)]
pub struct Art {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Art {
    fn from(image: image::RgbaImage) -> Self {
        let (width, height) = image.dimensions();
        Self {
            bytes: image.into_raw(),
            width,
            height,
        }
    }
}

/// Bakes a rounded-rect alpha mask into the artwork.
///
/// Slint's renderers clip to rectangular bounds only, so `clip: true` plus a
/// `border-radius` still paints square corners. Rounding has to happen here.
fn rounded_alpha(image: &mut image::RgbaImage, radius: u32) {
    let (width, height) = image.dimensions();
    if radius == 0 || width < 2 || height < 2 {
        return;
    }
    let r = radius.min(width.min(height) / 2) as f32;
    let (fw, fh) = (width as f32, height as f32);
    // Coverage is sampled per pixel instead of building a separate mask and
    // resampling it, so the body stays fully opaque and the corners clear.
    // 4x4 samples is enough to hide stair-stepping on the arc.
    const SAMPLES: u32 = 4;
    const TOTAL: u32 = SAMPLES * SAMPLES;
    let step = 1.0 / SAMPLES as f32;
    for y in 0..height {
        for x in 0..width {
            let mut inside = 0;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let fx = x as f32 + (sx as f32 + 0.5) * step;
                    let fy = y as f32 + (sy as f32 + 0.5) * step;
                    let dx = (r - fx).max(fx - (fw - r)).max(0.0);
                    let dy = (r - fy).max(fy - (fh - r)).max(0.0);
                    if dx * dx + dy * dy <= r * r {
                        inside += 1;
                    }
                }
            }
            let coverage = inside * 255 / TOTAL;
            let pixel = image.get_pixel_mut(x, y);
            pixel.0[3] = (pixel.0[3] as u32 * coverage / 255) as u8;
        }
    }
}

/// Corner radii expressed as a fraction of the artwork, matching how large
/// each slot is on screen: 10 px on the 52 px bar, 12 px on the 370 px hero.
const BAR_RADIUS_RATIO: f32 = 10.0 / 52.0;
const EXPANDED_RADIUS_RATIO: f32 = 12.0 / 370.0;

fn decode_cover(path: &str) -> Option<image::RgbaImage> {
    use lofty::file::TaggedFileExt;
    const MAX_ART_BYTES: usize = 8 * 1024 * 1024;
    fn decode(data: &[u8]) -> Option<image::RgbaImage> {
        if data.len() > MAX_ART_BYTES {
            return None;
        }
        let mut reader = image::ImageReader::new(std::io::Cursor::new(data))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(2048);
        limits.max_image_height = Some(2048);
        limits.max_alloc = Some(32 * 1024 * 1024);
        reader.limits(limits);
        Some(reader.decode().ok()?.into_rgba8())
    }
    // Some files keep text in their primary tag and artwork in another tag.
    // A corrupt/unsupported picture must not mask a usable later picture.
    if let Ok(file) = lofty::read_from_path(path) {
        if let Some(art) = file
            .tags()
            .iter()
            .flat_map(|tag| tag.pictures())
            .find_map(|picture| decode(picture.data()))
        {
            return Some(art);
        }
    }
    let sidecar = library::sidecar_image(std::path::Path::new(path))?;
    use std::io::Read;
    let mut data = Vec::new();
    std::fs::File::open(sidecar)
        .ok()?
        .take((MAX_ART_BYTES + 1) as u64)
        .read_to_end(&mut data)
        .ok()?;
    decode(&data)
}

fn row_cover(path: &str) -> Option<Art> {
    let decoded = decode_cover(path)?;
    let span = decoded.width().min(decoded.height());
    let square = image::imageops::crop_imm(
        &decoded,
        (decoded.width() - span) / 2,
        (decoded.height() - span) / 2,
        span,
        span,
    );
    let mut tile = image::imageops::resize(
        &square.to_image(),
        64,
        64,
        image::imageops::FilterType::Triangle,
    );
    rounded_alpha(&mut tile, 11);
    Some(Art::from(tile))
}

fn cover(path: &str) -> Option<(Art, Art)> {
    let decoded = image::DynamicImage::ImageRgba8(decode_cover(path)?)
        .thumbnail(384, 384)
        .into_rgba8();
    let span = decoded.width().min(decoded.height());
    let mut bar = decoded.clone();
    rounded_alpha(&mut bar, (span as f32 * BAR_RADIUS_RATIO).round() as u32);
    let mut expanded = decoded;
    rounded_alpha(
        &mut expanded,
        (span as f32 * EXPANDED_RADIUS_RATIO).round() as u32,
    );
    Some((Art::from(bar), Art::from(expanded)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_webp_sidecars_and_skips_corrupt_embedded_pictures() {
        use lofty::{
            config::WriteOptions,
            file::TaggedFileExt,
            picture::{MimeType, Picture},
            tag::{Tag, TagExt, TagType},
        };
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("track.wav");
        // A minimal valid PCM file keeps this check independent of audio devices.
        let mut wav = b"RIFF".to_vec();
        wav.extend_from_slice(&38u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&2u32.to_le_bytes());
        wav.extend_from_slice(&[0, 0]);
        std::fs::write(&audio, wav).unwrap();
        let sidecar = audio.with_extension("webp");
        image::RgbaImage::from_pixel(2048, 1024, image::Rgba([220, 80, 60, 255]))
            .save(&sidecar)
            .unwrap();
        assert!(
            row_cover(audio.to_str().unwrap()).is_some(),
            "bounded 2K WebP sidecar"
        );
        let data = std::fs::read(&sidecar).unwrap();
        std::fs::remove_file(sidecar).unwrap();
        let mut info = Tag::new(TagType::RiffInfo);
        info.insert_text(
            lofty::tag::ItemKey::TrackTitle,
            "Text-only metadata tag".into(),
        );
        info.save_to_path(&audio, WriteOptions::default()).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.push_picture(
            Picture::unchecked(b"corrupt cover".to_vec())
                .mime_type(MimeType::Png)
                .build(),
        );
        tag.push_picture(
            Picture::unchecked(data)
                .mime_type(MimeType::from_str("image/webp"))
                .build(),
        );
        tag.save_to_path(&audio, WriteOptions::default()).unwrap();
        let file = lofty::read_from_path(&audio).unwrap();
        assert!(
            file.tags()
                .iter()
                .flat_map(|tag| tag.pictures())
                .next()
                .unwrap()
                .data()
                == b"corrupt cover",
            "fixture starts with an invalid embedded picture"
        );
        assert!(
            row_cover(audio.to_str().unwrap()).is_some(),
            "later valid embedded WebP picture"
        );
    }

    #[test]
    fn row_cover_bounds_sidecars_and_handles_missing_or_bad_images() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("track.wav");
        let path = audio.to_str().unwrap();
        assert!(row_cover(path).is_none());
        let sidecar = audio.with_extension("png");
        let mut picture = image::RgbaImage::from_pixel(160, 80, image::Rgba([0, 0, 255, 255]));
        for y in 0..80 {
            for x in 40..120 {
                picture.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
        picture.save(&sidecar).unwrap();
        let tile = row_cover(path).unwrap();
        assert_eq!(
            (tile.width, tile.height, tile.bytes.len()),
            (64, 64, 64 * 64 * 4)
        );
        assert_eq!(tile.bytes[3], 0, "rounded corner");
        let center = (32 * 64 + 32) * 4;
        assert_eq!(
            &tile.bytes[center..center + 4],
            &[255, 0, 0, 255],
            "center crop"
        );
        std::fs::write(&sidecar, b"not an image").unwrap();
        assert!(row_cover(path).is_none());
        std::fs::File::create(&sidecar)
            .unwrap()
            .set_len(8 * 1024 * 1024 + 1)
            .unwrap();
        assert!(
            row_cover(path).is_none(),
            "oversized sidecar must be rejected"
        );
    }

    /// Guards the "rounded corners" requirement. Slint only clips to
    /// rectangular bounds, so a regression here silently renders square
    /// artwork again.
    #[test]
    fn rounded_alpha_clears_the_corners_and_keeps_the_body() {
        let mut art = image::RgbaImage::from_pixel(64, 64, image::Rgba([255, 0, 0, 255]));
        rounded_alpha(&mut art, 12);
        let alpha = |x: u32, y: u32| art.get_pixel(x, y).0[3];
        assert_eq!(alpha(0, 0), 0, "top-left corner must be transparent");
        assert_eq!(alpha(63, 0), 0, "top-right corner must be transparent");
        assert_eq!(alpha(0, 63), 0, "bottom-left corner must be transparent");
        assert_eq!(alpha(63, 63), 0, "bottom-right corner must be transparent");
        assert_eq!(alpha(32, 32), 255, "centre must stay opaque");
        assert_eq!(alpha(32, 0), 255, "edge midpoint must stay opaque");
    }

    #[test]
    fn rounded_alpha_leaves_a_rectangle_alone_at_zero_radius() {
        let mut art = image::RgbaImage::from_pixel(32, 32, image::Rgba([0, 255, 0, 255]));
        rounded_alpha(&mut art, 0);
        assert_eq!(art.get_pixel(0, 0).0[3], 255);
    }
}
