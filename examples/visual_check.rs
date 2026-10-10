use monolith_desktop::identity;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{io::Write, rc::Rc};
slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 || args.len() > 7 {
        return Err(
            "Usage: visual_check OUTPUT.ppm WIDTH HEIGHT PAGE [check|populated|guide|light|scrolled] [STEP]"
                .into(),
        );
    }
    let width: f32 = args[2].parse()?;
    let height: f32 = args[3].parse()?;
    let page: i32 = args[4].parse()?;
    if !width.is_finite()
        || !height.is_finite()
        || width < 420.
        || height < 380.
        || !(0..5).contains(&page)
    {
        return Err("Use a finite size of at least 420x380 and page 0 through 4".into());
    }
    let mode = args.get(5).map(String::as_str);
    if mode.is_some_and(|value| {
        value != "check"
            && value != "populated"
            && value != "guide"
            && value != "light"
            && value != "scrolled"
    }) {
        return Err("Optional mode must be check, populated, guide, light or scrolled".into());
    }
    let check = mode == Some("check");
    let scroll = mode == Some("scrolled");
    let ui = AppWindow::new()?;
    // The Settings card draws the developer identity, so every snapshot has to
    // carry the same decoded values the real app does.
    {
        let who = identity::Identity::default();
        ui.set_dev_name(who.name.into());
        ui.set_dev_role(who.role.into());
        ui.set_dev_portfolio(who.portfolio.into());
        ui.set_dev_github_url(who.github_url.into());
        ui.set_dev_portfolio_url(who.portfolio_url.into());
        ui.set_dev_photo(identity::portrait().unwrap_or_default());
    }
    ui.set_selected_page(page);
    if mode == Some("light") {
        ui.set_theme_follow_system(false);
        ui.set_light(true);
    }
    // STEP selects a position in the guide script, which is how the Back
    // button gets reviewed: it is hidden on the first step by design. It is
    // applied after the window settles, because the shell resets the step
    // whenever the page changes and that handler lands on the next tick.
    let guide_step = match args.get(6) {
        Some(step) => Some(step.parse::<i32>()?),
        None => (mode == Some("scrolled")).then_some(0),
    };
    if mode == Some("guide") {
        // Opens the coaching overlay over the page it describes, so the copy
        // can be reviewed against the controls it points at.
        ui.set_guide_step(if page == 4 { 5 } else { page });
        ui.set_guide_open(true);
    }
    // Synthetic rows for the review modes, so a snapshot has something to draw
    // rather than an empty shell. The collapsed rail is covered by the sidebar
    // toggle assertion in `check` instead: a capture taken immediately after
    // startup can come back blank.
    if matches!(mode, Some("populated") | Some("guide") | Some("light")) {
        // Synthetic rows exercise the populated layout only; no audio is loaded.
        let rows: Vec<TrackRow> = (1..=12)
            .map(|id| TrackRow {
                id,
                title: format!("Track {id} with a deliberately long title for eliding").into(),
                detail: format!("Artist {id} · Album {}", (id + 2) / 3).into(),
                duration: format!("{}:{:02}", 2 + id % 3, (id * 7) % 60).into(),
                favorite: id % 4 == 0,
                thumbnail: if id % 3 == 0 {
                    slint::Image::default()
                } else {
                    // Synthetic cover swatches; never presented as fetched artwork.
                    let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(64, 64);
                    for (i, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
                        let x = i % 64;
                        let y = i / 64;
                        let dx = (10.5 - x.min(63 - x) as f32).max(0.);
                        let dy = (10.5 - y.min(63 - y) as f32).max(0.);
                        let alpha = ((11. - (dx * dx + dy * dy).sqrt()).clamp(0., 1.) * 255.) as u8;
                        *pixel = slint::Rgba8Pixel::new(
                            (70 + id * 12) as u8,
                            (45 + y) as u8,
                            (100 + x) as u8,
                            alpha,
                        );
                    }
                    slint::Image::from_rgba8(pixels)
                },
            })
            .collect();
        // Keep the synthetic now-playing cover consistent with its row.
        ui.set_cover(rows[1].thumbnail.clone());
        ui.set_has_cover(true);
        ui.set_tracks(ModelRc::from(Rc::new(VecModel::from(rows))));
        ui.set_playlists(ModelRc::from(Rc::new(VecModel::from(
            ["All music", "Favorites", "Evening", "Focus"]
                .map(slint::SharedString::from)
                .to_vec(),
        ))));
        ui.set_playlist_ids(ModelRc::from(Rc::new(VecModel::from(vec![0, -1, 3, 4]))));
        ui.set_total_tracks(12);
        ui.set_album_count(4);
        ui.set_favorite_count(3);
        ui.set_current_id(2);
        ui.set_current_title("Track 2 with a deliberately long title for eliding".into());
        ui.set_current_artist("Artist 2".into());
        ui.set_playing(true);
        ui.set_player_open(false);
        ui.set_duration(200.);
        ui.set_position(70.);
        ui.set_elapsed("1:10".into());
        ui.set_total_time("3:20".into());
    }
    ui.window().set_size(slint::LogicalSize::new(width, height));
    if page == 4 && mode == Some("check") {
        ui.set_theme_follow_system(false);
        ui.set_light(true);
    }
    ui.show()?;
    let capture = ui.clone_strong();
    let stepper = ui.clone_strong();
    let output = args[1].clone();
    let run = move || {
        let capture_ui = capture.clone_strong();
        let output_ui = output.clone();
        // A second copy, for the check path's own snapshot: that one runs from a
        // later timer and cannot share the first.
        let target = output.clone();
        let snapshot_ui = capture_ui.clone_strong();
        let finish = move || {
            let pixels = snapshot_ui
                .window()
                .take_snapshot()
                .expect("capture rendered UI");
            let mut file = std::io::BufWriter::new(std::fs::File::create(&output_ui).unwrap());
            writeln!(file, "P6\n{} {}\n255", pixels.width(), pixels.height()).unwrap();
            for pixel in pixels.as_bytes().chunks_exact(4) {
                file.write_all(&pixel[..3]).unwrap();
            }
            file.flush().unwrap();
            slint::quit_event_loop().unwrap();
        };
        if !check {
            finish();
            return;
        }
        use slint::platform::{Key, PointerEventButton, WindowEvent};
        let window = capture_ui.window();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        let key = |text: slint::SharedString| {
            window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            window.dispatch_event(WindowEvent::KeyReleased { text });
        };
        capture_ui.set_player_open(false);
        // The rail is 224 px wide and collapses to 72 px below 900 px.
        let x = if width < 900. { 36. } else { 112. };
        // Nav rows start at 94 px inside a scrollable list with a 4 px gap,
        // and tighten from 44 px to 36 px on a short window. Clicking on a
        // fixed 48 px stride happened to land inside the right row but
        // under-counted how many rows fit, so the rows are walked for real.
        let player_height = if height < 440. {
            88.
        } else if width < 760. {
            152.
        } else {
            88.
        };
        let rail_height = height - player_height;
        let tight = rail_height < 394.;
        let nav_row = if tight { 40. } else { 44. };
        let nav_top = if tight { 60. } else { 94. };
        let nav_y = |page: i32| nav_top + page as f32 * (nav_row + 4.) + nav_row / 2.;
        // On a short window the rail genuinely cannot show every row at
        // once, so only the visible ones are clicked; Tab still reaches all
        // five because focus scrolls them into view.
        let nav_area =
            (rail_height - nav_top - if tight && width < 900. { 12. } else { 64. }).max(0.);
        let mouse_pages = (((nav_area + 4.) / (nav_row + 4.)).floor() as i32).clamp(1, 5);
        let click = |x: f32, y: f32| {
            let position = slint::LogicalPosition::new(x, y);
            window.dispatch_event(WindowEvent::PointerMoved { position });
            window.dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            window.dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        };
        for page in 0..mouse_pages {
            click(x, nav_y(page));
            assert_eq!(capture_ui.get_selected_page(), page, "mouse page {page}");
            // Focus is on that row now, so Space and Enter must activate it.
            capture_ui.set_selected_page((page + 1) % 5);
            key(" ".into());
            assert_eq!(capture_ui.get_selected_page(), page, "Space page {page}");
            capture_ui.set_selected_page((page + 1) % 5);
            key("\n".into());
            assert_eq!(capture_ui.get_selected_page(), page, "Enter page {page}");
        }
        // Tab reaches every row, including ones the rail had to scroll.
        click(x, nav_y(0));
        for page in 1..5 {
            key(Key::Tab.into());
            key(" ".into());
            assert_eq!(capture_ui.get_selected_page(), page, "Tab page {page}");
        }
        if width < 900. {
            // No expansion button is offered when the full sidebar cannot fit.
            click(36., if tight { 28. } else { rail_height - 32. });
            assert!(
                capture_ui.get_guide_open(),
                "Help has its own reachable slot"
            );
            capture_ui.set_guide_open(false);
            capture_ui.set_selected_page(page);
            println!("PASS: narrow navigation, keyboard and Help; no inert expand button.");
            slint::Timer::single_shot(std::time::Duration::from_millis(400), finish);
            return;
        }
        let rail_width = if width < 900. { 72. } else { 224. };
        click(
            if width < 900. { 52. } else { rail_width - 36. },
            rail_height - 32.,
        );
        assert!(capture_ui.get_rail_collapsed(), "sidebar closes on demand");
        // The rail now slides rather than snapping, so the collapse toggle
        // travels with it. Clicking again on the same frame aimed at where the
        // button used to be, which is a miss, not a failure to reopen. The
        // second click waits for the rail to finish moving.
        let reopen = capture_ui.clone_strong();
        let finished = capture_ui.clone_strong();
        let final_page = page;
        let click_at = move |ui: &AppWindow, px: f32, py: f32| {
            use slint::platform::{PointerEventButton, WindowEvent};
            let window = ui.window();
            let position = slint::LogicalPosition::new(px, py);
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
                window.dispatch_event(event);
            }
        };
        slint::Timer::single_shot(
            std::time::Duration::from_millis(400),
            Box::new(move || {
                click_at(&reopen, 36., rail_height - 32.);
                assert!(!reopen.get_rail_collapsed(), "sidebar reopens on demand");
                finished.set_selected_page(final_page);
                finished.set_player_open(true);
                println!(
                    "PASS: five pages, mouse, keyboard activation, sidebar toggle, and expanded player state."
                );
                let done = finished.clone_strong();
                slint::Timer::single_shot(
                    std::time::Duration::from_millis(350),
                    Box::new(move || {
                        let pixels = done.window().take_snapshot().expect("capture rendered UI");
                        let mut file =
                            std::io::BufWriter::new(std::fs::File::create(&target).unwrap());
                        writeln!(file, "P6\n{} {}\n255", pixels.width(), pixels.height()).unwrap();
                        for pixel in pixels.as_bytes().chunks_exact(4) {
                            file.write_all(&pixel[..3]).unwrap();
                        }
                        file.flush().unwrap();
                        slint::quit_event_loop().unwrap();
                    }) as Box<dyn FnOnce()>,
                );
            }) as Box<dyn FnOnce()>,
        );
        return;
    };
    if let Some(value) = guide_step {
        // The step is applied only after the window has settled, because the
        // shell clears it whenever the page changes and that handler lands on
        // the first tick after show. The capture then waits one more frame, so
        // the card for that step is actually rendered before the snapshot.
        let run: Rc<dyn Fn()> = Rc::new(run);
        slint::Timer::single_shot(std::time::Duration::from_millis(900), move || {
            if scroll {
                stepper
                    .window()
                    .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: slint::LogicalPosition::new(width * 0.7, height * 0.4),
                        delta_x: 0.,
                        delta_y: -2400.,
                    });
            } else {
                stepper.set_guide_step(value);
            }
            let run = run.clone();
            slint::Timer::single_shot(std::time::Duration::from_millis(250), move || run());
        });
    } else {
        slint::Timer::single_shot(std::time::Duration::from_millis(900), run);
    }
    slint::run_event_loop()?;
    Ok(())
}
