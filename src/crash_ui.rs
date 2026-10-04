//! Shows the crash dialog.
//!
//! This lives beside the binary rather than in the library because the window
//! itself is generated into the binary by `include_modules!`, so only code in
//! that crate can construct it. Everything worth testing stays in
//! `monolith_desktop::crash`.

use slint::ComponentHandle;

/// Shown once the event loop has unwound, so `run` starts a fresh one on a
/// window that is not already half torn down.
pub fn show(summary: String, details: String, url: String) {
    let Ok(window) = crate::CrashWindow::new() else {
        return;
    };
    window.set_summary(summary.into());
    window.set_details(details.into());
    window.set_issue_url(url.into());
    // The browser must not be able to launch anything but a web page, so the
    // link goes through the same checked opener the rest of the app uses.
    window.on_open_issue(move |link| {
        let link = link.to_string();
        std::thread::spawn(move || {
            let _ = monolith_desktop::identity::open(&link);
        });
    });
    window.on_dismissed(|| {
        let _ = slint::quit_event_loop();
    });
    // If even this fails the report is already on disk, so there is nothing
    // left to retry.
    let _ = window.run();
}
