//! Offline integration check: generated silence, isolated SQLite, real audio device.
#![allow(dead_code)]
#[path = "../src/controller.rs"]
mod controller;
use controller::Command;
use monolith_desktop::library::{Library, Result};
use slint::{ComponentHandle, Model};
use std::{
    cell::Cell,
    io::Write,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool, mpsc},
    time::{Duration, Instant},
};
slint::include_modules!();
fn main() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("silence.wav");
    let data_size = 48000u32 * 2 * 8;
    let mut file = std::fs::File::create(&path)?;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_size).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&48000u32.to_le_bytes())?;
    file.write_all(&96000u32.to_le_bytes())?;
    file.write_all(&2u16.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_size.to_le_bytes())?;
    file.write_all(&vec![0; data_size as usize])?;
    drop(file);
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()?;
    let app = AppWindow::new()?;
    let db_path = dir.path().join("library.sqlite3");
    let db = Library::open(&db_path)?;
    let (tx, rx) = mpsc::channel();
    let worker = controller::spawn(
        db,
        app.as_weak(),
        rx,
        tx.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    tx.send(Command::Import(vec![path.clone(), path.clone()]))?;
    app.show()?;
    let phase = Rc::new(Cell::new(0));
    let tick = phase.clone();
    let weak = app.as_weak();
    let sender = tx.clone();
    let started = Instant::now();
    let capture = std::env::args().nth(1);
    let second = dir.path().join("second.wav");
    std::fs::copy(&path, &second)?;
    let selected = Rc::new(Cell::new(-1));
    let selected = selected.clone();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(250),
        move || {
            let ui = weak.upgrade().unwrap();
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "Workflow timed out at phase {}: {}",
                tick.get(),
                ui.get_status()
            );
            match tick.get() {
                0 if ui.get_total_tracks() == 1 => {
                    assert_eq!(ui.get_tracks().row_count(), 1);
                    let id = ui.get_tracks().row_data(0).unwrap().id;
                    sender.send(Command::Play(id as i64)).unwrap();
                    tick.set(1);
                }
                1 if ui.get_playing() => {
                    sender.send(Command::Seek(2.)).unwrap();
                    sender.send(Command::Volume(35.)).unwrap();
                    sender.send(Command::Toggle).unwrap();
                    tick.set(2);
                }
                2 if !ui.get_playing() => {
                    assert!(ui.get_position() >= 1.9);
                    let id = ui.get_current_id();
                    sender.send(Command::Favorite(id as i64)).unwrap();
                    sender.send(Command::NewPlaylist("Evening".into())).unwrap();
                    tick.set(3);
                }
                3 if ui.get_playlists().row_count() == 3 => {
                    let id = ui.get_current_id();
                    sender.send(Command::AddToPlaylist(id as i64, 2)).unwrap();
                    sender.send(Command::Filter(2, "silence".into())).unwrap();
                    tick.set(4);
                }
                4 if ui.get_playlist_index() == 2 && ui.get_tracks().row_count() == 1 => {
                    assert!(ui.get_tracks().row_data(0).unwrap().favorite);
                    if let Some(path) = &capture {
                        let pixels = ui.window().take_snapshot().unwrap();
                        let mut file =
                            std::io::BufWriter::new(std::fs::File::create(path).unwrap());
                        writeln!(file, "P6\n{} {}\n255", pixels.width(), pixels.height()).unwrap();
                        for pixel in pixels.as_bytes().chunks_exact(4) {
                            file.write_all(&pixel[..3]).unwrap();
                        }
                    }
                    sender.send(Command::Toggle).unwrap();
                    tick.set(5);
                }
                5 if ui.get_playing() => {
                    sender.send(Command::Stop).unwrap();
                    sender
                        .send(Command::Filter(0, "does-not-exist".into()))
                        .unwrap();
                    tick.set(6);
                }
                6 if !ui.get_playing() && ui.get_tracks().row_count() == 0 => {
                    assert_eq!(ui.get_position(), 0.);
                    sender.send(Command::Import(vec![second.clone()])).unwrap();
                    sender.send(Command::Filter(0, String::new())).unwrap();
                    tick.set(7);
                }
                7 if ui.get_total_tracks() == 2 && ui.get_tracks().row_count() == 2 => {
                    let id = ui.get_tracks().row_data(0).unwrap().id;
                    selected.set(id);
                    sender.send(Command::Play(id as i64)).unwrap();
                    tick.set(8);
                }
                8 if ui.get_playing() && ui.get_current_id() == selected.get() => {
                    sender.send(Command::Next(1)).unwrap();
                    tick.set(9);
                }
                9 if ui.get_playing() && ui.get_current_id() != selected.get() => {
                    selected.set(ui.get_current_id());
                    sender.send(Command::Repeat(2)).unwrap();
                    sender.send(Command::Seek(7.8)).unwrap();
                    tick.set(10);
                }
                10 if ui.get_playing()
                    && ui.get_current_id() == selected.get()
                    && ui.get_position() < 1. =>
                {
                    sender.send(Command::Repeat(0)).unwrap();
                    sender.send(Command::Seek(7.8)).unwrap();
                    tick.set(11);
                }
                11 if !ui.get_playing() => {
                    slint::quit_event_loop().unwrap();
                    tick.set(12);
                }
                _ => {}
            }
        },
    );
    slint::run_event_loop()?;
    timer.stop();
    tx.send(Command::Shutdown)?;
    worker.join().expect("worker panic");
    let db = Library::open(&db_path)?;
    assert_eq!(db.tracks(0)?.len(), 2);
    assert_eq!(db.tracks(-1)?.len(), 1);
    assert_eq!(db.tracks(db.playlists()?[0].0)?.len(), 1);
    assert_eq!(db.setting("volume", "0"), "0.35");
    // All playback file handles must have been released at stop/shutdown.
    std::fs::rename(&path, dir.path().join("released.wav"))?;
    println!(
        "PASS: duplicate import, metadata, play/pause/resume/seek/stop, volume, favorite, playlist, search, next/end/repeat, persistence, file release."
    );
    Ok(())
}
