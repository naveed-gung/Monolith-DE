//! A single worker owns helper install state. Downloads stay off the UI
//! thread, cancel never blocks playback, and the saved tool path goes back
//! through the normal Save command so Settings and the library agree.
use crate::{AppWindow, controller::Command};
use monolith_desktop::{
    tools::{self, Tool},
    toolsetup,
};
use slint::ComponentHandle;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

enum Action {
    Install(usize),
    Shutdown,
}

/// Push the detection result onto the two settings properties for `tool`.
pub fn report(ui: &AppWindow, tool: Tool, found: Option<&tools::Found>) {
    let text = match found {
        Some(f) => format!("Found: {} · {}", f.path.display(), f.version),
        None => format!("{} is not installed.", tool.label()),
    };
    let managed = found.is_some_and(|f| f.managed);
    let present = found.is_some();
    match tool {
        Tool::YtDlp => {
            ui.set_ytdlp_info(text.into());
            ui.set_ytdlp_found(present);
            ui.set_ytdlp_managed(managed);
        }
        Tool::Ffmpeg => {
            ui.set_ffmpeg_info(text.into());
            ui.set_ffmpeg_found(present);
            ui.set_ffmpeg_managed(managed);
        }
    }
}

pub struct Tools {
    tx: mpsc::Sender<Action>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Tools {
    pub fn bind(ui: &AppWindow, commands: mpsc::Sender<Command>) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(false));
        let weak = ui.as_weak();
        let worker_cancel = cancelled.clone();
        let worker_running = running.clone();
        let worker = std::thread::spawn(move || {
            while let Ok(action) = rx.recv() {
                let Action::Install(index) = action else {
                    break;
                };
                let Some(tool) = Tool::ALL.get(index).copied() else {
                    continue;
                };
                let weak_ui = weak.clone();
                let found = toolsetup::install(tool, &worker_cancel, move |part, text| {
                    let _ = weak_ui.upgrade_in_event_loop(move |ui| {
                        ui.set_tool_progress(part);
                        ui.set_tool_status(text.into());
                    });
                });
                worker_running.store(false, Ordering::Release);
                let commands = commands.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    match found {
                        Ok(found) => {
                            ui.set_tool_status(
                                format!("{} ready: {}", tool.label(), found.version).into(),
                            );
                            let _ = commands.send(Command::Save(
                                tool.setting_key().into(),
                                found.path.to_string_lossy().into_owned(),
                            ));
                            report(&ui, tool, Some(&found));
                        }
                        Err(error) => {
                            ui.set_tool_status(error.to_string().into());
                            let _ = commands.send(Command::RescanTools);
                        }
                    }
                    ui.set_tool_progress(0.);
                    ui.set_tool_busy(false);
                });
            }
        });
        let (start_cancel, start_running, start_weak, start_tx) =
            (cancelled.clone(), running.clone(), ui.as_weak(), tx.clone());
        ui.on_install_tool(move |index| {
            if index < 0 || start_running.swap(true, Ordering::AcqRel) {
                return;
            }
            start_cancel.store(false, Ordering::Relaxed);
            if let Some(ui) = start_weak.upgrade() {
                ui.set_tool_busy(true);
                ui.set_tool_progress(0.);
                ui.set_tool_status("Preparing download…".into());
            }
            let _ = start_tx.send(Action::Install(index as usize));
        });
        let (cancel, weak) = (cancelled.clone(), ui.as_weak());
        ui.on_cancel_tool(move || {
            cancel.store(true, Ordering::Relaxed);
            if let Some(ui) = weak.upgrade() {
                ui.set_tool_status("Cancelling…".into());
            }
        });
        Self {
            tx,
            cancelled,
            worker: Some(worker),
        }
    }
}

impl Drop for Tools {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Action::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
