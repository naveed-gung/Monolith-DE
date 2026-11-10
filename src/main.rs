// Suppresses the console on Windows release builds. The `windows` half of the
// guard is not optional: `windows_subsystem` is a Windows-only attribute and a
// Linux release build would fail to compile with it applied.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod controller;
mod crash_ui;
mod native;
mod nearby_ui;
mod tool_ui;
mod update_ui;
use controller::Command;
use monolith_desktop::{
    crash, identity,
    library::{EXTENSIONS, Library, PLAYLIST_EXTENSIONS, Result},
    transfer,
};
use slint::{ComponentHandle, Model, winit_030::WinitWindowAccessor};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};
slint::include_modules!();

fn backdrop(app: &AppWindow, enabled: bool) -> bool {
    #[cfg(windows)]
    {
        let result = app.window().with_winit_window(|window| {
            if enabled {
                window_vibrancy::apply_acrylic(window, Some((18, 18, 20, 180)))
            } else {
                window_vibrancy::clear_acrylic(window)
            }
        });
        return matches!(result, Some(Ok(())));
    }
    #[cfg(not(windows))]
    {
        let _ = (app, enabled);
        false
    }
}
fn message(ui: &slint::Weak<AppWindow>, text: String) {
    let _ = ui.upgrade_in_event_loop(move |u| u.set_status(text.into()));
}
fn run() -> Result<()> {
    let data = std::env::var_os("MONOLITH_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or(
            dirs::data_local_dir()
                .ok_or("No application data folder")?
                .join("MonolithDesktop"),
        );
    std::fs::create_dir_all(&data)?;
    let db = Library::open(&data.join("library.sqlite3"))?;
    let low_power = db.setting("low_power", "1") == "1";
    let glass = cfg!(windows) && db.setting("glass", "1") == "1";
    // Dark is the product default, so a fresh install ignores the Windows
    // setting until the user picks something in Settings.
    let theme_follow_system = db.setting("theme_follow_system", "0") == "1";
    let light = db.setting("theme_light", "0") == "1";
    // Closed by default: the window is wide enough to work without the labels,
    // and the user can open it from the toggle at the foot of the rail.
    let rail_collapsed = db.setting("rail_collapsed", "1") == "1";
    // A genuine first run drives the opening coach mark, which starts on
    // Downloads because that is where Monolith most often surprises someone.
    let first_run = db.setting("guide_seen", "0") == "0";
    let mut show_theme_hint = false;
    if first_run {
        db.save("guide_seen", "1")?;
        db.save("theme_hint_seen", "1")?;
        show_theme_hint = !theme_follow_system && !light;
    }
    let default_downloads = dirs::audio_dir()
        .or_else(dirs::download_dir)
        .unwrap_or_else(|| data.clone())
        .join("Monolith");
    let default_downloads =
        PathBuf::from(db.setting("music_folder", default_downloads.to_string_lossy().as_ref()));
    std::fs::create_dir_all(&default_downloads)?;
    db.save("music_folder", default_downloads.to_string_lossy().as_ref())?;
    let downloads = db.setting(
        "download_folder",
        default_downloads.to_string_lossy().as_ref(),
    );
    std::fs::create_dir_all(&downloads)?;
    db.save("download_folder", &downloads)?;
    // Clear staging folders left behind by a crash or power loss.
    transfer::sweep_stale_staging(
        std::path::Path::new(&downloads),
        std::time::Duration::from_secs(24 * 60 * 60),
    );
    let downloader = db.setting("downloader", "");
    let ffmpeg = db.setting("ffmpeg", "");
    let saved_tools: Vec<String> = monolith_desktop::tools::Tool::ALL
        .iter()
        .map(|tool| db.setting(tool.setting_key(), ""))
        .collect();
    // Local portrait stays outside Git/packages and survives app updates.
    // Resolve disk I/O before creating the UI/event loop.
    let developer_photo = identity::avatar_from(&data).unwrap_or_default();
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(if low_power { "software" } else { "femtovg" }.into())
        .with_winit_window_attributes_hook(|a| a.with_transparent(true))
        .select()?;
    let app = AppWindow::new()?;
    let _updater = update_ui::Updater::bind(&app, data.join("updates"));
    app.set_low_power(low_power);
    app.set_system_light(native::system_light_theme());
    app.set_theme_follow_system(theme_follow_system);
    app.set_light(light);
    app.set_rail_collapsed(rail_collapsed);
    app.set_glass_available(cfg!(windows));
    app.set_downloader_path(downloader.into());
    app.set_ffmpeg_path(ffmpeg.into());
    app.set_download_folder(downloads.into());
    app.set_data_folder(data.to_string_lossy().as_ref().into());
    app.set_theme_hint(show_theme_hint);
    if first_run {
        // The opening tour walks the app rather than parking on one page, so
        // the card travels with the app as it is explained.
        app.set_guide_touring(true);
        app.set_selected_page(0);
        app.set_guide_step(0);
        app.set_guide_open(true);
    }
    {
        // The developer card is decoded here and nowhere else, so the name and
        // the links never exist as strings in the binary.
        let who = identity::Identity::default();
        app.set_dev_name(who.name.into());
        app.set_dev_role(who.role.into());
        app.set_dev_portfolio(who.portfolio.into());
        app.set_dev_github_url(who.github_url.into());
        app.set_dev_portfolio_url(who.portfolio_url.into());
        app.set_dev_photo(developer_photo);
    }
    {
        // Opening a browser must not block the UI thread, and must not be
        // able to launch anything other than a web page.
        let weak = app.as_weak();
        app.on_open_link(move |link| {
            let link = link.to_string();
            // Cloned per click: the handler is FnMut, so it may fire many times.
            let weak = weak.clone();
            std::thread::spawn(move || {
                if let Err(error) = identity::open(&link) {
                    message(&weak, error.to_string());
                }
            });
        });
    }
    {
        // The "seen" flag is already persisted at startup, so dismissing only
        // has to hide the banner. The library handle has moved into the worker.
        let weak = app.as_weak();
        app.on_theme_hint_dismissed(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_theme_hint(false);
            }
        });
    }
    {
        // The tour is purely presentational and the shell already closes itself.
        // The "seen" flag is written at startup, so finishing and cancelling
        // both count and neither needs to reach the library.
        app.on_guide_done(|| {});
    }
    {
        let weak = app.as_weak();
        app.on_open_settings(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_selected_page(4);
            }
        });
    }
    app.set_reduce_motion(!native::animations_enabled());
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker = controller::spawn(db, app.as_weak(), rx, tx.clone(), cancel.clone());
    let sharing = nearby_ui::Sharing::bind(&app, data.clone(), tx.clone())?;
    let _tool_installer = tool_ui::Tools::bind(&app, tx.clone());
    {
        // Probe helpers in the background: saved paths first, then the managed
        // folder, PATH and the usual package-manager homes. A discovered path
        // goes through Save so Settings and the library stay in agreement.
        let weak = app.as_weak();
        let tx = tx.clone();
        std::thread::spawn(move || {
            for (tool, saved) in monolith_desktop::tools::Tool::ALL
                .into_iter()
                .zip(saved_tools)
            {
                let found = monolith_desktop::tools::detect(tool, &saved);
                if let Some(f) = &found {
                    let path = f.path.to_string_lossy().into_owned();
                    if path != saved {
                        let _ = tx.send(Command::Save(tool.setting_key().into(), path));
                    }
                }
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    tool_ui::report(&ui, tool, found.as_ref());
                });
            }
        });
    }
    macro_rules! bind {
        ($method:ident,$variant:ident) => {{
            let sender = tx.clone();
            app.$method(move || {
                let _ = sender.send(Command::$variant);
            });
        }};
        ($method:ident,$variant:ident,$cast:ty) => {{
            let sender = tx.clone();
            app.$method(move |v| {
                let _ = sender.send(Command::$variant(v as $cast));
            });
        }};
    }
    {
        let tx = tx.clone();
        app.on_sort_changed(move |order| {
            let _ = tx.send(Command::Sort(order));
        });
    }
    {
        // Also sent once here: the page is set before the first render, so
        // without this the worker would not know Songs has a different meaning.
        let sender = tx.clone();
        app.on_page_changed(move |page| {
            let _ = sender.send(Command::Page(page));
        });
        let _ = tx.send(Command::Page(app.get_selected_page()));
    }
    bind!(on_toggle_play, Toggle);
    bind!(on_stop_play, Stop);
    bind!(on_next_track, Next, i32);
    bind!(on_seek, Seek, f32);
    bind!(on_volume_changed, Volume, f32);
    bind!(on_play_track, Play, i64);
    bind!(on_request_thumbnail, Thumbnail, i64);
    bind!(on_favorite_track, Favorite, i64);
    bind!(on_remove_track, Remove, i64);
    bind!(on_toggle_selection, Select, i64);
    bind!(on_select_visible, SelectVisible, bool);
    bind!(on_clear_selection, ClearSelection);
    {
        let sender = tx.clone();
        app.on_batch_remove(move |ids| {
            let _ = sender.send(Command::Batch(
                ids.iter().map(|id| id as i64).collect(),
                monolith_desktop::library::BatchAction::Remove,
            ));
        });
        let sender = tx.clone();
        app.on_batch_remove_playlist(move |ids, playlist| {
            let _ = sender.send(Command::Batch(
                ids.iter().map(|id| id as i64).collect(),
                monolith_desktop::library::BatchAction::RemoveFrom(playlist as i64),
            ));
        });
        let sender = tx.clone();
        app.on_batch_add(move |ids, playlist| {
            let _ = sender.send(Command::Batch(
                ids.iter().map(|id| id as i64).collect(),
                monolith_desktop::library::BatchAction::AddTo(playlist as i64),
            ));
        });
        let sender = tx.clone();
        app.on_batch_create(move |ids, name| {
            let _ = sender.send(Command::Batch(
                ids.iter().map(|id| id as i64).collect(),
                monolith_desktop::library::BatchAction::CreatePlaylist(name.to_string()),
            ));
        });
    }
    {
        let tx = tx.clone();
        app.on_remove_from_playlist(move |id, playlist| {
            let _ = tx.send(Command::RemoveFromPlaylist(id as i64, playlist as i64));
        });
    }
    bind!(on_shuffle_changed, Shuffle, bool);
    bind!(on_repeat_changed, Repeat, i32);
    {
        let tx = tx.clone();
        let weak = app.as_weak();
        app.on_locate_track(move |id| {
            let sender = tx.clone();
            let weak = weak.clone();
            std::thread::spawn(move || {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Locate this song's audio file")
                    .add_filter("Audio", EXTENSIONS)
                    .pick_file()
                {
                    let _ = sender.send(Command::Relink(id as i64, path));
                } else {
                    message(
                        &weak,
                        "File selection cancelled. The song is unchanged.".into(),
                    );
                }
            });
        });
    }
    {
        let tx = tx.clone();
        app.on_filter_changed(move |index, query| {
            let _ = tx.send(Command::Filter(index.max(0) as usize, query.to_string()));
        });
    }
    {
        let tx = tx.clone();
        app.on_create_playlist(move |name| {
            let _ = tx.send(Command::NewPlaylist(name.to_string()));
        });
    }
    {
        let tx = tx.clone();
        app.on_add_to_playlist(move |id, index| {
            let _ = tx.send(Command::AddToPlaylist(id as i64, index.max(0) as usize));
        });
    }
    {
        let tx = tx.clone();
        let weak = app.as_weak();
        app.on_delete_playlist(move || {
            let Some(id) = weak
                .upgrade()
                .and_then(|u| {
                    u.get_playlist_ids()
                        .row_data(u.get_playlist_index() as usize)
                })
                .filter(|id| *id > 0)
            else {
                return;
            };
            let tx = tx.clone();
            std::thread::spawn(move || {
                if rfd::MessageDialog::new()
                    .set_title("Delete playlist?")
                    .set_description("Tracks stay in your library. Original files are kept.")
                    .set_buttons(rfd::MessageButtons::OkCancel)
                    .show()
                    == rfd::MessageDialogResult::Ok
                {
                    let _ = tx.send(Command::DeletePlaylist(id as i64));
                }
            });
        });
    }
    {
        let tx = tx.clone();
        let weak = app.as_weak();
        app.on_import_files(move |folder| {
            let sender = tx.clone();
            let weak = weak.clone();
            std::thread::spawn(move || {
                if folder {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Choose your music folder")
                        .pick_folder()
                    {
                        let _ = sender.send(Command::Folder(path));
                    }
                } else if let Some(paths) = rfd::FileDialog::new()
                    .set_title("Import music")
                    .add_filter("Audio", EXTENSIONS)
                    .pick_files()
                {
                    let _ = sender.send(Command::Import(paths));
                } else {
                    message(&weak, "Import cancelled.".into());
                }
            });
        });
    }
    {
        let tx = tx.clone();
        let weak = app.as_weak();
        app.on_import_playlist(move || {
            let sender = tx.clone();
            let weak = weak.clone();
            std::thread::spawn(move || {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Import playlist")
                    .add_filter("Playlists", PLAYLIST_EXTENSIONS)
                    .pick_file()
                {
                    let _ = sender.send(Command::ImportPlaylist(path));
                } else {
                    message(&weak, "Playlist import cancelled.".into());
                }
            });
        });
    }
    {
        let tx = tx.clone();
        app.on_prune_missing(move || {
            let _ = tx.send(Command::PruneMissing);
        });
    }
    {
        let tx = tx.clone();
        app.on_rail_changed(move |collapsed| {
            let _ = tx.send(Command::Save(
                "rail_collapsed".into(),
                if collapsed { "1".into() } else { "0".into() },
            ));
        });
    }
    {
        let tx = tx.clone();
        let weak = app.as_weak();
        app.on_theme_changed(move |choice| {
            let (follow, light_value) = match choice {
                0 => ("1", "0"),
                1 => ("0", "1"),
                _ => ("0", "0"),
            };
            let _ = tx.send(Command::Save("theme_follow_system".into(), follow.into()));
            let _ = tx.send(Command::Save("theme_light".into(), light_value.into()));
            if choice == 0 {
                message(&weak, "Theme follows Windows.".into());
            }
        });
    }
    {
        let tx = tx.clone();
        let weak = app.as_weak();
        app.on_power_changed(move |enabled| {
            let _ = tx.send(Command::Save(
                "low_power".into(),
                if enabled { "1" } else { "0" }.into(),
            ));
            message(
                &weak,
                "Rendering preference saved. Restart Monolith to apply.".into(),
            );
        });
    }
    {
        let tx = tx.clone();
        let weak = app.as_weak();
        app.on_glass_changed(move |enabled| {
            if let Some(app) = weak.upgrade() {
                let applied = backdrop(&app, enabled);
                let value = enabled && applied;
                app.set_glass(value);
                let _ = tx.send(Command::Save(
                    "glass".into(),
                    if value { "1" } else { "0" }.into(),
                ));
                if enabled && !applied {
                    app.set_status("Translucency is unavailable; using a solid background.".into());
                }
            }
        });
    }
    {
        let tx = tx.clone();
        app.on_choose_downloader(move || {
            let tx = tx.clone();
            std::thread::spawn(move || {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose yt-dlp executable")
                    .pick_file()
                {
                    let _ = tx.send(Command::Save(
                        "downloader".into(),
                        path.to_string_lossy().into_owned(),
                    ));
                    let _ = tx.send(Command::RescanTools);
                }
            });
        });
    }
    {
        let tx = tx.clone();
        app.on_choose_ffmpeg(move || {
            let tx = tx.clone();
            std::thread::spawn(move || {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose ffmpeg executable")
                    .pick_file()
                {
                    let _ = tx.send(Command::Save(
                        "ffmpeg".into(),
                        path.to_string_lossy().into_owned(),
                    ));
                    let _ = tx.send(Command::RescanTools);
                }
            });
        });
    }
    {
        let tx = tx.clone();
        app.on_choose_download_folder(move || {
            let tx = tx.clone();
            std::thread::spawn(move || {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Save downloads to")
                    .pick_folder()
                {
                    let _ = tx.send(Command::Save(
                        "download_folder".into(),
                        path.to_string_lossy().into_owned(),
                    ));
                }
            });
        });
    }
    {
        let tx = tx.clone();
        app.on_start_download(move |url, mp3| {
            let _ = tx.send(Command::Download(url.to_string(), mp3));
        });
    }
    {
        let cancel = cancel.clone();
        app.on_cancel_download(move || cancel.store(true, Ordering::Relaxed));
    }
    app.show()?;
    // winit 0.30 creates the native window inside the running event loop, so
    // backdrop and tray/media setup wait for its handle instead of running here.
    #[cfg(windows)]
    let integration = std::rc::Rc::new(std::cell::RefCell::new(None));
    {
        let weak = app.as_weak();
        #[cfg(windows)]
        let (slot, tx) = (integration.clone(), tx.clone());
        slint::spawn_local(async move {
            let Some(app) = weak.upgrade() else { return };
            if let Err(e) = app.window().winit_window().await {
                eprintln!("Native integration: winit window unavailable: {e}");
                return;
            }
            app.set_glass(glass && backdrop(&app, true));
            #[cfg(windows)]
            match native::Native::new(&app, tx) {
                Ok(native) => {
                    let native = std::rc::Rc::new(native);
                    let state = native.clone();
                    let weak = app.as_weak();
                    app.set_tray_available(true);
                    app.on_transport_changed(move || {
                        if let Some(ui) = weak.upgrade() {
                            if let Err(e) = state.sync(&ui) {
                                eprintln!("Media controls: {e}");
                            }
                        }
                    });
                    let weak = app.as_weak();
                    app.on_hide_to_tray(move || {
                        if let Some(ui) = weak.upgrade() {
                            native::hide_to_tray(&ui);
                        }
                    });
                    let _ = native.sync(&app);
                    *slot.borrow_mut() = Some(native);
                }
                Err(e) => {
                    eprintln!("Native integration failed: {e}");
                    app.set_status(
                        format!("Music is ready; Windows tray/media integration unavailable: {e}")
                            .into(),
                    );
                }
            }
        })?;
    }
    app.window().on_close_requested(|| {
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });
    let outcome = slint::run_event_loop_until_quit();
    #[cfg(windows)]
    drop(integration);
    cancel.store(true, Ordering::Relaxed);
    drop(sharing);
    let _ = tx.send(Command::Shutdown);
    let _ = worker.join();
    outcome?;
    Ok(())
}
fn main() {
    crash::install_hook();
    // A panic anywhere under the event loop unwinds through here, which is the
    // only point at which a fresh window can still be shown.
    let outcome = std::panic::catch_unwind(run);
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!("Monolith: {error}");
            rfd::MessageDialog::new()
                .set_title("Monolith could not start")
                .set_description(error.to_string())
                .set_level(rfd::MessageLevel::Error)
                .show();
        }
        Err(payload) => {
            let text = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            let details = crash::report(&text);
            let url = crash::issue_url(&text, &details);
            crash_ui::show(crash::headline(&text), details, url);
        }
    }
}
