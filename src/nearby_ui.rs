//! One foreground sharing worker plus an off-thread cover-thumbnail loader.
//! Transport, hashing, and artwork decoding stay off Slint's UI thread.
use crate::{AppWindow, ShareDevice, ShareSong, SharingState, controller::Command};
use monolith_desktop::{
    bluetooth, lan,
    library::{self, Library, Result, Track},
    nearby,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap, VecDeque},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const MAX_SHARE_BATCH: usize = 100;

enum Action {
    Refresh(PathBuf, i32),
    Send(Vec<i64>, String),
    Receive,
    Stop,
    Close,
    Shutdown,
}

enum ThumbAction {
    Catalog(Vec<(i64, String)>),
    Request(i64),
    Select(i64),
    Shutdown,
}

#[derive(Clone)]
struct SongEntry {
    id: i64,
    label: String,
    title: String,
    detail: String,
    duration: String,
    search: String,
}

/// Clean artist/album subtitle: avoids redundant `"Artist · Artist"` or `"Artist · Singles"`.
fn format_track_detail(artist: &str, album: &str) -> String {
    let artist = artist.trim();
    let album = album.trim();
    if album.is_empty()
        || album.eq_ignore_ascii_case("Singles")
        || album.eq_ignore_ascii_case("Unknown album")
        || album.eq_ignore_ascii_case(artist)
    {
        artist.to_owned()
    } else {
        format!("{artist} · {album}")
    }
}

trait Transport: std::io::Read + std::io::Write {}
impl<T: std::io::Read + std::io::Write> Transport for T {}

enum Peer {
    Bluetooth(bluetooth::Peer),
    App(lan::Peer),
}
impl Peer {
    fn id(&self) -> String {
        match self {
            Self::Bluetooth(p) => format!("bluetooth:{:x}", p.address),
            Self::App(p) => format!("app:{}:{}", p.address, p.fingerprint),
        }
    }
    fn display(&self) -> (String, String) {
        match self {
            Self::Bluetooth(p) => (p.name.clone(), "Paired Bluetooth · RFCOMM".into()),
            Self::App(p) => match p.name.split_once(" · ") {
                Some((alias, addr)) => (
                    alias.to_owned(),
                    format!("{addr}:{} · TLS 1.3", p.address.port()),
                ),
                None => (p.name.clone(), format!("{} · TLS 1.3", p.address)),
            },
        }
    }
}

fn show_peers(ui: &slint::Weak<AppWindow>, peers: &[Peer]) {
    let labels: Vec<slint::SharedString> = peers
        .iter()
        .map(|p| match p {
            Peer::Bluetooth(p) => p.name.clone().into(),
            Peer::App(p) => p.name.clone().into(),
        })
        .collect();
    let items: Vec<ShareDevice> = peers
        .iter()
        .map(|p| {
            let (name, detail) = p.display();
            ShareDevice {
                name: name.into(),
                detail: detail.into(),
            }
        })
        .collect();
    let ids: Vec<slint::SharedString> = peers.iter().map(|p| p.id().into()).collect();
    update(ui, move |u| {
        let previous = u.get_device_ids().row_data(u.get_device_index() as usize);
        let selected = previous
            .and_then(|id| ids.iter().position(|candidate| *candidate == id))
            .or_else(|| (ids.len() == 1).then_some(0))
            .map(|i| i as i32)
            .unwrap_or(-1);
        u.set_device_ids(ModelRc::new(VecModel::from(ids)));
        u.set_devices(ModelRc::new(VecModel::from(labels)));
        u.set_device_items(ModelRc::new(VecModel::from(items)));
        u.set_device_index(selected);
    });
}

/// A device that stops answering is dropped after this long, so one lost
/// discovery round does not make it flicker out of the picker.
const PEER_TTL: Duration = Duration::from_secs(15);
const THUMB_CACHE_LIMIT: usize = 160;

enum Server {
    Bluetooth(bluetooth::Listener),
    App(lan::Listener),
}

/// Our LAN listener, so scans keep answering others and skip ourselves.
fn own_lan(listener: &Option<Server>) -> Option<&lan::Listener> {
    match listener {
        Some(Server::App(server)) => Some(server),
        _ => None,
    }
}

/// Explicit Bluetooth Receive mode. LAN visibility is implicit and keeps the
/// song and device pickers usable.
fn receiving(listener: &Option<Server>) -> bool {
    matches!(listener, Some(Server::Bluetooth(_)))
}

/// Wi-Fi mode is visible whenever the Share page is active on it, like the mobile
/// Share hub: no separate Receive step. Returns a notice if binding failed.
fn arm_lan(
    listener: &mut Option<Server>,
    cancel: &Arc<AtomicBool>,
    ui: &slint::Weak<AppWindow>,
) -> Option<String> {
    if matches!(listener, Some(Server::App(_))) {
        return None;
    }
    match lan::Listener::bind(cancel.clone()) {
        Ok(server) => {
            let name = server.name.clone();
            let port = server.port();
            let port_str = if port > 0 {
                port.to_string()
            } else {
                String::new()
            };
            update(ui, move |u| {
                u.set_receiving_name(name.into());
                u.set_receiving_port(port_str.into());
            });
            *listener = Some(Server::App(server));
            None
        }
        Err(e) => {
            update(ui, |u| {
                u.set_receiving_name("".into());
                u.set_receiving_port("".into());
            });
            Some(format!(
                "Other devices can't find this one right now ({e}). You can still send."
            ))
        }
    }
}

