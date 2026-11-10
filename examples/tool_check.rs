//! Real managed-tool install check: downloads yt-dlp, verifies its published
//! SHA-256, swaps it into the managed tools folder and re-probes it.
use monolith_desktop::{tools::{self, Tool}, toolsetup};
use std::sync::atomic::AtomicBool;
fn main() {
    let cancel = AtomicBool::new(false);
    let found = toolsetup::install(Tool::YtDlp, &cancel, |p, t| {
        eprintln!("[{p:>5.1}] {t}");
    })
    .expect("yt-dlp install failed");
    eprintln!("INSTALLED {} managed={} version={}", found.path.display(), found.managed, found.version);
    // Detection must now find it in the managed dir.
    let det = tools::detect(Tool::YtDlp, "").expect("detect failed");
    eprintln!("DETECTED {} managed={} version={}", det.path.display(), det.managed, det.version);
    assert!(det.managed);
    let det2 = tools::detect(Tool::Ffmpeg, "");
    eprintln!("FFMPEG {:?}", det2.map(|f| (f.path, f.version)));
}
