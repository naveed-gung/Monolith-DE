//! Pointer/keyboard isolation and rendered anchors for the modal app guide.
use slint::{ComponentHandle, ModelRc, VecModel, platform::WindowEvent};
use std::{cell::Cell, path::PathBuf, rc::Rc, time::Duration};
slint::include_modules!();

fn click(ui: &AppWindow, x: f32, y: f32) {
    use slint::platform::PointerEventButton;
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
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
}

// Find the primary button in the rendered card rather than mirroring its layout.
fn primary(ui: &AppWindow, _rail: f32) -> (f32, f32) {
    let pixels = ui.window().take_snapshot().unwrap();
    let width = pixels.width() as usize;
    for x in (12..width - 12).rev() {
        let mut start = None;
        for y in 12..pixels.height() as usize - 12 {
            let p = &pixels.as_bytes()[(y * width + x) * 4..][..4];
            let red = p[0] > 160 && (p[0] as u16) > p[1] as u16 + 70;
            if red {
                start.get_or_insert(y);
            } else if let Some(top) = start.take() {
                let middle = (top + y) / 2;
                let inside = &pixels.as_bytes()[(middle * width + x.saturating_sub(12)) * 4..][..4];
                if y - top >= 25 && inside[0] > 160 && inside[0] as u16 > inside[1] as u16 + 70 {
                    return (x as f32 - 12., (top + y) as f32 / 2.);
                }
            }
        }
    }
    panic!("Guide primary button must be rendered inside the viewport");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let output = PathBuf::from(args.get(1).ok_or("OUTPUT_DIRECTORY WIDTH HEIGHT")?);
    let width: f32 = args.get(2).ok_or("WIDTH")?.parse()?;
    let height: f32 = args.get(3).ok_or("HEIGHT")?.parse()?;
    std::fs::create_dir_all(&output)?;
    let ui = AppWindow::new()?;
    ui.set_reduce_motion(true);
    ui.set_rail_collapsed(width >= 900. && args.get(4).is_some_and(|s| s == "rail"));
    if args.get(4).is_some_and(|s| s == "light") {
        ui.set_light(true);
    }
    ui.set_total_tracks(1);
    if args.get(4).is_some_and(|s| s == "empty") {
        ui.set_total_tracks(0);
    }
    ui.set_current_id(1);
    if ui.get_total_tracks() > 0 {
        ui.set_tracks(ModelRc::new(VecModel::from(vec![TrackRow {
            id: 1,
            title: "Offline guide fixture".into(),
            detail: "Local artist".into(),
            duration: "3:20".into(),
            ..Default::default()
        }])));
    }
    let actions = Rc::new(Cell::new(0));
    let count = actions.clone();
    ui.on_toggle_play(move || count.set(count.get() + 1));
    let count = actions.clone();
    ui.on_play_track(move |_| count.set(count.get() + 1));
    let count = actions.clone();
    ui.on_favorite_track(move |_| count.set(count.get() + 1));
    let count = actions.clone();
    ui.on_import_files(move |_| count.set(count.get() + 1));
    let completed = Rc::new(Cell::new(0));
    let count = completed.clone();
    ui.on_guide_done(move || count.set(count.get() + 1));
    ui.window().set_size(slint::LogicalSize::new(width, height));
    ui.show()?;
    let compact = width < 760.;
    let rail_height = height - if compact && height >= 440. { 152. } else { 88. };
    let tight = rail_height < 394.;
    let nav_top = if tight { 60. } else { 94. };
    let nav_row = if tight { 40. } else { 44. };
    let rail = if width < 900. || ui.get_rail_collapsed() {
        72.
    } else {
        224.
    };
    let nav_y = move |page: i32| nav_top + page as f32 * (nav_row + 4.) + nav_row / 2.;
    let help_y = if tight && width < 900. {
        28.
    } else {
        rail_height
            - 32.
            - if width >= 900. && rail == 72. {
                44.
            } else {
                0.
            }
    };
    let phase = Cell::new(0);
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(150), move || {
        let save = |page: i32| {
            assert_eq!(ui.get_selected_page(), page, "guide selects its target section");
            let pixels = ui.window().take_snapshot().unwrap();
            let path = output.join(format!("step-{}.png", ui.get_guide_step()));
            image::save_buffer(&path, pixels.as_bytes(), pixels.width(), pixels.height(), image::ColorType::Rgba8).unwrap();
            // Every step has a real, visible card and remains on its section.
            // Only sidebar introductions use a lateral tail.
            if ui.get_guide_step() == 0 || ui.get_guide_step() >= 7 {
                let x = (rail + 7.) as usize;
                let y = nav_y(page) as usize;
                let p = &pixels.as_bytes()[(y * pixels.width() as usize + x) * 4..][..4];
                assert!(p[0] > 20, "bubble tail meets nav item {page}");
            }
            primary(&ui, rail)
        };
        match phase.get() {
            0 => { click(&ui, 36., nav_y(4)); click(&ui, 36., help_y); }
            1 => {
                assert!(ui.get_guide_open()); assert_eq!(ui.get_guide_step(), 0);
                for page in 0..5 { click(&ui, 36., nav_y(page)); }
                click(&ui, width - 50., height - 40.);
                click(&ui, rail + 150., 230.);
                assert_eq!(ui.get_selected_page(), 0, "scrim blocks sidebar/page clicks");
                assert_eq!(actions.get(), 0, "underlying actions blocked");
                save(0);
                key(&ui, slint::platform::Key::Tab.into());
                key(&ui, slint::platform::Key::Backtab.into());
                key(&ui, "\n".into());
            }
            2 => {
                assert_eq!(ui.get_guide_step(), 1);
                key(&ui, slint::platform::Key::Tab.into()); key(&ui, " ".into());
            }
            3 => { assert_eq!(ui.get_guide_step(), 0); key(&ui, slint::platform::Key::RightArrow.into()); }
            4..=13 => {
                let step = phase.get() - 3;
                assert_eq!(ui.get_guide_step(), step as i32);
                let expected = if step <= 6 { 0 } else { (step - 6) as i32 };
                let (x, y) = save(expected); click(&ui, x, y);
            }
            14 => {
                assert!(!ui.get_guide_open()); assert_eq!(completed.get(), 1);
                assert_eq!(actions.get(), 0);
                click(&ui, 36., help_y);
            }
            15 => {
                assert!(ui.get_guide_open()); assert_eq!(ui.get_guide_step(), 0);
                assert_eq!(ui.get_selected_page(), 0, "Help always restarts at Library");
                key(&ui, slint::platform::Key::Escape.into());
            }
            16 => {
                assert!(!ui.get_guide_open()); click(&ui, 36., nav_y(1));
                assert_eq!(ui.get_selected_page(), 1, "normal navigation restored");
                println!("PASS: eleven-step contextual guide, Help restarts at Library, real Next/Back, blocked page input, Tab/Backtab trap, Done/Escape; {width}x{height}.");
                slint::quit_event_loop().unwrap();
            }
            _ => unreachable!(),
        }
        phase.set(phase.get() + 1);
    });
    slint::run_event_loop()?;
    Ok(())
}
