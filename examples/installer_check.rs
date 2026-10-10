//! Installer layout and callback checks. Never installs or changes the machine.
use slint::ComponentHandle;
use std::{cell::Cell, rc::Rc};
slint::slint! { export { SetupWindow } from "../ui/installer/setup.slint"; }
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        return Err("OUTPUT.png WIDTH HEIGHT STEP [check|error]".into());
    }
    let width: u32 = args[2].parse()?;
    let height: u32 = args[3].parse()?;
    let step: i32 = args[4].parse()?;
    let check = args.get(5).is_some_and(|s| s == "check");
    let ui = SetupWindow::new()?;
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    ui.set_updating(true);
    ui.set_install_path(r"C:\Apps\Monolith".into());
    ui.set_step(step);
    ui.set_progress(0.6);
    ui.set_status("Registering Monolith and its uninstaller.".into());
    if args.get(5).is_some_and(|s| s == "error") {
        ui.set_failed(true);
        ui.set_status("Close Monolith (including its tray icon), then retry setup.".into());
    }
    let installs = Rc::new(Cell::new(0));
    let count = installs.clone();
    ui.on_install(move || count.set(count.get() + 1));
    let launches = Rc::new(Cell::new(0));
    let count = launches.clone();
    ui.on_run_app(move || count.set(count.get() + 1));
    let quits = Rc::new(Cell::new(0));
    let count = quits.clone();
    ui.on_quit(move || count.set(count.get() + 1));
    ui.window()
        .set_size(slint::LogicalSize::new(width as f32, height as f32));
    ui.show()?;
    let output = args[1].clone();
    slint::Timer::single_shot(std::time::Duration::from_millis(600), move || {
        if check {
            use slint::platform::{PointerEventButton, WindowEvent};
            let click = || {
                let position = slint::LogicalPosition::new(width as f32 - 85., height as f32 - 39.);
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
            };
            ui.set_step(0);
            click();
            assert_eq!(ui.get_step(), 1);
            let saved = ui.get_install_path();
            ui.set_install_path("".into());
            click();
            assert_eq!(ui.get_step(), 1);
            ui.set_install_path(saved);
            click();
            assert_eq!(ui.get_step(), 2);
            click();
            assert_eq!(installs.get(), 1);
            ui.set_step(3);
            ui.set_failed(true);
            click();
            assert_eq!(installs.get(), 2);
            ui.set_failed(false);
            ui.set_step(4);
            ui.set_launch(false);
            click();
            assert_eq!(quits.get(), 1);
            ui.set_launch(true);
            click();
            assert_eq!(launches.get(), 1);
            println!(
                "PASS: installer navigation, empty-path guard, install/retry, finish choices; no installation."
            );
        }
        let pixels = ui.window().take_snapshot().unwrap();
        image::save_buffer(
            output,
            pixels.as_bytes(),
            pixels.width(),
            pixels.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
        slint::quit_event_loop().unwrap();
    });
    slint::run_event_loop()?;
    Ok(())
}
