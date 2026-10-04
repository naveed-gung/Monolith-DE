//! Renders the crash dialog so its layout can be reviewed, and checks that the
//! report link really is prefilled before anyone sees it.
//!
//! Usage: crash_check OUTPUT.ppm [SUMMARY]

use monolith_desktop::crash;
use slint::ComponentHandle;
use std::io::Write;

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        return Err("Usage: crash_check OUTPUT.ppm [SUMMARY]".into());
    }
    let output = args[1].clone();
    // A realistic panic message, not "boom": a short one would hide the
    // scrolling this dialog exists to provide.
    let summary = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "index out of bounds: the len is 0 but the index is 3".to_string());
    let details = crash::report(&summary);
    let url = crash::issue_url(&summary, &details);

    assert!(
        url.starts_with("https://github.com/"),
        "the report must be a web address"
    );
    assert!(
        url.contains("issues/new?title="),
        "the issue must be prefilled"
    );
    assert!(url.contains("body="), "the report must be prefilled");

    let window = CrashWindow::new()?;
    window.set_summary(summary.into());
    window.set_details(details.into());
    window.set_issue_url(url.into());
    window
        .window()
        .set_size(slint::LogicalSize::new(620., 460.));
    window.show()?;

    let snapshot = window.clone_strong();
    let target = output.clone();
    slint::Timer::single_shot(std::time::Duration::from_millis(700), move || {
        let pixels = snapshot
            .window()
            .take_snapshot()
            .expect("capture rendered UI");
        let mut file = std::io::BufWriter::new(std::fs::File::create(&target).unwrap());
        writeln!(file, "P6\n{} {}\n255", pixels.width(), pixels.height()).unwrap();
        for pixel in pixels.as_bytes().chunks_exact(4) {
            file.write_all(&pixel[..3]).unwrap();
        }
        file.flush().unwrap();
        slint::quit_event_loop().unwrap();
    });
    slint::run_event_loop()?;
    println!("PASS: crash dialog rendered with a prefilled https report link");
    Ok(())
}
