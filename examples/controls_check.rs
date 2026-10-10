//! Renderer-level review of every glyph morph phase and status-dot alignment.
use slint::ComponentHandle;
slint::slint! {
    import { Theme } from "../ui/theme.slint";
    import { NavGlyph } from "../ui/glyphs.slint";
    import { StatusBar, Slider, ProgressBar } from "../ui/controls.slint";
    export component ControlsReview inherits Window {
        width: 560px; height: 630px;
        background: Theme.canvas;
        Text { x: 20px; y: 16px; text: "Icon morph: rest / 25% / 50% / 75% / active"; color: Theme.foreground; font-size: 16px; }
        for kind in 5: Rectangle {
            x: 20px; y: 55px + kind * 54px; width: 520px; height: 48px;
            background: Theme.surface;
            for phase in 5: NavGlyph {
                x: 35px + phase * 100px; y: 12px;
                width: 24px; height: 24px;
                kind: kind; morph: phase * 0.25; tint: Theme.accent;
            }
        }
        StatusBar { x: 20px; y: 340px; width: 520px; text: "Paste a YouTube or YouTube Music link."; }
        StatusBar { x: 20px; y: 390px; width: 520px; text: "An actionable error stays aligned, too."; error: true; }
        for value[index] in [0, 10, 50, 100]: Slider {
            x: 30px; y: 450px + index * 36px; width: 500px; height: 24px;
            value: value; label: "Playback position";
        }
        ProgressBar { x: 37px; y: 610px; width: 486px; progress: 0.5; }
    }
}

fn capture(ui: &ControlsReview, output: &str) {
    let pixels = ui.window().take_snapshot().expect("render controls");
    assert_eq!((pixels.width(), pixels.height()), (560, 630));
    image::save_buffer(output, pixels.as_bytes(), 560, 630, image::ColorType::Rgba8)
        .expect("write review image");
    for y in [357, 407] {
        let index = (y * 560 + 35) * 4;
        let rgba = &pixels.as_bytes()[index..index + 4];
        assert!(
            rgba[0] as u16 > rgba[1] as u16 + 50,
            "dot centered at y={y}: {rgba:?}"
        );
    }
    for phase in 0..5 {
        let index = (193 * 560 + 73 + phase * 100) * 4;
        let rgba = &pixels.as_bytes()[index..index + 4];
        assert!(
            rgba[0] as u16 > rgba[1] as u16 + 50,
            "search handle survives phase {phase}: {rgba:?}"
        );
    }
    // Fill begins at the left edge at every value, including 0% and 100%.
    let accent = |x: usize, y: usize| {
        let p = &pixels.as_bytes()[(y * 560 + x) * 4..][..4];
        p[0] as u16 > p[1] as u16 + 50
    };
    for (index, value) in [0, 10, 50, 100].iter().enumerate() {
        let y = 462 + index * 36;
        assert_eq!(accent(42, y), *value > 0, "left origin at {value}%");
        if *value > 0 {
            let end = 37 + 486 * value / 100;
            assert!(accent(end - 5, y), "fill ends at {value}%");
            if *value < 100 {
                assert!(!accent(end + 6, y), "unplayed tail at {value}%");
            }
        }
    }
    assert!(
        accent(42, 612) && !accent(290, 612),
        "download fill starts left"
    );
    println!(
        "PASS: status dots centered, search handles intact; captured 25 glyph states; seek/volume/download fills start left."
    );
    slint::quit_event_loop().unwrap();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args().nth(1).ok_or("Pass an output PNG path")?;
    let ui = ControlsReview::new()?;
    let ready = std::rc::Rc::new(std::cell::Cell::new(false));
    let armed = ready.clone();
    let weak = ui.as_weak();
    let gpu_output = output.clone();
    // FemtoVG snapshots read the back buffer, which is undefined after present.
    // Capture a settled frame before swap; software snapshots render on demand.
    let notified = ui
        .window()
        .set_rendering_notifier(move |state, _| {
            if matches!(state, slint::RenderingState::AfterRendering) && armed.replace(false) {
                if let Some(ui) = weak.upgrade() {
                    capture(&ui, &gpu_output);
                }
            }
        })
        .is_ok();
    ui.show()?;
    let timer_ui = ui.clone_strong();
    slint::Timer::single_shot(std::time::Duration::from_millis(400), move || {
        if notified {
            ready.set(true);
            timer_ui.window().request_redraw();
        } else {
            capture(&timer_ui, &output);
        }
    });
    slint::run_event_loop()?;
    Ok(())
}
