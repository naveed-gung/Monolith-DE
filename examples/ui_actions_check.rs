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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;
    ui.set_reduce_motion(true);
    ui.set_total_tracks(2);
    ui.set_current_id(1);
    ui.set_playing(true);
    ui.set_tracks(ModelRc::from(Rc::new(VecModel::from(vec![
        TrackRow {
            id: 1,
            title: "Current track".into(),
            ..Default::default()
        },
        TrackRow {
            id: 2,
            title: "Next track".into(),
            ..Default::default()
        },
    ]))));
    let toggles = Rc::new(Cell::new(0));
    let played = Rc::new(Cell::new(-1));
    let downloads = Rc::new(Cell::new(0));
    let cancelled = Rc::new(Cell::new(0));
    let count = toggles.clone();
    ui.on_toggle_play(move || count.set(count.get() + 1));
    let id = played.clone();
    ui.on_play_track(move |track| id.set(track));
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
                "PASS: current-row pause, track selection, empty URL guard, download and cancel callbacks; no network."
            );
            slint::quit_event_loop().unwrap();
        });
    });
    slint::run_event_loop()?;
    Ok(())
}