impl Server {
    fn accept(&self) -> Result<Option<(Box<dyn Transport>, String)>> {
        match self {
            Self::Bluetooth(s) => Ok(s
                .accept()?
                .map(|(stream, name)| (Box::new(stream) as Box<dyn Transport>, name))),
            Self::App(s) => Ok(s.accept()?.map(|stream| {
                (
                    Box::new(stream) as Box<dyn Transport>,
                    "Nearby Monolith app".into(),
                )
            })),
        }
    }
}

/// Strict case-insensitive alphabetical sort (Title A→Z, then Artist A→Z, then ID).
pub(crate) fn sort_tracks_alphabetical(tracks: &mut [Track]) {
    tracks.sort_by(|a, b| {
        a.title
            .trim()
            .to_lowercase()
            .cmp(&b.title.trim().to_lowercase())
            .then_with(|| {
                a.artist
                    .trim()
                    .to_lowercase()
                    .cmp(&b.artist.trim().to_lowercase())
            })
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn sync_selected_preview(
    ui: &SharingState<'_>,
    entries: &[SongEntry],
    selected: &BTreeSet<i64>,
    thumb_tx: &mpsc::Sender<ThumbAction>,
) {
    let rows = ui.get_song_results();
    let row_count = rows.row_count();
    let all_filtered =
        row_count > 0 && (0..row_count).all(|i| rows.row_data(i).is_some_and(|r| r.checked));
    ui.set_selected_count(selected.len() as i32);
    ui.set_batch(selected.len() > 1);
    ui.set_all_filtered_selected(all_filtered);

    let lead = entries
        .iter()
        .enumerate()
        .find(|(_, e)| selected.contains(&e.id));
    if let Some((idx, entry)) = lead {
        ui.set_song_index(idx as i32);
        ui.set_selected_title(entry.title.clone().into());
        ui.set_selected_detail(entry.detail.clone().into());
        let _ = thumb_tx.send(ThumbAction::Select(entry.id));
    } else {
        ui.set_song_index(-1);
        ui.set_selected_title("".into());
        ui.set_selected_detail("".into());
        ui.set_selected_thumbnail(slint::Image::default());
    }
}

fn update_rows_checked_in_place(
    ui: &SharingState<'_>,
    entries: &[SongEntry],
    selected: &BTreeSet<i64>,
    thumb_tx: &mpsc::Sender<ThumbAction>,
) {
    let rows = ui.get_song_results();
    for index in 0..rows.row_count() {
        let Some(mut row) = rows.row_data(index) else {
            continue;
        };
        let checked = selected.contains(&(row.id as i64));
        if row.checked != checked {
            row.checked = checked;
            rows.set_row_data(index, row);
        }
    }
    sync_selected_preview(ui, entries, selected, thumb_tx);
}

fn apply_song_filter(
    ui: &SharingState<'_>,
    entries: &[SongEntry],
    selected: &mut BTreeSet<i64>,
    thumb_tx: &mpsc::Sender<ThumbAction>,
) {
    if !entries.is_empty() {
        let valid: BTreeSet<i64> = entries.iter().map(|e| e.id).collect();
        selected.retain(|id| valid.contains(id));
    }
    let query = ui.get_song_query().trim().to_lowercase();
    let existing = ui.get_song_results();
    let mut ui_thumbs: HashMap<i32, slint::Image> = HashMap::new();
    for i in 0..existing.row_count() {
        if let Some(row) = existing.row_data(i) {
            if row.thumbnail.size().width > 0 {
                ui_thumbs.insert(row.id, row.thumbnail);
            }
        }
    }
    let results = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| query.is_empty() || e.search.contains(&query))
        .map(|(index, e)| ShareSong {
            index: index as i32,
            id: e.id as i32,
            label: e.label.clone().into(),
            title: e.title.clone().into(),
            detail: e.detail.clone().into(),
            duration: e.duration.clone().into(),
            thumbnail: ui_thumbs.remove(&(e.id as i32)).unwrap_or_default(),
            checked: selected.contains(&e.id),
        })
        .collect::<Vec<_>>();
    ui.set_song_results(ModelRc::new(VecModel::from(results)));
    sync_selected_preview(ui, entries, selected, thumb_tx);
}

fn resolve_thumb(
    id: i64,
    paths: &HashMap<i64, String>,
    cache: &mut VecDeque<(i64, Option<crate::controller::Art>)>,
) -> Option<crate::controller::Art> {
    if let Some(pos) = cache.iter().position(|(k, _)| *k == id) {
        let (_, art) = cache.remove(pos).unwrap();
        cache.push_back((id, art.clone()));
        return art;
    }
    let path = paths.get(&id)?;
    let art = crate::controller::row_cover(path);
    cache.push_back((id, art.clone()));
    if cache.len() > THUMB_CACHE_LIMIT {
        cache.pop_front();
    }
    art
}

fn spawn_thumb_worker(weak: slint::Weak<AppWindow>) -> (mpsc::Sender<ThumbAction>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<ThumbAction>();
    let handle = std::thread::spawn(move || {
        let mut paths: HashMap<i64, String> = HashMap::new();
        let mut cache: VecDeque<(i64, Option<crate::controller::Art>)> = VecDeque::new();
        let mut selected_id: Option<i64> = None;

        while let Ok(msg) = rx.recv() {
            match msg {
                ThumbAction::Shutdown => break,
                ThumbAction::Catalog(list) => {
                    paths = list.into_iter().collect();
                    cache.retain(|(id, _)| paths.contains_key(id));
                }
                ThumbAction::Select(id) => {
                    selected_id = Some(id);
                    let art = resolve_thumb(id, &paths, &mut cache);
                    update(&weak, move |u| {
                        let image = art.as_ref().map(|a| a.to_image()).unwrap_or_default();
                        u.set_selected_thumbnail(image);
                    });
                }
                ThumbAction::Request(id) => {
                    let art = resolve_thumb(id, &paths, &mut cache);
                    let also_selected = selected_id == Some(id);
                    update(&weak, move |u| {
                        let image = art.as_ref().map(|a| a.to_image()).unwrap_or_default();
                        if also_selected {
                            u.set_selected_thumbnail(image.clone());
                        }
                        let rows = u.get_song_results();
                        for index in 0..rows.row_count() {
                            let Some(mut row) = rows.row_data(index) else {
                                continue;
                            };
                            if row.id as i64 == id {
                                row.thumbnail = image.clone();
                                rows.set_row_data(index, row);
                            }
                        }
                    });
                }
            }
        }
    });
    (tx, handle)
}

fn refresh_action(app: &AppWindow) -> Action {
    Action::Refresh(
        PathBuf::from(app.get_download_folder().as_str()),
        app.global::<SharingState>().get_method(),
    )
}

pub struct Sharing {
    tx: mpsc::Sender<Action>,
    thumb_tx: mpsc::Sender<ThumbAction>,
    cancel: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    thumb_worker: Option<JoinHandle<()>>,
}

fn update(ui: &slint::Weak<AppWindow>, f: impl FnOnce(SharingState<'_>) + Send + 'static) {
    let _ = ui.upgrade_in_event_loop(move |app| f(app.global::<SharingState>()));
}

fn state(ui: &slint::Weak<AppWindow>, text: String, error: bool, busy: bool, listening: bool) {
    update(ui, move |u| {
        u.set_status(text.into());
        u.set_status_error(error);
        u.set_busy(busy);
        u.set_listening(listening);
        u.set_incoming(false);
    });
}

fn progress(ui: slint::Weak<AppWindow>) -> impl FnMut(u64, u64) {
    let mut last = Instant::now() - Duration::from_secs(1);
    move |done, total| {
        if last.elapsed() < Duration::from_millis(200) && done != total {
            return;
        }
        last = Instant::now();
        update(&ui, move |u| {
            u.set_progress(done as f32 / total as f32);
            u.set_status_error(false);
            u.set_status(
                format!(
                    "{:.1} / {:.1} MB · {}",
                    done as f64 / 1_000_000.,
                    total as f64 / 1_000_000.,
                    if done == total {
                        "Verifying library receipt…"
                    } else {
                        "Transferring…"
                    }
                )
                .into(),
            );
        });
    }
}

impl Sharing {
    pub fn bind(app: &AppWindow, data: PathBuf, controller: mpsc::Sender<Command>) -> Self {
        let ui = app.global::<SharingState>();
        ui.set_bluetooth_native(cfg!(windows));
        let (tx, rx) = mpsc::channel();
        let (decision_tx, decisions) = mpsc::channel();
        let (thumb_tx, thumb_worker) = spawn_thumb_worker(app.as_weak());
        let catalog: Rc<RefCell<Vec<SongEntry>>> = Rc::new(RefCell::new(Vec::new()));
        let selected_ids: Rc<RefCell<BTreeSet<i64>>> = Rc::new(RefCell::new(BTreeSet::new()));
        let cancel = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(true));
        let awaiting = Arc::new(AtomicBool::new(false));
        let (weak, worker_cancel, worker_running, worker_closed, worker_awaiting) = (
            app.as_weak(),
            cancel.clone(),
            running.clone(),
            closed.clone(),
            awaiting.clone(),
        );
        let worker_thumbs = thumb_tx.clone();

        let (cat_tx, cat_rx) = mpsc::channel::<Vec<SongEntry>>();
        let catalog_drain = catalog.clone();
        let drain_rc = Rc::new(move || {
            while let Ok(entries) = cat_rx.try_recv() {
                *catalog_drain.borrow_mut() = entries;
            }
        });

        let worker_cat_tx = cat_tx.clone();
        let ui_cat_sync = catalog.clone();
        let ui_sel_sync = selected_ids.clone();
        let ui_drain_sync = drain_rc.clone();
        let worker = std::thread::spawn(move || {
            let mut session = None;
            let mut listener: Option<Server> = None;
            let mut method = 0;
            let mut songs: Vec<Track> = Vec::new();
            let mut peers: Vec<Peer> = Vec::new();
            let mut last_seen: HashMap<String, Instant> = HashMap::new();
            let mut destination = PathBuf::new();
            let mut listening_until: Option<Instant> = None;
            let mut next_discovery = Instant::now();
            loop {
                if worker_cancel.load(Ordering::Relaxed)
                    || worker_closed.load(Ordering::Relaxed)
                    || listening_until.is_some_and(|t| Instant::now() > t)
                {
                    listening_until = None;
                    match listener.take() {
                        Some(Server::Bluetooth(_)) => state(
                            &weak,
                            "Receiving stopped. Choose Receive to start another session.".into(),
                            false,
                            false,
                            false,
                        ),
                        Some(Server::App(_)) => update(&weak, |u| {
                            u.set_receiving_name("".into());
                            u.set_receiving_port("".into());
                        }),
                        None => {}
                    }
                }
                let action = if listener.is_some()
                    || (method == 1 && !worker_closed.load(Ordering::Relaxed))
                {
                    match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(a) => Some(a),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(_) => break,
                    }
                } else {
                    match rx.recv() {
                        Ok(a) => Some(a),
                        Err(_) => break,
                    }
                };
                if let Some(action) = action {
                    if matches!(action, Action::Shutdown) {
                        break;
                    }
                    if matches!(action, Action::Close) {
                        listener = None;
                        session = None;
                        listening_until = None;
                        method = 0;
                        worker_running.store(false, Ordering::Release);
                        continue;
                    }
                    if matches!(action, Action::Stop) {
                        listener = None;
                        listening_until = None;
                        if !worker_closed.load(Ordering::Relaxed) {
                            worker_cancel.store(false, Ordering::Relaxed);
                            next_discovery = Instant::now();
                        }
                        state(&weak, "Sharing cancelled.".into(), false, false, false);
                        continue;
                    }
                    if worker_closed.load(Ordering::Relaxed)
                        || (worker_cancel.load(Ordering::Relaxed)
                            && !matches!(action, Action::Refresh(..)))
                    {
                        worker_running.store(false, Ordering::Release);
                        continue;
                    }
                    worker_running.store(true, Ordering::Release);
                    worker_cancel.store(false, Ordering::Relaxed);
                    let result: Result<String> = (|| match action {
                        Action::Refresh(folder, transport) => {
                            if !(transport == 1 && matches!(listener, Some(Server::App(_)))) {
                                listener = None;
                                listening_until = None;
                            }
                            session = None;
                            peers.clear();
                            last_seen.clear();
                            method = transport;
                            destination = folder;
                            update(&weak, |u| {
                                u.set_devices(ModelRc::default());
                                u.set_device_items(ModelRc::default());
                                u.set_device_ids(ModelRc::default());
                                u.set_device_index(-1);
                                u.set_available(false);
                            });
                            if method != 1 {
                                update(&weak, |u| {
                                    u.set_receiving_name("".into());
                                    u.set_receiving_port("".into());
                                });
                            }

                            songs = Library::open(&data.join("library.sqlite3"))?.tracks(0)?;
                            songs.retain(|s| std::path::Path::new(&s.path).is_file());
                            sort_tracks_alphabetical(&mut songs);

                            let _ = worker_thumbs.send(ThumbAction::Catalog(
                                songs.iter().map(|s| (s.id, s.path.clone())).collect(),
                            ));
                            let entries: Vec<SongEntry> = songs
                                .iter()
                                .map(|s| {
                                    let label = format!("{} · {}", s.title, s.artist);
                                    let detail = format_track_detail(&s.artist, &s.album);
                                    let raw = format!("{} {} {}", s.title, s.artist, s.album);
                                    let search = raw.to_lowercase();
                                    SongEntry {
                                        id: s.id,
                                        label,
                                        title: s.title.clone(),
                                        detail,
                                        duration: library::clock(s.seconds),
                                        search,
                                    }
                                })
                                .collect();
                            let _ = worker_cat_tx.send(entries.clone());
                            let labels: Vec<slint::SharedString> =
                                entries.iter().map(|e| e.label.clone().into()).collect();
                            let ui_thumbs = worker_thumbs.clone();
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                let u = app.global::<SharingState>();
                                u.set_songs(ModelRc::new(VecModel::from(labels)));
                                u.invoke_filter_songs();
                                let _ = &ui_thumbs;
                            });
                            if method == 0 {
                                return Ok(String::new());
                            }

                            let mut notice = None;
                            if method == 1 {
                                notice = arm_lan(&mut listener, &worker_cancel, &weak);
                                peers = lan::discover(&worker_cancel, own_lan(&listener))?
                                    .into_iter()
                                    .map(Peer::App)
                                    .collect();
                                let now = Instant::now();
                                for peer in &peers {
                                    last_seen.insert(peer.id(), now);
                                }
                            } else {
                                session = Some(bluetooth::Session::new()?);
                                peers = session
                                    .as_ref()
                                    .unwrap()
                                    .peers()?
                                    .into_iter()
                                    .map(Peer::Bluetooth)
                                    .collect();
                            }
                            show_peers(&weak, &peers);
                            next_discovery = Instant::now() + Duration::from_secs(5);
                            update(&weak, move |u| {
                                u.set_available(true);
                                u.set_progress(0.);
                            });
                            let hint = match (method, peers.is_empty()) {
                                (1, true) => {
                                    "Ready on Wi-Fi · Open Monolith → Share → Wi-Fi on your other device to connect automatically."
                                }
                                (1, false) => {
                                    "Check one or more songs, pick a device, then send. The recipient approves each song."
                                }
                                _ => "Choose a paired device running Monolith, or press Receive.",
                            };
                            Ok(match notice {
                                Some(notice) => format!("{notice} {hint}"),
                                None => hint.into(),
                            })
                        }
                        Action::Receive => {
                            if method == 1 {
                                return Ok(arm_lan(&mut listener, &worker_cancel, &weak)
                                    .unwrap_or_else(|| "Visible to nearby Monolith devices while the Share page is active. Each song asks for your approval.".into()));
                            }
                            listener = Some(Server::Bluetooth(
                                session
                                    .as_ref()
                                    .ok_or("Refresh devices first.")?
                                    .listen(worker_cancel.clone())?,
                            ));
                            listening_until = Some(Instant::now() + Duration::from_secs(300));
                            Ok("Receiving for 5 minutes. Each song needs your approval. Cancel or leave Share to stop.".into())
                        }
                        Action::Send(track_ids, peer_id) => {
                            let peer = peers.iter().find(|p| p.id() == peer_id).ok_or(
                                "That device is no longer available. Choose a nearby device again.",
                            )?;
                            if track_ids.is_empty() {
                                return Err("Choose at least one song to share.".into());
                            }
                            if track_ids.len() > MAX_SHARE_BATCH {
                                return Err("Max 100 songs per batch (100 MB each).".into());
                            }
                            let wanted: BTreeSet<i64> = track_ids.into_iter().collect();
                            let selected: Vec<&Track> =
                                songs.iter().filter(|s| wanted.contains(&s.id)).collect();
                            if selected.is_empty() {
                                return Err("Selected songs unavailable. Refresh.".into());
                            }
                            let offers: Vec<_> = selected
                                .iter()
                                .map(|s| {
                                    if worker_cancel.load(Ordering::Relaxed)
                                        || worker_closed.load(Ordering::Relaxed)
                                    {
                                        return Err("Sharing cancelled.".into());
                                    }
                                    nearby::Offer::from_file(
                                        std::path::Path::new(&s.path),
                                        &s.title,
                                    )
                                })
                                .collect::<Result<_>>()?;
                            let total = selected.len();
                            let mut saved = 0;
                            for (song, offer) in selected.into_iter().zip(offers) {
                                let path = std::path::Path::new(&song.path);
                                let art = nearby::artwork_for(path);
                                let offer = offer.with_artwork(art.as_deref());
                                let outcome: Result<()> = (|| {
                                    let mut stream: Box<dyn Transport> = match peer {
                                        Peer::Bluetooth(p) => Box::new(
                                            session
                                                .as_ref()
                                                .ok_or("Refresh devices first.")?
                                                .connect(p, worker_cancel.clone())?,
                                        ),
                                        Peer::App(p) => {
                                            Box::new(lan::connect(p, worker_cancel.clone())?)
                                        }
                                    };
                                    let note = format!(
                                        "Song {} of {total} · waiting for recipient approval…",
                                        saved + 1
                                    );
                                    update(&weak, move |u| {
                                        u.set_progress(0.);
                                        u.set_status_error(false);
                                        u.set_status(note.into());
                                    });
                                    nearby::send_with_artwork(
                                        &mut stream,
                                        path,
                                        &offer,
                                        art.as_deref(),
                                        &worker_cancel,
                                        progress(weak.clone()),
                                    )
                                })();
                                if let Err(e) = outcome {
                                    return Err(
                                        format!("{saved} of {total} songs received. {e}").into()
                                    );
                                }
                                saved += 1;
                            }
                            let msg = if saved == 1 {
                                "1 song received and added to the recipient's library.".to_owned()
                            } else {
                                format!(
                                    "{saved} songs received and added to the recipient's library."
                                )
                            };
                            Ok(msg)
                        }
                        _ => unreachable!(),
                    })();
                    worker_running.store(false, Ordering::Release);
                    let is_err = result.is_err();
                    state(
                        &weak,
                        result.unwrap_or_else(|e| e.to_string()),
                        is_err,
                        false,
                        receiving(&listener),
                    );
                } else if method == 1
                    && !worker_cancel.load(Ordering::Relaxed)
                    && !worker_closed.load(Ordering::Relaxed)
                    && Instant::now() >= next_discovery
                {
                    let _ = arm_lan(&mut listener, &worker_cancel, &weak);
                    if let Ok(found) = lan::discover(&worker_cancel, own_lan(&listener)) {
                        let now = Instant::now();
                        for peer in found.into_iter().map(Peer::App) {
                            let id = peer.id();
                            last_seen.insert(id.clone(), now);
                            match peers.iter().position(|p| p.id() == id) {
                                Some(i) => peers[i] = peer,
                                None => peers.push(peer),
                            }
                        }
                        last_seen.retain(|_, seen| now.duration_since(*seen) < PEER_TTL);
                        peers.retain(|p| last_seen.contains_key(&p.id()));
                        show_peers(&weak, &peers);
                    }
                    next_discovery = Instant::now() + Duration::from_secs(5);
                }
                if let Some(server) = listener.as_ref() {
                    match server.accept() {
                        Ok(None) => {}
                        Err(e) => {
                            listener = None;
                            state(&weak, e.to_string(), true, false, false);
                        }
                        Ok(Some((mut stream, peer))) => {
                            worker_running.store(true, Ordering::Release);
                            state(
                                &weak,
                                "Checking incoming Monolith offer…".into(),
                                false,
                                true,
                                false,
                            );
                            while decisions.try_recv().is_ok() {}
                            let result = nearby::receive(
                                &mut stream,
                                &destination,
                                &worker_cancel,
                                |offer| {
                                    worker_awaiting.store(true, Ordering::Release);
                                    let art_note = if offer.artwork.is_some() {
                                        " · with cover art"
                                    } else {
                                        ""
                                    };
                                    let (title, detail) = (
                                        offer.title.clone(),
                                        format!(
                                            "From {} · {:.1} MB{art_note} · {}",
                                            peer,
                                            offer.size as f64 / 1_000_000.,
                                            offer.name
                                        ),
                                    );
                                    update(&weak, move |u| {
                                        u.set_incoming_title(title.into());
                                        u.set_incoming_detail(detail.into());
                                        u.set_incoming(true);
                                        u.set_status_error(false);
                                        u.set_status("Accept or decline within 60 seconds.".into());
                                    });
                                    let until = Instant::now() + Duration::from_secs(60);
                                    while Instant::now() < until
                                        && !worker_cancel.load(Ordering::Relaxed)
                                        && !worker_closed.load(Ordering::Relaxed)
                                    {
                                        if let Ok(accepted) =
                                            decisions.recv_timeout(Duration::from_millis(100))
                                        {
                                            return accepted;
                                        }
                                    }
                                    false
                                },
                                progress(weak.clone()),
                                |path| {
                                    let (reply, rx) = mpsc::channel();
                                    controller.send(Command::Shared(path.to_owned(), reply))?;
                                    rx.recv_timeout(Duration::from_secs(10))?
                                        .map_err(|e| e.into())
                                },
                            );
                            worker_awaiting.store(false, Ordering::Release);
                            worker_running.store(false, Ordering::Release);
                            let is_err = result.is_err();
                            state(
                                &weak,
                                result
                                    .map(|p| {
                                        format!(
                                            "{} saved and added to your library.",
                                            p.file_name().unwrap_or_default().to_string_lossy()
                                        )
                                    })
                                    .unwrap_or_else(|e| e.to_string()),
                                is_err,
                                false,
                                receiving(&listener),
                            );
                        }
                    }
                }
            }
            drop(listener);
            drop(session);
        });

        let start = |action: Action,
                     tx: &mpsc::Sender<Action>,
                     running: &AtomicBool,
                     ui: &SharingState<'_>| {
            running.store(true, Ordering::Release);
            ui.set_busy(true);
            ui.set_progress(0.);
            ui.set_status_error(false);
            ui.set_status("Preparing sharing.".into());
            if tx.send(action).is_err() {
                running.store(false, Ordering::Release);
                ui.set_busy(false);
                ui.set_status_error(true);
                ui.set_status("Sharing worker unavailable. Restart Monolith.".into());
            }
        };

        {
            let tx = thumb_tx.clone();
            ui.on_request_thumbnail(move |id| {
                if id > 0 {
                    let _ = tx.send(ThumbAction::Request(id as i64));
                }
            });
        }

        {
            let weak = app.as_weak();
            let cat = ui_cat_sync.clone();
            let sel = ui_sel_sync.clone();
            let drain = ui_drain_sync.clone();
            let t_tx = thumb_tx.clone();
            ui.on_filter_songs(move || {
                drain();
                if let Some(app) = weak.upgrade() {
                    let ui = app.global::<SharingState>();
                    apply_song_filter(&ui, &cat.borrow(), &mut sel.borrow_mut(), &t_tx);
                }
            });
        }

        {
            let weak = app.as_weak();
            let cat = catalog.clone();
            let sel = selected_ids.clone();
            let drain = drain_rc.clone();
            let t_tx = thumb_tx.clone();
            ui.on_select_song(move |index| {
                drain();
                if let Some(app) = weak.upgrade() {
                    let entries = cat.borrow();
                    if let Some(entry) = usize::try_from(index).ok().and_then(|i| entries.get(i)) {
                        let ui = app.global::<SharingState>();
                        let mut set = sel.borrow_mut();
                        if !set.remove(&entry.id) {
                            if set.len() >= MAX_SHARE_BATCH {
                                ui.set_status_error(false);
                                ui.set_status("Up to 100 songs can be sent per batch.".into());
                            } else {
                                set.insert(entry.id);
                            }
                        }
                        update_rows_checked_in_place(&ui, &entries, &set, &t_tx);
                    }
                }
            });
        }

        {
            let weak = app.as_weak();
            let cat = catalog.clone();
            let sel = selected_ids.clone();
            let drain = drain_rc.clone();
            let t_tx = thumb_tx.clone();
            ui.on_toggle_song(move |id| {
                drain();
                let id = i64::from(id);
                if id <= 0 {
                    return;
                }
                if let Some(app) = weak.upgrade() {
                    let ui = app.global::<SharingState>();
                    let entries = cat.borrow();
                    let mut set = sel.borrow_mut();
                    if !set.remove(&id) {
                        if set.len() >= MAX_SHARE_BATCH {
                            ui.set_status_error(false);
                            ui.set_status("Up to 100 songs can be sent per batch.".into());
                        } else {
                            set.insert(id);
                        }
                    }
                    update_rows_checked_in_place(&ui, &entries, &set, &t_tx);
                }
            });
        }

        {
            let weak = app.as_weak();
            let cat = catalog.clone();
            let sel = selected_ids.clone();
            let drain = drain_rc.clone();
            let t_tx = thumb_tx.clone();
            ui.on_toggle_filtered_songs(move || {
                drain();
                if let Some(app) = weak.upgrade() {
                    let ui = app.global::<SharingState>();
                    let entries = cat.borrow();
                    let mut set = sel.borrow_mut();
                    let rows = ui.get_song_results();
                    let visible_ids: Vec<i64> = (0..rows.row_count())
                        .filter_map(|i| rows.row_data(i).map(|r| i64::from(r.id)))
                        .collect();
                    let all_checked =
                        !visible_ids.is_empty() && visible_ids.iter().all(|id| set.contains(id));
                    if all_checked {
                        for id in visible_ids {
                            set.remove(&id);
                        }
                    } else {
                        let mut capped = false;
                        for id in visible_ids {
                            if set.len() >= MAX_SHARE_BATCH {
                                capped = true;
                                break;
                            }
                            set.insert(id);
                        }
                        if capped {
                            ui.set_status_error(false);
                            ui.set_status("Selected first 100 songs (batch limit).".into());
                        }
                    }
                    update_rows_checked_in_place(&ui, &entries, &set, &t_tx);
                }
            });
        }

        {
            let weak = app.as_weak();
            let cat = catalog.clone();
            let sel = selected_ids.clone();
            let drain = drain_rc.clone();
            let t_tx = thumb_tx.clone();
            ui.on_clear_selected_songs(move || {
                drain();
                if let Some(app) = weak.upgrade() {
                    let ui = app.global::<SharingState>();
                    let entries = cat.borrow();
                    let mut set = sel.borrow_mut();
                    set.clear();
                    update_rows_checked_in_place(&ui, &entries, &set, &t_tx);
                }
            });
        }

        {
            let (weak, sender, active, is_closed, sel) = (
                app.as_weak(),
                tx.clone(),
                running.clone(),
                closed.clone(),
                selected_ids.clone(),
            );
            ui.on_open(move |use_selection| {
                if let Some(app) = weak.upgrade() {
                    let ui = app.global::<SharingState>();
                    let ids: BTreeSet<i64> = if use_selection {
                        app.get_selected_ids()
                            .iter()
                            .map(i64::from)
                            .take(MAX_SHARE_BATCH)
                            .collect()
                    } else {
                        BTreeSet::new()
                    };
                    let count = ids.len() as i32;
                    *sel.borrow_mut() = ids;
                    ui.set_selected_count(count);
                    ui.set_batch(count > 1);
                    ui.set_destination(app.get_download_folder());
                    ui.set_method(0);
                    ui.set_song_query("".into());
                    ui.set_incoming(false);
                    ui.set_listening(false);
                    ui.set_receiving_name("".into());
                    ui.set_receiving_port("".into());
                    ui.set_status_error(false);
                    is_closed.store(false, Ordering::Release);
                    start(refresh_action(&app), &sender, &active, &ui);
                }
            });
        }

        {
            let (weak, sender, active) = (app.as_weak(), tx.clone(), running.clone());
            ui.on_refresh(move || {
                if let Some(app) = weak.upgrade() {
                    let ui = app.global::<SharingState>();
                    if !active.load(Ordering::Acquire) {
                        start(refresh_action(&app), &sender, &active, &ui);
                    }
                }
            });
        }

        {
            let (weak, sender, active) = (app.as_weak(), tx.clone(), running.clone());
            ui.on_choose_method(move |method| {
                if let Some(app) = weak.upgrade() {
                    let ui = app.global::<SharingState>();
                    if !active.load(Ordering::Acquire) {
                        ui.set_method(method);
                        start(refresh_action(&app), &sender, &active, &ui);
                    }
                }
            });
        }

        {
            let (nearby, sender, active, sel, cat, drain) = (
                app.as_weak(),
                tx.clone(),
                running.clone(),
                selected_ids.clone(),
                catalog.clone(),
                drain_rc.clone(),
            );
            ui.on_send_song(move |_song, peer| {
                drain();
                if let Some(app) = nearby.upgrade() {
                    let ui = app.global::<SharingState>();
                    if peer >= 0 && !active.load(Ordering::Acquire) {
                        let set = sel.borrow();
                        if set.is_empty() {
                            return;
                        }
                        let entries = cat.borrow();
                        let mut ids: Vec<i64> = entries
                            .iter()
                            .filter(|e| set.contains(&e.id))
                            .map(|e| e.id)
                            .collect();
                        if ids.is_empty() {
                            ids.extend(set.iter().copied());
                        }
                        let Some(peer_id) = ui.get_device_ids().row_data(peer as usize) else {
                            return;
                        };
                        start(
                            Action::Send(ids, peer_id.to_string()),
                            &sender,
                            &active,
                            &ui,
                        );
                    }
                }
            });
        }

        {
            let (nearby, sender, active) = (app.as_weak(), tx.clone(), running.clone());
            ui.on_receive_song(move || {
                if let Some(app) = nearby.upgrade() {
                    let ui = app.global::<SharingState>();
                    if !active.load(Ordering::Acquire) {
                        start(Action::Receive, &sender, &active, &ui);
                    }
                }
            });
        }

        {
            let (flag, nearby, sender, waiting) =
                (cancel.clone(), app.as_weak(), tx.clone(), awaiting.clone());
            ui.on_cancel(move || {
                flag.store(true, Ordering::Relaxed);
                waiting.store(false, Ordering::Release);
                let _ = sender.send(Action::Stop);
                if let Some(app) = nearby.upgrade() {
                    let ui = app.global::<SharingState>();
                    ui.set_incoming(false);
                    ui.set_status_error(false);
                    ui.set_status("Cancelling sharing…".into());
                }
            });
        }

        {
            let waiting = awaiting.clone();
            let nearby = app.as_weak();
            ui.on_decide(move |yes| {
                if waiting.swap(false, Ordering::AcqRel) {
                    let _ = decision_tx.send(yes);
                    if let Some(app) = nearby.upgrade() {
                        let ui = app.global::<SharingState>();
                        ui.set_incoming(false);
                        ui.set_status_error(false);
                        ui.set_status(
                            if yes {
                                "Receiving song…"
                            } else {
                                "Declining song…"
                            }
                            .into(),
                        );
                    }
                }
            });
        }

        {
            let (flag, is_closed, sender) = (cancel.clone(), closed.clone(), tx.clone());
            ui.on_close(move || {
                if !is_closed.swap(true, Ordering::AcqRel) {
                    flag.store(true, Ordering::Relaxed);
                    let _ = sender.send(Action::Close);
                }
            });
        }

        Self {
            tx,
            thumb_tx,
            cancel,
            closed,
            worker: Some(worker),
            thumb_worker: Some(thumb_worker),
        }
    }
}

impl Drop for Sharing {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Action::Shutdown);
        let _ = self.thumb_tx.send(ThumbAction::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(thumb_worker) = self.thumb_worker.take() {
            let _ = thumb_worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_share_songs_alphabetically_and_deduplicates_detail() {
        let mut tracks = vec![
            Track {
                id: 1,
                path: "z.mp3".into(),
                title: "zebra".into(),
                artist: "B".into(),
                album: "X".into(),
                seconds: 10.,
                favorite: false,
            },
            Track {
                id: 2,
                path: "a2.mp3".into(),
                title: "  Alpha".into(),
                artist: "Zed".into(),
                album: "X".into(),
                seconds: 10.,
                favorite: false,
            },
            Track {
                id: 3,
                path: "a1.mp3".into(),
                title: "alpha".into(),
                artist: "amy".into(),
                album: "X".into(),
                seconds: 10.,
                favorite: false,
            },
        ];
        sort_tracks_alphabetical(&mut tracks);
        let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![3, 2, 1]);
        assert_eq!(format_track_detail("T-Series", "T-Series"), "T-Series");
        assert_eq!(format_track_detail("Sidhu", "Singles"), "Sidhu");
        assert_eq!(format_track_detail("Artist", "Album"), "Artist · Album");
    }
}
