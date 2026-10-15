//! One foreground sharing worker. Transport and disk work stay off Slint's thread.
use crate::{AppWindow, NearbyWindow, controller::Command};
use monolith_desktop::{
    bluetooth, lan,
    library::{Library, Result, Track},
    nearby,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    path::PathBuf,
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
fn show_peers(ui: &slint::Weak<NearbyWindow>, peers: &[Peer]) {
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
enum Server {
    Bluetooth(bluetooth::Listener),
    App(lan::Listener),
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
fn refresh_action(app: &AppWindow, ui: &NearbyWindow) -> Action {
    Action::Refresh(
        app.get_current_id() as i64,
        PathBuf::from(app.get_download_folder().as_str()),
        if app.get_selecting() {
            app.get_selected_ids().iter().map(|id| id as i64).collect()
        } else {
            vec![]
        },
        ui.get_method(),
    )
}
pub struct Sharing {
    window: NearbyWindow,
    tx: mpsc::Sender<Action>,
    cancel: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
fn update(ui: &slint::Weak<NearbyWindow>, f: impl FnOnce(NearbyWindow) + Send + 'static) {
    let _ = ui.upgrade_in_event_loop(f);
}
fn state(ui: &slint::Weak<NearbyWindow>, text: String, busy: bool, listening: bool) {
    update(ui, move |u| {
        u.set_status(text.into());
        u.set_busy(busy);
        u.set_listening(listening);
        u.set_incoming(false);
    });
}
fn progress(ui: slint::Weak<NearbyWindow>) -> impl FnMut(u64, u64) {
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
    pub fn bind(app: &AppWindow, data: PathBuf, controller: mpsc::Sender<Command>) -> Result<Self> {
        let window = NearbyWindow::new()?;
        let (tx, rx) = mpsc::channel();
        let (decision_tx, decisions) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(true));
        let awaiting = Arc::new(AtomicBool::new(false));
        let (weak, worker_cancel, worker_running, worker_closed, worker_awaiting) = (
            window.as_weak(),
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
            let mut destination = PathBuf::new();
            let mut listening_until = Instant::now();
            let mut next_discovery = Instant::now();
            loop {
                if worker_cancel.load(Ordering::Relaxed)
                    || worker_closed.load(Ordering::Relaxed)
                    || Instant::now() > listening_until
                {
                    if listener.take().is_some() {
                        state(
                            &weak,
                            "Receiving stopped. Choose Receive to start another session.".into(),
                            false,
                            false,
                        );
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
                    if matches!(action, Action::Stop) {
                        listener = None;
                        state(&weak, "Sharing stopped.".into(), false, false);
                        continue;
                    }
                    if worker_closed.load(Ordering::Relaxed) {
                        worker_running.store(false, Ordering::Release);
                        continue;
                    }
                    worker_cancel.store(false, Ordering::Relaxed);
                    let result: Result<String> = (|| match action {
                        Action::Refresh(current, folder, selected, transport) => {
                            listener = None;
                            session = None;
                            peers.clear();
                            method = transport;
                            batch = selected.len() > 1;
                            destination = folder;
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
                                u.set_receiving_name("".into());
                                u.set_songs(ModelRc::new(VecModel::from(labels)));
                                u.set_song_index(selected);
                                u.set_devices(ModelRc::default());
                                u.set_device_ids(ModelRc::default());
                                u.set_device_index(-1);
                                u.set_available(false);
                            });
                            if method == 0 {
                                return Ok("Choose Monolith app or Bluetooth to continue.".into());
                            }
                            if method == 1 {
                                peers = lan::discover(&worker_cancel)?
                                    .into_iter()
                                    .map(Peer::App)
                                    .collect();
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
                            Ok(if method == 1 { "Open Receive on the other Monolith device, then choose it here. The recipient approves each song." } else { "Choose a paired device running Monolith, or Receive a song." }.into())
                        }
                        Action::Receive => {
                            listener = Some(if method == 1 {
                                let server = lan::Listener::bind(worker_cancel.clone())?;
                                let name = server.name.clone();
                                update(&weak, move |u| u.set_receiving_name(name.into()));
                                Server::App(server)
                            } else {
                                Server::Bluetooth(
                                    session
                                        .as_ref()
                                        .ok_or("Refresh devices first.")?
                                        .listen(worker_cancel.clone())?,
                                )
                            });
                            listening_until = Instant::now() + Duration::from_secs(300);
                            Ok("Receiving for 5 minutes. Each song needs your approval; Close or Cancel stops receiving.".into())
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
                                    if worker_cancel.load(Ordering::Relaxed) {
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
                                    nearby::send(
                                        &mut stream,
                                        std::path::Path::new(&song.path),
                                        &offer,
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
                        listener.is_some(),
                    );
                }
                // Keep the picker current without disabling buttons or changing song choices.
                // Stable endpoint IDs prevent a disappearing/reordered device from rerouting Send.
                else if method == 1
                    && listener.is_none()
                    && !worker_cancel.load(Ordering::Relaxed)
                    && !worker_closed.load(Ordering::Relaxed)
                    && Instant::now() >= next_discovery
                {
                    if let Ok(found) = lan::discover(&worker_cancel) {
                        peers = found.into_iter().map(Peer::App).collect();
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
                                listener.is_some(),
                            );
                        }
                    }
                }
            }
            drop(listener);
            drop(session);
        });
        let start =
            |action: Action, tx: &mpsc::Sender<Action>, running: &AtomicBool, ui: &NearbyWindow| {
                if running.swap(true, Ordering::AcqRel) {
                    return;
                }
                ui.set_busy(true);
                ui.set_progress(0.);
                ui.set_status("Preparing sharing…".into());
                if tx.send(action).is_err() {
                    running.store(false, Ordering::Release);
                    ui.set_busy(false);
                    ui.set_status("Sharing worker unavailable. Restart Monolith.".into());
                }
            };
        let (nearby, app_weak, sender, active, is_closed) = (
            window.clone_strong(),
            app.as_weak(),
            tx.clone(),
            running.clone(),
            closed.clone(),
        );
        app.on_open_sharing(move || {
            if let Some(app) = app_weak.upgrade() {
                is_closed.store(false, Ordering::Release);
                nearby.set_light(if app.get_theme_follow_system() {
                    app.get_system_light()
                } else {
                    app.get_light()
                });
                nearby.set_reduce_motion(app.get_reduce_motion());
                nearby.set_destination(app.get_download_folder());
                nearby.set_method(0);
                let _ = nearby.show();
                start(refresh_action(&app, &nearby), &sender, &active, &nearby);
            }
        });
        let (nearby, app_weak, sender, active) =
            (window.as_weak(), app.as_weak(), tx.clone(), running.clone());
        window.on_refresh(move || {
            if let (Some(ui), Some(app)) = (nearby.upgrade(), app_weak.upgrade()) {
                start(refresh_action(&app, &ui), &sender, &active, &ui);
            }
        });
        let (nearby, app_weak, sender, active) =
            (window.as_weak(), app.as_weak(), tx.clone(), running.clone());
        window.on_choose_method(move |method| {
            if let (Some(ui), Some(app)) = (nearby.upgrade(), app_weak.upgrade()) {
                ui.set_method(method);
                start(refresh_action(&app, &ui), &sender, &active, &ui);
            }
        });
        let (nearby, sender, active) = (window.as_weak(), tx.clone(), running.clone());
        window.on_send_song(move |song, peer| {
            if let Some(ui) = nearby.upgrade() {
                if song >= 0 && peer >= 0 {
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
        let (nearby, sender, active) = (window.as_weak(), tx.clone(), running.clone());
        window.on_receive_song(move || {
            if let Some(ui) = nearby.upgrade() {
                start(Action::Receive, &sender, &active, &ui);
            }
        });
        let (flag, nearby, sender, waiting) = (
            cancel.clone(),
            window.as_weak(),
            tx.clone(),
            awaiting.clone(),
        );
        window.on_cancel(move || {
            flag.store(true, Ordering::Relaxed);
            waiting.store(false, Ordering::Release);
            let _ = sender.send(Action::Stop);
            if let Some(ui) = nearby.upgrade() {
                ui.set_incoming(false);
                ui.set_status("Cancelling sharing…".into());
            }
        });
        let waiting = awaiting.clone();
        let nearby = window.as_weak();
        window.on_decide(move |yes| {
            if waiting.swap(false, Ordering::AcqRel) {
                let _ = decision_tx.send(yes);
                if let Some(ui) = nearby.upgrade() {
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
        window.window().on_close_requested(move || {
            is_closed.store(true, Ordering::Relaxed);
            flag.store(true, Ordering::Relaxed);
            let _ = sender.send(Action::Stop);
            slint::CloseRequestResponse::HideWindow
        });
        Ok(Self {
            window,
            tx,
            cancel,
            closed,
            worker: Some(worker),
        })
    }
}
impl Drop for Sharing {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.window.hide();
        let _ = self.tx.send(Action::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
