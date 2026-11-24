//! One foreground sharing worker. Transport and disk work stay off Slint's thread.
use crate::{AppWindow, ShareSong, SharingState, controller::Command};
use monolith_desktop::{
    bluetooth, lan,
    library::{Library, Result, Track},
    nearby,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    cell::RefCell,
    collections::HashMap,
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

enum Action {
    Refresh(i64, PathBuf, Vec<i64>, i32),
    Send(usize, String),
    Receive,
    Stop,
    Close,
    Shutdown,
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
}
fn show_peers(ui: &slint::Weak<AppWindow>, peers: &[Peer]) {
    let labels: Vec<slint::SharedString> = peers
        .iter()
        .map(|p| match p {
            Peer::Bluetooth(p) => p.name.clone().into(),
            Peer::App(p) => p.name.clone().into(),
        })
        .collect();
    let ids: Vec<slint::SharedString> = peers.iter().map(|p| p.id().into()).collect();
    update(ui, move |u| {
        let previous = u.get_device_ids().row_data(u.get_device_index() as usize);
        let selected = previous
            .and_then(|id| ids.iter().position(|candidate| *candidate == id))
            .map(|i| i as i32)
            .unwrap_or(-1);
        u.set_device_ids(ModelRc::new(VecModel::from(ids)));
        u.set_devices(ModelRc::new(VecModel::from(labels)));
        u.set_device_index(selected);
    });
}
/// A device that stops answering is dropped after this long, so one lost
/// discovery round does not make it flicker out of the picker.
const PEER_TTL: Duration = Duration::from_secs(15);
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
            update(ui, move |u| u.set_receiving_name(name.into()));
            *listener = Some(Server::App(server));
            None
        }
        Err(e) => {
            update(ui, |u| u.set_receiving_name("".into()));
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
fn filter_songs(ui: &SharingState<'_>) {
    let query = ui.get_song_query().trim().to_lowercase();
    let songs = ui.get_songs();
    let results = songs
        .iter()
        .enumerate()
        .filter(|(_, label)| query.is_empty() || label.to_lowercase().contains(&query))
        .map(|(index, label)| ShareSong {
            label,
            index: index as i32,
        })
        .collect::<Vec<_>>();
    let selected = ui.get_song_index();
    if !results.iter().any(|song| song.index == selected) {
        ui.set_song_index(-1);
    }
    ui.set_song_results(ModelRc::new(VecModel::from(results)));
}

fn refresh_action(app: &AppWindow, selected: Vec<i64>) -> Action {
    Action::Refresh(
        app.get_current_id() as i64,
        PathBuf::from(app.get_download_folder().as_str()),
        selected,
        app.global::<SharingState>().get_method(),
    )
}
pub struct Sharing {
    tx: mpsc::Sender<Action>,
    cancel: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
fn update(ui: &slint::Weak<AppWindow>, f: impl FnOnce(SharingState<'_>) + Send + 'static) {
    let _ = ui.upgrade_in_event_loop(move |app| f(app.global::<SharingState>()));
}
fn state(ui: &slint::Weak<AppWindow>, text: String, busy: bool, listening: bool) {
    update(ui, move |u| {
        u.set_status(text.into());
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
        let (tx, rx) = mpsc::channel();
        let (decision_tx, decisions) = mpsc::channel();
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
        let worker = std::thread::spawn(move || {
            let mut session = None;
            let mut listener: Option<Server> = None;
            let mut method = 0;
            let mut batch = false;
            let mut songs: Vec<Track> = Vec::new();
            let mut peers: Vec<Peer> = Vec::new();
            let mut last_seen: HashMap<String, Instant> = HashMap::new();
            let mut destination = PathBuf::new();
            // Only explicit Bluetooth receiving expires; Wi-Fi lasts while the page is active.
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
                        ),
                        Some(Server::App(_)) => update(&weak, |u| u.set_receiving_name("".into())),
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
                            // The cancelled work has finished (single worker), so let
                            // Wi-Fi mode become visible again on the next idle tick.
                            worker_cancel.store(false, Ordering::Relaxed);
                            next_discovery = Instant::now();
                        }
                        state(&weak, "Sharing cancelled.".into(), false, false);
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
                        Action::Refresh(current, folder, selected, transport) => {
                            // Keep an existing Wi-Fi listener so our address stays stable.
                            if !(transport == 1 && matches!(listener, Some(Server::App(_)))) {
                                listener = None;
                                listening_until = None;
                            }
                            session = None;
                            peers.clear();
                            last_seen.clear();
                            method = transport;
                            batch = selected.len() > 1;
                            destination = folder;
                            update(&weak, |u| {
                                u.set_devices(ModelRc::default());
                                u.set_device_ids(ModelRc::default());
                                u.set_device_index(-1);
                                u.set_available(false);
                            });
                            if method != 1 {
                                update(&weak, |u| u.set_receiving_name("".into()));
                            }
                            if method == 0 {
                                songs.clear();
                                return Ok(String::new());
                            }
                            songs = Library::open(&data.join("library.sqlite3"))?.tracks(0)?;
                            if !selected.is_empty() {
                                if selected.len() > 100 {
                                    return Err("Share up to 100 songs per batch. Each song can be up to 100 MB.".into());
                                }
                                songs.retain(|s| selected.contains(&s.id));
                                if songs.len() != selected.len() {
                                    return Err(
                                        "A selected song no longer exists. Refresh your selection."
                                            .into(),
                                    );
                                }
                            }
                            let batch_value = batch;
                            let labels: Vec<slint::SharedString> = songs
                                .iter()
                                .map(|s| format!("{} · {}", s.title, s.artist).into())
                                .collect();
                            let selected = songs
                                .iter()
                                .position(|s| s.id == current)
                                .or_else(|| (!songs.is_empty()).then_some(0))
                                .map(|i| i as i32)
                                .unwrap_or(-1);
                            update(&weak, move |u| {
                                u.set_batch(batch_value);
                                u.set_songs(ModelRc::new(VecModel::from(labels)));
                                u.set_song_index(selected);
                                filter_songs(&u);
                            });
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
                                    "Looking for Monolith on this Wi-Fi… Open Share on the other device; it appears here automatically."
                                }
                                (1, false) => {
                                    "Choose a device and send. The recipient approves each song."
                                }
                                _ => "Choose a paired device running Monolith, or Receive a song.",
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
                        Action::Send(song_index, peer_id) => {
                            let peer = peers.iter().find(|p| p.id() == peer_id).ok_or(
                                "That device is no longer available. Choose a nearby device again.",
                            )?;
                            let selected: Vec<&Track> = if batch {
                                songs.iter().collect()
                            } else {
                                vec![songs.get(song_index).ok_or("Choose a song.")?]
                            };
                            // Preflight every file before sending the first song.
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
                                // Read cover art per song (not in preflight) to cap memory
                                // at one cover. Missing art never blocks the song.
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
                            Ok(format!(
                                "{saved} songs received and added to the recipient's library."
                            ))
                        }
                        _ => unreachable!(),
                    })();
                    worker_running.store(false, Ordering::Release);
                    state(
                        &weak,
                        result.unwrap_or_else(|e| e.to_string()),
                        false,
                        receiving(&listener),
                    );
                }
                // Keep the picker current without disabling buttons or changing song choices.
                // Stable endpoint IDs prevent a disappearing/reordered device from rerouting Send.
                else if method == 1
                    && !worker_cancel.load(Ordering::Relaxed)
                    && !worker_closed.load(Ordering::Relaxed)
                    && Instant::now() >= next_discovery
                {
                    // Re-arm after a cancel or failed bind; throttled to the scan interval.
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
                            state(&weak, e.to_string(), false, false);
                        }
                        Ok(Some((mut stream, peer))) => {
                            worker_running.store(true, Ordering::Release);
                            state(
                                &weak,
                                "Checking incoming Monolith offer…".into(),
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
                                    let (title, detail) = (
                                        offer.title.clone(),
                                        format!(
                                            "From {} · {:.1} MB · {}",
                                            peer,
                                            offer.size as f64 / 1_000_000.,
                                            offer.name
                                        ),
                                    );
                                    update(&weak, move |u| {
                                        u.set_incoming_title(title.into());
                                        u.set_incoming_detail(detail.into());
                                        u.set_incoming(true);
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
                            // Keep the explicit five-minute session for the next individually approved song.
                            worker_running.store(false, Ordering::Release);
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
            ui.set_status("Preparing sharing.".into());
            if tx.send(action).is_err() {
                running.store(false, Ordering::Release);
                ui.set_busy(false);
                ui.set_status("Sharing worker unavailable. Restart Monolith.".into());
            }
        };
        let selection = Rc::new(RefCell::new(Vec::new()));
        let (weak, sender, active, is_closed, selected) = (
            app.as_weak(),
            tx.clone(),
            running.clone(),
            closed.clone(),
            selection.clone(),
        );
        let filter_weak = app.as_weak();
        ui.on_filter_songs(move || {
            if let Some(app) = filter_weak.upgrade() {
                filter_songs(&app.global::<SharingState>());
            }
        });
        ui.on_open(move |use_selection| {
            if let Some(app) = weak.upgrade() {
                let ui = app.global::<SharingState>();
                let ids = if use_selection {
                    app.get_selected_ids().iter().map(i64::from).collect()
                } else {
                    Vec::new()
                };
                selected.replace(ids);
                ui.set_destination(app.get_download_folder());
                ui.set_method(0);
                ui.set_song_query("".into());
                ui.set_incoming(false);
                ui.set_listening(false);
                ui.set_receiving_name("".into());
                is_closed.store(false, Ordering::Release);
                start(
                    refresh_action(&app, selected.borrow().clone()),
                    &sender,
                    &active,
                    &ui,
                );
            }
        });
        let (weak, sender, active, selected) = (
            app.as_weak(),
            tx.clone(),
            running.clone(),
            selection.clone(),
        );
        ui.on_refresh(move || {
            if let Some(app) = weak.upgrade() {
                let ui = app.global::<SharingState>();
                if !active.load(Ordering::Acquire) {
                    start(
                        refresh_action(&app, selected.borrow().clone()),
                        &sender,
                        &active,
                        &ui,
                    );
                }
            }
        });
        let (weak, sender, active) = (app.as_weak(), tx.clone(), running.clone());
        ui.on_choose_method(move |method| {
            if let Some(app) = weak.upgrade() {
                let ui = app.global::<SharingState>();
                if !active.load(Ordering::Acquire) {
                    ui.set_method(method);
                    start(
                        refresh_action(&app, selection.borrow().clone()),
                        &sender,
                        &active,
                        &ui,
                    );
                }
            }
        });
        let (nearby, sender, active) = (app.as_weak(), tx.clone(), running.clone());
        ui.on_send_song(move |song, peer| {
            if let Some(app) = nearby.upgrade() {
                let ui = app.global::<SharingState>();
                if song >= 0 && peer >= 0 && !active.load(Ordering::Acquire) {
                    let Some(peer_id) = ui.get_device_ids().row_data(peer as usize) else {
                        return;
                    };
                    start(
                        Action::Send(song as usize, peer_id.to_string()),
                        &sender,
                        &active,
                        &ui,
                    );
                }
            }
        });
        let (nearby, sender, active) = (app.as_weak(), tx.clone(), running.clone());
        ui.on_receive_song(move || {
            if let Some(app) = nearby.upgrade() {
                let ui = app.global::<SharingState>();
                if !active.load(Ordering::Acquire) {
                    start(Action::Receive, &sender, &active, &ui);
                }
            }
        });
        let (flag, nearby, sender, waiting) =
            (cancel.clone(), app.as_weak(), tx.clone(), awaiting.clone());
        ui.on_cancel(move || {
            flag.store(true, Ordering::Relaxed);
            waiting.store(false, Ordering::Release);
            let _ = sender.send(Action::Stop);
            if let Some(app) = nearby.upgrade() {
                let ui = app.global::<SharingState>();
                ui.set_incoming(false);
                ui.set_status("Cancelling sharing…".into());
            }
        });
        let waiting = awaiting.clone();
        let nearby = app.as_weak();
        ui.on_decide(move |yes| {
            if waiting.swap(false, Ordering::AcqRel) {
                let _ = decision_tx.send(yes);
                if let Some(app) = nearby.upgrade() {
                    let ui = app.global::<SharingState>();
                    ui.set_incoming(false);
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
        let (flag, is_closed, sender) = (cancel.clone(), closed.clone(), tx.clone());
        ui.on_close(move || {
            if !is_closed.swap(true, Ordering::AcqRel) {
                flag.store(true, Ordering::Relaxed);
                let _ = sender.send(Action::Close);
            }
        });
        Self {
            tx,
            cancel,
            closed,
            worker: Some(worker),
        }
    }
}
impl Drop for Sharing {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Action::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
