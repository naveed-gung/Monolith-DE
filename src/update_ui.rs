//! A single worker owns update state. Cancel and shutdown never block playback.
use crate::AppWindow;
use monolith_desktop::{library::Result, updates};
use slint::ComponentHandle;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

enum Action {
    Check,
    Download,
    Install,
    Shutdown,
}
pub struct Updater {
    tx: mpsc::Sender<Action>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Updater {
    pub fn bind(ui: &AppWindow, directory: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(false));
        let weak = ui.as_weak();
        let worker_cancel = cancelled.clone();
        let worker_running = running.clone();
        let worker = std::thread::spawn(move || {
            let mut release = None;
            let mut package = None;
            while let Ok(action) = rx.recv() {
                let result: Result<String> = match action {
                    Action::Shutdown => break,
                    Action::Check => {
                        package = None;
                        release = None;
                        updates::check(&worker_cancel).map(|found| {
                            let status = found.as_ref().map(|r|
                                format!("Version {} available · {:.1} MB. Download when you're ready.", r.version, r.size as f64 / 1_000_000.))
                                .unwrap_or_else(|| "You're using the latest published stable version.".into());
                            release = found;
                            status
                        })
                    }
                    Action::Download => match release.as_ref() {
                        Some(r) => updates::download(r, &directory, &worker_cancel).map(|path| {
                            package = Some(path);
                            "Installer verified. Open it when you're ready; your library stays intact.".into()
                        }),
                        None => Err("Check for an update first.".into()),
                    },
                    Action::Install => match (package.as_ref(), release.as_ref()) {
                        (Some(path), Some(r)) => updates::open_installer(path, r, &worker_cancel).map(|()| {
                            #[cfg(windows)] {
                                let _ = weak.upgrade_in_event_loop(|_| { let _ = slint::quit_event_loop(); });
                            }
                            "Installer opened. Complete installation in your system's installer.".into()
                        }),
                        _ => Err("Download and verify the installer first.".into()),
                    },
                };
                let available = release.is_some();
                let ready = package.is_some();
                let text = result.unwrap_or_else(|e| e.to_string());
                worker_running.store(false, Ordering::Release);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_update_status(text.into());
                    ui.set_update_available(available);
                    ui.set_update_ready(ready);
                    ui.set_update_busy(false);
                });
            }
        });
        for (index, text) in [
            "Checking GitHub…",
            "Downloading installer…",
            "Verifying installer…",
        ]
        .into_iter()
        .enumerate()
        {
            let (sender, cancel, running, weak) =
                (tx.clone(), cancelled.clone(), running.clone(), ui.as_weak());
            let start = move || {
                if running.swap(true, Ordering::AcqRel) {
                    return;
                }
                cancel.store(false, Ordering::Relaxed);
                if let Some(ui) = weak.upgrade() {
                    ui.set_update_busy(true);
                    ui.set_update_status(text.into());
                }
                let action = match index {
                    0 => Action::Check,
                    1 => Action::Download,
                    _ => Action::Install,
                };
                let _ = sender.send(action);
            };
            match index {
                0 => ui.on_check_update(start),
                1 => ui.on_download_update(start),
                _ => ui.on_install_update(start),
            }
        }
        let (cancel, weak) = (cancelled.clone(), ui.as_weak());
        ui.on_cancel_update(move || {
            cancel.store(true, Ordering::Relaxed);
            if let Some(ui) = weak.upgrade() {
                ui.set_update_status("Cancelling update…".into());
            }
        });
        Self {
            tx,
            cancelled,
            worker: Some(worker),
        }
    }
}

impl Drop for Updater {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Action::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
