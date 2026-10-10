//! Real pointer/keyboard dispatch against UI callbacks. No media or network work.
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};
slint::include_modules!();

fn click(ui: &AppWindow, x: f32, y: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    let position = slint::LogicalPosition::new(x, y);
    for event in [
        WindowEvent::PointerMoved { position },
        WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        },
        WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        },
    ] {
        ui.window().dispatch_event(event);
    }
}

fn key(ui: &AppWindow, text: slint::SharedString) {
    for event in [
        slint::platform::WindowEvent::KeyPressed { text: text.clone() },
        slint::platform::WindowEvent::KeyReleased { text },
    ] {
        ui.window().dispatch_event(event);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;
    ui.set_reduce_motion(true);
    ui.set_total_tracks(2);
    ui.set_current_id(1);
    ui.set_playing(true);
    ui.set_tracks(ModelRc::from(Rc::new(VecModel::from(vec![
        TrackRow {
            id: 1,
            title: "Current track with a deliberately long title for pointer checks".into(),
            detail: "Current artist and album for subtitle pointer checks".into(),
            duration: "3:20".into(),
            ..Default::default()
        },
        TrackRow {
            id: 2,
            title: "Next track with a deliberately long title for pointer checks".into(),
            detail: "Next artist and album".into(),
            duration: "4:00".into(),
            ..Default::default()
        },
    ]))));
    let toggles = Rc::new(Cell::new(0));
    let played = Rc::new(Cell::new(-1));
    let favorites = Rc::new(Cell::new(0));
    let removals = Rc::new(Cell::new(0));
    let playlist_removals = Rc::new(Cell::new(0));
    let located = Rc::new(Cell::new(0));
    let count = playlist_removals.clone();
    ui.on_remove_from_playlist(move |id, playlist| {
        assert_eq!(playlist, 3);
        assert_eq!(id, 1);
        count.set(count.get() + 1);
    });
    let count = located.clone();
    ui.on_locate_track(move |id| {
        assert_eq!(id, 2);
        count.set(count.get() + 1);
    });
    let downloads = Rc::new(Cell::new(0));
    let cancelled = Rc::new(Cell::new(0));
    let count = toggles.clone();
    ui.on_toggle_play(move || count.set(count.get() + 1));
    let id = played.clone();
    ui.on_play_track(move |track| id.set(track));
    let count = favorites.clone();
    ui.on_favorite_track(move |id| {
        assert_eq!(id, 1);
        count.set(count.get() + 1);
    });
    let count = removals.clone();
    ui.on_remove_track(move |id| {
        assert_eq!(id, 1);
        count.set(count.get() + 1);
    });
    let count = downloads.clone();
    ui.on_start_download(move |url, mp3| {
        assert_eq!(url, "https://youtu.be/test");
        assert!(mp3);
        count.set(count.get() + 1);
    });
    let count = cancelled.clone();
    ui.on_cancel_download(move || count.set(count.get() + 1));
    ui.window().set_size(slint::LogicalSize::new(1280., 820.));
    ui.show()?;
    slint::Timer::single_shot(Duration::from_millis(700), move || {
        click(&ui, 284., 234.);
        assert_eq!(toggles.get(), 1, "current row pause routes to transport");
        use slint::platform::WindowEvent;
        for event in [
            WindowEvent::KeyPressed { text: " ".into() },
            WindowEvent::KeyReleased { text: " ".into() },
        ] {
            ui.window().dispatch_event(event);
        }
        assert_eq!(toggles.get(), 2, "focused row transport supports Space");
        click(&ui, 284., 286.);
        assert_eq!(played.get(), 2, "another row selects its track");
        // Click the title, subtitle, whitespace and duration, not just the tile.
        for (x, y) in [(450., 226.), (450., 244.), (850., 234.), (1080., 234.)] {
            let before = toggles.get();
            click(&ui, x, y);
            assert_eq!(toggles.get(), before + 1, "row body at {x},{y}");
        }
        click(&ui, 450., 278.);
        assert_eq!(played.get(), 2, "song title chooses the track");
        let before = toggles.get();
        click(&ui, 1134., 234.); // Favorite, beyond the playback hit area.
        click(&ui, 1182., 234.); // Disabled collection action.
        click(&ui, 1230., 234.); // Remove action.
        assert_eq!(toggles.get(), before, "row actions never toggle playback");
        assert_eq!(favorites.get(), 1, "favorite owns its click");
        assert_eq!(removals.get(), 0, "removal waits for confirmation");
        click(&ui, 450., 234.);
        assert_eq!(toggles.get(), before, "warning blocks underlying playback");
        key(&ui, "\n".into()); // Safe default is Cancel.
        assert_eq!(removals.get(), 0);
        click(&ui, 1230., 234.);
        key(&ui, slint::platform::Key::Tab.into());
        key(&ui, "\n".into());
        assert_eq!(removals.get(), 1, "confirm removes exactly once");
        ui.set_playlists(ModelRc::new(VecModel::from(vec![
            "All music".into(),
            "Favorites".into(),
            "Evening".into(),
        ])));
        ui.set_playlist_ids(ModelRc::new(VecModel::from(vec![0, -1, 3])));
        ui.set_playlist_index(2);
        click(&ui, 1182., 234.);
        key(&ui, slint::platform::Key::Escape.into());
        assert_eq!(
            playlist_removals.get(),
            0,
            "Escape cancels playlist removal"
        );
        click(&ui, 1182., 234.);
        key(&ui, slint::platform::Key::Backtab.into());
        key(&ui, " ".into());
        assert_eq!(
            playlist_removals.get(),
            1,
            "playlist removal requires confirmation"
        );
        ui.set_unavailable_title("Moved fixture".into());
        ui.set_unavailable_path("Music/moved.wav".into());
        ui.set_unavailable_track_id(2);
        key(&ui, "\n".into());
        assert_eq!(located.get(), 0, "missing-file popup defaults to Cancel");
        ui.set_unavailable_track_id(2);
        key(&ui, slint::platform::Key::Tab.into());
        key(&ui, "\n".into());
        assert_eq!(located.get(), 1, "Locate routes the exact missing track");
        assert_eq!(ui.get_unavailable_track_id(), -1);
        click(&ui, 46., 776.);
        assert!(ui.get_player_open(), "artwork opens the expanded player");
        ui.set_player_open(false);
        ui.set_selected_page(3);
        let ui = ui.clone_strong();
        slint::Timer::single_shot(Duration::from_millis(100), move || {
            click(&ui, 1150., 369.);
            assert_eq!(downloads.get(), 0, "empty URL cannot submit");
            ui.set_download_url("https://youtu.be/test".into());
            ui.set_download_format(1);
            click(&ui, 1150., 369.);
            assert_eq!(downloads.get(), 1, "Download submits selected format");
            ui.set_downloading(true);
            click(&ui, 1150., 369.);
            assert_eq!(cancelled.get(), 1, "active download offers cancellation");
            assert_eq!(downloads.get(), 1, "active job cannot submit twice");
            println!(
                "PASS: whole-row title/detail/gap/duration playback, separate row actions, Cancel/Remove warnings, playlist confirmation, Locate popup, keyboard pause, empty URL guard, download and cancel callbacks; no network."
            );
            slint::quit_event_loop().unwrap();
        });
    });
    slint::run_event_loop()?;
    Ok(())
}
