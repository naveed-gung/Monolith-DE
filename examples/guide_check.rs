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
    let area = ui.get_guide_card_area();
    for x in ((area.x + 12.) as usize..(area.x + area.width - 12.) as usize).rev() {
        let mut start = None;
        for y in (area.y + 12.) as usize..(area.y + area.height - 12.) as usize {
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
    let scrolled = args.get(4).is_some_and(|s| s == "scrolled");
    std::fs::create_dir_all(&output)?;
    let ui = AppWindow::new()?;
    ui.set_reduce_motion(true);
    // Help must leave bulk-selection mode before measuring guide targets.
    ui.set_selecting(true);
    ui.set_rail_collapsed(width >= 900. && args.get(4).is_some_and(|s| s == "rail"));
    if args.get(4).is_some_and(|s| s == "light") {
        ui.set_light(true);
    }
    ui.set_total_tracks(1);
    if args.get(4).is_some_and(|s| s == "empty") {
        ui.set_total_tracks(0);
    }
    ui.set_current_id(if ui.get_total_tracks() > 0 { 1 } else { -1 });
    if ui.get_total_tracks() > 0 {
        ui.set_tracks(ModelRc::new(VecModel::from(
            (1..=if scrolled { 20 } else { 1 })
                .map(|id| TrackRow {
                    id,
                    title: format!("Offline guide fixture {id}").into(),
                    detail: "Local artist".into(),
                    duration: "3:20".into(),
                    thumbnail: if id == 1 {
                        slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
                            &[220u8, 80, 60, 255],
                            1,
                            1,
                        ))
                    } else {
                        Default::default()
                    },
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        )));
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
    let count = actions.clone();
    ui.on_open_sharing(move || count.set(count.get() + 1));
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
            assert_eq!(ui.get_guide_spotlight_area(), ui.get_guide_target(), "spotlight covers the actual target without clipping");
            ui.window().dispatch_event(WindowEvent::PointerExited);
            let pixels = ui.window().take_snapshot().unwrap();
            let path = output.join(format!("step-{}.png", ui.get_guide_step()));
            image::save_buffer(&path, pixels.as_bytes(), pixels.width(), pixels.height(), image::ColorType::Rgba8).unwrap();
            let target = ui.get_guide_target();
            let card = ui.get_guide_card_area();
            if ui.get_guide_step() == 13 {
                assert!(target.width <= 280., "appearance spotlight fits its three choices");
            }
            if ui.get_guide_step() == 5 {
                assert!(target.width <= 220. && target.height <= 44., "sharing spotlight fits Share and Select");
            }
            if ui.get_guide_step() == 14 {
                assert!(target.width <= 130. && target.height <= 52., "folder spotlight fits the actual chooser, not the stretched row");
            }
            assert!(target.width > 0. && target.height > 0., "step has a real target");
            assert!(target.x >= 0. && target.x + target.width <= width + 1., "target is horizontally in view; step {}", ui.get_guide_step());
            assert!(target.y >= 0. && target.y + target.height <= height + 1., "target is vertically in view; step {}", ui.get_guide_step());
            assert!(card.y + card.height <= height + 1. && card.y >= 0., "card is fully in view");
            assert!(card.y + card.height <= target.y || card.y >= target.y + target.height || card.x >= target.x + target.width || card.x + card.width <= target.x,
                "card must leave its target readable; step {}", ui.get_guide_step());
            if ui.get_total_tracks() > 0 && matches!(ui.get_guide_step(), 2 | 8) {
                let offset = (((target.y + 20.) as usize) * pixels.width() as usize + (target.x + 20.) as usize) * 4;
                assert_eq!(&pixels.as_bytes()[offset..offset+3], &[220,80,60], "first song is scrolled fully into the bright region");
            }
            click(&ui, target.x + target.width / 2., target.y + target.height / 2.);
            assert_eq!(actions.get(), 0, "bright target remains unclickable");
            if ui.get_total_tracks() > 0 || ui.get_guide_step() % 3 == 0 {
                ui.window().dispatch_event(WindowEvent::PointerExited);
                ui.set_guide_open(false);
                let plain = ui.window().take_snapshot().unwrap();
                let mut equal = 0; let mut samples = 0;
                for y in ((target.y + 6.) as usize..(target.y + target.height - 6.) as usize).step_by(4) {
                    for x in ((target.x + 6.) as usize..(target.x + target.width - 6.) as usize).step_by(4) {
                        let offset = (y * pixels.width() as usize + x) * 4;
                        equal += usize::from(pixels.as_bytes()[offset..offset+3] == plain.as_bytes()[offset..offset+3]); samples += 1;
                    }
                }
                assert!(equal * 10 >= samples * 9, "spotlight preserves normal color, step {}: {equal}/{samples}", ui.get_guide_step());
                ui.set_guide_open(true);
            }
            // Every step has a real, visible card and remains on its section.
            // Only sidebar introductions use a lateral tail.
            if ui.get_guide_step() % 3 == 0 {
                let x = (rail + 7.) as usize;
                let y = nav_y(page) as usize;
                let p = &pixels.as_bytes()[(y * pixels.width() as usize + x) * 4..][..4];
                assert!(p[0] > 20, "bubble tail meets nav item {page}");
            }
            primary(&ui, rail)
        };
        match phase.get() {
            0 => {
                if scrolled {
                    ui.window().dispatch_event(WindowEvent::PointerScrolled { position: slint::LogicalPosition::new(rail+150.,230.), delta_x: 0., delta_y: -400. });
                } else { click(&ui, 36., nav_y(4)); }
                click(&ui, 36., help_y);
            }
            1 => {
                assert!(ui.get_guide_open()); assert_eq!(ui.get_guide_step(), 0);
                assert!(!ui.get_selecting(), "Help exits selection before starting the tour");
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
            4..=17 => {
                let step = phase.get() - 3;
                assert_eq!(ui.get_guide_step(), step as i32);
                let expected = (step / 3) as i32;
                let (x, y) = save(expected); click(&ui, x, y);
            }
            18 => {
                assert!(!ui.get_guide_open()); assert_eq!(completed.get(), 1);
                assert_eq!(actions.get(), 0);
                click(&ui, 36., help_y);
            }
            19 => {
                assert!(ui.get_guide_open()); assert_eq!(ui.get_guide_step(), 0);
                assert_eq!(ui.get_selected_page(), 0, "Help always restarts at Library");
                key(&ui, slint::platform::Key::Escape.into());
            }
            20 => {
                assert!(!ui.get_guide_open()); click(&ui, 36., nav_y(1));
                assert_eq!(ui.get_selected_page(), 1, "normal navigation restored");
                println!("PASS: five sections, three steps each, Help restarts at Library, real Next/Back, blocked page input, Tab/Backtab trap, Done/Escape; {width}x{height}.");
                slint::quit_event_loop().unwrap();
            }
            _ => unreachable!(),
        }
        phase.set(phase.get() + 1);
    });
    slint::run_event_loop()?;
    Ok(())
}
